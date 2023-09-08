use crate::coroutine::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::Suspender;
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Constants.
pub mod constants;

/// Coroutine local abstraction.
pub mod local;

/// Coroutine suspender abstraction.
pub mod suspender;

#[cfg(feature = "korosensei")]
pub use korosensei::{CoroutineImpl, SuspenderImpl};
#[allow(missing_docs)]
#[cfg(feature = "korosensei")]
mod korosensei;

#[cfg(all(feature = "boost", not(feature = "korosensei")))]
mod boost;

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

/// min stack size for backtrace
pub const DEFAULT_STACK_SIZE: usize = 64 * 1024;

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

/// A trait implemented for coroutines.
pub trait Coroutine<'c>: Debug + Eq + PartialEq + Ord + PartialOrd + Current<'c> {
    /// The type of value this coroutine accepts as a resume argument.
    type Resume;

    /// The type of value this coroutine yields.
    type Yield: Copy + Eq + PartialEq;

    /// The type of value this coroutine returns upon completion.
    type Return: Copy + Eq + PartialEq;

    /// Create a new coroutine.
    ///
    ///# Errors
    /// if stack allocate failed.
    fn new<F>(name: String, f: F, stack_size: usize) -> std::io::Result<Self>
    where
        F: FnOnce(
            &dyn Suspender<Resume = Self::Resume, Yield = Self::Yield>,
            Self::Resume,
        ) -> Self::Return,
        F: 'c,
        Self: Sized;

    /// Resumes the execution of this coroutine.
    ///
    /// The argument will be passed into the coroutine as a resume argument.
    ///
    /// # Errors
    /// if current coroutine state is unexpected.
    fn resume_with(
        &mut self,
        arg: Self::Resume,
    ) -> std::io::Result<CoroutineState<Self::Yield, Self::Return>>;

    /// Get the name of this coroutine.
    fn get_name(&self) -> &str;

    /// put/get some custom data to it.
    fn local(&self) -> &CoroutineLocal<'c>;
}

/// A trait implemented for describing changes in the state of the coroutine.
pub trait StateMachine<'c>: Coroutine<'c> {
    /// Returns the current state of this `StateMachine`.
    fn state(&self) -> CoroutineState<Self::Yield, Self::Return>;

    /// created -> ready
    /// syscall -> ready
    /// suspend -> ready
    ///
    /// # Errors
    /// if change state fails.
    fn ready(&self) -> std::io::Result<()>;

    /// created -> running
    /// ready -> running
    /// suspend -> running
    ///
    /// # Errors
    /// if change state fails.
    fn running(&self) -> std::io::Result<()>;

    /// running -> suspend
    ///
    /// # Errors
    /// if change state fails.
    fn suspend(&self, val: Self::Yield, timestamp: u64) -> std::io::Result<()>;

    /// running -> syscall
    /// inner: syscall -> syscall
    ///
    /// # Errors
    /// if change state fails.
    fn syscall(
        &self,
        val: Self::Yield,
        syscall: Syscall,
        syscall_state: SyscallState,
    ) -> std::io::Result<()>;

    /// running -> complete
    ///
    /// # Errors
    /// if change state fails.
    fn complete(&self, val: Self::Return) -> std::io::Result<()>;
}

/// A trait implemented for coroutines when Resume is ().
pub trait SimpleCoroutine<'c>: Coroutine<'c, Resume = ()> {
    /// Resumes the execution of this coroutine.
    ///
    /// # Errors
    /// see `resume_with`
    fn resume(&mut self) -> std::io::Result<CoroutineState<Self::Yield, Self::Return>>;
}

impl<'c, SimpleCoroutineImpl: Coroutine<'c, Resume = ()>> SimpleCoroutine<'c>
    for SimpleCoroutineImpl
{
    fn resume(&mut self) -> std::io::Result<CoroutineState<Self::Yield, Self::Return>> {
        self.resume_with(())
    }
}

