use crate::coroutine::constants::CoroutineState;
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::{DelaySuspender, Suspender, SUSPENDER};
use crate::coroutine::{Coroutine, Current, COROUTINE};
use corosensei::stack::DefaultStack;
use corosensei::Yielder;
use corosensei::{CoroutineResult, ScopedCoroutine};
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};

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

    fn resume_with(&mut self, arg: Self::Resume) -> CoroutineState<Self::Yield, Self::Return> {
        let mut current = self.state.get_mut();
        match current {
            CoroutineState::Created | CoroutineState::Ready | CoroutineState::Suspend(_, 0) => {
                self.state.set(CoroutineState::Running);
            }
            CoroutineState::Complete(r) => return CoroutineState::Complete(*r),
            _ => panic!("{} unexpected state {current}", self.name),
        }
        CoroutineImpl::<Param, Yield, Return>::init_current(self);
        let r = match self.inner.resume(arg) {
            CoroutineResult::Yield(y) => {
                current = self.state.get_mut();
                match current {
                    CoroutineState::Running => {
                        let new_state =
                            CoroutineState::Suspend(y, SuspenderImpl::<Yield, Param>::timestamp());
                        let previous = self.state.replace(new_state);
                        assert_eq!(
                            CoroutineState::Running,
                            previous,
                            "{} unexpected state {}",
                            self.get_name(),
                            previous
                        );
                        new_state
                    }
                    CoroutineState::SystemCall(val, syscall, state) => {
                        CoroutineState::SystemCall(*val, *syscall, *state)
                    }
                    _ => panic!("{} unexpected state {current}", self.name),
                }
            }
            CoroutineResult::Return(r) => {
                let state = CoroutineState::Complete(r);
                let current = self.state.replace(state);
                assert_eq!(
                    CoroutineState::Running,
                    current,
                    "{} unexpected state {}",
                    self.get_name(),
                    current
                );
                state
            }
        };
        CoroutineImpl::<Param, Yield, Return>::clean_current();
        r
    }

    fn get_name(&self) -> &str {
        &self.name
    }

    fn local(&self) -> &CoroutineLocal<'c> {
        &self.local
    }
}
