use io_uring::opcode::{
    Accept, AsyncCancel, Close, Connect, EpollCtl, Fsync, MkDirAt, OpenAt, PollAdd, PollRemove,
    Read, Readv, Recv, RecvMsg, RenameAt, Send, SendMsg, SendZc, Shutdown, Socket, Timeout,
    TimeoutRemove, TimeoutUpdate, Write, Writev,
};
use io_uring::squeue::Entry;
use io_uring::types::{epoll_event, Fd, Timespec};
use io_uring::{CompletionQueue, IoUring, Probe};
use libc::{
    c_char, c_int, c_uint, c_void, iovec, mode_t, msghdr, off_t, size_t, sockaddr, socklen_t,
};
use once_cell::sync::Lazy;
use std::collections::VecDeque;
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(test)]
mod tests;

pub trait Operator: Debug {
    fn async_cancel(&self, user_data: usize) -> std::io::Result<()>;

    /// poll

    fn epoll_ctl(
        &self,
        user_data: usize,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut libc::epoll_event,
    ) -> std::io::Result<()>;

    fn poll_add(&self, user_data: usize, fd: c_int, flags: c_int) -> std::io::Result<()>;

    fn poll_remove(&self, user_data: usize) -> std::io::Result<()>;

    /// timeout

    fn timeout_add(&self, user_data: usize, timeout: Option<Duration>) -> std::io::Result<()>;

    fn timeout_update(&self, user_data: usize, timeout: Option<Duration>) -> std::io::Result<()>;

    fn timeout_remove(&self, user_data: usize) -> std::io::Result<()>;

    /// file IO

    fn openat(
        &self,
        user_data: usize,
        dir_fd: c_int,
        pathname: *const c_char,
        flags: c_int,
        mode: mode_t,
    ) -> std::io::Result<()>;

    fn mkdirat(
        &self,
        user_data: usize,
        dir_fd: c_int,
        pathname: *const c_char,
        mode: mode_t,
    ) -> std::io::Result<()>;

    fn renameat(
        &self,
        user_data: usize,
        old_dir_fd: c_int,
        old_path: *const c_char,
        new_dir_fd: c_int,
        new_path: *const c_char,
    ) -> std::io::Result<()>;

    fn renameat2(
        &self,
        user_data: usize,
        old_dir_fd: c_int,
        old_path: *const c_char,
        new_dir_fd: c_int,
        new_path: *const c_char,
        flags: c_uint,
    ) -> std::io::Result<()>;

    fn fsync(&self, user_data: usize, fd: c_int) -> std::io::Result<()>;

    /// socket

    fn socket(
        &self,
        user_data: usize,
        domain: c_int,
        ty: c_int,
        protocol: c_int,
    ) -> std::io::Result<()>;

    fn accept(
        &self,
        user_data: usize,
        socket: c_int,
        address: *mut sockaddr,
        address_len: *mut socklen_t,
    ) -> std::io::Result<()>;

    fn accept4(
        &self,
        user_data: usize,
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: c_int,
    ) -> std::io::Result<()>;

    fn connect(
        &self,
        user_data: usize,
        socket: c_int,
        address: *const sockaddr,
        len: socklen_t,
    ) -> std::io::Result<()>;

    fn shutdown(&self, user_data: usize, socket: c_int, how: c_int) -> std::io::Result<()>;

    fn close(&self, user_data: usize, fd: c_int) -> std::io::Result<()>;

    /// read

    fn recv(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
    ) -> std::io::Result<()>;

    fn read(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
    ) -> std::io::Result<()>;

