#[cfg(target_os = "linux")]
use crate::syscall::LinuxNetSyscall;
use crate::syscall::UnixNetSyscall;
use libc::{c_int, iovec, msghdr, off_t, size_t, sockaddr, socklen_t, ssize_t};
use open_coroutine_core::coroutine::constants::{Syscall, SyscallState};
use open_coroutine_core::coroutine::{Current, StateMachine};
use open_coroutine_core::scheduler::SchedulableCoroutine;
use std::ffi::c_void;

#[derive(Debug, Default)]
pub struct LinuxNetSyscallFacade<I: UnixNetSyscall> {
    inner: I,
}

impl<I: UnixNetSyscall> LinuxNetSyscallFacade<I> {
    pub fn new(inner: I) -> Self {
        LinuxNetSyscallFacade { inner }
    }
}

macro_rules! impl_facade {
    ( $self: expr, $syscall:ident, $($arg: expr),* $(,)* ) => {{
        if let Some(coroutine) = SchedulableCoroutine::current() {
            //协程进入系统调用状态
            coroutine
                .syscall((), Syscall::$syscall, SyscallState::Computing)
                .expect("change to syscall state failed !");
            let r = $self.inner.$syscall($($arg, )*);
            coroutine
                .syscall_resume()
                .expect("change to running state failed !");
            return r;
        }
        $self.inner.$syscall($($arg, )*)
    }};
}

impl<I: UnixNetSyscall> UnixNetSyscall for LinuxNetSyscallFacade<I> {
    fn socket(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int) -> c_int>,
        domain: c_int,
        ty: c_int,
        protocol: c_int,
    ) -> c_int {
        impl_facade!(self, socket, fn_ptr, domain, ty, protocol)
    }

    fn accept(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut sockaddr, *mut socklen_t) -> c_int>,
        socket: c_int,
        address: *mut sockaddr,
        address_len: *mut socklen_t,
    ) -> c_int {
        impl_facade!(self, accept, fn_ptr, socket, address, address_len)
    }

    fn connect(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const sockaddr, socklen_t) -> c_int>,
        socket: c_int,
        address: *const sockaddr,
        len: socklen_t,
    ) -> c_int {
        impl_facade!(self, connect, fn_ptr, socket, address, len)
    }

    fn shutdown(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int) -> c_int>,
        socket: c_int,
        how: c_int,
    ) -> c_int {
        impl_facade!(self, shutdown, fn_ptr, socket, how)
    }

    fn close(&self, fn_ptr: Option<&extern "C" fn(c_int) -> c_int>, fd: c_int) -> c_int {
        impl_facade!(self, close, fn_ptr, fd)
    }

    fn recv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t, c_int) -> ssize_t>,
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
    ) -> ssize_t {
        impl_facade!(self, recv, fn_ptr, socket, buf, len, flags)
    }

    fn read(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t) -> ssize_t>,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
    ) -> ssize_t {
        impl_facade!(self, read, fn_ptr, fd, buf, count)
    }

    fn pread(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut c_void, size_t, off_t) -> ssize_t>,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
        offset: off_t,
    ) -> ssize_t {
        impl_facade!(self, pread, fn_ptr, fd, buf, count, offset)
    }

    fn readv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> ssize_t {
        impl_facade!(self, readv, fn_ptr, fd, iov, iovcnt)
    }

    fn preadv(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int, off_t) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> ssize_t {
        impl_facade!(self, preadv, fn_ptr, fd, iov, iovcnt, offset)
    }

    fn recvmsg(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut msghdr, c_int) -> ssize_t>,
        fd: c_int,
        msg: *mut msghdr,
        flags: c_int,
    ) -> ssize_t {
        impl_facade!(self, recvmsg, fn_ptr, fd, msg, flags)
    }

    fn send(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t, c_int) -> ssize_t>,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
    ) -> ssize_t {
        impl_facade!(self, send, fn_ptr, socket, buf, len, flags)
    }

    fn write(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t) -> ssize_t>,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
    ) -> ssize_t {
        impl_facade!(self, write, fn_ptr, fd, buf, count)
    }

    fn pwrite(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const c_void, size_t, off_t) -> ssize_t>,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
        offset: off_t,
    ) -> ssize_t {
        impl_facade!(self, pwrite, fn_ptr, fd, buf, count, offset)
    }

    fn writev(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> ssize_t {
        impl_facade!(self, writev, fn_ptr, fd, iov, iovcnt)
    }

    fn pwritev(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const iovec, c_int, off_t) -> ssize_t>,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> ssize_t {
        impl_facade!(self, pwritev, fn_ptr, fd, iov, iovcnt, offset)
    }

    fn sendmsg(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *const msghdr, c_int) -> ssize_t>,
        fd: c_int,
        msg: *const msghdr,
        flags: c_int,
    ) -> ssize_t {
        impl_facade!(self, sendmsg, fn_ptr, fd, msg, flags)
    }
}

#[cfg(target_os = "linux")]
impl<I: LinuxNetSyscall> LinuxNetSyscall for LinuxNetSyscallFacade<I> {
    fn epoll_ctl(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, c_int, c_int, *mut libc::epoll_event) -> c_int>,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut libc::epoll_event,
    ) -> c_int {
        impl_facade!(self, epoll_ctl, fn_ptr, epfd, op, fd, event)
    }

    fn accept4(
        &self,
        fn_ptr: Option<&extern "C" fn(c_int, *mut sockaddr, *mut socklen_t, c_int) -> c_int>,
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: c_int,
    ) -> c_int {
        impl_facade!(self, accept4, fn_ptr, fd, addr, len, flg)
    }
}
