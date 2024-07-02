use crate::scheduler::SchedulableCoroutine;
use nix::sys::pthread::{pthread_self, Pthread};
use std::ffi::c_void;

#[repr(C)]
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct TaskNode {
    timestamp: u64,
    pthread: Pthread,
    coroutine: *const c_void,
}

impl TaskNode {
    pub(crate) fn new(timestamp: u64, coroutine: *const SchedulableCoroutine) -> Self {
        TaskNode {
            timestamp,
            pthread: pthread_self(),
            coroutine: coroutine.cast::<c_void>(),
        }
    }

    pub(crate) fn timestamp(&self) -> u64 {
        self.timestamp
    }

    pub(crate) fn pthread(&self) -> Pthread {
        self.pthread
    }

    pub(crate) fn coroutine(&self) -> &SchedulableCoroutine {
        unsafe { &*(self.coroutine.cast::<SchedulableCoroutine>()) }
    }
}
