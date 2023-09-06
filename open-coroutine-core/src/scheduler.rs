use crate::coroutine::constants::Syscall;
use crate::coroutine::suspender::Suspender;
use crate::coroutine::{CoroutineImpl, Current};
use std::fmt::Debug;
use std::time::Duration;

/// A type for Scheduler.
pub type SchedulableCoroutine = CoroutineImpl<'static, (), (), ()>;

/// A trait implemented for schedulers.
pub trait Scheduler<'s>: Debug + Current<'s> {
    /// Set the default stack stack size for the coroutines in this scheduler.
    /// If it has not been set, it will be `crate::coroutine::DEFAULT_STACK_SIZE`.
    fn set_stack_size(&self, stack_size: usize);

    /// Submit a closure to new coroutine, then the coroutine will be push into ready queue.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler, but only allow one thread to execute scheduling.
    fn submit(
        &self,
        f: impl FnOnce(&dyn Suspender<Resume = (), Yield = ()>, ()),
        stack_size: Option<usize>,
    );

    /// Resume a coroutine from the system call table to the ready queue,
    /// it's generally only required for framework level crates.
    ///
    /// If we can't find the coroutine, nothing happens.
    fn try_resume(&self, co_name: &str);

    /// Schedule the coroutines.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler, but only allow one thread to execute scheduling.
    fn try_schedule(&mut self) {
        _ = self.try_timeout_schedule(Duration::MAX.as_secs());
    }

    /// Try scheduling the coroutines for up to `dur`.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler, but only allow one thread to execute scheduling.
    fn try_timed_schedule(&mut self, dur: Duration) -> u64 {
        self.try_timeout_schedule(crate::get_timeout_time(dur))
    }

    /// Attempt to schedule the coroutines before the `timeout_time` timestamp.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler, but only allow one thread to execute scheduling.
    /// Returns the left time in ns.
    fn try_timeout_schedule(&mut self, timeout_time: u64) -> u64;

    /// Add a listener to this scheduler.
    fn add_listener(&mut self, listener: impl Listener);
}

/// A trait implemented for schedulers, mainly used for monitoring.
pub trait Listener {
    /// callback when a coroutine is created.
    /// This will be called by `Scheduler` when a coroutine is created.
    fn on_create(&self, _: &SchedulableCoroutine) {}

    /// callback when a coroutine is suspended.
    /// This will be called by `Scheduler` when a coroutine is suspended.
    fn on_suspend(&self, _: &SchedulableCoroutine) {}

    /// callback when a coroutine enters syscall.
    /// This will be called by `Scheduler` when a coroutine enters syscall.
    fn on_syscall(&self, _: &SchedulableCoroutine, _: Syscall) {}

    /// callback when a coroutine is finished.
    /// This will be called by `Scheduler` when a coroutine is finished.
    fn on_finish(&self, _: &SchedulableCoroutine) {}
}
