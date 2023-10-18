use crate::blocker::Blocker;
use crate::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::suspender::SimpleDelaySuspender;
use crate::coroutine::{Current, Named, StateMachine};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use crate::net::operator::Operator;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use crate::net::operator::OperatorImpl;
use crate::net::selector::{Selector, SelectorImpl};
use crate::pool::constants::PoolState;
use crate::pool::join::JoinHandle;
use crate::pool::task::TaskImpl;
use crate::pool::{CoroutinePool, CoroutinePoolImpl, Pool};
use crate::scheduler::{SchedulableCoroutine, SchedulableSuspender};
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use dashmap::DashMap;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use libc::{
    c_uint, epoll_event, iovec, mode_t, msghdr, off_t, size_t, sockaddr, socklen_t, ssize_t,
};
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::panic::RefUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[allow(trivial_numeric_casts, clippy::cast_possible_truncation)]
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
                    let thread_id = windows_sys::Win32::System::Threading::GetCurrentThread();
                } else {
                    let thread_id = libc::pthread_self();
                }
            }
            thread_id as usize
        }
    }
}

pub trait EventLoop<'e>: Pool<'e, JoinHandleImpl<'e>> {
    fn wait_event(&self, timeout: Option<Duration>) -> std::io::Result<usize>;

    fn wait_just(&self, timeout: Option<Duration>) -> std::io::Result<usize>;
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug)]
pub struct JoinHandleImpl<'e>(*const EventLoopImpl<'e>, *const c_char);

impl<'e> JoinHandleImpl<'e> {
    #[allow(box_pointers)]
    pub(crate) fn new(event_loop: *const EventLoopImpl<'e>, name: &str) -> Self {
        let boxed: &'static mut CString = Box::leak(Box::from(
            CString::new(name).expect("init JoinHandle failed!"),
        ));
        let cstr: &'static CStr = boxed.as_c_str();
        JoinHandleImpl(event_loop, cstr.as_ptr())
    }

    /// create a error instance.
    #[must_use]
    pub fn error() -> Self {
        Self::new(std::ptr::null(), "")
    }
}

impl JoinHandle for JoinHandleImpl<'_> {
    fn get_name(&self) -> std::io::Result<&str> {
        unsafe { CStr::from_ptr(self.1) }
            .to_str()
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "Invalid task name"))
    }

    fn timeout_at_join(&self, timeout_time: u64) -> std::io::Result<Result<Option<usize>, &str>> {
        let name = self.get_name()?;
        if name.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "Invalid task name"));
        }
        let event_loop = unsafe { &*self.0 };
        event_loop
            .wait_result(
                name,
                Duration::from_nanos(timeout_time.saturating_sub(open_coroutine_timer::now())),
            )
            .map(|r| r.expect("result is None !").1)
    }
}

#[derive(Debug)]
pub struct EventLoopImpl<'e> {
    cpu: usize,
    pool: CoroutinePoolImpl<'e>,
    selector: SelectorImpl,
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    operator: OperatorImpl<'e>,
    #[allow(clippy::type_complexity)]
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    wait_table: DashMap<usize, Arc<(Mutex<Option<ssize_t>>, Condvar)>>,
    stop: Arc<(Mutex<bool>, Condvar)>,
    shared_stop: Arc<(Mutex<AtomicUsize>, Condvar)>,
}

impl EventLoopImpl<'_> {
    #[allow(clippy::cast_possible_truncation)]
    pub fn new(
        name: String,
        cpu: usize,
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
        shared_stop: Arc<(Mutex<AtomicUsize>, Condvar)>,
    ) -> std::io::Result<Self> {
        Ok(EventLoopImpl {
            cpu,
            pool: CoroutinePoolImpl::new(
                name,
                cpu,
                stack_size,
                min_size,
                max_size,
                keep_alive_time,
                crate::blocker::DelayBlocker::default(),
            ),
            selector: SelectorImpl::new()?,
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            operator: OperatorImpl::new(cpu as u32)?,
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            wait_table: DashMap::new(),
            stop: Arc::new((Mutex::new(false), Condvar::new())),
            shared_stop,
        })
    }

    fn map_name<'c>(token: usize) -> Option<&'c str> {
        unsafe { CStr::from_ptr((token as *const c_void).cast::<c_char>()) }
            .to_str()
            .ok()
    }

    pub fn add_read_event(&self, fd: c_int) -> std::io::Result<()> {
        self.selector.add_read_event(fd, token())
    }

    pub fn add_write_event(&self, fd: c_int) -> std::io::Result<()> {
        self.selector.add_write_event(fd, token())
    }

    pub fn del_event(&self, fd: c_int) -> std::io::Result<()> {
        self.selector.del_event(fd)
    }

    pub fn del_read_event(&self, fd: c_int) -> std::io::Result<()> {
        self.selector.del_read_event(fd)
    }

    pub fn del_write_event(&self, fd: c_int) -> std::io::Result<()> {
        self.selector.del_write_event(fd)
    }
}

