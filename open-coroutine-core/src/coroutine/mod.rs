use crate::coroutine::constants::CoroutineState;
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::{DelaySuspender, Suspender, SuspenderImpl};
use std::cell::Cell;
use std::ffi::c_void;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Constants.
pub mod constants;

/// Coroutine local abstraction.
pub mod local;

/// Coroutine suspender abstraction.
pub mod suspender;

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
pub trait Coroutine<'c>: Debug + Current<'c> {
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
    /// todo change to `std::io::Resule<CoroutineState<Self::Yield, Self::Return>>`
    fn resume_with(&mut self, arg: Self::Resume) -> CoroutineState<Self::Yield, Self::Return>;

    /// Get the name of this coroutine.
    fn get_name(&self) -> &str;

    /// put/get some custom data to it.
    fn local(&self) -> &CoroutineLocal<'c>;
}

/// A trait implemented for coroutines when Resume is ().
pub trait SimpleCoroutine<'c>: Coroutine<'c, Resume = ()> {
    /// Resumes the execution of this coroutine.
    fn resume(&mut self) -> CoroutineState<Self::Yield, Self::Return>;
}

impl<'c, SimpleCoroutineImpl: Coroutine<'c, Resume = ()>> SimpleCoroutine<'c>
    for SimpleCoroutineImpl
{
    fn resume(&mut self) -> CoroutineState<Self::Yield, Self::Return> {
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
    static COROUTINE: Cell<*const c_void> = Cell::new(std::ptr::null());
}
#[cfg(all(feature = "korosensei", not(feature = "boost")))]
pub use korosensei::CoroutineImpl;
#[allow(missing_docs)]
#[cfg(feature = "korosensei")]
mod korosensei {
    use super::*;
    use corosensei::stack::DefaultStack;
    use corosensei::{CoroutineResult, ScopedCoroutine};

    pub struct CoroutineImpl<'c, Param, Yield, Return>
    where
        Yield: Copy + Eq + PartialEq,
        Return: Copy + Eq + PartialEq,
    {
        name: String,
        inner: ScopedCoroutine<'c, Param, Yield, Return, DefaultStack>,
        state: Cell<CoroutineState<Yield, Return>>,
        local: CoroutineLocal<'c>,
    }

    impl<Param, Yield, Return> Drop for CoroutineImpl<'_, Param, Yield, Return>
    where
        Yield: Copy + Eq + PartialEq,
        Return: Copy + Eq + PartialEq,
    {
        fn drop(&mut self) {
            //for test_yield case
            if self.inner.started() && !self.inner.done() {
                unsafe { self.inner.force_reset() };
            }
        }
    }

    impl<Param, Yield, Return> Debug for CoroutineImpl<'_, Param, Yield, Return>
    where
        Yield: Copy + Eq + PartialEq + Debug,
        Return: Copy + Eq + PartialEq + Debug,
    {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Coroutine")
                .field("name", &self.name)
                .field("status", &self.state)
                .field("local", &self.local)
                .finish()
        }
    }

    impl<'c, Param, Yield, Return> Current<'c> for CoroutineImpl<'c, Param, Yield, Return>
    where
        Yield: Copy + Eq + PartialEq,
        Return: Copy + Eq + PartialEq,
    {
        #[allow(clippy::ptr_as_ptr)]
        fn init_current(suspender: &CoroutineImpl<'c, Param, Yield, Return>) {
            COROUTINE.with(|c| c.set(suspender as *const _ as *const c_void));
        }

        fn current() -> Option<&'c Self> {
            COROUTINE.with(|boxed| {
                let ptr = boxed.get();
                if ptr.is_null() {
                    None
                } else {
                    Some(unsafe { &*(ptr).cast::<CoroutineImpl<'c, Param, Yield, Return>>() })
                }
            })
        }

        fn clean_current() {
            COROUTINE.with(|boxed| boxed.set(std::ptr::null()));
        }
    }

    impl<'c, Param, Yield, Return> Coroutine<'c> for CoroutineImpl<'c, Param, Yield, Return>
    where
        Yield: Copy + Eq + PartialEq + Debug,
        Return: Copy + Eq + PartialEq + Debug,
    {
        type Resume = Param;
        type Yield = Yield;
        type Return = Return;

        fn new<F>(name: String, f: F, stack_size: usize) -> std::io::Result<Self>
        where
            F: FnOnce(
                &dyn Suspender<Resume = Self::Resume, Yield = Self::Yield>,
                Self::Resume,
            ) -> Self::Return,
            F: 'c,
            Self: Sized,
        {
            let stack = DefaultStack::new(stack_size.max(page_size()))?;
            let inner = ScopedCoroutine::with_stack(stack, |y, p| {
                let suspender = SuspenderImpl::new(y);
                SuspenderImpl::<Param, Yield>::init_current(&suspender);
                let r = f(&suspender, p);
                SuspenderImpl::<Param, Yield>::clean_current();
                r
            });
            Ok(CoroutineImpl {
                name,
                inner,
                state: Cell::new(CoroutineState::Created),
                local: CoroutineLocal::default(),
            })
        }

        fn resume_with(&mut self, arg: Self::Resume) -> CoroutineState<Self::Yield, Self::Return> {
            let mut current = self.state.get_mut();
            match current {
                CoroutineState::Created | CoroutineState::Ready | CoroutineState::Suspend(_, 0) => {
                    self.state.set(CoroutineState::Running);
                }
                CoroutineState::Complete(r) => return CoroutineState::Complete(*r),
                _ => panic!("{} unexpected state {current}", self.name),
            }
            CoroutineImpl::<Param, Yield, Return>::init_current(self);
            let r = match self.inner.resume(arg) {
                CoroutineResult::Yield(y) => {
                    current = self.state.get_mut();
                    match current {
                        CoroutineState::Running => {
                            let new_state = CoroutineState::Suspend(
                                y,
                                SuspenderImpl::<Yield, Param>::timestamp(),
                            );
                            let previous = self.state.replace(new_state);
                            assert_eq!(
                                CoroutineState::Running,
                                previous,
                                "{} unexpected state {}",
                                self.get_name(),
                                previous
                            );
                            new_state
                        }
                        CoroutineState::SystemCall(val, syscall, state) => {
                            CoroutineState::SystemCall(*val, *syscall, *state)
                        }
                        _ => panic!("{} unexpected state {current}", self.name),
                    }
                }
                CoroutineResult::Return(r) => {
                    let state = CoroutineState::Complete(r);
                    let current = self.state.replace(state);
                    assert_eq!(
                        CoroutineState::Running,
                        current,
                        "{} unexpected state {}",
                        self.get_name(),
                        current
                    );
                    state
                }
            };
            CoroutineImpl::<Param, Yield, Return>::clean_current();
            r
        }

        fn get_name(&self) -> &str {
            &self.name
        }

        fn local(&self) -> &CoroutineLocal<'c> {
            &self.local
        }
    }
}

