use crate::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::{DelaySuspender, Suspender, SuspenderImpl};
use crate::coroutine::{Coroutine, Current, Named, StateMachine, COROUTINE};
use corosensei::stack::DefaultStack;
use corosensei::{CoroutineResult, ScopedCoroutine};
use std::cell::Cell;
use std::cmp::Ordering;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::panic::{AssertUnwindSafe, UnwindSafe};

#[allow(box_pointers)]
pub struct CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
{
    name: String,
    pub(crate) inner: ScopedCoroutine<'c, Param, Yield, std::thread::Result<Return>, DefaultStack>,
    state: Cell<CoroutineState<Yield, Return>>,
    local: CoroutineLocal<'c>,
}

impl<Param, Yield, Return> Drop for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
{
    #[allow(box_pointers)]
    fn drop(&mut self) {
        //for test_yield case
        if self.inner.started() && !self.inner.done() {
            unsafe { self.inner.force_reset() };
        }
    }
}

impl<Param, Yield, Return> Debug for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + Debug + UnwindSafe,
    Return: Copy + Eq + PartialEq + Debug + UnwindSafe,
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
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
{
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(current: &CoroutineImpl<'c, Param, Yield, Return>) {
        COROUTINE.with(|c| c.set(current as *const _ as *const c_void));
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

impl<Param, Yield, Return> Eq for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
}

impl<Param, Yield, Return> PartialEq<Self> for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(&other.name)
    }
}

impl<Param, Yield, Return> Ord for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

impl<Param, Yield, Return> PartialOrd<Self> for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Param, Yield, Return> Named for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn get_name(&self) -> &str {
        &self.name
    }
}

impl<'c, Param, Yield, Return> Coroutine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + Debug + UnwindSafe,
    Return: Copy + Eq + PartialEq + Debug + UnwindSafe,
{
    type Resume = Param;
    type Yield = Yield;
    type Return = Return;

    #[allow(box_pointers)]
    fn new<F>(name: String, f: F, stack_size: usize) -> std::io::Result<Self>
    where
        F: FnOnce(
            &dyn Suspender<Resume = Self::Resume, Yield = Self::Yield>,
            Self::Resume,
        ) -> Self::Return,
        F: UnwindSafe,
        F: 'c,
        Self: Sized,
    {
        let stack = DefaultStack::new(stack_size.max(crate::coroutine::page_size()))?;
        let inner = ScopedCoroutine::with_stack(stack, |y, p| {
            let suspender = SuspenderImpl(y);
            SuspenderImpl::<Param, Yield>::init_current(&suspender);
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| f(&suspender, p)));
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
        if let CoroutineState::Complete(r) = self.state() {
            return Ok(CoroutineState::Complete(r));
        }
        self.running()?;
        Self::init_current(self);
        #[allow(box_pointers)]
        let r = match self.inner.resume(arg) {
            CoroutineResult::Yield(y) => {
                let current = self.state();
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
            #[allow(box_pointers)]
            CoroutineResult::Return(result) => {
                if let Ok(returns) = result {
                    self.complete(returns)?;
                    CoroutineState::Complete(returns)
                } else {
                    let error = result.unwrap_err();
                    let message = error
                        .downcast_ref::<&'static str>()
                        .unwrap_or(&"coroutine failed without message");
                    self.error(message)?;
                    CoroutineState::Error(message)
                }
            }
        };
        Self::clean_current();
        Ok(r)
    }

    fn local(&self) -> &CoroutineLocal<'c> {
        &self.local
    }
}

impl<'c, Param, Yield, Return> StateMachine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,

    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn state(&self) -> CoroutineState<Self::Yield, Self::Return> {
        self.state.get()
    }

    fn ready(&self) -> std::io::Result<()> {
        let current = self.state();
        match current {
            CoroutineState::Created => {
                self.state.set(CoroutineState::Ready);
                return Ok(());
            }
            CoroutineState::Suspend(_, timestamp) => {
                if timestamp <= open_coroutine_timer::now() {
                    self.state.set(CoroutineState::Ready);
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
                CoroutineState::<Yield, Return>::Ready
            ),
        ))
    }

    fn running(&self) -> std::io::Result<()> {
        let current = self.state();
        match current {
            CoroutineState::Created | CoroutineState::Ready => {
                self.state.set(CoroutineState::Running);
                return Ok(());
            }
            CoroutineState::Suspend(_, timestamp) => {
                if timestamp <= open_coroutine_timer::now() {
                    self.state.set(CoroutineState::Running);
                    return Ok(());
                }
            }
            CoroutineState::SystemCall(_, _, _) => return Ok(()),
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
        let current = self.state();
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
        let current = self.state();
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

    fn syscall_resume(&self) -> std::io::Result<()> {
        let current = self.state();
        if let CoroutineState::SystemCall(_, _, SyscallState::Finished | SyscallState::Timeout) =
            current
        {
            self.state.set(CoroutineState::Running);
            return Ok(());
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

    fn complete(&self, val: Self::Return) -> std::io::Result<()> {
        let current = self.state();
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

    fn error(&self, val: &'static str) -> std::io::Result<()> {
        let current = self.state();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Error(val));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Error(val)
            ),
        ))
    }
}