unsafe impl Send for EventLoopImpl<'_> {}

unsafe impl Sync for EventLoopImpl<'_> {}

impl RefUnwindSafe for EventLoopImpl<'_> {}

impl Default for EventLoopImpl<'_> {
    fn default() -> Self {
        Self::new(
            format!("open-coroutine-event-loop-{}", uuid::Uuid::new_v4()),
            1,
            crate::coroutine::DEFAULT_STACK_SIZE,
            0,
            65536,
            0,
            Arc::new((Mutex::new(AtomicUsize::new(0)), Condvar::new())),
        )
        .expect("create event-loop failed")
    }
}

impl Named for EventLoopImpl<'_> {
    fn get_name(&self) -> &str {
        self.pool.get_name()
    }
}

impl<'e> Pool<'e, JoinHandleImpl<'e>> for EventLoopImpl<'e> {
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
        loop {
            if left.is_zero() {
                return Err(Error::new(ErrorKind::TimedOut, "wait timeout"));
            }
            if PoolState::Running == self.get_state() {
                //开启了单独的线程
                if let Ok(r) = self.pool.wait_result(task_name, left.min(once)) {
                    return Ok(r);
                }
            } else {
                _ = self.wait_event(Some(left.min(once)))?;
                if let Some(r) = self.pool.try_get_result(task_name) {
                    return Ok(Some(r));
                }
            }
            left = left.saturating_sub(once);
        }
    }

    fn submit_raw(&self, task: TaskImpl<'e>) -> JoinHandleImpl<'e> {
        let join_handle = self.pool.submit_raw(task);
        let task_name = join_handle.get_name().expect("Invalid task name");
        JoinHandleImpl::new(self, task_name)
    }

    fn pop(&self) -> Option<TaskImpl<'e>> {
        self.pool.pop()
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
            .name(arc.pool.get_name().to_string())
            .spawn(move || {
                // thread per core
                _ = core_affinity::set_for_current(core_affinity::CoreId { id: consumer.cpu });
                while PoolState::Running == consumer.get_state()
                    || !consumer.pool.is_empty()
                    || consumer.pool.get_running_size() > 0
                {
                    _ = consumer.wait_event(Some(Duration::from_millis(10)));
                }
                let (lock, cvar) = &*consumer.stop.clone();
                let mut pending = lock.lock().unwrap();
                *pending = false;
                cvar.notify_one();
                // notify shared stop flag
                let (lock, cvar) = &*consumer.shared_stop.clone();
                let pending = lock.lock().unwrap();
                _ = pending.fetch_add(1, Ordering::Release);
                cvar.notify_one();
                crate::warn!("{} has exited", consumer.get_name());
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
                return Err(Error::new(ErrorKind::TimedOut, "stop timeout !"));
            }
            _ = self.change_state(PoolState::Stopped);
            return Ok(());
        }
        let mut left = wait_time;
        let once = Duration::from_millis(10);
        loop {
            if left.is_zero() {
                return Err(Error::new(ErrorKind::TimedOut, "stop timeout !"));
            }
            _ = self.wait_event(Some(left.min(once)))?;
            if self.pool.is_empty() && self.pool.get_running_size() == 0 {
                _ = self.change_state(PoolState::Stopped);
                return Ok(());
            }
            left = left.saturating_sub(once);
        }
    }
}

