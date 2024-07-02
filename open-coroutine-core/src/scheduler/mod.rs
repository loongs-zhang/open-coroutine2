use crate::common::{Current, Named};
use crate::constants::{CoroutineState, SyscallState, DEFAULT_STACK_SIZE};
use crate::coroutine::suspender::{Suspender, SuspenderImpl};
use crate::coroutine::{Coroutine, CoroutineImpl, SimpleCoroutine, StateMachine};
use crate::scheduler::listener::Listener;
use dashmap::DashMap;
use open_coroutine_queue::LocalQueue;
use open_coroutine_timer::TimerList;
use std::collections::VecDeque;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::panic::UnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Listener abstraction and impl.
pub mod listener;

mod current;

/// A type for Scheduler.
pub type SchedulableCoroutine<'s> = CoroutineImpl<'s, (), (), ()>;

/// A type for Scheduler.
pub type SchedulableSuspender<'s> = SuspenderImpl<'s, (), ()>;

/// A trait implemented for schedulers.
pub trait Scheduler<'s>: Debug + Default + Named + Current<'s> + Listener {
    /// Extension points within the open-coroutine framework.
    fn init(&mut self);

    /// Set the default stack stack size for the coroutines in this scheduler.
    /// If it has not been set, it will be `crate::constants::DEFAULT_STACK_SIZE`.
    fn set_stack_size(&self, stack_size: usize);

    /// Submit a closure to new coroutine, then the coroutine will be push into ready queue.
    ///
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// if create coroutine fails.
    fn submit(
        &self,
        f: impl FnOnce(&dyn Suspender<Resume = (), Yield = ()>, ()) + UnwindSafe + 's,
        stack_size: Option<usize>,
    ) -> std::io::Result<()>;

    /// Resume a coroutine from the system call table to the ready queue,
    /// it's generally only required for framework level crates.
    ///
    /// If we can't find the coroutine, nothing happens.
    ///
    /// # Errors
    /// if change to ready fails.
    fn try_resume(&self, co_name: &'s str) -> std::io::Result<()>;

    /// Schedule the coroutines.
    ///
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// see `try_timeout_schedule`.
    fn try_schedule(&mut self) -> std::io::Result<()> {
        _ = self.try_timeout_schedule(Duration::MAX.as_secs())?;
        Ok(())
    }

    /// Try scheduling the coroutines for up to `dur`.
    ///
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// see `try_timeout_schedule`.
    fn try_timed_schedule(&mut self, dur: Duration) -> std::io::Result<u64> {
        self.try_timeout_schedule(open_coroutine_timer::get_timeout_time(dur))
    }

    /// Attempt to schedule the coroutines before the `timeout_time` timestamp.
    ///
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// Returns the left time in ns.
    ///
    /// # Errors
    /// if change to ready fails.
    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64>;

    /// Returns `true` if the ready queue, suspend queue, and syscall queue are all empty.
    fn is_empty(&self) -> bool {
        self.size() == 0
    }

    /// Returns the number of coroutines owned by this scheduler.
    fn size(&self) -> usize;

    /// Add a listener to this scheduler.
    fn add_listener(&mut self, listener: impl Listener + 's) {
        self.add_raw_listener(Box::new(listener));
    }

    /// Add a raw listener to this scheduler.
    fn add_raw_listener(&mut self, listener: Box<dyn Listener + 's>);
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug)]
pub struct SchedulerImpl<'s> {
    name: String,
    stack_size: AtomicUsize,
    ready: LocalQueue<'s, SchedulableCoroutine<'s>>,
    suspend: TimerList<SchedulableCoroutine<'s>>,
    syscall: DashMap<&'s str, SchedulableCoroutine<'s>>,
    syscall_suspend: TimerList<&'s str>,
    listeners: VecDeque<Box<dyn Listener + 's>>,
}

