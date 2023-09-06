use crate::coroutine::Current;
use std::cell::Cell;
use std::ffi::c_void;
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
    fn delay_with(&self, arg: Self::Yield, delay: Duration) -> Self::Resume {
        self.until_with(arg, crate::get_timeout_time(delay))
    }

    /// Delay the execution of the coroutine.
    fn until_with(&self, arg: Self::Yield, timestamp: u64) -> Self::Resume;

    /// When can a coroutine be resumed.
    fn timestamp() -> u64;
}

thread_local! {
    static TIMESTAMP: Cell<u64> = Cell::new(0);
}

impl<'s, DelaySuspenderImpl: Suspender<'s>> DelaySuspender<'s> for DelaySuspenderImpl {
    fn until_with(&self, arg: Self::Yield, timestamp: u64) -> Self::Resume {
        TIMESTAMP.with(|c| c.set(timestamp));
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

    /// Delay the execution of the coroutine.
    fn until(&self, timestamp: u64) -> Self::Resume;
}

impl<'s, SimpleDelaySuspenderImpl: DelaySuspender<'s, Yield = ()>> SimpleDelaySuspender<'s>
    for SimpleDelaySuspenderImpl
{
    fn delay(&self, delay: Duration) -> Self::Resume {
        self.delay_with((), delay)
    }

    fn until(&self, timestamp: u64) -> Self::Resume {
        self.until_with((), timestamp)
    }
}

thread_local! {
    pub(crate) static SUSPENDER: Cell<*const c_void> = Cell::new(std::ptr::null());
}