/// Create a new coroutine.
#[macro_export]
macro_rules! co {
    ($f:expr, $size:expr $(,)?) => {
        $crate::coroutine::CoroutineImpl::new(uuid::Uuid::new_v4().to_string(), $f, $size)
            .expect("create coroutine failed !")
    };
    ($f:expr $(,)?) => {
        $crate::coroutine::CoroutineImpl::new(
            uuid::Uuid::new_v4().to_string(),
            $f,
            $crate::coroutine::DEFAULT_STACK_SIZE,
        )
        .expect("create coroutine failed !")
    };
    ($name:literal, $f:expr, $size:expr $(,)?) => {
        $crate::coroutine::CoroutineImpl::new($name, $f, $size).expect("create coroutine failed !")
    };
    ($name:literal, $f:expr $(,)?) => {
        $crate::coroutine::CoroutineImpl::new($name, $f, $crate::coroutine::DEFAULT_STACK_SIZE)
            .expect("create coroutine failed !")
    };
}

thread_local! {
    pub(crate) static COROUTINE: Cell<*const c_void> = Cell::new(std::ptr::null());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_return() {
        let mut coroutine = co!(|_: &dyn Suspender<'_, Yield = (), Resume = i32>, param| {
            assert_eq!(0, param);
            1
        });
        assert_eq!(
            CoroutineState::Complete(1),
            coroutine.resume_with(0).unwrap()
        );
    }

    #[test]
    fn test_yield_once() {
        let mut coroutine = co!(|suspender: &dyn Suspender<'_, Resume = i32, Yield = i32>,
                                 param| {
            assert_eq!(1, param);
            _ = suspender.suspend_with(2);
        });
        assert_eq!(
            CoroutineState::Suspend(2, 0),
            coroutine.resume_with(1).unwrap()
        );
    }

    #[test]
    fn test_yield() {
        let mut coroutine = co!(|suspender, input| {
            assert_eq!(1, input);
            assert_eq!(3, suspender.suspend_with(2));
            assert_eq!(5, suspender.suspend_with(4));
            6
        });
        assert_eq!(
            CoroutineState::Suspend(2, 0),
            coroutine.resume_with(1).unwrap()
        );
        assert_eq!(
            CoroutineState::Suspend(4, 0),
            coroutine.resume_with(3).unwrap()
        );
        assert_eq!(
            CoroutineState::Complete(6),
            coroutine.resume_with(5).unwrap()
        );
    }

    #[test]
    fn test_current() {
        assert!(CoroutineImpl::<i32, i32, i32>::current().is_none());
        let mut coroutine = co!(|_: &dyn Suspender<'_, Resume = i32, Yield = i32>, input| {
            assert_eq!(0, input);
            assert!(CoroutineImpl::<i32, i32, i32>::current().is_some());
            1
        });
        assert_eq!(
            CoroutineState::Complete(1),
            coroutine.resume_with(0).unwrap()
        );
    }

    #[test]
    fn test_backtrace() {
        let mut coroutine = co!(|suspender, input| {
            assert_eq!(1, input);
            println!("{:?}", backtrace::Backtrace::new());
            assert_eq!(3, suspender.suspend_with(2));
            println!("{:?}", backtrace::Backtrace::new());
            4
        });
        assert_eq!(
            CoroutineState::Suspend(2, 0),
            coroutine.resume_with(1).unwrap()
        );
        assert_eq!(
            CoroutineState::Complete(4),
            coroutine.resume_with(3).unwrap()
        );
    }

    #[test]
    fn test_context() {
        let mut coroutine = co!(|_: &dyn Suspender<'_, Resume = (), Yield = ()>, ()| {
            let current = CoroutineImpl::<(), (), ()>::current().unwrap();
            assert_eq!(2, *current.local().get("1").unwrap());
            *current.local().get_mut("1").unwrap() = 3;
            ()
        });
        assert!(coroutine.local().put("1", 1).is_none());
        assert_eq!(Some(1), coroutine.local().put("1", 2));
        assert_eq!(CoroutineState::Complete(()), coroutine.resume().unwrap());
        assert_eq!(Some(3), coroutine.local().remove("1"));
    }
}
