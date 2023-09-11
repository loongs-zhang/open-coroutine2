use std::fmt::Debug;
use std::time::Duration;

/// A trait for blocking current thread.
pub trait Blocker: Debug {
    /// Block current thread for a while.
    fn block(&self, dur: Duration);
}

#[allow(missing_docs)]
#[derive(Debug, Copy, Clone)]
pub struct SleepBlocker {}

impl Blocker for SleepBlocker {
    fn block(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}