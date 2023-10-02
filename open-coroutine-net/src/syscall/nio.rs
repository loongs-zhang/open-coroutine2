use crate::event_loop::{EventLoop, EventLoopImpl};
use crate::selector::Selector;
#[cfg(target_os = "linux")]
use crate::syscall::LinuxNetSyscall;
use crate::syscall::UnixNetSyscall;
use libc::{c_int, iovec, msghdr, off_t, size_t, sockaddr, socklen_t, ssize_t};
use open_coroutine_core::pool::CoroutinePool;
use std::ffi::c_void;
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

extern "C" {
    #[cfg(not(any(target_os = "dragonfly", target_os = "vxworks")))]
    #[cfg_attr(
        any(
            target_os = "linux",
            target_os = "emscripten",
            target_os = "fuchsia",
            target_os = "l4re"
        ),
        link_name = "__errno_location"
    )]
    #[cfg_attr(
        any(
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "android",
            target_os = "redox",
            target_env = "newlib"
        ),
        link_name = "__errno"
    )]
    #[cfg_attr(
        any(target_os = "solaris", target_os = "illumos"),
        link_name = "___errno"
    )]
    #[cfg_attr(
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "watchos"
        ),
        link_name = "__error"
    )]
    #[cfg_attr(target_os = "haiku", link_name = "_errnop")]
    fn errno_location() -> *mut c_int;
}

extern "C" fn reset_errno() {
    set_errno(0);
}

extern "C" fn set_errno(errno: c_int) {
    unsafe { errno_location().write(errno) }
}

extern "C" fn set_non_blocking(socket: c_int) {
    assert!(
        set_non_blocking_flag(socket, true),
        "set_non_blocking failed !"
    );
}

extern "C" fn set_blocking(socket: c_int) {
    assert!(
        set_non_blocking_flag(socket, false),
        "set_blocking failed !"
    );
}

extern "C" fn set_non_blocking_flag(socket: c_int, on: bool) -> bool {
    let flags = unsafe { libc::fcntl(socket, libc::F_GETFL) };
    if flags < 0 {
        return false;
    }
    unsafe {
        libc::fcntl(
            socket,
            libc::F_SETFL,
            if on {
                flags | libc::O_NONBLOCK
            } else {
                flags & !libc::O_NONBLOCK
            },
        ) == 0
    }
}

#[must_use]
extern "C" fn is_blocking(socket: c_int) -> bool {
    !is_non_blocking(socket)
}

#[must_use]
extern "C" fn is_non_blocking(socket: c_int) -> bool {
    let flags = unsafe { libc::fcntl(socket, libc::F_GETFL) };
    if flags < 0 {
        return false;
    }
    (flags & libc::O_NONBLOCK) != 0
}

#[derive(Debug, Default)]
pub struct NioLinuxNetSyscall<'n, I: UnixNetSyscall> {
    event_loop: EventLoopImpl<'n>,
    inner: I,
}

impl<I: UnixNetSyscall> NioLinuxNetSyscall<'_, I> {
    pub fn new(
        name: String,
        cpu: usize,
        stack_size: usize,
        min_size: usize,
        max_size: usize,
        keep_alive_time: u64,
        inner: I,
    ) -> std::io::Result<Self> {
        Ok(NioLinuxNetSyscall {
            inner,
            event_loop: EventLoopImpl::new(
                name,
                cpu,
                stack_size,
                min_size,
                max_size,
                keep_alive_time,
            )?,
        })
    }
}

macro_rules! impl_read_nio_msghdr {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_msghdr!($self, $syscall, mut, true, wait_read,
        $fn_ptr, $socket, $msg, $($arg, )*)
    };
}

macro_rules! impl_write_nio_msghdr {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_msghdr!($self, $syscall, const, false, wait_write,
        $fn_ptr, $socket, $msg, $($arg, )*)
    };
}

