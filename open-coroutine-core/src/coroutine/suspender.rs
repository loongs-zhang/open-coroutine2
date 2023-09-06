use crate::coroutine::Current;
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::time::Duration;

/// A trait implemented for suspend the execution of the coroutine.
pub trait Suspender<'s>: Current<'s> {
    /// The type of value this coroutine accepts as a resume argument.
    type Resume;

    /// The type of value the coroutine yields.
    type Yield;

    /// Suspend the execution of the coroutine.
    fn suspend_with(&self, arg: Self::Yield) -> Self::Resume;
}

/// A trait implemented for suspend the execution of the coroutine.
pub trait SimpleSuspender<'s>: Suspender<'s, Yield = ()> {
    /// Suspend the execution of the coroutine.
    fn suspend(&self) -> Self::Resume;
}

impl<'s, SimpleSuspenderImpl: Suspender<'s, Yield = ()>> SimpleSuspender<'s>
    for SimpleSuspenderImpl
{
    fn suspend(&self) -> Self::Resume {
        self.suspend_with(())
    }
}

/// A trait implemented for suspend the execution of the coroutine.
pub trait DelaySuspender<'s>: Suspender<'s> {
    /// Delay the execution of the coroutine.
    fn delay_with(&self, arg: Self::Yield, delay: Duration) -> Self::Resume;

    /// When can a coroutine be resumed.
    fn timestamp() -> u64;
}

thread_local! {
    static TIMESTAMP: Cell<u64> = Cell::new(0);
}

impl<'s, DelaySuspenderImpl: Suspender<'s>> DelaySuspender<'s> for DelaySuspenderImpl {
    fn delay_with(&self, arg: Self::Yield, delay: Duration) -> Self::Resume {
        TIMESTAMP.with(|c| {
            c.set(
                u64::try_from(delay.as_nanos())
                    .map(|ns| crate::now().saturating_add(ns))
                    .unwrap_or(u64::MAX),
            );
        });
        self.suspend_with(arg)
    }

    fn timestamp() -> u64 {
        TIMESTAMP.with(Cell::take)
    }
}

/// A trait implemented for suspend the execution of the coroutine.
pub trait SimpleDelaySuspender<'s>: DelaySuspender<'s, Yield = ()> {
    /// Delay the execution of the coroutine.
    fn delay(&self, delay: Duration) -> Self::Resume;
}

impl<'s, SimpleDelaySuspenderImpl: DelaySuspender<'s, Yield = ()>> SimpleDelaySuspender<'s>
    for SimpleDelaySuspenderImpl
{
    fn delay(&self, delay: Duration) -> Self::Resume {
        self.delay_with((), delay)
    }
}

thread_local! {
    static SUSPENDER: Cell<*const c_void> = Cell::new(std::ptr::null());
}

#[cfg(all(feature = "korosensei", not(feature = "boost")))]
pub use korosensei::SuspenderImpl;
#[allow(missing_docs)]
#[cfg(feature = "korosensei")]
mod korosensei {
    use super::*;
    use corosensei::Yielder;

    pub struct SuspenderImpl<'s, Param, Yield>(&'s Yielder<Param, Yield>);

    impl<Param, Yield> Debug for SuspenderImpl<'_, Param, Yield> {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Suspender").finish()
        }
    }

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

    impl<'s, Param, Yield> SuspenderImpl<'s, Param, Yield> {
        pub fn new(yielder: &'s Yielder<Param, Yield>) -> Self {
            SuspenderImpl(yielder)
        }
    }
}

#[cfg(feature = "boost")]
mod boost {}
