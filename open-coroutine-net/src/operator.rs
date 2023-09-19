use libc::{c_char, c_int, c_uint, c_void, off_t, size_t};
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::marker::PhantomData;
use std::time::Duration;

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use libc::{iovec, mode_t, msghdr, socklen_t, sockaddr};
    }
}

cfg_if::cfg_if! {
    if #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))] {
        use io_uring::opcode::{
            Accept, AsyncCancel, Close, Connect, EpollCtl, Fsync, MkDirAt, OpenAt, PollAdd, PollRemove,
            Read, Readv, Recv, RecvMsg, RenameAt, Send, SendMsg, Shutdown, Socket, Timeout, TimeoutRemove,
            TimeoutUpdate, Write, Writev,
        };
        use io_uring::squeue::Entry;
        use io_uring::types::{epoll_event, Fd, Timespec};
        use io_uring::{CompletionQueue, IoUring, Probe};
        use std::sync::Mutex;
        use std::collections::VecDeque;
        use once_cell::sync::Lazy;

        static SUPPORT: Lazy<bool> = Lazy::new(|| crate::version::current_kernel_version() >= crate::version::kernel_version(5, 6, 0));

        static PROBE: Lazy<Probe> = Lazy::new(|| {
            let mut probe = Probe::new();
            if let Ok(io_uring) = IoUring::new(2) {
                if let Ok(()) = io_uring.submitter().register_probe(&mut probe) {
                    return probe;
                }
            }
            panic!("probe init failed !")
        });

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

        static SUPPORT_WRITE: Lazy<bool> = support!(Write);

        static SUPPORT_WRITEV: Lazy<bool> = support!(Writev);

        static SUPPORT_SENDMSG: Lazy<bool> = support!(SendMsg);
    }
}

/// Returns `true` if support `io_uring`.
#[must_use]
pub fn support_io_uring() -> bool {
    cfg_if::cfg_if! {
        if #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))] {
            *SUPPORT
        } else {
            false
        }
    }
}

pub struct Operator<'o> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
    inner: IoUring,
    #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
    backlog: Mutex<VecDeque<&'o Entry>>,
    _phantom_data: PhantomData<&'o c_void>,
}

impl Debug for Operator<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        cfg_if::cfg_if! {
            if #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))] {
                f.debug_struct("Operator")
                    .field("backlog", &self.backlog)
                    .finish()
            } else {
                f.debug_struct("Operator").finish()
            }
        }
    }
}

// check https://www.rustwiki.org.cn/en/reference/introduction.html for help information
macro_rules! impl_if_support {
    ( $support:expr, $impls:expr ) => {
        return {
            #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
            if $support {
                $impls
            }
            Err(Error::new(ErrorKind::Unsupported, "unsupported"))
        }
    };
}

