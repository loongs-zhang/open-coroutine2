use crate::selector::{Selector, SelectorImpl};
use libc::{c_char, c_int, c_void};
use open_coroutine_core::blocker::DelayBlocker;
use open_coroutine_core::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use open_coroutine_core::coroutine::suspender::SimpleDelaySuspender;
use open_coroutine_core::coroutine::{Current, Named, StateMachine};
use open_coroutine_core::pool::{CoroutinePool, CoroutinePoolImpl, Pool};
use open_coroutine_core::scheduler::{SchedulableCoroutine, SchedulableSuspender};
use polling::Events;
use std::cell::UnsafeCell;
use std::ffi::{CStr, CString};
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub trait EventLoop<'e>: Pool {
    fn submit(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'e,
        param: Option<usize>,
    ) -> &str;

    #[allow(clippy::type_complexity)]
    fn submit_and_wait(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'e,
        param: Option<usize>,
        wait_time: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>>;

    #[must_use]
    fn token() -> usize {
        if let Some(co) = SchedulableCoroutine::current() {
            #[allow(box_pointers)]
            let boxed: &'static mut CString = Box::leak(Box::from(
                CString::new(co.get_name()).expect("build name failed!"),
            ));
            let cstr: &'static CStr = boxed.as_c_str();
            cstr.as_ptr().cast::<c_void>() as usize
        } else {
            0
        }
    }

    fn wait_write(&self, socket: c_int, added: AtomicBool, dur: Duration) -> std::io::Result<()> {
        if added
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.get_selector().add_write_event(socket, Self::token())?;
        }
        self.slice_wait(Some(dur), true)
    }

    fn wait_read(&self, socket: c_int, added: AtomicBool, dur: Duration) -> std::io::Result<()> {
        if added
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.get_selector().add_write_event(socket, Self::token())?;
        }
        self.slice_wait(Some(dur), true)
    }

    fn slice_wait(&self, timeout: Option<Duration>, schedule: bool) -> std::io::Result<()> {
        if !schedule || timeout.is_none() {
            return self.wait(timeout, schedule);
        }
        let timeout_time = open_coroutine_timer::get_timeout_time(timeout.unwrap());
        loop {
            let left_time = timeout_time.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                return Ok(());
            }
            self.wait(
                Some(Duration::from_nanos(left_time.min(10_000_000))),
                schedule,
            )?;
        }
    }

    fn get_selector(&self) -> &SelectorImpl;

    fn wait_just(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.wait(timeout, false)
    }

    fn wait(&self, timeout: Option<Duration>, schedule: bool) -> std::io::Result<()>;

    fn start(self) -> std::io::Result<Arc<EventLoopImpl<'e>>>
    where
        'e: 'static;

    fn stop(&self, wait_time: Duration) -> std::io::Result<()>;
}

#[derive(Debug)]
pub struct EventLoopImpl<'e> {
    run: AtomicBool,
    cpu: usize,
    pool: UnsafeCell<CoroutinePoolImpl<'e>>,
    selector: SelectorImpl,
}

impl EventLoopImpl<'_> {
    pub fn new(
        name: String,
        cpu: usize,
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
    ) -> std::io::Result<Self> {
        Ok(EventLoopImpl {
            run: AtomicBool::new(false),
            cpu,
            pool: UnsafeCell::new(CoroutinePoolImpl::new(
                name,
                stack_size,
                min_size,
                max_size,
                keep_alive_time,
                DelayBlocker {},
            )),
            selector: SelectorImpl::new()?,
        })
    }

    fn map_name<'c>(token: usize) -> Option<&'c str> {
        if token == 0 {
            return None;
        }
        unsafe { CStr::from_ptr((token as *const c_void).cast::<c_char>()) }
            .to_str()
            .ok()
    }
}

unsafe impl Send for EventLoopImpl<'_> {}

unsafe impl Sync for EventLoopImpl<'_> {}

impl RefUnwindSafe for EventLoopImpl<'_> {}

impl Default for EventLoopImpl<'_> {
    fn default() -> Self {
        Self::new(
            uuid::Uuid::new_v4().to_string(),
            1,
            open_coroutine_core::coroutine::DEFAULT_STACK_SIZE,
            0,
            65536,
            0,
        )
        .expect("create event-loop failed")
    }
}

