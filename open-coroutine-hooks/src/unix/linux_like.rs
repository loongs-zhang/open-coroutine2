use libc::{c_int, epoll_event, sockaddr, socklen_t};
use once_cell::sync::Lazy;

static ACCEPT4: Lazy<extern "C" fn(c_int, *mut sockaddr, *mut socklen_t, c_int) -> c_int> =
    init_hook!("accept4");

#[allow(unreachable_pub)]
#[no_mangle]
pub extern "C" fn accept4(
    fd: c_int,
    addr: *mut sockaddr,
    len: *mut socklen_t,
    flg: c_int,
) -> c_int {
    open_coroutine_core::syscall::facade::accept4(Some(Lazy::force(&ACCEPT4)), fd, addr, len, flg)
}

static EPOLL_CTL: Lazy<extern "C" fn(c_int, c_int, c_int, *mut epoll_event) -> c_int> =
    init_hook!("epoll_ctl");

#[allow(unreachable_pub)]
#[no_mangle]
pub extern "C" fn epoll_ctl(epfd: c_int, op: c_int, fd: c_int, event: *mut epoll_event) -> c_int {
    open_coroutine_core::syscall::facade::epoll_ctl(
        Some(Lazy::force(&EPOLL_CTL)),
        epfd,
        op,
        fd,
        event,
    )
}
