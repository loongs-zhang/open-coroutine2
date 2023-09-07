use crate::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::{DelaySuspender, Suspender, SUSPENDER};
use crate::coroutine::{Coroutine, Current, StateMachine, COROUTINE};
use corosensei::stack::DefaultStack;
use corosensei::Yielder;
use corosensei::{CoroutineResult, ScopedCoroutine};
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};

#[allow(missing_debug_implementations)]
pub struct SuspenderImpl<'s, Param, Yield>(&'s Yielder<Param, Yield>);

impl<'s, Param, Yield> Current<'s> for SuspenderImpl<'s, Param, Yield> {
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(suspender: &SuspenderImpl<'s, Param, Yield>) {
        SUSPENDER.with(|c| c.set(suspender as *const _ as *const c_void));
    }

    fn current() -> Option<&'s Self> {
        SUSPENDER.with(|boxed| {
            let ptr = boxed.get();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr).cast::<SuspenderImpl<'s, Param, Yield>>() })
            }
        })
    }

    fn clean_current() {
        SUSPENDER.with(|boxed| boxed.set(std::ptr::null()));
    }
}

impl<'s, Param, Yield> Suspender<'s> for SuspenderImpl<'s, Param, Yield> {
    type Resume = Param;
    type Yield = Yield;

    fn suspend_with(&self, arg: Self::Yield) -> Self::Resume {
        SuspenderImpl::<Param, Yield>::clean_current();
        let param = self.0.suspend(arg);
        SuspenderImpl::<Param, Yield>::init_current(self);
        param
    }
}

pub struct CoroutineImpl<'c, Param, Yield, Return>
where
    Yield: Copy + Eq + PartialEq,
    Return: Copy + Eq + PartialEq,
{
    name: String,
    inner: ScopedCoroutine<'c, Param, Yield, Return, DefaultStack>,
    state: Cell<CoroutineState<Yield, Return>>,
    local: CoroutineLocal<'c>,
}

impl<Param, Yield, Return> Drop for CoroutineImpl<'_, Param, Yield, Return>
where
    Yield: Copy + Eq + PartialEq,
    Return: Copy + Eq + PartialEq,
{
    fn drop(&mut self) {
        //for test_yield case
        if self.inner.started() && !self.inner.done() {
            unsafe { self.inner.force_reset() };
        }
    }
}

impl<Param, Yield, Return> Debug for CoroutineImpl<'_, Param, Yield, Return>
where
    Yield: Copy + Eq + PartialEq + Debug,
    Return: Copy + Eq + PartialEq + Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coroutine")
            .field("name", &self.name)
            .field("status", &self.state)
            .field("local", &self.local)
            .finish()
    }
}

impl<'c, Param, Yield, Return> Current<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Yield: Copy + Eq + PartialEq,
    Return: Copy + Eq + PartialEq,
{
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(suspender: &CoroutineImpl<'c, Param, Yield, Return>) {
        COROUTINE.with(|c| c.set(suspender as *const _ as *const c_void));
    }

    fn current() -> Option<&'c Self> {
        COROUTINE.with(|boxed| {
            let ptr = boxed.get();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr).cast::<CoroutineImpl<'c, Param, Yield, Return>>() })
            }
        })
    }

    fn clean_current() {
        COROUTINE.with(|boxed| boxed.set(std::ptr::null()));
    }
}

