#[cfg(target_os = "linux")]
use crate::syscall::LinuxNetSyscall;
use crate::syscall::UnixNetSyscall;

#[derive(Debug, Copy, Clone, Default)]
pub struct RawLinuxNetSyscall {}

impl UnixNetSyscall for RawLinuxNetSyscall {}

#[cfg(target_os = "linux")]
impl LinuxNetSyscall for RawLinuxNetSyscall {}