impl SchedulerImpl<'_> {
    #[allow(missing_docs)]
    #[must_use]
    pub fn new(name: String, stack_size: usize) -> Self {
        let mut scheduler = SchedulerImpl {
            name,
            stack_size: AtomicUsize::new(stack_size),
            ready: LocalQueue::default(),
            suspend: TimerList::default(),
            syscall: DashMap::default(),
            syscall_suspend: TimerList::default(),
            listeners: VecDeque::default(),
        };
        scheduler.init();
        scheduler
    }

    fn check_ready(&mut self) -> std::io::Result<()> {
        // Check if the elements in the suspend queue are ready
        for _ in 0..self.suspend.entry_len() {
            if let Some((exec_time, _)) = self.suspend.front() {
                if open_coroutine_timer::now() < *exec_time {
                    break;
                }
                if let Some((_, mut entry)) = self.suspend.pop_front() {
                    while !entry.is_empty() {
                        if let Some(coroutine) = entry.pop_front() {
                            if let Err(e) = coroutine.ready() {
                                Self::clean_current();
                                return Err(e);
                            }
                            self.ready.push_back(coroutine);
                        }
                    }
                }
            }
        }
        // Check if the elements in the syscall suspend queue are ready
        for _ in 0..self.syscall_suspend.entry_len() {
            if let Some((exec_time, _)) = self.syscall_suspend.front() {
                if open_coroutine_timer::now() < *exec_time {
                    break;
                }
                if let Some((_, mut entry)) = self.syscall_suspend.pop_front() {
                    while !entry.is_empty() {
                        if let Some(co_name) = entry.pop_front() {
                            if let Some(r) = self.syscall.remove(&co_name) {
                                let coroutine = r.1;
                                match coroutine.state() {
                                    CoroutineState::SystemCall(val, syscall, state) => {
                                        if let SyscallState::Suspend(_) = state {
                                            if let Err(e) = coroutine.syscall(
                                                val,
                                                syscall,
                                                SyscallState::Timeout,
                                            ) {
                                                Self::clean_current();
                                                return Err(e);
                                            }
                                        }
                                        self.ready.push_back(coroutine);
                                    }
                                    _ => unreachable!("check_ready should never execute to here"),
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for SchedulerImpl<'_> {
    fn default() -> Self {
        Self::new(
            format!("open-coroutine-scheduler-{}", uuid::Uuid::new_v4()),
            DEFAULT_STACK_SIZE,
        )
    }
}

impl Drop for SchedulerImpl<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert!(
                self.ready.is_empty(),
                "There are still coroutines to be carried out in the ready queue:{:#?} !",
                self.ready
            );
            assert!(
                self.suspend.is_empty(),
                "There are still coroutines to be carried out in the suspend queue:{:#?} !",
                self.suspend
            );
            assert!(
                self.syscall.is_empty(),
                "There are still coroutines to be carried out in the syscall queue:{:#?} !",
                self.syscall
            );
        }
    }
}

impl Eq for SchedulerImpl<'_> {}

impl PartialEq for SchedulerImpl<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(&other.name)
    }
}

impl Named for SchedulerImpl<'_> {
    fn get_name(&self) -> &str {
        &self.name
    }
}

impl<'s> Scheduler<'s> for SchedulerImpl<'s> {
    fn init(&mut self) {
        #[cfg(all(unix, feature = "preemptive-schedule"))]
        self.add_listener(crate::monitor::creator::MonitorTaskCreator::default());
    }

    fn set_stack_size(&self, stack_size: usize) {
        self.stack_size.store(stack_size, Ordering::Release);
    }

    /// # Examples
    /// ```
    /// use std::time::Duration;
    /// use open_coroutine_core::common::Current;
    /// use open_coroutine_core::constants::{CoroutineState, Syscall, SyscallState};
    /// use open_coroutine_core::coroutine::StateMachine;
    /// use open_coroutine_core::coroutine::suspender::SimpleSuspender;
    /// use open_coroutine_core::scheduler::{SchedulableCoroutine, SchedulableSuspender, Scheduler, SchedulerImpl};
    ///
    /// let mut scheduler = SchedulerImpl::default();
    /// scheduler.submit( |_, _| {
    ///     println!("1");
    ///     if let Some(coroutine) = SchedulableCoroutine::current() {
    ///         let timeout_time = open_coroutine_timer::get_timeout_time(Duration::from_millis(10));
    ///         coroutine.syscall((), Syscall::nanosleep, SyscallState::Suspend(timeout_time))
    ///             .expect("change to syscall state failed !");
    ///         if let Some(suspender) = SchedulableSuspender::current() {
    ///             suspender.suspend();
    ///         }
    ///         match coroutine.state() {
    ///             CoroutineState::SystemCall(_, Syscall::nanosleep, state) => match state {
    ///                 SyscallState::Timeout => println!("syscall nanosleep finished !"),
    ///                 _ => unreachable!("should never execute to here"),
    ///             },
    ///             _ => unreachable!("should never execute to here"),
    ///         };
    ///         coroutine.syscall_resume().expect("change to running state failed !");
    ///     }
    ///     println!("2");
    /// }, None).expect("submit failed");
    /// scheduler.try_schedule().expect("schedule failed");
    /// std::thread::sleep(Duration::from_millis(10));
    /// scheduler.try_schedule().expect("schedule failed");
    /// ```
    fn submit(
        &self,
        f: impl FnOnce(&dyn Suspender<Resume = (), Yield = ()>, ()) + UnwindSafe + 's,
        stack_size: Option<usize>,
    ) -> std::io::Result<()> {
        let coroutine = SchedulableCoroutine::new(
            format!("{}|{}", self.name, uuid::Uuid::new_v4()),
            f,
            stack_size.unwrap_or(self.stack_size.load(Ordering::Acquire)),
        )?;
        coroutine.ready()?;
        self.on_create(&coroutine);
        self.ready.push_back(coroutine);
        Ok(())
    }

    fn try_resume(&self, co_name: &'s str) -> std::io::Result<()> {
        if let Some(r) = self.syscall.remove(&co_name) {
            let coroutine = r.1;
            match coroutine.state() {
                CoroutineState::SystemCall(val, syscall, _) => {
                    coroutine.syscall(val, syscall, SyscallState::Computing)?;
                }
                _ => unreachable!("try_resume should never execute to here"),
            }
            self.ready.push_back(coroutine);
        }
        Ok(())
    }

    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64> {
        Self::init_current(self);
        loop {
            let left_time = timeout_time.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                Self::clean_current();
                return Ok(0);
            }
            self.check_ready()?;
            // schedule coroutines
            match self.ready.pop_front() {
                None => {
                    Self::clean_current();
                    return Ok(left_time);
                }
                Some(mut coroutine) => {
                    self.on_resume(timeout_time, &coroutine);
                    match coroutine.resume() {
                        Ok(state) => {
                            match state {
                                CoroutineState::Suspend((), timestamp) => {
                                    self.on_suspend(timeout_time, &coroutine);
                                    if timestamp <= open_coroutine_timer::now() {
                                        self.ready.push_back(coroutine);
                                    } else {
                                        self.suspend.insert(timestamp, coroutine);
                                    }
                                }
                                CoroutineState::SystemCall((), syscall, state) => {
                                    self.on_syscall(timeout_time, &coroutine, syscall, state);
                                    let co_name = Box::leak(Box::from(coroutine.get_name()));
                                    if let SyscallState::Suspend(timestamp) = state {
                                        self.syscall_suspend.insert(timestamp, co_name);
                                    }
                                    _ = self.syscall.insert(co_name, coroutine);
                                }
                                CoroutineState::Complete(()) => {
                                    self.on_complete(timeout_time, &coroutine);
                                }
                                CoroutineState::Error(message) => {
                                    self.on_error(timeout_time, &coroutine, message);
                                }
                                _ => {
                                    Self::clean_current();
                                    return Err(Error::new(
                                        ErrorKind::Other,
                                        "try_timeout_schedule should never execute to here",
                                    ));
                                }
                            };
                        }
                        Err(e) => {
                            Self::clean_current();
                            return Err(e);
                        }
                    };
                }
            }
        }
    }

    fn size(&self) -> usize {
        self.ready.len() + self.suspend.len() + self.syscall.len()
    }

    fn add_raw_listener(&mut self, listener: Box<dyn Listener + 's>) {
        self.listeners.push_back(listener);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coroutine::suspender::{SimpleDelaySuspender, SimpleSuspender};

    #[test]
    fn test_simple() -> std::io::Result<()> {
        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(|_, _| println!("1"), None)?;
        scheduler.submit(|_, _| println!("2"), None)?;
        scheduler.try_schedule()
    }

    #[test]
    fn test_backtrace() -> std::io::Result<()> {
        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(|_, _| (), None)?;
        scheduler.submit(|_, _| println!("{:?}", backtrace::Backtrace::new()), None)?;
        scheduler.try_schedule()
    }

    #[test]
    fn with_suspend() -> std::io::Result<()> {
        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(
            |suspender, _| {
                println!("[coroutine1] suspend");
                suspender.suspend();
                println!("[coroutine1] back");
            },
            None,
        )?;
        scheduler.submit(
            |suspender, _| {
                println!("[coroutine2] suspend");
                suspender.suspend();
                println!("[coroutine2] back");
            },
            None,
        )?;
        scheduler.try_schedule()
    }

    #[test]
    fn with_delay() -> std::io::Result<()> {
        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(
            |suspender, _| {
                println!("[coroutine] delay");
                suspender.delay(Duration::from_millis(100));
                println!("[coroutine] back");
            },
            None,
        )?;
        scheduler.try_schedule()?;
        std::thread::sleep(Duration::from_millis(100));
        scheduler.try_schedule()
    }

    #[cfg(feature = "korosensei")]
    #[test]
    fn test_trap() -> std::io::Result<()> {
        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(
            |_, _| {
                println!("Before trap");
                unsafe { std::ptr::write_volatile(1 as *mut u8, 0) };
                println!("After trap");
            },
            None,
        )?;
        scheduler.submit(|_, _| println!("200"), None)?;
        scheduler.try_schedule()
    }

    #[cfg(all(feature = "korosensei", not(debug_assertions)))]
    #[test]
    fn test_invalid_memory_reference() -> std::io::Result<()> {
        use std::ffi::c_void;

        let mut scheduler = SchedulerImpl::default();
        scheduler.submit(
            |_, _| {
                println!("Before invalid memory reference");
                // 没有加--release运行，会收到SIGABRT信号，不好处理，直接禁用测试
                unsafe { _ = &*((1usize as *mut c_void).cast::<SchedulableCoroutine>()) };
                println!("After invalid memory reference");
            },
            None,
        )?;
        scheduler.submit(|_, _| println!("200"), None)?;
        scheduler.try_schedule()
    }
}