#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    unused_variables
)]
impl Operator<'_> {
    pub fn new(_cpu: u32) -> std::io::Result<Self> {
        Ok(Operator {
            #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
            inner: IoUring::builder().build(1024)?,
            #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
            backlog: Mutex::new(VecDeque::new()),
            _phantom_data: PhantomData,
        })
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
    #[allow(box_pointers)]
    fn push_sq(&self, entry: Entry) -> std::io::Result<()> {
        let entry = Box::leak(Box::new(entry));
        if unsafe { self.inner.submission_shared().push(entry).is_err() } {
            if let Ok(mut backlog) = self.backlog.lock() {
                backlog.push_back(entry);
            }
        }
        _ = self.inner.submit()?;
        Ok(())
    }

    pub fn async_cancel(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_ASYNC_CANCEL, {
            let entry = AsyncCancel::new(user_data as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// select impl
    #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))]
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

                if let Ok(mut backlog) = self.backlog.lock() {
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
            }
            return Ok((count, cq));
        })
    }

    /// poll

    #[cfg(target_os = "linux")]
    pub fn epoll_ctl(
        &self,
        user_data: usize,
        epfd: c_int,
        op: c_int,
        fd: c_int,
        event: *mut libc::epoll_event,
    ) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_EPOLL_CTL, {
            let entry = EpollCtl::new(Fd(epfd), Fd(fd), op, event as *const epoll_event)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    pub fn poll_add(&self, user_data: usize, fd: c_int, flags: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_POLL_ADD, {
            let entry = PollAdd::new(Fd(fd), flags as u32)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    pub fn poll_remove(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_POLL_REMOVE, {
            let entry = PollRemove::new(user_data as u64)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// timeout

    pub fn timeout_add(&self, user_data: usize, timeout: Option<Duration>) -> std::io::Result<()> {
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

    pub fn timeout_update(
        &self,
        user_data: usize,
        timeout: Option<Duration>,
    ) -> std::io::Result<()> {
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

    pub fn timeout_remove(&self, user_data: usize) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_TIMEOUT_REMOVE, {
            let entry = TimeoutRemove::new(user_data as u64).build();
            return self.push_sq(entry);
        })
    }

    /// file IO

    pub fn renameat(
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

    pub fn renameat2(
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

    pub fn fsync(&self, user_data: usize, fd: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_FSYNC, {
            let entry = Fsync::new(Fd(fd)).build().user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// socket

    pub fn socket(
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

    #[cfg(target_os = "linux")]
    pub fn accept4(
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

    pub fn shutdown(&self, user_data: usize, socket: c_int, how: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_SHUTDOWN, {
            let entry = Shutdown::new(Fd(socket), how)
                .build()
                .user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    pub fn close(&self, user_data: usize, fd: c_int) -> std::io::Result<()> {
        impl_if_support!(*SUPPORT_CLOSE, {
            let entry = Close::new(Fd(fd)).build().user_data(user_data as u64);
            return self.push_sq(entry);
        })
    }

    /// read

    pub fn recv(
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

    pub fn read(
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

    pub fn pread(
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

    /// write

    pub fn send(
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

    pub fn write(
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

    pub fn pwrite(
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
}

#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    unused_variables
)]
#[cfg(unix)]
impl Operator<'_> {
    /// file IO

    pub fn openat(
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

    pub fn mkdirat(
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

    /// socket

    pub fn accept(
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

    pub fn connect(
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

    /// read

    pub fn readv(
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

    pub fn preadv(
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

    pub fn recvmsg(
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

    pub fn writev(
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

    pub fn pwritev(
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

    pub fn sendmsg(
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

#[allow(box_pointers)]
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_unsupported() {
        assert!(!support_io_uring());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test() {
        eprintln!("support_io_uring->{}", support_io_uring());
    }

    cfg_if::cfg_if! {
        if #[cfg(all(target_os = "linux", target_arch = "x86_64", feature = "io_uring"))] {
            use io_uring::SubmissionQueue;
            use slab::Slab;
            use std::io::{BufRead, BufReader, Write};
            use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
            use std::os::unix::io::{AsRawFd, RawFd};
            use std::sync::atomic::{AtomicBool, Ordering};
            use std::sync::Arc;

            #[derive(Clone, Debug)]
            enum Token {
                Accept,
                Poll {
                    fd: RawFd,
                },
                Read {
                    fd: RawFd,
                    buf_index: usize,
                },
                Write {
                    fd: RawFd,
                    buf_index: usize,
                    offset: usize,
                    len: usize,
                },
            }

            struct AcceptCount {
                entry: Entry,
                count: usize,
            }

            impl AcceptCount {
                fn new(fd: RawFd, token: usize, count: usize) -> AcceptCount {
                    AcceptCount {
                        entry: Accept::new(Fd(fd), std::ptr::null_mut(), std::ptr::null_mut())
                            .build()
                            .user_data(token as _),
                        count,
                    }
                }

                fn push_to(&mut self, sq: &mut SubmissionQueue<'_>) {
                    while self.count > 0 {
                        unsafe {
                            match sq.push(&self.entry) {
                                Ok(_) => self.count -= 1,
                                Err(_) => break,
                            }
                        }
                    }

                    sq.sync();
                }
            }

            fn crate_server(port: u16, server_started: Arc<AtomicBool>) -> std::io::Result<()> {
                let mut ring: IoUring = IoUring::builder()
                    .setup_sqpoll(1000)
                    .setup_sqpoll_cpu(0)
                    .build(1024)?;
                let listener = TcpListener::bind(("127.0.0.1", port))?;

                let mut backlog = VecDeque::new();
                let mut bufpool = Vec::with_capacity(64);
                let mut buf_alloc = Slab::with_capacity(64);
                let mut token_alloc = Slab::with_capacity(64);

                println!("listen {}", listener.local_addr()?);
                server_started.store(true, Ordering::Release);

                let (submitter, mut sq, mut cq) = ring.split();

                let mut accept =
                    AcceptCount::new(listener.as_raw_fd(), token_alloc.insert(Token::Accept), 1);

                accept.push_to(&mut sq);

                loop {
                    match submitter.submit_and_wait(1) {
                        Ok(_) => (),
                        Err(ref err) if err.raw_os_error() == Some(libc::EBUSY) => (),
                        Err(err) => return Err(err.into()),
                    }
                    cq.sync();

                    // clean backlog
                    loop {
                        if sq.is_full() {
                            match submitter.submit() {
                                Ok(_) => (),
                                Err(ref err) if err.raw_os_error() == Some(libc::EBUSY) => break,
                                Err(err) => return Err(err.into()),
                            }
                        }
                        sq.sync();

                        match backlog.pop_front() {
                            Some(sqe) => unsafe {
                                let _ = sq.push(&sqe);
                            },
                            None => break,
                        }
                    }

                    accept.push_to(&mut sq);

                    for cqe in &mut cq {
                        let ret = cqe.result();
                        let token_index = cqe.user_data() as usize;

                        if ret < 0 {
                            eprintln!(
                                "token {:?} error: {:?}",
                                token_alloc.get(token_index),
                                Error::from_raw_os_error(-ret)
                            );
                            continue;
                        }

                        let token = &mut token_alloc[token_index];
                        match token.clone() {
                            Token::Accept => {
                                println!("accept");

                                accept.count += 1;

                                let fd = ret;
                                let poll_token = token_alloc.insert(Token::Poll { fd });

                                let poll_e = PollAdd::new(Fd(fd), libc::POLLIN as _)
                                    .build()
                                    .user_data(poll_token as _);

                                unsafe {
                                    if sq.push(&poll_e).is_err() {
                                        backlog.push_back(poll_e);
                                    }
                                }
                            }
                            Token::Poll { fd } => {
                                let (buf_index, buf) = match bufpool.pop() {
                                    Some(buf_index) => (buf_index, &mut buf_alloc[buf_index]),
                                    None => {
                                        let buf = vec![0u8; 2048].into_boxed_slice();
                                        let buf_entry = buf_alloc.vacant_entry();
                                        let buf_index = buf_entry.key();
                                        (buf_index, buf_entry.insert(buf))
                                    }
                                };

                                *token = Token::Read { fd, buf_index };

                                let read_e = Recv::new(Fd(fd), buf.as_mut_ptr(), buf.len() as _)
                                    .build()
                                    .user_data(token_index as _);

                                unsafe {
                                    if sq.push(&read_e).is_err() {
                                        backlog.push_back(read_e);
                                    }
                                }
                            }
                            Token::Read { fd, buf_index } => {
                                if ret == 0 {
                                    bufpool.push(buf_index);
                                    _ = token_alloc.remove(token_index);
                                    println!("shutdown connection");
                                    unsafe { _ = libc::close(fd) };

                                    println!("Server closed");
                                    return Ok(());
                                } else {
                                    let len = ret as usize;
                                    let buf = &buf_alloc[buf_index];

                                    *token = Token::Write {
                                        fd,
                                        buf_index,
                                        len,
                                        offset: 0,
                                    };

                                    let write_e = Send::new(Fd(fd), buf.as_ptr(), len as _)
                                        .build()
                                        .user_data(token_index as _);

                                    unsafe {
                                        if sq.push(&write_e).is_err() {
                                            backlog.push_back(write_e);
                                        }
                                    }
                                }
                            }
                            Token::Write {
                                fd,
                                buf_index,
                                offset,
                                len,
                            } => {
                                let write_len = ret as usize;

                                let entry = if offset + write_len >= len {
                                    bufpool.push(buf_index);

                                    *token = Token::Poll { fd };

                                    PollAdd::new(Fd(fd), libc::POLLIN as _)
                                        .build()
                                        .user_data(token_index as _)
                                } else {
                                    let offset = offset + write_len;
                                    let len = len - offset;

                                    let buf = &buf_alloc[buf_index][offset..];

                                    *token = Token::Write {
                                        fd,
                                        buf_index,
                                        offset,
                                        len,
                                    };

                                    io_uring::opcode::Write::new(Fd(fd), buf.as_ptr(), len as _)
                                        .build()
                                        .user_data(token_index as _)
                                };

                                unsafe {
                                    if sq.push(&entry).is_err() {
                                        backlog.push_back(entry);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            fn crate_client(port: u16, server_started: Arc<AtomicBool>) {
                //等服务端起来
                while !server_started.load(Ordering::Acquire) {}
                let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
                let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(3))
                    .unwrap_or_else(|_| panic!("connect to 127.0.0.1:{port} failed !"));
                let mut data: [u8; 512] = [b'1'; 512];
                data[511] = b'\n';
                let mut buffer: Vec<u8> = Vec::with_capacity(512);
                for _ in 0..3 {
                    //写入stream流，如果写入失败，提示"写入失败"
                    assert_eq!(512, stream.write(&data).expect("Failed to write!"));
                    print!("Client Send: {}", String::from_utf8_lossy(&data[..]));

                    let mut reader = BufReader::new(&stream);
                    //一直读到换行为止（b'\n'中的b表示字节），读到buffer里面
                    assert_eq!(
                        512,
                        reader
                            .read_until(b'\n', &mut buffer)
                            .expect("Failed to read into buffer")
                    );
                    print!("Client Received: {}", String::from_utf8_lossy(&buffer[..]));
                    assert_eq!(&data, &buffer as &[u8]);
                    buffer.clear();
                }
                //发送终止符
                assert_eq!(1, stream.write(&[b'e']).expect("Failed to write!"));
                println!("client closed");
            }

            #[test]
            fn original() -> std::io::Result<()> {
                let port = 8488;
                let server_started = Arc::new(AtomicBool::new(false));
                let clone = server_started.clone();
                let handle = std::thread::spawn(move || crate_server(port, clone));
                std::thread::spawn(move || crate_client(port, server_started))
                    .join()
                    .expect("client has error");
                handle.join().expect("server has error")
            }

            fn crate_server2(port: u16, server_started: Arc<AtomicBool>) -> std::io::Result<()> {
                let operator = Operator::new(0)?;
                let listener = TcpListener::bind(("127.0.0.1", port))?;

                let mut bufpool = Vec::with_capacity(64);
                let mut buf_alloc = Slab::with_capacity(64);
                let mut token_alloc = Slab::with_capacity(64);

                println!("listen {}", listener.local_addr()?);
                server_started.store(true, Ordering::Release);

                operator.accept(
                    token_alloc.insert(Token::Accept),
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )?;

                loop {
                    let mut r = operator.select(None)?;

                    for cqe in &mut r.1 {
                        let ret = cqe.result();
                        let token_index = cqe.user_data() as usize;

                        if ret < 0 {
                            eprintln!(
                                "token {:?} error: {:?}",
                                token_alloc.get(token_index),
                                Error::from_raw_os_error(-ret)
                            );
                            continue;
                        }

                        let token = &mut token_alloc[token_index];
                        match token.clone() {
                            Token::Accept => {
                                println!("accept");

                                let fd = ret;
                                let poll_token = token_alloc.insert(Token::Poll { fd });

                                operator.poll_add(poll_token, fd, libc::POLLIN as _)?;
                            }
                            Token::Poll { fd } => {
                                let (buf_index, buf) = match bufpool.pop() {
                                    Some(buf_index) => (buf_index, &mut buf_alloc[buf_index]),
                                    None => {
                                        let buf = vec![0u8; 2048].into_boxed_slice();
                                        let buf_entry = buf_alloc.vacant_entry();
                                        let buf_index = buf_entry.key();
                                        (buf_index, buf_entry.insert(buf))
                                    }
                                };

                                *token = Token::Read { fd, buf_index };

                                operator.recv(token_index, fd, buf.as_mut_ptr() as _, buf.len(), 0)?;
                            }
                            Token::Read { fd, buf_index } => {
                                if ret == 0 {
                                    bufpool.push(buf_index);
                                    _ = token_alloc.remove(token_index);
                                    println!("shutdown connection");
                                    unsafe { _ = libc::close(fd) };

                                    println!("Server closed");
                                    return Ok(());
                                } else {
                                    let len = ret as usize;
                                    let buf = &buf_alloc[buf_index];

                                    *token = Token::Write {
                                        fd,
                                        buf_index,
                                        len,
                                        offset: 0,
                                    };

                                    operator.send(token_index, fd, buf.as_ptr() as _, len, 0)?;
                                }
                            }
                            Token::Write {
                                fd,
                                buf_index,
                                offset,
                                len,
                            } => {
                                let write_len = ret as usize;

                                if offset + write_len >= len {
                                    bufpool.push(buf_index);

                                    *token = Token::Poll { fd };

                                    operator.poll_add(token_index, fd, libc::POLLIN as _)?;
                                } else {
                                    let offset = offset + write_len;
                                    let len = len - offset;

                                    let buf = &buf_alloc[buf_index][offset..];

                                    *token = Token::Write {
                                        fd,
                                        buf_index,
                                        offset,
                                        len,
                                    };

                                    operator.write(token_index, fd, buf.as_ptr() as _, len)?;
                                };
                            }
                        }
                    }
                }
            }

            #[test]
            fn framework() -> std::io::Result<()> {
                let port = 8498;
                let server_started = Arc::new(AtomicBool::new(false));
                let clone = server_started.clone();
                let handle = std::thread::spawn(move || crate_server2(port, clone));
                std::thread::spawn(move || crate_client(port, server_started))
                    .join()
                    .expect("client has error");
                handle.join().expect("server has error")
            }
        }
    }
}
