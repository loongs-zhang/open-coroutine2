use crate::blocker::Blocker;
use crate::coroutine::Named;
use crate::pool::task::{Task, TaskImpl};
use std::panic::UnwindSafe;

/// Task abstraction and impl.
pub mod task;

/// The `CoroutinePool` abstraction.
pub trait CoroutinePool: Named + Default {
    /// Create a new `CoroutinePool` instance.
    fn new(
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
        blocker: impl Blocker,
    ) -> Self
    where
        Self: Sized;

    /// Extension points within the open-coroutine framework.
    fn init(&mut self);

    /// Set the default stack stack size for the coroutines in this pool.
    /// If it has not been set, it will be `crate::coroutine::DEFAULT_STACK_SIZE`.
    fn set_stack_size(&self, stack_size: usize);

    /// Set the minimum number of coroutines to run in this pool.
    fn set_min_size(&self, min_size: usize);

    /// Get the minimum number of coroutines to run in this pool.
    fn get_min_size(&self) -> usize;

    /// Gets the number of coroutines currently running in this pool.
    fn get_running_size(&self) -> usize;

    /// Gets the number of currently idle coroutines in this pool.
    fn get_idle_size(&self) -> usize;

    /// Set the maximum number of coroutines to run in this pool.
    fn set_max_size(&self, max_size: usize);

    /// Get the maximum number of coroutines to run in this pool.
    fn get_max_size(&self) -> usize;

    /// Set the maximum idle time for coroutines running in this pool.
    /// `keep_alive_time` has `ns` units.
    fn set_keep_alive_time(&self, keep_alive_time: u64);

    /// Get the maximum idle time for coroutines running in this pool.
    /// Returns in `ns` units.
    fn get_keep_alive_time(&self) -> u64;

    /// Submit new task to this pool.
    fn submit(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'static,
        param: Option<usize>,
    ) {
        self.submit_raw(TaskImpl::new(
            name.unwrap_or(format!("{}|{}", self.get_name(), uuid::Uuid::new_v4())),
            func,
            param,
        ));
    }

    /// Submit new task to this pool.
    fn submit_raw(&self, task: impl Task);

    /// pop a task
    fn pop(&self) -> Option<TaskImpl>;

    /// Returns `true` if the task queue is empty.
    fn is_empty(&self) -> bool;

    /// Create a coroutine in this pool.
    ///
    /// # Errors
    /// if create failed.
    fn grow(&self) -> std::io::Result<()>;
}
