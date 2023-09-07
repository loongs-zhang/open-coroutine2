use crate::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::suspender::Suspender;
use crate::coroutine::{Coroutine, CoroutineImpl, Current, SimpleCoroutine, StateMachine};
use dashmap::DashMap;
use open_coroutine_queue::{LocalQueue, WorkStealQueue};
use open_coroutine_timer::TimerList;
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// A type for Scheduler.
pub type SchedulableCoroutine<'s> = CoroutineImpl<'s, (), (), ()>;

/// A trait implemented for schedulers.
pub trait Scheduler<'s>: Debug + Current<'s> + Listener {
    /// Set the default stack stack size for the coroutines in this scheduler.
    /// If it has not been set, it will be `crate::coroutine::DEFAULT_STACK_SIZE`.
    fn set_stack_size(&self, stack_size: usize);

    /// Submit a closure to new coroutine, then the coroutine will be push into ready queue.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// if create coroutine fails.
    fn submit(
        &self,
        f: impl FnOnce(&dyn Suspender<Resume = (), Yield = ()>, ()) + 's,
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
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    ///
    /// # Errors
    /// see `try_timeout_schedule`.
    fn try_timed_schedule(&mut self, dur: Duration) -> std::io::Result<u64> {
        self.try_timeout_schedule(open_coroutine_timer::get_timeout_time(dur))
    }

    /// Attempt to schedule the coroutines before the `timeout_time` timestamp.
    /// Allow multiple threads to concurrently submit coroutine to the scheduler,
    /// but only allow one thread to execute scheduling.
    /// Returns the left time in ns.
    ///
    /// # Errors
    /// if change to ready fails.
    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64>;

    /// Add a listener to this scheduler.
    fn add_listener(&mut self, listener: impl Listener + 'static);
}

/// A trait implemented for schedulers, mainly used for monitoring.
pub trait Listener: Debug {
    /// callback when a coroutine is created.
    /// This will be called by `Scheduler` when a coroutine is created.
    fn on_create(&self, _: &SchedulableCoroutine) {}

    /// callback when a coroutine is suspended.
    /// This will be called by `Scheduler` when a coroutine is suspended.
    fn on_suspend(&self, _: &SchedulableCoroutine) {}

    /// callback when a coroutine enters syscall.
    /// This will be called by `Scheduler` when a coroutine enters syscall.
    fn on_syscall(&self, _: &SchedulableCoroutine, _: Syscall, _: SyscallState) {}

    /// callback when a coroutine is completed.
    /// This will be called by `Scheduler` when a coroutine is completed.
    fn on_complete(&self, _: &SchedulableCoroutine) {}
}

#[allow(missing_docs, box_pointers)]
#[derive(Debug)]
pub struct SchedulerImpl<'s> {
    name: String,
    stack_size: AtomicUsize,
    ready: LocalQueue<'s, SchedulableCoroutine<'s>>,
    suspend: TimerList<SchedulableCoroutine<'s>>,
    syscall: DashMap<&'s str, SchedulableCoroutine<'s>>,
    listeners: VecDeque<Box<dyn Listener>>,
}

impl SchedulerImpl<'_> {
    #[allow(missing_docs, box_pointers)]
    #[must_use]
    pub fn new(name: String, stack_size: usize) -> Self {
        SchedulerImpl {
            name,
            stack_size: AtomicUsize::new(stack_size),
            ready: WorkStealQueue::get_instance().local_queue(),
            suspend: TimerList::default(),
            syscall: DashMap::default(),
            listeners: VecDeque::default(),
        }
    }
}