// #[cfg(all(feature = "boost", not(feature = "korosensei")))]
// pub use boost::CoroutineImpl;
#[cfg(feature = "boost")]
mod boost {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_return() {
        let mut coroutine = co!(|_: &dyn Suspender<'_, Yield = (), Resume = i32>, param| {
            assert_eq!(0, param);
            1
        });
        assert_eq!(CoroutineState::Complete(1), coroutine.resume_with(0));
    }

    #[test]
    fn test_yield_once() {
        let mut coroutine = co!(|suspender: &dyn Suspender<'_, Resume = i32, Yield = i32>,
                                 param| {
            assert_eq!(1, param);
            _ = suspender.suspend_with(2);
        });
        assert_eq!(CoroutineState::Suspend(2, 0), coroutine.resume_with(1));
    }

    #[test]
    fn test_yield() {
        let mut coroutine = co!(|suspender, input| {
            assert_eq!(1, input);
            assert_eq!(3, suspender.suspend_with(2));
            assert_eq!(5, suspender.suspend_with(4));
            6
        });
        assert_eq!(CoroutineState::Suspend(2, 0), coroutine.resume_with(1));
        assert_eq!(CoroutineState::Suspend(4, 0), coroutine.resume_with(3));
        assert_eq!(CoroutineState::Complete(6), coroutine.resume_with(5));
    }

    #[test]
    fn test_current() {
        assert!(CoroutineImpl::<i32, i32, i32>::current().is_none());
        let mut coroutine = co!(|_: &dyn Suspender<'_, Resume = i32, Yield = i32>, input| {
            assert_eq!(0, input);
            assert!(CoroutineImpl::<i32, i32, i32>::current().is_some());
            1
        });
        assert_eq!(CoroutineState::Complete(1), coroutine.resume_with(0));
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
        assert_eq!(CoroutineState::Suspend(2, 0), coroutine.resume_with(1));
        assert_eq!(CoroutineState::Complete(4), coroutine.resume_with(3));
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
        assert_eq!(CoroutineState::Complete(()), coroutine.resume());
        assert_eq!(Some(3), coroutine.local().remove("1"));
    }
}
