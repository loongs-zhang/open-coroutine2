use crate::blocker::Blocker;
use crate::coroutine::constants::{Syscall, SyscallState};
use crate::coroutine::suspender::SimpleSuspender;
use crate::coroutine::Named;
use crate::pool::task::{Task, TaskImpl};
use crate::scheduler::{Listener, SchedulableCoroutine, Scheduler, SchedulerImpl};
use crossbeam_deque::{Injector, Steal};
use std::fmt::Debug;
use std::panic::UnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Task abstraction and impl.
pub mod task;

/// The `CoroutinePool` abstraction.
pub trait CoroutinePool<'p>: Debug + Default + Named + Listener {
    /// Create a new `CoroutinePool` instance.
    fn new(
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
        blocker: impl Blocker + 'p,
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
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'p,
        param: Option<usize>,
    ) {
        self.submit_raw(TaskImpl::new(
            name.unwrap_or(format!("{}|{}", self.get_name(), uuid::Uuid::new_v4())),
            func,
            param,
        ));
    }

    /// Submit new task to this pool.
    fn submit_raw(&self, task: TaskImpl<'p>);

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

#[allow(missing_docs, box_pointers, dead_code)]
#[derive(Debug)]
pub struct CoroutinePoolImpl<'p> {
    //任务队列
    task_queue: Injector<TaskImpl<'p>>,
    //工作协程组
    workers: SchedulerImpl<'p>,
    //当前协程数
    running: AtomicUsize,
    //当前空闲协程数
    idle: AtomicUsize,
    //最小协程数，即核心协程数
    min_size: AtomicUsize,
    //最大协程数
    max_size: AtomicUsize,
    //非核心协程的最大存活时间，单位ns
    keep_alive_time: AtomicU64,
    //阻滞器
    blocker: Box<dyn Blocker + 'p>,
}

impl Named for CoroutinePoolImpl<'_> {
    fn get_name(&self) -> &str {
        self.workers.get_name()
    }
}

impl Default for CoroutinePoolImpl<'_> {
    fn default() -> Self {
        let blocker = crate::blocker::SleepBlocker {};
        Self::new(crate::coroutine::DEFAULT_STACK_SIZE, 0, 65536, 0, blocker)
    }
}

impl Listener for CoroutinePoolImpl<'_> {
    fn on_suspend(&self, _: u64, _: &SchedulableCoroutine) {
        _ = self.grow();
    }

    fn on_syscall(&self, _: u64, _: &SchedulableCoroutine, _: Syscall, _: SyscallState) {
        _ = self.grow();
    }
}

impl<'p> CoroutinePool<'p> for CoroutinePoolImpl<'p> {
    #[allow(box_pointers)]
    fn new(
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
        blocker: impl Blocker + 'p,
    ) -> Self
    where
        Self: Sized,
    {
        CoroutinePoolImpl {
            workers: SchedulerImpl::new(uuid::Uuid::new_v4().to_string(), stack_size),
            running: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            min_size: AtomicUsize::new(min_size),
            max_size: AtomicUsize::new(max_size),
            task_queue: Injector::default(),
            keep_alive_time: AtomicU64::new(keep_alive_time),
            blocker: Box::new(blocker),
        }
    }

    #[allow(box_pointers)]
    fn init(&mut self) {
        let listener = unsafe { Box::from_raw(self) };
        self.workers.add_raw_listener(listener);
    }

    fn set_stack_size(&self, stack_size: usize) {
        self.workers.set_stack_size(stack_size);
    }

    fn set_min_size(&self, min_size: usize) {
        self.min_size.store(min_size, Ordering::Release);
    }

    fn get_min_size(&self) -> usize {
        self.min_size.load(Ordering::Acquire)
    }

    fn get_running_size(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }

    fn get_idle_size(&self) -> usize {
        self.idle.load(Ordering::Acquire)
    }

    fn set_max_size(&self, max_size: usize) {
        self.max_size.store(max_size, Ordering::Release);
    }

    fn get_max_size(&self) -> usize {
        self.max_size.load(Ordering::Acquire)
    }

    fn set_keep_alive_time(&self, keep_alive_time: u64) {
        self.keep_alive_time
            .store(keep_alive_time, Ordering::Release);
    }

    fn get_keep_alive_time(&self) -> u64 {
        self.keep_alive_time.load(Ordering::Acquire)
    }

    fn submit_raw(&self, task: TaskImpl<'p>) {
        self.task_queue.push(task);
    }

    fn pop(&self) -> Option<TaskImpl> {
        // Fast path, if len == 0, then there are no values
        if self.is_empty() {
            return None;
        }
        loop {
            match self.task_queue.steal() {
                Steal::Success(item) => return Some(item),
                Steal::Retry => continue,
                Steal::Empty => return None,
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.task_queue.is_empty()
    }

    fn grow(&self) -> std::io::Result<()> {
        // if self.is_empty() {
        //     return Ok(());
        // }
        // if self.running.load(Ordering::Acquire) >= self.get_max_size() {
        //     return Ok(());
        // }
        // let create_time = open_coroutine_timer::now();
        // _ = self.workers.submit(
        //     move |suspender, ()| {
        //         loop {
        //             match self.task_queue.steal() {
        //                 Steal::Empty => {
        //                     let running = self.running.load(Ordering::Acquire);
        //                     if open_coroutine_timer::now().saturating_sub(create_time)
        //                         >= self.get_keep_alive_time()
        //                         && running > self.get_min_size()
        //                     {
        //                         //回收worker协程
        //                         _ = self.running.fetch_sub(1, Ordering::Release);
        //                         _ = self.idle.fetch_sub(1, Ordering::Release);
        //                         return;
        //                     }
        //                     _ = self.idle.fetch_add(1, Ordering::Release);
        //                     match self.idle.load(Ordering::Acquire).cmp(&running) {
        //                         //让出CPU给下一个协程
        //                         std::cmp::Ordering::Less => suspender.suspend(),
        //                         //避免CPU在N个无任务的协程中空轮询
        //                         std::cmp::Ordering::Equal => {
        //                             self.blocker.block(std::time::Duration::from_millis(1));
        //                         }
        //                         std::cmp::Ordering::Greater => {
        //                             unreachable!("should never execute to here");
        //                         }
        //                     }
        //                 }
        //                 Steal::Success(task) => {
        //                     _ = self.idle.fetch_sub(1, Ordering::Release);
        //                     let _task_name = task.get_name();
        //                     let _result = task.run();
        //                     // assert!(
        //                     //     RESULT_TABLE.insert(task_name, result).is_none(),
        //                     //     "The previous result was not retrieved in a timely manner"
        //                     // );
        //                 }
        //                 Steal::Retry => continue,
        //             }
        //         }
        //     },
        //     None,
        // )?;
        // _ = self.running.fetch_add(1, Ordering::Release);
        Ok(())
    }
}
