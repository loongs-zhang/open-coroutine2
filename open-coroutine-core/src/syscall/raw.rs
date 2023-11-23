#[cfg(target_os = "linux")]
use crate::syscall::LinuxSyscall;
use crate::syscall::UnixSyscall;

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct RawLinuxSyscall {}

impl UnixSyscall for RawLinuxSyscall {}

#[cfg(target_os = "linux")]
impl LinuxSyscall for RawLinuxSyscall {}