impl Named for EventLoopImpl<'_> {
    fn get_name(&self) -> &str {
        unsafe { (*self.pool.get()).get_name() }
    }
}

impl Pool for EventLoopImpl<'_> {
    fn set_min_size(&self, min_size: usize) {
        unsafe { (*self.pool.get()).set_min_size(min_size) };
    }

    fn get_min_size(&self) -> usize {
        unsafe { (*self.pool.get()).get_min_size() }
    }

    fn get_running_size(&self) -> usize {
        unsafe { (*self.pool.get()).get_running_size() }
    }

    fn set_max_size(&self, max_size: usize) {
        unsafe { (*self.pool.get()).set_max_size(max_size) };
    }

    fn get_max_size(&self) -> usize {
        unsafe { (*self.pool.get()).get_max_size() }
    }

    fn set_keep_alive_time(&self, keep_alive_time: u64) {
        unsafe { (*self.pool.get()).set_keep_alive_time(keep_alive_time) };
    }

    fn get_keep_alive_time(&self) -> u64 {
        unsafe { (*self.pool.get()).get_keep_alive_time() }
    }

    fn size(&self) -> usize {
        unsafe { (*self.pool.get()).size() }
    }
}

impl<'e> EventLoop<'e> for EventLoopImpl<'e> {
    fn submit(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'e,
        param: Option<usize>,
    ) -> &str {
        let pool = unsafe { &*self.pool.get() };
        pool.submit(name, func, param)
    }

    fn submit_and_wait(
        &self,
        name: Option<String>,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'e,
        param: Option<usize>,
        wait_time: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>> {
        let task_name = self.submit(name, func, param);
        let pool = unsafe { &*self.pool.get() };
        let timeout_time = open_coroutine_timer::get_timeout_time(wait_time);
        loop {
            let left_time = timeout_time.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                return Err(Error::new(ErrorKind::Other, "wait timeout"));
            }
            self.wait(Some(Duration::from_nanos(left_time.min(10_000_000))), true)?;
            if let Some(r) = pool.try_get_result(task_name) {
                return Ok(Some(r));
            }
        }
    }

    fn get_selector(&self) -> &SelectorImpl {
        &self.selector
    }

    fn wait(&self, timeout: Option<Duration>, schedule: bool) -> std::io::Result<()> {
        let pool = unsafe { &mut *self.pool.get() };
        let timeout = if schedule {
            timeout
                .map(|time| {
                    if let Some(coroutine) = SchedulableCoroutine::current() {
                        if let Some(suspender) = SchedulableSuspender::current() {
                            let timeout_time = open_coroutine_timer::get_timeout_time(time);
                            cfg_if::cfg_if! {
                                if #[cfg(target_os = "linux")] {
                                    coroutine
                                        .syscall((), Syscall::epoll_wait, SyscallState::Suspend(timeout_time))
                                        .expect("change to syscall state failed !");
                                    //协程环境只yield，不执行任务
                                    suspender.delay(time);
                                    match coroutine.state() {
                                        CoroutineState::SystemCall(_, Syscall::epoll_wait, SyscallState::Timeout) => {},
                                        _ => unreachable!("wait should never execute to here"),
                                    };
                                } else if #[cfg(any(
                                    target_os = "macos",
                                    target_os = "ios",
                                    target_os = "tvos",
                                    target_os = "watchos",
                                    target_os = "freebsd",
                                    target_os = "dragonfly",
                                    target_os = "openbsd",
                                    target_os = "netbsd"
                                ))] {
                                    coroutine
                                        .syscall((), Syscall::kevent, SyscallState::Suspend(timeout_time))
                                        .expect("change to syscall state failed !");
                                    //协程环境只yield，不执行任务
                                    suspender.delay(time);
                                    match coroutine.state() {
                                        CoroutineState::SystemCall(_, Syscall::kevent, SyscallState::Timeout) => {},
                                        _ => unreachable!("wait should never execute to here"),
                                    };
                                } else if #[cfg(windows)] {
                                    coroutine
                                        .syscall((), Syscall::iocp, SyscallState::Suspend(timeout_time))
                                        .expect("change to syscall state failed !");
                                    //协程环境只yield，不执行任务
                                    suspender.delay(time);
                                    match coroutine.state() {
                                        CoroutineState::SystemCall(_, Syscall::iocp, SyscallState::Timeout) => {},
                                        _ => unreachable!("wait should never execute to here"),
                                    };
                                }
                            }
                            coroutine
                                .syscall_resume()
                                .expect("change to running state failed !");
                            return Duration::ZERO;
                        }
                    }
                    pool.try_timed_schedule(time)
                        .map(Duration::from_nanos)
                        .expect("has bug, notice !")
                })
        } else {
            timeout
        };
        let mut events = Events::new();
        _ = self.selector.select(&mut events, timeout)?;
        //遍历events
        for event in events.iter() {
            if let Some(co_name) = Self::map_name(event.key) {
                pool.try_resume(co_name).expect("has bug, notice !");
            }
        }
        Ok(())
    }

    fn start(self) -> std::io::Result<Arc<EventLoopImpl<'e>>>
    where
        'e: 'static,
    {
        self.run.store(true, Ordering::Release);
        let arc = Arc::new(self);
        let consumer = arc.clone();
        let join_handle = std::thread::Builder::new()
            .name(format!("open-coroutine-event-loop-{}", unsafe {
                (*arc.pool.get()).get_name()
            }))
            .spawn(move || {
                // thread per core
                _ = core_affinity::set_for_current(core_affinity::CoreId { id: consumer.cpu });
                let pool = unsafe { &*consumer.pool.get() };
                while consumer.run.load(Ordering::Acquire) || !pool.is_empty() {
                    _ = consumer.wait(Some(Duration::from_millis(10)), true);
                }
            })
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
        std::mem::forget(join_handle);
        Ok(arc)
    }

    fn stop(&self, wait_time: Duration) -> std::io::Result<()> {
        let pool = unsafe { &mut *self.pool.get() };
        _ = pool.stop(Duration::ZERO);
        self.run.store(false, Ordering::Release);
        let timeout_time = open_coroutine_timer::get_timeout_time(wait_time);
        loop {
            let left_time = timeout_time.saturating_sub(open_coroutine_timer::now());
            self.wait(Some(Duration::from_nanos(left_time.min(10_000_000))), true)?;
            if pool.is_empty() {
                return Ok(());
            }
            if left_time == 0 {
                return Err(Error::new(ErrorKind::Other, "stop timeout !"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(debug_assertions))]
    #[test]
    fn test_wait() -> std::io::Result<()> {
        let event_loop = EventLoopImpl::default();
        event_loop.set_max_size(1);
        let task_name = uuid::Uuid::new_v4().to_string();
        _ = event_loop.submit(None, |_| panic!("test panic, just ignore it"), None);
        let result = event_loop.submit_and_wait(
            Some(task_name.clone()),
            |_| {
                println!("2");
                Some(2)
            },
            None,
            Duration::from_millis(100),
        );
        assert_eq!(Some((task_name, Ok(Some(2)))), result.unwrap());
        event_loop.stop(Duration::from_secs(3))
    }

    #[test]
    fn test_simple_auto() -> std::io::Result<()> {
        let event_loop = EventLoopImpl::default().start()?;
        event_loop.set_max_size(1);
        _ = event_loop.submit(None, |_| panic!("test panic, just ignore it"), None);
        _ = event_loop.submit(
            None,
            |_| {
                println!("2");
                Some(2)
            },
            None,
        );
        let now = open_coroutine_timer::now();
        event_loop.slice_wait(Some(Duration::from_millis(10)), true)?;
        assert!(open_coroutine_timer::now() - now >= 10_000_000);
        event_loop.stop(Duration::from_secs(30))
    }

    #[test]
    fn test_wait_auto() -> std::io::Result<()> {
        let event_loop = EventLoopImpl::default().start()?;
        event_loop.set_max_size(1);
        let task_name = uuid::Uuid::new_v4().to_string();
        _ = event_loop.submit(None, |_| panic!("test panic, just ignore it"), None);
        let result = event_loop.submit_and_wait(
            Some(task_name.clone()),
            |_| {
                println!("2");
                Some(2)
            },
            None,
            Duration::from_millis(100),
        );
        assert_eq!(Some((task_name, Ok(Some(2)))), result.unwrap());
        event_loop.stop(Duration::from_secs(30))
    }
}