macro_rules! impl_nio_msghdr {
    ( $self: expr, $syscall:ident, $ref_type:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        impl_non_blocking!($socket, {
            let msghdr = unsafe { *$msg };
            let mut vec = std::collections::VecDeque::from(unsafe {
                #[allow(trivial_numeric_casts)]
                Vec::from_raw_parts(
                    msghdr.msg_iov,
                    msghdr.msg_iovlen as usize,
                    msghdr.msg_iovlen as usize,
                )
            });
            let mut length = 0;
            let mut pices = std::collections::VecDeque::new();
            for iovec in &vec {
                length += iovec.iov_len;
                pices.push_back(length);
            }

            let socket = $socket;
            let added = AtomicBool::new(false);
            let mut total = 0;
            let mut r = 0;
            while total < length {
                // find from-index
                let mut from_index = 0;
                for (i, v) in pices.iter().enumerate() {
                    if total < *v {
                        from_index = i;
                        break;
                    }
                }
                // calculate offset
                let current_offset = if from_index > 0 {
                    total.saturating_sub(pices[from_index.saturating_sub(1)])
                } else {
                    total
                };
                // remove already done
                for _ in 0..from_index {
                    _ = vec.pop_front();
                    _ = pices.pop_front();
                }
                // build syscall args
                vec[0] = iovec {
                    iov_base: (vec[0].iov_base as usize + current_offset) as *mut c_void,
                    iov_len: vec[0].iov_len - current_offset,
                };
                cfg_if::cfg_if! {
                    if #[cfg(any(
                        target_os = "linux",
                        target_os = "l4re",
                        target_os = "android",
                        target_os = "emscripten"
                    ))] {
                        let len = vec.len();
                    } else {
                        let len = c_int::try_from(vec.len()).unwrap();
                    }
                }
                let mut new_msg = msghdr {
                    msg_name: msghdr.msg_name,
                    msg_namelen: msghdr.msg_namelen,
                    msg_iov: vec.get_mut(0).unwrap(),
                    msg_iovlen: len,
                    msg_control: msghdr.msg_control,
                    msg_controllen: msghdr.msg_controllen,
                    msg_flags: msghdr.msg_flags,
                };
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    &mut new_msg as *$ref_type msghdr,
                    $($arg, )*
                );
                if r != -1 {
                    reset_errno();
                    total += r as size_t;
                    if total >= length || $condition && r==0 {
                        r = total as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $self.event_loop.$wait_event(socket, &added, Duration::from_millis(10)).is_err() {
                        break;
                    }
                } else if error_kind != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
            r
        })
    };
}

macro_rules! impl_read_nio_iovec {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $iov:expr, $iovcnt:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_iovec!($self, $syscall, true, wait_read,
        $fn_ptr, $socket, $iov, $iovcnt, $($arg, )*)
    }
}

macro_rules! impl_write_nio_iovec {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $iov:expr, $iovcnt:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_iovec!($self, $syscall, false, wait_write,
        $fn_ptr, $socket, $iov, $iovcnt, $($arg, )*)
    }
}

macro_rules! impl_nio_iovec {
    ( $self: expr, $syscall:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $iov:expr, $iovcnt:expr, $($arg: expr),* $(,)* ) => {
        impl_non_blocking!($socket, {
            let mut vec = std::collections::VecDeque::from(unsafe {
                Vec::from_raw_parts($iov as *mut iovec, $iovcnt as usize, $iovcnt as usize)
            });
            let mut length = 0;
            let mut pices = std::collections::VecDeque::new();
            for iovec in &vec {
                length += iovec.iov_len;
                pices.push_back(length);
            }

            let socket = $socket;
            let added = AtomicBool::new(false);
            let mut total = 0;
            let mut r = 0;
            while total < length {
                // find from-index
                let mut from_index = 0;
                for (i, v) in pices.iter().enumerate() {
                    if total < *v {
                        from_index = i;
                        break;
                    }
                }
                // calculate offset
                let current_offset = if from_index > 0 {
                    total.saturating_sub(pices[from_index.saturating_sub(1)])
                } else {
                    total
                };
                // remove already done
                for _ in 0..from_index {
                    _ = vec.pop_front();
                    _ = pices.pop_front();
                }
                // build syscall args
                vec[0] = iovec {
                    iov_base: (vec[0].iov_base as usize + current_offset) as *mut c_void,
                    iov_len: vec[0].iov_len - current_offset,
                };
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    vec.get(0).unwrap(),
                    c_int::try_from(vec.len()).unwrap(),
                    $($arg, )*
                );
                if r != -1 {
                    reset_errno();
                    total += r as size_t;
                    if total >= length || $condition && r==0 {
                        r = total as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $self.event_loop.$wait_event(socket, &added, Duration::from_millis(10)).is_err() {
                        break;
                    }
                } else if error_kind != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
            r
        })
    };
}

macro_rules! impl_read_nio_buf {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $buffer:expr, $length:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_buf!($self, $syscall, mut, true, wait_read,
        $fn_ptr, $socket, $buffer, $length, $($arg, )*)
    };
}

