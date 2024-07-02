use crate::coroutine::suspender::SimpleDelaySuspender;
use crate::scheduler::SchedulableSuspender;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[allow(clippy::pedantic, missing_docs)]
pub fn page_size() -> usize {
    static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);
    let mut ret = PAGE_SIZE.load(Ordering::Relaxed);
    if ret == 0 {
        unsafe {
            cfg_if::cfg_if! {
                if #[cfg(windows)] {
                    let mut info = std::mem::zeroed();
                    windows_sys::Win32::System::SystemInformation::GetSystemInfo(&mut info);
                    ret = info.dwPageSize as usize
                } else {
                    ret = libc::sysconf(libc::_SC_PAGESIZE) as usize;
                }
            }
        }
        PAGE_SIZE.store(ret, Ordering::Relaxed);
    }
    ret
}

/// Give the object a name.
pub trait Named {
    /// Get the name of this object.
    fn get_name(&self) -> &str;
}

/// A trait implemented for which needs `current()`.
pub trait Current<'c> {
    /// Init the current.
    fn init_current(current: &Self)
    where
        Self: Sized;

    /// Get the current if has.
    fn current() -> Option<&'c Self>
    where
        Self: Sized;

    /// clean the current.
    fn clean_current()
    where
        Self: Sized;
}

/// A trait for blocking current thread.
pub trait Blocker: Debug + Named {
    /// Block current thread for a while.
    fn block(&self, dur: Duration);
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct DelayBlocker {}

/// const `DELAY_BLOCKER_NAME`.
pub const DELAY_BLOCKER_NAME: &str = "DelayBlocker";

impl Named for DelayBlocker {
    fn get_name(&self) -> &str {
        DELAY_BLOCKER_NAME
    }
}

impl Blocker for DelayBlocker {
    fn block(&self, dur: Duration) {
        if let Some(suspender) = SchedulableSuspender::current() {
            suspender.delay(dur);
        }
    }
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct SleepBlocker {}

/// const `SLEEP_BLOCKER_NAME`.
pub const SLEEP_BLOCKER_NAME: &str = "SleepBlocker";

impl Named for SleepBlocker {
    fn get_name(&self) -> &str {
        SLEEP_BLOCKER_NAME
    }
}

impl Blocker for SleepBlocker {
    fn block(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

#[allow(missing_docs)]
#[repr(C)]
#[derive(Debug, Default)]
pub struct CondvarBlocker(Mutex<()>, Condvar);

/// const `CONDVAR_BLOCKER_NAME`.
pub const CONDVAR_BLOCKER_NAME: &str = "CondvarBlocker";

impl Named for CondvarBlocker {
    fn get_name(&self) -> &str {
        CONDVAR_BLOCKER_NAME
    }
}

impl Blocker for CondvarBlocker {
    fn block(&self, dur: Duration) {
        _ = self.1.wait_timeout(self.0.lock().unwrap(), dur);
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "net")] {
        use crate::net::event_loop::{EventLoop, EventLoopImpl};
        use std::sync::Arc;

        #[allow(missing_docs)]
        #[repr(C)]
        #[derive(Debug)]
        pub struct NetBlocker(pub Arc<EventLoopImpl<'static>>);

        /// const `NET_BLOCKER_NAME`.
        pub const NET_BLOCKER_NAME: &str = "NetBlocker";

        impl Named for NetBlocker {
            fn get_name(&self) -> &str {
                NET_BLOCKER_NAME
            }
        }

        impl Blocker for NetBlocker {
            fn block(&self, dur: Duration) {
                _ = self.0.wait_event(Some(dur));
            }
        }
    }
}

cfg_if::cfg_if! {
    if #[cfg(all(unix, feature = "net", feature = "preemptive-schedule"))] {
        use crate::net::core::EventLoops;
        use crate::pool::Pool;

        #[allow(missing_docs)]
        #[repr(C)]
        #[derive(Debug)]
        pub(crate) struct MonitorNetBlocker(Arc<EventLoopImpl<'static>>);

        impl MonitorNetBlocker {
            pub(crate) fn new() -> Self {
                MonitorNetBlocker(EventLoops::monitor().clone())
            }
        }

        /// const `MONITOR_NET_BLOCKER_NAME`.
        pub const MONITOR_NET_BLOCKER_NAME: &str = "MonitorNetBlocker";

        impl Named for MonitorNetBlocker {
            fn get_name(&self) -> &str {
                MONITOR_NET_BLOCKER_NAME
            }
        }

        impl Blocker for MonitorNetBlocker {
            fn block(&self, dur: Duration) {
                _ = self.0.wait_just(Some(dur));
                while !self.0.is_empty() {
                    if let Some(task) = self.0.pop() {
                        EventLoops::submit_raw(task);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn condvar_blocker() {
        let blocker = CondvarBlocker::default();
        let time = open_coroutine_timer::now();
        blocker.block(Duration::from_secs(1));
        let cost = Duration::from_nanos(open_coroutine_timer::now().saturating_sub(time));
        if Ordering::Less == cost.cmp(&Duration::from_secs(1)) {
            crate::error!("condvar_blocker cost {cost:?}");
        }
    }
}
