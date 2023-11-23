use crate::common::{Current, Named};
use crate::coroutine::StateMachine;
use crate::net::core::EventLoops;
use crate::net::selector::Selector;
use crate::syscall::common::{reset_errno, set_errno};
#[cfg(target_os = "linux")]
use crate::syscall::LinuxSyscall;
use crate::syscall::UnixSyscall;
use crate::{impl_non_blocking, log_syscall};
#[cfg(target_os = "linux")]
use libc::epoll_event;
use libc::{
    c_int, c_uint, c_void, fd_set, iovec, msghdr, nfds_t, off_t, pollfd, size_t, sockaddr,
    socklen_t, ssize_t, timespec, timeval,
};
use std::io::Error;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[repr(C)]
#[derive(Debug, Default)]
pub struct NioLinuxSyscall<I: UnixSyscall> {
    inner: I,
}

macro_rules! impl_nio {
    ( $self: expr, $syscall:ident, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $($arg: expr),* $(,)* ) => {
        $crate::impl_non_blocking!($socket, {
            let socket = $socket;
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut r;
            loop {
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    $($arg, )*
                );
                $crate::log_syscall!(socket, "nop", r);
                if r != -1 {
                    $crate::syscall::common::reset_errno();
                    break;
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $crate::net::core::EventLoops::$wait_event(
                        socket,
                        &added,
                        Some(std::time::Duration::from_millis(10)),
                    )
                    .is_err()
                    {
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
        impl_nio_buf!($self, $syscall, mut, true, wait_read_event,
        $fn_ptr, $socket, $buffer, $length, $($arg, )*)
    };
}

macro_rules! impl_write_nio_buf {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $buffer:expr, $length:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_buf!($self, $syscall, const, false, wait_write_event,
        $fn_ptr, $socket, $buffer, $length, $($arg, )*)
    };
}

macro_rules! impl_nio_buf {
    ( $self: expr, $syscall:ident, $ptr_type:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $buffer:expr, $length:expr, $($arg: expr),* $(,)* ) => {
        $crate::impl_non_blocking!($socket, {
            let socket = $socket;
            let length = $length;
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut done = 0;
            let mut r = 0;
            while done < length {
                r = $self.inner.$syscall(
                    $fn_ptr,
                    socket,
                    ($buffer as usize + done) as *$ptr_type c_void,
                    length - done,
                    $($arg, )*
                );
                $crate::log_syscall!(socket, done, r);
                if r != -1 {
                    $crate::syscall::common::reset_errno();
                    done += r as size_t;
                    if done >= length || $condition && r==0 {
                        r = done as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $crate::net::core::EventLoops::$wait_event(
                        socket,
                        &added,
                        Some(std::time::Duration::from_millis(10)),
                    )
                    .is_err()
                    {
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
        impl_nio_iovec!($self, $syscall, true, wait_read_event,
        $fn_ptr, $socket, $iov, $iovcnt, $($arg, )*)
    }
}

macro_rules! impl_write_nio_iovec {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $iov:expr, $iovcnt:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_iovec!($self, $syscall, false, wait_write_event,
        $fn_ptr, $socket, $iov, $iovcnt, $($arg, )*)
    }
}

macro_rules! impl_nio_iovec {
    ( $self: expr, $syscall:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $iov:expr, $iovcnt:expr, $($arg: expr),* $(,)* ) => {
        $crate::impl_non_blocking!($socket, {
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
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut done = 0;
            let mut r = 0;
            while done < length {
                // find from-index
                let mut from_index = 0;
                for (i, v) in pices.iter().enumerate() {
                    if done < *v {
                        from_index = i;
                        break;
                    }
                }
                // calculate offset
                let current_offset = if from_index > 0 {
                    done.saturating_sub(pices[from_index.saturating_sub(1)])
                } else {
                    done
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
                $crate::log_syscall!(socket, done, r);
                if r != -1 {
                    $crate::syscall::common::reset_errno();
                    done += r as size_t;
                    if done >= length || $condition && r==0 {
                        r = done as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $crate::net::core::EventLoops::$wait_event(
                        socket,
                        &added,
                        Some(std::time::Duration::from_millis(10)),
                    )
                    .is_err()
                    {
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

macro_rules! impl_read_nio_msghdr {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_msghdr!($self, $syscall, mut, true, wait_read_event,
        $fn_ptr, $socket, $msg, $($arg, )*)
    };
}

macro_rules! impl_write_nio_msghdr {
    ( $self: expr, $syscall:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        impl_nio_msghdr!($self, $syscall, const, false, wait_write_event,
        $fn_ptr, $socket, $msg, $($arg, )*)
    };
}

macro_rules! impl_nio_msghdr {
    ( $self: expr, $syscall:ident, $ref_type:ident, $condition:expr, $wait_event:ident,
      $fn_ptr:expr, $socket:expr, $msg:expr, $($arg: expr),* $(,)* ) => {
        $crate::impl_non_blocking!($socket, {
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
            let added = std::sync::atomic::AtomicBool::new(false);
            let mut done = 0;
            let mut r = 0;
            while done < length {
                // find from-index
                let mut from_index = 0;
                for (i, v) in pices.iter().enumerate() {
                    if done < *v {
                        from_index = i;
                        break;
                    }
                }
                // calculate offset
                let current_offset = if from_index > 0 {
                    done.saturating_sub(pices[from_index.saturating_sub(1)])
                } else {
                    done
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
                $crate::log_syscall!(socket, done, r);
                if r != -1 {
                    $crate::syscall::common::reset_errno();
                    done += r as size_t;
                    if done >= length || $condition && r==0 {
                        r = done as ssize_t;
                        break;
                    }
                }
                let error_kind = std::io::Error::last_os_error().kind();
                if error_kind == std::io::ErrorKind::WouldBlock {
                    if $crate::net::core::EventLoops::$wait_event(
                        socket,
                        &added,
                        Some(std::time::Duration::from_millis(10)),
                    )
                    .is_err()
                    {
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

impl<I: UnixSyscall> UnixSyscall for NioLinuxSyscall<I> {
    fn sleep(&self, _: Option<&extern "C" fn(c_uint) -> c_uint>, secs: c_uint) -> c_uint {
        let timeout = open_coroutine_timer::get_timeout_time(Duration::from_secs(u64::from(secs)));
        loop {
            let left_time = timeout.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                break;
            }
            _ = EventLoops::wait_event(Some(Duration::from_nanos(left_time)));
        }
        reset_errno();
        0
    }

    fn usleep(&self, _: Option<&extern "C" fn(c_uint) -> c_int>, microseconds: c_uint) -> c_int {
        let timeout =
            open_coroutine_timer::get_timeout_time(Duration::from_micros(u64::from(microseconds)));
        loop {
            let left_time = timeout.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                break;
            }
            _ = EventLoops::wait_event(Some(Duration::from_nanos(left_time)));
        }
        reset_errno();
        0
    }

    fn nanosleep(
        &self,
        _: Option<&extern "C" fn(*const timespec, *mut timespec) -> c_int>,
        rqtp: *const timespec,
        rmtp: *mut timespec,
    ) -> c_int {
        if rqtp.is_null() {
            set_errno(libc::EINVAL);
            return -1;
        }
        let t = unsafe { *rqtp };
        if t.tv_sec < 0 || t.tv_nsec < 0 || t.tv_nsec > 999_999_999 {
            set_errno(libc::EINVAL);
            return -1;
        }
        let timeout = open_coroutine_timer::get_timeout_time(Duration::new(
            t.tv_sec as u64,
            if let Ok(tv_nsec) = u32::try_from(t.tv_nsec) {
                tv_nsec
            } else {
                set_errno(libc::EINVAL);
                return -1;
            },
        ));
        loop {
            let left_time = timeout.saturating_sub(open_coroutine_timer::now());
            if left_time == 0 {
                break;
            }
            _ = EventLoops::wait_event(Some(Duration::from_nanos(left_time)));
        }
        reset_errno();
        if !rmtp.is_null() {
            unsafe {
                (*rmtp).tv_sec = 0;
                (*rmtp).tv_nsec = 0;
            }
        }
        0
    }

    fn poll(
        &self,
        fn_ptr: Option<&extern "C" fn(*mut pollfd, nfds_t, c_int) -> c_int>,
        fds: *mut pollfd,
        nfds: nfds_t,
        timeout: c_int,
    ) -> c_int {
        let mut t = if timeout < 0 { c_int::MAX } else { timeout };
        let mut x = 1;
        let mut r;
        // just check select every x ms
        loop {
            r = self.inner.poll(fn_ptr, fds, nfds, 0);
            log_syscall!("nop", "nop", r);
            if r != 0 || t == 0 {
                break;
            }
            _ = EventLoops::wait_event(Some(Duration::from_millis(t.min(x) as u64)));
            if t != c_int::MAX {
                t = if t > x { t - x } else { 0 };
            }
            if x < 16 {
                x <<= 1;
            }
        }
        r
    }

    #[allow(clippy::many_single_char_names, clippy::cast_possible_truncation)]
    fn select(
        &self,
        fn_ptr: Option<
            &extern "C" fn(c_int, *mut fd_set, *mut fd_set, *mut fd_set, *mut timeval) -> c_int,
        >,
        nfds: c_int,
        readfds: *mut fd_set,
        writefds: *mut fd_set,
        errorfds: *mut fd_set,
        timeout: *mut timeval,
    ) -> c_int {
        let mut t = if timeout.is_null() {
            c_uint::MAX
        } else {
            unsafe { ((*timeout).tv_sec as c_uint) * 1_000_000 + (*timeout).tv_usec as c_uint }
        };
        let mut o = timeval {
            tv_sec: 0,
            tv_usec: 0,
        };
        let mut s: [fd_set; 3] = unsafe { std::mem::zeroed() };
        if !readfds.is_null() {
            s[0] = unsafe { *readfds };
        }
        if !writefds.is_null() {
            s[1] = unsafe { *writefds };
        }
        if !errorfds.is_null() {
            s[2] = unsafe { *errorfds };
        }
        let mut x = 1;
        let mut r;
        // just check poll every x ms
        loop {
            r = self
                .inner
                .select(fn_ptr, nfds, readfds, writefds, errorfds, &mut o);
            log_syscall!("nop", "nop", r);
            if r != 0 || t == 0 {
                break;
            }
            _ = EventLoops::wait_event(Some(Duration::from_millis(u64::from(t.min(x)))));
            if t != c_uint::MAX {
                t = if t > x { t - x } else { 0 };
            }
            if x < 16 {
                x <<= 1;
            }

            if !readfds.is_null() {
                unsafe { *readfds = s[0] };
            }
            if !writefds.is_null() {
                unsafe { *writefds = s[1] };
            }
            if !errorfds.is_null() {
                unsafe { *errorfds = s[2] };
            }
            o.tv_sec = 0;
            o.tv_usec = 0;
        }
        r
    }

    fn socket(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int) -> c_int>,
        domain: c_int,
        ty: c_int,
        protocol: c_int,
    ) -> c_int {
        self.inner.socket(fn_ptr, domain, ty, protocol)
    }

    fn listen(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int) -> c_int>,
        socket: c_int,
        backlog: c_int,
    ) -> c_int {
        self.inner.listen(fn_ptr, socket, backlog)
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
            wait_read_event,
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
            let mut r = self.inner.connect(fn_ptr, socket, address, len);
            crate::log_syscall!(socket, "nop", r);
            if r == 0 {
                reset_errno();
                return r;
            }
            loop {
                let errno = Error::last_os_error().raw_os_error();
                if errno == Some(libc::EINPROGRESS) || errno == Some(libc::ENOTCONN) {
                    //阻塞，直到写事件发生
                    if EventLoops::wait_write_event(socket, &added, Some(Duration::from_millis(10)))
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
                        crate::log_syscall!(socket, "getsockopt", r);
                    }
                    if r != 0 {
                        r = -1;
                        break;
                    }
                    if err != 0 {
                        set_errno(err);
                        r = -1;
                        break;
                    };
                    unsafe {
                        let mut address = std::mem::zeroed();
                        let mut address_len = std::mem::zeroed();
                        r = libc::getpeername(socket, &mut address, &mut address_len);
                        crate::log_syscall!(socket, "getpeername", r);
                    }
                    if r == 0 {
                        reset_errno();
                        r = 0;
                        break;
                    }
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
        //取消对fd的监听
        match how {
            libc::SHUT_RD => EventLoops::del_read_event(socket),
            libc::SHUT_WR => EventLoops::del_write_event(socket),
            libc::SHUT_RDWR => EventLoops::del_event(socket),
            _ => {
                set_errno(libc::EINVAL);
                return -1;
            }
        };
        self.inner.shutdown(fn_ptr, socket, how)
    }

    fn close(&self, fn_ptr: Option<&extern "C" fn(c_int) -> c_int>, fd: c_int) -> c_int {
        //取消对fd的监听
        EventLoops::del_event(fd);
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

    fn recvfrom(
        &self,
        fn_ptr: Option<
            &extern "C" fn(
                c_int,
                *mut c_void,
                size_t,
                c_int,
                *mut sockaddr,
                *mut socklen_t,
            ) -> ssize_t,
        >,
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
        addr: *mut sockaddr,
        addrlen: *mut socklen_t,
    ) -> ssize_t {
        impl_read_nio_buf!(self, recvfrom, fn_ptr, socket, buf, len, flags, addr, addrlen)
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

    fn sendto(
        &self,
        fn_ptr: Option<
            &extern "C" fn(
                c_int,
                *const c_void,
                size_t,
                c_int,
                *const sockaddr,
                socklen_t,
            ) -> ssize_t,
        >,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
        addr: *const sockaddr,
        addrlen: socklen_t,
    ) -> ssize_t {
        impl_write_nio_buf!(self, sendto, fn_ptr, socket, buf, len, flags, addr, addrlen)
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
impl<I: LinuxSyscall> LinuxSyscall for NioLinuxSyscall<I> {
    fn epoll_ctl(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int, *mut epoll_event) -> c_int>,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut epoll_event,
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
        impl_nio!(self, accept4, wait_read_event, fn_ptr, fd, addr, len, flg)
    }
}