impl Default for SchedulerImpl<'_> {
    fn default() -> Self {
        Self::new(
            uuid::Uuid::new_v4().to_string(),
            crate::coroutine::DEFAULT_STACK_SIZE,
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

thread_local! {
    pub(crate) static SCHEDULER: Cell<*const c_void> = Cell::new(std::ptr::null());
}

impl<'s> Current<'s> for SchedulerImpl<'s> {
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(current: &Self)
    where
        Self: Sized,
    {
        SCHEDULER.with(|c| c.set(current as *const _ as *const c_void));
    }

    fn current() -> Option<&'s Self>
    where
        Self: Sized,
    {
        SCHEDULER.with(|boxed| {
            let ptr = boxed.get();
            if ptr.is_null() {
                None
            } else {
                Some(unsafe { &*(ptr).cast::<SchedulerImpl<'s>>() })
            }
        })
    }

    fn clean_current()
    where
        Self: Sized,
    {
        SCHEDULER.with(|boxed| boxed.set(std::ptr::null()));
    }
}

#[allow(box_pointers)]
impl Listener for SchedulerImpl<'_> {
    fn on_create(&self, coroutine: &SchedulableCoroutine) {
        for listener in &self.listeners {
            listener.on_create(coroutine);
        }
    }

    fn on_suspend(&self, coroutine: &SchedulableCoroutine) {
        for listener in &self.listeners {
            listener.on_suspend(coroutine);
        }
    }

    fn on_syscall(&self, coroutine: &SchedulableCoroutine, syscall: Syscall, state: SyscallState) {
        for listener in &self.listeners {
            listener.on_syscall(coroutine, syscall, state);
        }
    }

    fn on_complete(&self, coroutine: &SchedulableCoroutine) {
        for listener in &self.listeners {
            listener.on_complete(coroutine);
        }
    }
}

impl<'s> Scheduler<'s> for SchedulerImpl<'s> {
    fn set_stack_size(&self, stack_size: usize) {
        self.stack_size.store(stack_size, Ordering::Release);
    }

    fn submit(
        &self,
        f: impl FnOnce(&dyn Suspender<Resume = (), Yield = ()>, ()) + 's,
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
            coroutine.ready()?;
            self.ready.push_back(coroutine);
        }
        Ok(())
    }

    fn try_timeout_schedule(&mut self, timeout_time: u64) -> std::io::Result<u64> {
        loop {
            let left_time = timeout_time.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                return Ok(0);
            }
            // check ready
            for _ in 0..self.suspend.len() {
                if let Some(entry) = self.suspend.front() {
                    let exec_time = entry.get_timestamp();
                    if open_coroutine_timer::now() < exec_time {
                        break;
                    }
                    if let Some(mut entry) = self.suspend.pop_front() {
                        while !entry.is_empty() {
                            if let Some(coroutine) = entry.pop_front() {
                                coroutine.ready()?;
                                self.ready.push_back(coroutine);
                            }
                        }
                    }
                }
            }
            // schedule coroutines
            match self.ready.pop_front() {
                None => return Ok(left_time),
                Some(mut coroutine) => {
                    match coroutine.resume()? {
                        CoroutineState::Suspend(_, timestamp) => {
                            self.on_suspend(&coroutine);
                            if timestamp <= open_coroutine_timer::now() {
                                self.ready.push_back(coroutine);
                            } else {
                                self.suspend.insert(timestamp, coroutine);
                            }
                        }
                        CoroutineState::SystemCall(_, syscall, state) => {
                            self.on_syscall(&coroutine, syscall, state);
                            #[allow(box_pointers)]
                            let co_name = Box::leak(Box::from(coroutine.get_name()));
                            _ = self.syscall.insert(co_name, coroutine);
                        }
                        CoroutineState::Complete(_) => self.on_complete(&coroutine),
                        _ => {
                            return Err(Error::new(
                                ErrorKind::Other,
                                "should never execute to here",
                            ))
                        }
                    };
                }
            }
        }
    }

    #[allow(box_pointers)]
    fn add_listener(&mut self, listener: impl Listener + 'static) {
        self.listeners.push_back(Box::new(listener));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coroutine::suspender::{SimpleDelaySuspender, SimpleSuspender};

    #[test]
    fn test_simple() {
        let mut scheduler = SchedulerImpl::default();
        _ = scheduler.submit(
            |_, _| {
                println!("1");
            },
            None,
        );
        _ = scheduler.submit(
            |_, _| {
                println!("2");
            },
            None,
        );
        scheduler.try_schedule().unwrap();
    }

    #[test]
    fn test_backtrace() {
        let mut scheduler = SchedulerImpl::default();
        _ = scheduler.submit(|_, _| (), None);
        _ = scheduler.submit(
            |_, _| {
                println!("{:?}", backtrace::Backtrace::new());
            },
            None,
        );
        scheduler.try_schedule().unwrap();
    }

    #[test]
    fn with_suspend() {
        let mut scheduler = SchedulerImpl::default();
        _ = scheduler.submit(
            |suspender, _| {
                println!("[coroutine1] suspend");
                suspender.suspend();
                println!("[coroutine1] back");
            },
            None,
        );
        _ = scheduler.submit(
            |suspender, _| {
                println!("[coroutine2] suspend");
                suspender.suspend();
                println!("[coroutine2] back");
            },
            None,
        );
        scheduler.try_schedule().unwrap();
    }

    #[test]
    fn with_delay() {
        let mut scheduler = SchedulerImpl::default();
        _ = scheduler.submit(
            |suspender, _| {
                println!("[coroutine] delay");
                suspender.delay(Duration::from_millis(100));
                println!("[coroutine] back");
            },
            None,
        );
        scheduler.try_schedule().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        scheduler.try_schedule().unwrap();
    }
}