impl<'e> EventLoop<'e> for EventLoopImpl<'e> {
    fn wait_event(&self, timeout: Option<Duration>) -> std::io::Result<usize> {
        if let Some(time) = timeout {
            if let Some(coroutine) = SchedulableCoroutine::current() {
                if let Some(suspender) = SchedulableSuspender::current() {
                    let timeout_time = open_coroutine_timer::get_timeout_time(time);
                    let syscall = match coroutine.state() {
                        CoroutineState::Running => {
                            cfg_if::cfg_if! {
                                if #[cfg(target_os = "linux")] {
                                    Syscall::epoll_wait
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
                                    Syscall::kevent
                                } else if #[cfg(windows)] {
                                    Syscall::iocp
                                }
                            }
                        }
                        CoroutineState::SystemCall((), syscall, _) => syscall,
                        _ => unreachable!("wait should never execute to here"),
                    };
                    coroutine
                        .syscall((), syscall, SyscallState::Suspend(timeout_time))
                        .expect("change to syscall state failed !");
                    //协程环境只yield，不执行任务
                    suspender.delay(time);
                    return self.wait_just(Some(Duration::ZERO));
                }
            }
            return self.wait_just(Some(
                self.pool
                    .try_timed_schedule(time)
                    .map(Duration::from_nanos)?,
            ));
        }
        self.pool.try_schedule()?;
        self.wait_just(None)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn wait_just(&self, timeout: Option<Duration>) -> std::io::Result<usize> {
        cfg_if::cfg_if! {
            if #[cfg(all(target_os = "linux", feature = "io_uring"))] {
                let mut timeout = timeout;
                if crate::net::operator::support_io_uring() {
                    // use io_uring
                    let mut result = self.operator.select(timeout)?;
                    for cqe in &mut result.1 {
                        let syscall_result = cqe.result();
                        let token = cqe.user_data() as usize;
                        // resolve completed read/write tasks
                        if let Some((_, pair)) = self.wait_table.remove(&token) {
                            let (lock, cvar) = &*pair;
                            let mut pending = lock.lock().unwrap();
                            *pending = Some(syscall_result as ssize_t);
                            // notify the condvar that the value has changed.
                            cvar.notify_one();
                        }
                        if let Some(co_name) = Self::map_name(token) {
                            //notify coroutine
                            self.pool.try_resume(co_name).expect("has bug, notice !");
                        }
                    }
                    timeout = Some(Duration::ZERO);
                }
            }
        }
        cfg_if::cfg_if! {
            if #[cfg(target_os = "linux")] {
                let net_syscall = Syscall::epoll_wait;
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
                let net_syscall = Syscall::kevent;
            } else if #[cfg(windows)] {
                let net_syscall = Syscall::iocp;
            }
        }
        if let Some(coroutine) = SchedulableCoroutine::current() {
            match coroutine.state() {
                CoroutineState::SystemCall(
                    (),
                    syscall,
                    SyscallState::Computing | SyscallState::Timeout,
                ) => {
                    coroutine
                        .syscall((), syscall, SyscallState::Calling(net_syscall))
                        .expect("change to syscall state failed !");
                }
                _ => unreachable!("wait should never execute to here"),
            };
        }
        let mut events = Vec::with_capacity(1024);
        let result = self.selector.select(&mut events, timeout);
        if let Some(coroutine) = SchedulableCoroutine::current() {
            match coroutine.state() {
                CoroutineState::SystemCall((), syscall, SyscallState::Calling(_)) => {
                    coroutine
                        .syscall((), syscall, SyscallState::Computing)
                        .expect("change to syscall state failed !");
                }
                _ => unreachable!("wait should never execute to here"),
            };
        }
        let count = result?;
        if count > 0 {
            //遍历events
            for event in &events {
                if let Some(co_name) = Self::map_name(event.key) {
                    //notify coroutine
                    self.pool.try_resume(co_name).expect("has bug, notice !");
                }
            }
        }
        Ok(count)
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
macro_rules! wrap_result {
    ( $self: expr, $syscall:ident, $user_data: expr, $($arg: expr),* $(,)* ) => {{
        $self.operator
            .$syscall($user_data, $($arg, )*)
            .map(|()| {
                let arc = Arc::new((Mutex::new(None), Condvar::new()));
                assert!(
                    $self.wait_table.insert($user_data, arc.clone()).is_none(),
                    "The previous token was not retrieved in a timely manner"
                );
                arc
            })
    }};
}