    fn pread(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<()>;

    fn readv(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> std::io::Result<()>;

    fn preadv(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> std::io::Result<()>;

    fn recvmsg(
        &self,
        user_data: usize,
        fd: c_int,
        msg: *mut msghdr,
        flags: c_int,
    ) -> std::io::Result<()>;

    /// write

    fn send(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
    ) -> std::io::Result<()>;

    #[allow(clippy::too_many_arguments)]
    fn sendto(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
        addr: *const sockaddr,
        addrlen: socklen_t,
    ) -> std::io::Result<()>;

    fn write(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
    ) -> std::io::Result<()>;

    fn pwrite(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<()>;

    fn writev(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> std::io::Result<()>;

    fn pwritev(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> std::io::Result<()>;

    fn sendmsg(
        &self,
        user_data: usize,
        fd: c_int,
        msg: *const msghdr,
        flags: c_int,
    ) -> std::io::Result<()>;
}

pub struct OperatorImpl<'o> {
    inner: IoUring,
    backlog: Mutex<VecDeque<&'o Entry>>,
}

impl Debug for OperatorImpl<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operator")
            .field("backlog", &self.backlog)
            .finish()
    }
}

static SUPPORT: Lazy<bool> = Lazy::new(|| {
    crate::version::current_kernel_version() >= crate::version::kernel_version(5, 6, 0)
});

#[must_use]
pub fn support_io_uring() -> bool {
    *SUPPORT
}

static PROBE: Lazy<Probe> = Lazy::new(|| {
    let mut probe = Probe::new();
    if let Ok(io_uring) = IoUring::new(2) {
        if let Ok(()) = io_uring.submitter().register_probe(&mut probe) {
            return probe;
        }
    }
    panic!("probe init failed !")
});

// check https://www.rustwiki.org.cn/en/reference/introduction.html for help information
macro_rules! support {
    ( $opcode:ident ) => {
        once_cell::sync::Lazy::new(|| {
            if support_io_uring() {
                return PROBE.is_supported(io_uring::opcode::$opcode::CODE);
            }
            false
        })
    };
}

static SUPPORT_ASYNC_CANCEL: Lazy<bool> = support!(AsyncCancel);

static SUPPORT_OPENAT: Lazy<bool> = support!(OpenAt);

static SUPPORT_MK_DIR_AT: Lazy<bool> = support!(MkDirAt);

static SUPPORT_RENAME_AT: Lazy<bool> = support!(RenameAt);

static SUPPORT_FSYNC: Lazy<bool> = support!(Fsync);

static SUPPORT_TIMEOUT_ADD: Lazy<bool> = support!(Timeout);

static SUPPORT_TIMEOUT_UPDATE: Lazy<bool> = support!(TimeoutUpdate);

static SUPPORT_TIMEOUT_REMOVE: Lazy<bool> = support!(TimeoutRemove);

static SUPPORT_EPOLL_CTL: Lazy<bool> = support!(EpollCtl);

static SUPPORT_POLL_ADD: Lazy<bool> = support!(PollAdd);

static SUPPORT_POLL_REMOVE: Lazy<bool> = support!(PollRemove);

static SUPPORT_SOCKET: Lazy<bool> = support!(Socket);

static SUPPORT_ACCEPT: Lazy<bool> = support!(Accept);

static SUPPORT_CONNECT: Lazy<bool> = support!(Connect);

static SUPPORT_SHUTDOWN: Lazy<bool> = support!(Shutdown);

static SUPPORT_CLOSE: Lazy<bool> = support!(Close);

static SUPPORT_RECV: Lazy<bool> = support!(Recv);

static SUPPORT_READ: Lazy<bool> = support!(Read);

static SUPPORT_READV: Lazy<bool> = support!(Readv);

static SUPPORT_RECVMSG: Lazy<bool> = support!(RecvMsg);

static SUPPORT_SEND: Lazy<bool> = support!(Send);

static SUPPORT_SEND_ZC: Lazy<bool> = support!(SendZc);

static SUPPORT_WRITE: Lazy<bool> = support!(Write);

static SUPPORT_WRITEV: Lazy<bool> = support!(Writev);

static SUPPORT_SENDMSG: Lazy<bool> = support!(SendMsg);

// check https://www.rustwiki.org.cn/en/reference/introduction.html for help information
macro_rules! impl_if_support {
    ( $support:expr, $impls:expr ) => {
        return {
            if $support {
                $impls
            }
            Err(Error::new(ErrorKind::Unsupported, "unsupported"))
        }
    };
}

impl OperatorImpl<'_> {
    pub fn new(_cpu: u32) -> std::io::Result<Self> {
        Ok(OperatorImpl {
            inner: IoUring::builder().build(1024)?,
            backlog: Mutex::new(VecDeque::new()),
        })
    }

    fn push_sq(&self, entry: Entry) -> std::io::Result<()> {
        let entry = Box::leak(Box::new(entry));
        if unsafe { self.inner.submission_shared().push(entry).is_err() } {
            self.backlog.lock().unwrap().push_back(entry);
        }
        self.inner.submit().map(|_| ())
    }

    /// select impl

    pub fn select(&self, timeout: Option<Duration>) -> std::io::Result<(usize, CompletionQueue)> {
        impl_if_support!(support_io_uring(), {
            let mut sq = unsafe { self.inner.submission_shared() };
            let mut cq = unsafe { self.inner.completion_shared() };
            if sq.is_empty() {
                return Ok((0, cq));
            }
            self.timeout_add(0, timeout)?;
            let count = match self.inner.submit_and_wait(1) {
                Ok(count) => count,
                Err(err) => {
                    if err.raw_os_error() == Some(libc::EBUSY) {
                        0
                    } else {
                        return Err(err);
                    }
                }
            };
            cq.sync();

            // clean backlog
            loop {
                if sq.is_full() {
                    match self.inner.submit() {
                        Ok(_) => (),
                        Err(err) => {
                            if err.raw_os_error() == Some(libc::EBUSY) {
                                break;
                            }
                            return Err(err);
                        }
                    }
                }
                sq.sync();

                let mut backlog = self.backlog.lock().unwrap();
                match backlog.pop_front() {
                    Some(sqe) => {
                        if unsafe { sq.push(sqe).is_err() } {
                            backlog.push_front(sqe);
                            break;
                        }
                    }
                    None => break,
                }
            }
            return Ok((count, cq));
        })
    }
}

impl Operator for OperatorImpl<'_> {
    fn async_cancel(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_ASYNC_CANCEL, {
            let entry = AsyncCancel::new(user_data as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// poll

    fn epoll_ctl(
        &self,
        user_data: usize,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut libc::epoll_event,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_EPOLL_CTL, {
            let entry = EpollCtl::new(
                Fd(epfd),
                Fd(fd),
                op,
                event.cast_const() as u64 as *const epoll_event,
            )
            .build()
            .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn poll_add(&self, user_data: usize, fd: c_int, flags: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_POLL_ADD, {
            let entry = PollAdd::new(Fd(fd), flags as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn poll_remove(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_POLL_REMOVE, {
            let entry = PollRemove::new(user_data as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// timeout

    fn timeout_add(&self, user_data: usize, timeout: Option<Duration>) -> std::io::Result<()> {
        if let Some(duration) = timeout {
            impl_if_support!(*SUPPORT_TIMEOUT_ADD, {
                let timeout = Timespec::new()
                    .sec(duration.as_secs())
                    .nsec(duration.subsec_nanos());
                let entry = Timeout::new(&timeout).build().user_data(user_data as u64);
                return self.push_sq(entry);
            })
        }
        Ok(())
    }

    fn timeout_update(&self, user_data: usize, timeout: Option<Duration>) -> std::io::Result<()> {
        if let Some(duration) = timeout {
            impl_if_support!(*SUPPORT_TIMEOUT_UPDATE, {
                let timeout = Timespec::new()
                    .sec(duration.as_secs())
                    .nsec(duration.subsec_nanos());
                let entry = TimeoutUpdate::new(user_data as u64, &timeout)
                    .build()
                    .user_data(user_data as u64);
                return self.push_sq(entry);
            })
        }
        self.timeout_remove(user_data)
    }

    fn timeout_remove(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_TIMEOUT_REMOVE, {
            let entry = TimeoutRemove::new(user_data as u64).build();
            return self.push_sq(entry);
        })
    }

    /// file IO

    fn openat(
        &self,
        user_data: usize,
        dir_fd: c_int,
        pathname: *const c_char,
        flags: c_int,
        mode: mode_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_OPENAT, {
            let entry = OpenAt::new(Fd(dir_fd), pathname)
                .flags(flags)
                .mode(mode)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn mkdirat(
        &self,
        user_data: usize,
        dir_fd: c_int,
        pathname: *const c_char,
        mode: mode_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_MK_DIR_AT, {
            let entry = MkDirAt::new(Fd(dir_fd), pathname)
                .mode(mode)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn renameat(
        &self,
        user_data: usize,
        old_dir_fd: c_int,
        old_path: *const c_char,
        new_dir_fd: c_int,
        new_path: *const c_char,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_RENAME_AT, {
            let entry = RenameAt::new(Fd(old_dir_fd), old_path, Fd(new_dir_fd), new_path)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn renameat2(
        &self,
        user_data: usize,
        old_dir_fd: c_int,
        old_path: *const c_char,
        new_dir_fd: c_int,
        new_path: *const c_char,
        flags: c_uint,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_RENAME_AT, {
            let entry = RenameAt::new(Fd(old_dir_fd), old_path, Fd(new_dir_fd), new_path)
                .flags(flags)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn fsync(&self, user_data: usize, fd: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_FSYNC, {
            let entry = Fsync::new(Fd(fd)).build().user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// socket

    fn socket(
        &self,
        user_data: usize,
        domain: c_int,
        ty: c_int,
        protocol: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SOCKET, {
            let entry = Socket::new(domain, ty, protocol)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn accept(
        &self,
        user_data: usize,
        socket: c_int,
        address: *mut sockaddr,
        address_len: *mut socklen_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_ACCEPT, {
            let entry = Accept::new(Fd(socket), address, address_len)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn accept4(
        &self,
        user_data: usize,
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_ACCEPT, {
            let entry = Accept::new(Fd(fd), addr, len)
                .flags(flg)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn connect(
        &self,
        user_data: usize,
        socket: c_int,
        address: *const sockaddr,
        len: socklen_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_CONNECT, {
            let entry = Connect::new(Fd(socket), address, len)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn shutdown(&self, user_data: usize, socket: c_int, how: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SHUTDOWN, {
            let entry = Shutdown::new(Fd(socket), how)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn close(&self, user_data: usize, fd: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_CLOSE, {
            let entry = Close::new(Fd(fd)).build().user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// read

    fn recv(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *mut c_void,
        len: size_t,
        flags: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_RECV, {
            let entry = Recv::new(Fd(socket), buf.cast::<u8>(), len as u32)
                .flags(flags)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn read(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_READ, {
            let entry = Read::new(Fd(fd), buf.cast::<u8>(), count as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn pread(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *mut c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_READ, {
            let entry = Read::new(Fd(fd), buf.cast::<u8>(), count as u32)
                .offset(offset as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn readv(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_READV, {
            let entry = Readv::new(Fd(fd), iov, iovcnt as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn preadv(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_READV, {
            let entry = Readv::new(Fd(fd), iov, iovcnt as u32)
                .offset(offset as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn recvmsg(
        &self,
        user_data: usize,
        fd: c_int,
        msg: *mut msghdr,
        flags: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_RECVMSG, {
            let entry = RecvMsg::new(Fd(fd), msg)
                .flags(flags as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// write

    fn send(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SEND, {
            let entry = Send::new(Fd(socket), buf.cast::<u8>(), len as u32)
                .flags(flags)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn sendto(
        &self,
        user_data: usize,
        socket: c_int,
        buf: *const c_void,
        len: size_t,
        flags: c_int,
        addr: *const sockaddr,
        addrlen: socklen_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SEND_ZC, {
            let entry = SendZc::new(Fd(socket), buf.cast::<u8>(), len as u32)
                .flags(flags)
                .dest_addr(addr)
                .dest_addr_len(addrlen)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn write(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_WRITE, {
            let entry = Write::new(Fd(fd), buf.cast::<u8>(), count as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn pwrite(
        &self,
        user_data: usize,
        fd: c_int,
        buf: *const c_void,
        count: size_t,
        offset: off_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_WRITE, {
            let entry = Write::new(Fd(fd), buf.cast::<u8>(), count as u32)
                .offset(offset as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn writev(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_WRITEV, {
            let entry = Writev::new(Fd(fd), iov, iovcnt as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn pwritev(
        &self,
        user_data: usize,
        fd: c_int,
        iov: *const iovec,
        iovcnt: c_int,
        offset: off_t,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_WRITEV, {
            let entry = Writev::new(Fd(fd), iov, iovcnt as u32)
                .offset(offset as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    fn sendmsg(
        &self,
        user_data: usize,
        fd: c_int,
        msg: *const msghdr,
        flags: c_int,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SENDMSG, {
            let entry = SendMsg::new(Fd(fd), msg)
                .flags(flags as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }
}
