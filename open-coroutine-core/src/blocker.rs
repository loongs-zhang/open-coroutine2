use crate::coroutine::suspender::SimpleDelaySuspender;
use crate::coroutine::{Current, Named};
use crate::scheduler::SchedulableSuspender;
use std::fmt::Debug;
use std::time::Duration;

/// A trait for blocking current thread.
pub trait Blocker: Debug + Named {
    /// Block current thread for a while.
    fn block(&self, dur: Duration);
}

#[allow(missing_docs)]
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct DelayBlocker {}

impl Named for DelayBlocker {
    fn get_name(&self) -> &str {
        "DelayBlocker"
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
#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub struct SleepBlocker {}

impl Named for SleepBlocker {
    fn get_name(&self) -> &str {
        "SleepBlocker"
    }
}

impl Blocker for SleepBlocker {
    fn block(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}
