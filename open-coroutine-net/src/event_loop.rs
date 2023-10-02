use crate::selector::{Selector, SelectorImpl};
use dashmap::DashMap;
use open_coroutine_core::blocker::Blocker;
use open_coroutine_core::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use open_coroutine_core::coroutine::suspender::SimpleDelaySuspender;
use open_coroutine_core::coroutine::{Current, Named, StateMachine};
use open_coroutine_core::pool::constants::PoolState;
use open_coroutine_core::pool::task::TaskImpl;
use open_coroutine_core::pool::{CoroutinePool, CoroutinePoolImpl, Pool};
use open_coroutine_core::scheduler::{SchedulableCoroutine, SchedulableSuspender};
use polling::Events;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::panic::RefUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub trait EventLoop<'e>: Pool<'e> {
    #[allow(trivial_numeric_casts, clippy::cast_possible_truncation)]
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
            unsafe {
                cfg_if::cfg_if! {
                    if #[cfg(windows)] {
                        windows_sys::Win32::System::Threading::GetCurrentThread() as usize
                    } else {
                        libc::pthread_self() as usize
                    }
                }
            }
        }
    }

    fn wait_write(&self, socket: c_int, added: &AtomicBool, dur: Duration) -> std::io::Result<()> {
        let token = Self::token();
        if added
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.get_selector().add_write_event(socket, token)?;
        }
        self.wait_event(token, Some(dur))
    }

    fn wait_read(&self, socket: c_int, added: &AtomicBool, dur: Duration) -> std::io::Result<()> {
        let token = Self::token();
        if added
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.get_selector().add_read_event(socket, token)?;
        }
        self.wait_event(token, Some(dur))
    }

    fn get_selector(&self) -> &SelectorImpl;

    fn wait_event(&self, token: usize, timeout: Option<Duration>) -> std::io::Result<()> {
        self.wait(token, timeout, true)
    }

    fn wait_just(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.wait(Self::token(), timeout, false)
    }

    fn wait(&self, token: usize, timeout: Option<Duration>, schedule: bool) -> std::io::Result<()>;
}

#[derive(Debug)]
pub struct EventLoopImpl<'e> {
    cpu: usize,
    stop: Arc<(Mutex<bool>, Condvar)>,
    pool: CoroutinePoolImpl<'e>,
    selector: SelectorImpl,
    wait_threads: DashMap<usize, Arc<(Mutex<bool>, Condvar)>>,
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
            cpu,
            stop: Arc::new((Mutex::new(false), Condvar::new())),
            pool: CoroutinePoolImpl::new(
                name,
                cpu,
                stack_size,
                min_size,
                max_size,
                keep_alive_time,
                open_coroutine_core::blocker::DelayBlocker::default(),
            ),
            selector: SelectorImpl::new()?,
            wait_threads: DashMap::new(),
        })
    }

    fn map_name<'c>(
        wait_table: &DashMap<usize, Arc<(Mutex<bool>, Condvar)>>,
        token: usize,
    ) -> Option<&'c str> {
        if wait_table.contains_key(&token) {
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
        self.pool.get_name()
    }
}

