use crate::blocker::Blocker;
use crate::coroutine::suspender::SimpleSuspender;
use crate::coroutine::{Current, Named};
use crate::pool::creator::CoroutineCreator;
use crate::pool::task::{Task, TaskImpl};
use crate::scheduler::{Scheduler, SchedulerImpl};
use crossbeam_deque::{Injector, Steal};
use dashmap::DashMap;
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::Debug;
use std::panic::UnwindSafe;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Task abstraction and impl.
pub mod task;

mod creator;

/// The `CoroutinePool` abstraction.
pub trait CoroutinePool<'p>: Debug + Default + Named + Current<'p> {
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

    /// Submit a new task to this pool.
    ///
    /// Allow multiple threads to concurrently submit task to the pool,
    /// but only allow one thread to execute scheduling.
    fn submit(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'p,
        param: Option<usize>,
    ) -> &str {
        self.submit_raw(TaskImpl::new(
            name.unwrap_or(format!("{}|{}", self.get_name(), uuid::Uuid::new_v4())),
            func,
            param,
        ))
    }

    /// Submit new task to this pool.
    ///
    /// Allow multiple threads to concurrently submit task to the pool,
    /// but only allow one thread to execute scheduling.
    fn submit_raw(&self, task: TaskImpl<'p>) -> &str;

    /// pop a task
    fn pop(&self) -> Option<TaskImpl>;

    /// Returns `true` if the task queue is empty.
    fn is_empty(&self) -> bool;

    /// Create a coroutine in this pool.
    ///
    /// # Errors
    /// if create failed.
    fn grow(&self) -> std::io::Result<()>;

    /// Schedule the tasks.
    ///
    /// Allow multiple threads to concurrently submit task to the pool,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// see `try_timeout_schedule`.
    fn try_schedule(&mut self) -> std::io::Result<()> {
        _ = self.try_timeout_schedule(Duration::MAX.as_secs())?;
        Ok(())
    }

    /// Try scheduling the tasks for up to `dur`.
    ///
    /// Allow multiple threads to concurrently submit task to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// see `try_timeout_schedule`.
    fn try_timed_schedule(&mut self, dur: Duration) -> std::io::Result<u64> {
        self.try_timeout_schedule(open_coroutine_timer::get_timeout_time(dur))
    }

    /// Attempt to schedule the tasks before the `timeout_time` timestamp.
    ///
    /// Allow multiple threads to concurrently submit task to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// Returns the left time in ns.
    ///
    /// # Errors
    /// if change to ready fails.
    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64>;

    /// Attempt to obtain task results with the given `task_name`.
    fn try_get_result(&self, task_name: &str) -> Option<(String, Result<Option<usize>, &str>)>;

    /// Use the given 'task_name' to obtain task results, and if no results are found,
    /// block the current thread for `wait_timeout`.
    ///
    /// # Errors
    /// if timeout
    #[allow(clippy::type_complexity)]
    fn wait_result(
        &self,
        task_name: &str,
        wait_timeout: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>>;

    /// Submit a new task to this pool and wait for the task to complete.
    ///
    /// # Errors
    /// see `wait_result`
    #[allow(clippy::type_complexity)]
    fn submit_and_wait(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'p,
        param: Option<usize>,
        wait_timeout: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>> {
        let task_name = self.submit(name, func, param);
        self.wait_result(task_name, wait_timeout)
    }
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
    //任务执行结果
    results: DashMap<String, Result<Option<usize>, &'p str>>,
    //正在等待结果的
    waits: DashMap<&'p str, Arc<(Mutex<bool>, Condvar)>>,
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

thread_local! {
    static COROUTINE_POOL: Cell<*const c_void> = Cell::new(std::ptr::null());
}

impl<'p> Current<'p> for CoroutinePoolImpl<'p> {
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(current: &Self)
    where
        Self: Sized,
    {
        COROUTINE_POOL.with(|c| c.set(current as *const _ as *const c_void));
    }

    fn current() -> Option<&'p Self>
    where
        Self: Sized,
    {
        COROUTINE_POOL.with(|boxed| {
            let ptr = boxed.get();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr).cast::<CoroutinePoolImpl<'p>>() })
            }
        })
    }

    fn clean_current()
    where
        Self: Sized,
    {
        COROUTINE_POOL.with(|boxed| boxed.set(std::ptr::null()));
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
        let mut pool = CoroutinePoolImpl {
            workers: SchedulerImpl::new(uuid::Uuid::new_v4().to_string(), stack_size),
            running: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
            min_size: AtomicUsize::new(min_size),
            max_size: AtomicUsize::new(max_size),
            task_queue: Injector::default(),
            keep_alive_time: AtomicU64::new(keep_alive_time),
            blocker: Box::new(blocker),
            results: DashMap::new(),
            waits: DashMap::new(),
        };
        pool.init();
        pool
    }

    #[allow(box_pointers)]
    fn init(&mut self) {
        self.workers.add_listener(CoroutineCreator());
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

    #[allow(box_pointers)]
    fn submit_raw(&self, task: TaskImpl<'p>) -> &str {
        let task_name = Box::leak(Box::from(task.get_name()));
        self.task_queue.push(task);
        task_name
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
        if self.is_empty() {
            return Ok(());
        }
        if self.running.load(Ordering::Acquire) >= self.get_max_size() {
            return Ok(());
        }
        let create_time = open_coroutine_timer::now();
        self.workers.submit(
            move |suspender, ()| {
                loop {
                    let pool = Self::current().expect("current pool not found");
                    match pool.pop() {
                        None => {
                            let running = pool.running.load(Ordering::Acquire);
                            if open_coroutine_timer::now().saturating_sub(create_time)
                                >= pool.get_keep_alive_time()
                                && running > pool.get_min_size()
                            {
                                //回收worker协程
                                _ = pool.running.fetch_sub(1, Ordering::Release);
                                _ = pool.idle.fetch_sub(1, Ordering::Release);
                                return;
                            }
                            _ = pool.idle.fetch_add(1, Ordering::Release);
                            match pool.idle.load(Ordering::Acquire).cmp(&running) {
                                //让出CPU给下一个协程
                                std::cmp::Ordering::Less => suspender.suspend(),
                                //避免CPU在N个无任务的协程中空轮询
                                std::cmp::Ordering::Equal => {
                                    #[allow(box_pointers)]
                                    pool.blocker.block(Duration::from_millis(1));
                                }
                                std::cmp::Ordering::Greater => {
                                    unreachable!("should never execute to here");
                                }
                            }
                        }
                        Some(task) => {
                            _ = pool.idle.fetch_sub(1, Ordering::Release);
                            let (task_name, result) = task.run();
                            assert!(
                                pool.results.insert(task_name.clone(), result).is_none(),
                                "The previous result was not retrieved in a timely manner"
                            );
                            if let Some(arc) = pool.waits.get(&*task_name) {
                                let (lock, cvar) = &**arc;
                                let mut pending = lock.lock().unwrap();
                                *pending = false;
                                // Notify the condvar that the value has changed.
                                cvar.notify_one();
                            }
                        }
                    }
                }
            },
            None,
        )?;
        _ = self.running.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64> {
        Self::init_current(self);
        self.grow()?;
        let result = self.workers.try_timeout_schedule(timeout_time);
        Self::clean_current();
        result
    }

    fn try_get_result(&self, task_name: &str) -> Option<(String, Result<Option<usize>, &str>)> {
        self.results.remove(task_name)
    }

    #[allow(box_pointers)]
    fn wait_result(
        &self,
        task_name: &str,
        wait_timeout: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>> {
        let key = Box::leak(Box::from(task_name));
        let arc = if let Some(arc) = self.waits.get(key) {
            arc.clone()
        } else {
            let arc = Arc::new((Mutex::new(true), Condvar::new()));
            assert!(self.waits.insert(key, arc.clone()).is_none());
            arc
        };
        let (lock, cvar) = &*arc;
        let result = cvar
            .wait_timeout_while(lock.lock().unwrap(), wait_timeout, |&mut pending| pending)
            .unwrap();
        if result.1.timed_out() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "wait timeout",
            ));
        }
        assert!(self.waits.remove(key).is_some());
        Ok(self.try_get_result(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coroutine::suspender::{SimpleDelaySuspender, SuspenderImpl};

    #[test]
    fn test_simple() {
        let task_name = "test_simple";
        let mut pool = CoroutinePoolImpl::new(
            crate::coroutine::DEFAULT_STACK_SIZE,
            0,
            1,
            0,
            crate::blocker::SleepBlocker {},
        );
        {
            let const_ref = &pool;
            assert!(const_ref.is_empty());
            _ = const_ref.submit(
                Some(String::from("test_panic")),
                |_| panic!("message"),
                None,
            );
            assert!(!const_ref.is_empty());
            let name = const_ref.submit(
                Some(String::from(task_name)),
                |_| {
                    println!("2");
                    Some(2)
                },
                None,
            );
            assert_eq!(task_name, name);
        }
        {
            let mut_ref = &mut pool;
            _ = mut_ref.try_schedule();
        }
        let const_ref = &pool;
        assert_eq!(
            Some((String::from("test_panic"), Err("message"))),
            const_ref.try_get_result("test_panic")
        );
        assert_eq!(
            Some((String::from(task_name), Ok(Some(2)))),
            const_ref.try_get_result(task_name)
        );
    }

    #[test]
    fn test_suspend() -> std::io::Result<()> {
        let mut pool = CoroutinePoolImpl::new(
            crate::coroutine::DEFAULT_STACK_SIZE,
            0,
            2,
            0,
            crate::blocker::SleepBlocker {},
        );
        _ = pool.submit(
            None,
            |param| {
                println!("[coroutine] delay");
                if let Some(suspender) = SuspenderImpl::<(), ()>::current() {
                    suspender.delay(Duration::from_millis(100));
                }
                println!("[coroutine] back");
                param
            },
            None,
        );
        _ = pool.submit(
            None,
            |_| {
                println!("middle");
                Some(1)
            },
            None,
        );
        pool.try_schedule()?;
        std::thread::sleep(Duration::from_millis(100));
        pool.try_schedule()
    }

    #[test]
    fn test_wait() {
        let task_name = "test_wait";
        let mut pool = CoroutinePoolImpl::new(
            crate::coroutine::DEFAULT_STACK_SIZE,
            0,
            1,
            0,
            crate::blocker::SleepBlocker {},
        );
        {
            let const_ref = &pool;
            assert!(const_ref.is_empty());
            let name = const_ref.submit(
                Some(String::from(task_name)),
                |_| {
                    println!("2");
                    Some(2)
                },
                None,
            );
            assert_eq!(task_name, name);
            assert_eq!(None, const_ref.try_get_result(task_name));
            match const_ref.wait_result(task_name, Duration::from_millis(100)) {
                Ok(_) => panic!(),
                Err(_) => {}
            }
            assert_eq!(None, const_ref.try_get_result(task_name));
        }
        {
            let mut_ref = &mut pool;
            _ = mut_ref.try_schedule();
        }
        let const_ref = &pool;
        match const_ref.wait_result(task_name, Duration::from_secs(100)) {
            Ok(v) => assert_eq!(Some((String::from(task_name), Ok(Some(2)))), v),
            Err(e) => panic!("{e}"),
        }
    }
}