#[allow(clippy::type_complexity)]
#[cfg(all(target_os = "linux", feature = "io_uring"))]
impl EventLoopImpl<'_> {
    pub fn async_cancel(
        &self,
        user_data: usize,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, async_cancel, user_data,)
    }

    pub fn epoll_ctl(
        &self,
        user_data: usize,
        epfd: libc::c_int,
        op: libc::c_int,
        fd: libc::c_int,
        event: *mut epoll_event,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, epoll_ctl, user_data, epfd, op, fd, event)
    }

    pub fn poll_add(
        &self,
        user_data: usize,
        fd: libc::c_int,
        flags: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, poll_add, user_data, fd, flags)
    }

    pub fn poll_remove(
        &self,
        user_data: usize,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, poll_remove, user_data,)
    }

    pub fn timeout_add(
        &self,
        user_data: usize,
        timeout: Option<Duration>,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, timeout_add, user_data, timeout)
    }

    pub fn timeout_update(
        &self,
        user_data: usize,
        timeout: Option<Duration>,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, timeout_update, user_data, timeout)
    }

    pub fn timeout_remove(
        &self,
        user_data: usize,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, timeout_remove, user_data,)
    }

    pub fn openat(
        &self,
        user_data: usize,
        dir_fd: libc::c_int,
        pathname: *const libc::c_char,
        flags: libc::c_int,
        mode: mode_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, openat, user_data, dir_fd, pathname, flags, mode)
    }

    pub fn mkdirat(
        &self,
        user_data: usize,
        dir_fd: libc::c_int,
        pathname: *const libc::c_char,
        mode: mode_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, mkdirat, user_data, dir_fd, pathname, mode)
    }

    pub fn renameat(
        &self,
        user_data: usize,
        old_dir_fd: libc::c_int,
        old_path: *const libc::c_char,
        new_dir_fd: libc::c_int,
        new_path: *const libc::c_char,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, renameat, user_data, old_dir_fd, old_path, new_dir_fd, new_path)
    }

    pub fn renameat2(
        &self,
        user_data: usize,
        old_dir_fd: libc::c_int,
        old_path: *const libc::c_char,
        new_dir_fd: libc::c_int,
        new_path: *const libc::c_char,
        flags: c_uint,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, renameat2, user_data, old_dir_fd, old_path, new_dir_fd, new_path, flags)
    }

    pub fn fsync(
        &self,
        user_data: usize,
        fd: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, fsync, user_data, fd)
    }

    pub fn socket(
        &self,
        user_data: usize,
        domain: libc::c_int,
        ty: libc::c_int,
        protocol: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, socket, user_data, domain, ty, protocol)
    }

    pub fn accept(
        &self,
        user_data: usize,
        socket: libc::c_int,
        address: *mut sockaddr,
        address_len: *mut socklen_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, accept, user_data, socket, address, address_len)
    }

    pub fn accept4(
        &self,
        user_data: usize,
        fd: libc::c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, accept4, user_data, fd, addr, len, flg)
    }

    pub fn connect(
        &self,
        user_data: usize,
        socket: libc::c_int,
        address: *const sockaddr,
        len: socklen_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, connect, user_data, socket, address, len)
    }

    pub fn shutdown(
        &self,
        user_data: usize,
        socket: libc::c_int,
        how: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, shutdown, user_data, socket, how)
    }

    pub fn close(
        &self,
        user_data: usize,
        fd: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, close, user_data, fd)
    }

    pub fn recv(
        &self,
        user_data: usize,
        socket: libc::c_int,
        buf: *mut c_void,
        len: size_t,
        flags: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, recv, user_data, socket, buf, len, flags)
    }

    pub fn read(
        &self,
        user_data: usize,
        fd: libc::c_int,
        buf: *mut c_void,
        count: size_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, read, user_data, fd, buf, count)
    }

    pub fn pread(
        &self,
        user_data: usize,
        fd: libc::c_int,
        buf: *mut c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, pread, user_data, fd, buf, count, offset)
    }

    pub fn readv(
        &self,
        user_data: usize,
        fd: libc::c_int,
        iov: *const iovec,
        iovcnt: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, readv, user_data, fd, iov, iovcnt)
    }

    pub fn preadv(
        &self,
        user_data: usize,
        fd: libc::c_int,
        iov: *const iovec,
        iovcnt: libc::c_int,
        offset: off_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, preadv, user_data, fd, iov, iovcnt, offset)
    }

    pub fn recvmsg(
        &self,
        user_data: usize,
        fd: libc::c_int,
        msg: *mut msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, recvmsg, user_data, fd, msg, flags)
    }

    pub fn send(
        &self,
        user_data: usize,
        socket: libc::c_int,
        buf: *const c_void,
        len: size_t,
        flags: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, send, user_data, socket, buf, len, flags)
    }

    pub fn write(
        &self,
        user_data: usize,
        fd: libc::c_int,
        buf: *const c_void,
        count: size_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, write, user_data, fd, buf, count)
    }

    pub fn pwrite(
        &self,
        user_data: usize,
        fd: libc::c_int,
        buf: *const c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, pwrite, user_data, fd, buf, count, offset)
    }

    pub fn writev(
        &self,
        user_data: usize,
        fd: libc::c_int,
        iov: *const iovec,
        iovcnt: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, writev, user_data, fd, iov, iovcnt)
    }

    pub fn pwritev(
        &self,
        user_data: usize,
        fd: libc::c_int,
        iov: *const iovec,
        iovcnt: libc::c_int,
        offset: off_t,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, pwritev, user_data, fd, iov, iovcnt, offset)
    }

    pub fn sendmsg(
        &self,
        user_data: usize,
        fd: libc::c_int,
        msg: *const msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<Arc<(Mutex<Option<ssize_t>>, Condvar)>> {
        wrap_result!(self, sendmsg, user_data, fd, msg, flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
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