macro_rules! impl_write_nio_buf {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $buffer:expr, $length:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_buf!($self, $syscall, const, false, wait_write,
        $fn_ptr, $socket, $buffer, $length, $($arg, )*)
    };
}

macro_rules! impl_nio_buf {
    ( $self: expr, $syscall:ident, $ptr_type:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $buffer:expr, $length:expr, $($arg: expr),* $(,)* ) => {
        impl_non_blocking!($socket, {
            let socket = $socket;
            let added = AtomicBool::new(false);
            let mut total = 0;
            let mut r = 0;
            while total < $length {
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    ($buffer as usize + total) as *$ptr_type c_void,
                    $length - total,
                    $($arg, )*
                );
                if r != -1 {
                    reset_errno();
                    total += r as size_t;
                    if total >= $length || $condition && r==0 {
                        r = total as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $self.event_loop.$wait_event(socket, &added, Duration::from_millis(10)).is_err() {
                        break;
                    }
                } else if error_kind != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
            r
        })
    };
}

macro_rules! impl_nio {
    ( $self: expr, $syscall:ident, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $($arg: expr),* $(,)* ) => {
        impl_non_blocking!($socket, {
            let socket = $socket;
            let added = AtomicBool::new(false);
            let mut r = 0;
            loop {
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    $($arg, )*
                );
                if r != -1 {
                    reset_errno();
                    break;
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $self.event_loop.$wait_event(socket, &added, Duration::from_millis(10)).is_err() {
                        break;
                    }
                } else if error_kind != std::io::ErrorKind::Interrupted {
                    break;
                }
            }
            r
        })
    };
}

macro_rules! impl_non_blocking {
    ( $socket:expr, $impls:expr ) => {{
        let socket = $socket;
        let blocking = is_blocking(socket);
        if blocking {
            set_non_blocking(socket);
        }
        let r = $impls;
        if blocking {
            set_blocking(socket);
        }
        return r;
    }};
}

