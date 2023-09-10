use crate::coroutine::constants::{Syscall, SyscallState};
use crate::coroutine::Current;
use crate::pool::{CoroutinePool, CoroutinePoolImpl};
use crate::scheduler::{Listener, SchedulableCoroutine};

#[derive(Debug)]
pub(crate) struct CoroutineCreator();

impl Listener for CoroutineCreator {
    fn on_suspend(&self, _: u64, _: &SchedulableCoroutine) {
        if let Some(pool) = CoroutinePoolImpl::current() {
            _ = pool.grow();
        }
    }

    fn on_syscall(&self, _: u64, _: &SchedulableCoroutine, _: Syscall, _: SyscallState) {
        if let Some(pool) = CoroutinePoolImpl::current() {
            _ = pool.grow();
        }
    }
}