impl<'c, Param, Yield, Return> Coroutine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Yield: Copy + Eq + PartialEq + Debug,
    Return: Copy + Eq + PartialEq + Debug,
{
    type Resume = Param;
    type Yield = Yield;
    type Return = Return;

    fn new<F>(name: String, f: F, stack_size: usize) -> std::io::Result<Self>
    where
        F: FnOnce(
            &dyn Suspender<Resume = Self::Resume, Yield = Self::Yield>,
            Self::Resume,
        ) -> Self::Return,
        F: 'c,
        Self: Sized,
    {
        let stack = DefaultStack::new(stack_size.max(crate::coroutine::page_size()))?;
        let inner = ScopedCoroutine::with_stack(stack, |y, p| {
            let suspender = SuspenderImpl(y);
            SuspenderImpl::<Param, Yield>::init_current(&suspender);
            let r = f(&suspender, p);
            SuspenderImpl::<Param, Yield>::clean_current();
            r
        });
        Ok(CoroutineImpl {
            name,
            inner,
            state: Cell::new(CoroutineState::Created),
            local: CoroutineLocal::default(),
        })
    }

    fn resume_with(
        &mut self,
        arg: Self::Resume,
    ) -> std::io::Result<CoroutineState<Self::Yield, Self::Return>> {
        if let Some(r) = self.get_result() {
            return Ok(CoroutineState::Complete(r));
        }
        self.running()?;
        CoroutineImpl::<Param, Yield, Return>::init_current(self);
        let r = match self.inner.resume(arg) {
            CoroutineResult::Yield(y) => {
                let current = self.state.get();
                match current {
                    CoroutineState::Running => {
                        let timestamp = SuspenderImpl::<Yield, Param>::timestamp();
                        let new_state = CoroutineState::Suspend(y, timestamp);
                        self.suspend(y, timestamp)?;
                        new_state
                    }
                    CoroutineState::SystemCall(_, syscall, state) => {
                        self.syscall(y, syscall, state)?;
                        CoroutineState::SystemCall(y, syscall, state)
                    }
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Other,
                            format!("{} unexpected state {current}", self.name),
                        ))
                    }
                }
            }
            CoroutineResult::Return(r) => {
                self.complete(r)?;
                CoroutineState::Complete(r)
            }
        };
        CoroutineImpl::<Param, Yield, Return>::clean_current();
        Ok(r)
    }

    fn get_name(&self) -> &str {
        &self.name
    }

    fn local(&self) -> &CoroutineLocal<'c> {
        &self.local
    }
}

impl<'c, Param, Yield, Return> StateMachine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Return: Copy + Debug + Eq + PartialEq,
    Yield: Copy + Debug + Eq + PartialEq,
{
    fn get_result(&self) -> Option<Self::Return> {
        match self.state.get() {
            CoroutineState::Complete(r) => Some(r),
            _ => None,
        }
    }

    fn ready(&self) -> std::io::Result<()> {
        let current = self.state.get();
        if CoroutineState::Created == current {
            self.state.set(CoroutineState::Ready);
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Ready
            ),
        ))
    }

    fn running(&self) -> std::io::Result<()> {
        let current = self.state.get();
        match current {
            CoroutineState::Created
            | CoroutineState::Ready
            | CoroutineState::SystemCall(_, _, SyscallState::Finished) => {
                self.state.set(CoroutineState::Running);
                return Ok(());
            }
            CoroutineState::Suspend(_, timestamp) => {
                if timestamp <= open_coroutine_timer::now() {
                    self.state.set(CoroutineState::Running);
                    return Ok(());
                }
            }
            _ => {}
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Running
            ),
        ))
    }

    fn suspend(&self, val: Self::Yield, timestamp: u64) -> std::io::Result<()> {
        let current = self.state.get();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Suspend(val, timestamp));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Suspend(val, timestamp)
            ),
        ))
    }

    fn syscall(
        &self,
        val: Self::Yield,
        syscall: Syscall,
        syscall_state: SyscallState,
    ) -> std::io::Result<()> {
        let current = self.state.get();
        match current {
            CoroutineState::Running => {
                self.state
                    .set(CoroutineState::SystemCall(val, syscall, syscall_state));
                return Ok(());
            }
            CoroutineState::SystemCall(_, original_syscall, _) => {
                if original_syscall == syscall {
                    self.state
                        .set(CoroutineState::SystemCall(val, syscall, syscall_state));
                    return Ok(());
                }
            }
            _ => {}
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::SystemCall(val, syscall, syscall_state)
            ),
        ))
    }

    fn complete(&self, val: Self::Return) -> std::io::Result<()> {
        let current = self.state.get();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Complete(val));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Complete(val)
            ),
        ))
    }
}