impl<'e> Pool<'e> for EventLoopImpl<'e> {
    fn get_state(&self) -> PoolState {
        self.pool.get_state()
    }

    fn change_state(&self, state: PoolState) -> PoolState {
        self.pool.change_state(state)
    }

    fn set_min_size(&self, min_size: usize) {
        self.pool.set_min_size(min_size);
    }

    fn get_min_size(&self) -> usize {
        self.pool.get_min_size()
    }

    fn get_running_size(&self) -> usize {
        self.pool.get_running_size()
    }

    fn set_max_size(&self, max_size: usize) {
        self.pool.set_max_size(max_size);
    }

    fn get_max_size(&self) -> usize {
        self.pool.get_max_size()
    }

    fn set_keep_alive_time(&self, keep_alive_time: u64) {
        self.pool.set_keep_alive_time(keep_alive_time);
    }

    fn get_keep_alive_time(&self) -> u64 {
        self.pool.get_keep_alive_time()
    }

    fn size(&self) -> usize {
        self.pool.size()
    }

    fn wait_result(
        &self,
        task_name: &str,
        wait_time: Duration,
    ) -> std::io::Result<Option<(String, Result<Option<usize>, &str>)>> {
        let mut left = wait_time;
        let once = Duration::from_millis(10);
        let token = Self::token();
        loop {
            if left.is_zero() {
                return Err(Error::new(ErrorKind::Other, "wait timeout"));
            }
            if PoolState::Running == self.get_state() {
                //开启了单独的线程
                if let Ok(r) = self.pool.wait_result(task_name, left.min(once)) {
                    return Ok(r);
                }
            } else {
                self.wait_event(token, Some(left.min(once)))?;
                if let Some(r) = self.pool.try_get_result(task_name) {
                    return Ok(Some(r));
                }
            }
            left = left.saturating_sub(once);
        }
    }

    fn submit_raw(&self, task: TaskImpl<'e>) -> &str {
        self.pool.submit_raw(task)
    }

    #[allow(box_pointers)]
    fn change_blocker(&self, blocker: impl Blocker + 'e) -> Box<dyn Blocker>
    where
        'e: 'static,
    {
        self.pool.change_blocker(blocker)
    }

    fn start(self) -> std::io::Result<Arc<Self>>
    where
        'e: 'static,
    {
        assert_eq!(
            PoolState::Created,
            self.pool.change_state(PoolState::Running)
        );
        let arc = Arc::new(self);
        let consumer = arc.clone();
        let join_handle = std::thread::Builder::new()
            .name(format!("open-coroutine-event-loop-{}", arc.pool.get_name()))
            .spawn(move || {
                // thread per core
                _ = core_affinity::set_for_current(core_affinity::CoreId { id: consumer.cpu });
                let token = Self::token();
                while PoolState::Running == consumer.get_state()
                    || !consumer.pool.is_empty()
                    || consumer.pool.get_running_size() > 0
                {
                    _ = consumer.wait_event(token, Some(Duration::from_millis(10)));
                }
                let (lock, cvar) = &*consumer.stop.clone();
                let mut pending = lock.lock().unwrap();
                *pending = false;
                // Notify the condvar that the value has changed.
                cvar.notify_one();
            })
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
        std::mem::forget(join_handle);
        Ok(arc)
    }

    fn stop(&self, wait_time: Duration) -> std::io::Result<()> {
        let state = self.get_state();
        if PoolState::Stopped == state {
            return Ok(());
        }
        _ = self.pool.stop(Duration::ZERO);
        if PoolState::Running == state {
            //开启了单独的线程
            let (lock, cvar) = &*self.stop;
            let result = cvar
                .wait_timeout_while(lock.lock().unwrap(), wait_time, |&mut pending| pending)
                .unwrap();
            if result.1.timed_out() {
                return Err(Error::new(ErrorKind::Other, "stop timeout !"));
            }
            _ = self.change_state(PoolState::Stopped);
            return Ok(());
        }
        let mut left = wait_time;
        let once = Duration::from_millis(10);
        let token = Self::token();
        loop {
            if left.is_zero() {
                return Err(Error::new(ErrorKind::Other, "stop timeout !"));
            }
            self.wait_event(token, Some(left.min(once)))?;
            if self.pool.is_empty() && self.pool.get_running_size() == 0 {
                _ = self.change_state(PoolState::Stopped);
                return Ok(());
            }
            left = left.saturating_sub(once);
        }
    }
}

impl<'e> EventLoop<'e> for EventLoopImpl<'e> {
    fn get_selector(&self) -> &SelectorImpl {
        &self.selector
    }

    fn wait(&self, token: usize, timeout: Option<Duration>, schedule: bool) -> std::io::Result<()> {
        if SchedulableCoroutine::current().is_none() && !self.wait_threads.contains_key(&token) {
            _ = self
                .wait_threads
                .insert(token, Arc::new((Mutex::new(true), Condvar::new())));
        }
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
                    self.pool.try_timed_schedule(time)
                        .map(Duration::from_nanos)
                        .expect("has bug, notice !")
                })
        } else {
            timeout
        };
        let mut events = Events::new();
        let count = self.selector.select(&mut events, timeout)?;
        if count > 0 {
            //遍历events
            for event in events.iter() {
                if let Some(co_name) = Self::map_name(&self.wait_threads, event.key) {
                    //notify coroutine
                    self.pool.try_resume(co_name).expect("has bug, notice !");
                } else if let Some(arc) = self.wait_threads.get(&event.key) {
                    //notify thread
                    let (lock, cvar) = &*arc.clone();
                    let mut pending = lock.lock().unwrap();
                    *pending = false;
                    cvar.notify_one();
                }
            }
        } else if let Some(arc) = self.wait_threads.get(&token) {
            let (lock, cvar) = &*arc.clone();
            timeout.map_or_else(
                || {
                    drop(cvar.wait_while(lock.lock().unwrap(), |&mut pending| pending));
                    _ = self.wait_threads.remove(&token);
                },
                |wait_time| {
                    let r = cvar
                        .wait_timeout_while(lock.lock().unwrap(), wait_time, |&mut pending| pending)
                        .unwrap();
                    if !r.1.timed_out() {
                        _ = self.wait_threads.remove(&token);
                    }
                    drop(r);
                },
            );
            let mut pending = lock.lock().unwrap();
            *pending = true;
            cvar.notify_one();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple() -> std::io::Result<()> {
        let event_loop = EventLoopImpl::default();
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
        event_loop.stop(Duration::from_secs(3))
    }

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
        event_loop.stop(Duration::from_secs(3))
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
            Duration::from_secs(3),
        );
        assert_eq!(Some((task_name, Ok(Some(2)))), result.unwrap());
        event_loop.stop(Duration::from_secs(3))
    }
}