impl<I: UnixNetSyscall> UnixNetSyscall for NioLinuxNetSyscall<'_, I> {
    fn socket(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int) -> c_int>,
        domain: c_int,
        ty: c_int,
        protocol: c_int,
    ) -> c_int {
        self.inner.socket(fn_ptr, domain, ty, protocol)
    }

    fn accept(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut sockaddr, *mut socklen_t) -> c_int>,
        socket: c_int,
        address: *mut sockaddr,
        address_len: *mut socklen_t,
    ) -> c_int {
        impl_nio!(
            self,
            accept,
            wait_read,
            fn_ptr,
            socket,
            address,
            address_len
        )
    }

    fn connect(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const sockaddr, socklen_t) -> c_int>,
        socket: c_int,
        address: *const sockaddr,
        len: socklen_t,
    ) -> c_int {
        impl_non_blocking!(socket, {
            let added = AtomicBool::new(false);
            let mut r;
            loop {
                r = self.inner.connect(fn_ptr, socket, address, len);
                if r == 0 {
                    reset_errno();
                    break;
                }
                let errno = Error::last_os_error().raw_os_error();
                if errno == Some(libc::EINPROGRESS) {
                    //阻塞，直到写事件发生
                    if self
                        .event_loop
                        .wait_write(socket, &added, Duration::from_millis(10))
                        .is_err()
                    {
                        r = -1;
                        break;
                    }
                    let mut err: c_int = 0;
                    unsafe {
                        let mut len: socklen_t = std::mem::zeroed();
                        r = libc::getsockopt(
                            socket,
                            libc::SOL_SOCKET,
                            libc::SO_ERROR,
                            (std::ptr::addr_of_mut!(err)).cast::<c_void>(),
                            &mut len,
                        );
                    }
                    if r != 0 {
                        r = -1;
                        break;
                    }
                    if err == 0 {
                        reset_errno();
                        r = 0;
                        break;
                    };
                    set_errno(err);
                    r = -1;
                    break;
                } else if errno != Some(libc::EINTR) {
                    r = -1;
                    break;
                }
            }
            if r == -1 && Error::last_os_error().raw_os_error() == Some(libc::ETIMEDOUT) {
                set_errno(libc::EINPROGRESS);
            }
            r
        })
    }

    fn shutdown(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int) -> c_int>,
        socket: c_int,
        how: c_int,
    ) -> c_int {
        let selector = self.event_loop.get_selector();
        //取消对fd的监听
        if match how {
            libc::SHUT_RD => selector.del_read_event(socket),
            libc::SHUT_WR => selector.del_write_event(socket),
            libc::SHUT_RDWR => selector.del_event(socket),
            _ => {
                set_errno(libc::EINVAL);
                return -1;
            }
        }
        .is_err()
        {
            return -1;
        }
        self.inner.shutdown(fn_ptr, socket, how)
    }

    fn close(&self, fn_ptr: Option<&extern "C" fn(c_int) -> c_int>, fd: c_int) -> c_int {
        let selector = self.event_loop.get_selector();
        //取消对fd的监听
        if selector.del_event(fd).is_err() {
            return -1;
        }
        self.inner.close(fn_ptr, fd)
    }

    fn recv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t, c_int) -> ssize_t>,
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
    ) -> ssize_t {
        impl_read_nio_buf!(self, recv, fn_ptr, socket, buf, len, flags)
    }

    fn read(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t) -> ssize_t>,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
    ) -> ssize_t {
        impl_read_nio_buf!(self, read, fn_ptr, fd, buf, count,)
    }

    fn pread(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t, off_t) -> ssize_t>,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
        offset: off_t,
    ) -> ssize_t {
        impl_read_nio_buf!(self, pread, fn_ptr, fd, buf, count, offset)
    }

    fn readv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> ssize_t {
        impl_read_nio_iovec!(self, readv, fn_ptr, fd, iov, iovcnt,)
    }

    fn preadv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int, off_t) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> ssize_t {
        impl_read_nio_iovec!(self, preadv, fn_ptr, fd, iov, iovcnt, offset)
    }

    fn recvmsg(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut msghdr, c_int) -> ssize_t>,
        fd: c_int,
        msg: *mut msghdr,
        flags: c_int,
    ) -> ssize_t {
        impl_read_nio_msghdr!(self, recvmsg, fn_ptr, fd, msg, flags)
    }

    fn send(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t, c_int) -> ssize_t>,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
    ) -> ssize_t {
        impl_write_nio_buf!(self, send, fn_ptr, socket, buf, len, flags)
    }

    fn write(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t) -> ssize_t>,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
    ) -> ssize_t {
        impl_write_nio_buf!(self, write, fn_ptr, fd, buf, count,)
    }

    fn pwrite(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t, off_t) -> ssize_t>,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
        offset: off_t,
    ) -> ssize_t {
        impl_write_nio_buf!(self, pwrite, fn_ptr, fd, buf, count, offset)
    }

    fn writev(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> ssize_t {
        impl_write_nio_iovec!(self, writev, fn_ptr, fd, iov, iovcnt,)
    }

    fn pwritev(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int, off_t) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> ssize_t {
        impl_write_nio_iovec!(self, pwritev, fn_ptr, fd, iov, iovcnt, offset)
    }

    fn sendmsg(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const msghdr, c_int) -> ssize_t>,
        fd: c_int,
        msg: *const msghdr,
        flags: c_int,
    ) -> ssize_t {
        impl_write_nio_msghdr!(self, sendmsg, fn_ptr, fd, msg, flags)
    }
}

#[cfg(target_os = "linux")]
impl<I: LinuxNetSyscall> LinuxNetSyscall for NioLinuxNetSyscall<'_, I> {
    fn epoll_ctl(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int, *mut libc::epoll_event) -> c_int>,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut libc::epoll_event,
    ) -> c_int {
        self.inner.epoll_ctl(fn_ptr, epfd, op, fd, event)
    }

    fn accept4(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut sockaddr, *mut socklen_t, c_int) -> c_int>,
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: c_int,
    ) -> c_int {
        impl_nio!(self, accept4, wait_read, fn_ptr, fd, addr, len, flg)
    }
}
