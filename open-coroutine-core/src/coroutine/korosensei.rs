use crate::constants::{CoroutineState, Syscall, SyscallState};
use crate::coroutine::local::CoroutineLocal;
use crate::coroutine::suspender::{DelaySuspender, Suspender, SuspenderImpl};
use crate::coroutine::{Coroutine, Current, Named, StateMachine, COROUTINE};
use corosensei::stack::DefaultStack;
use corosensei::trap::TrapHandlerRegs;
use corosensei::{CoroutineResult, ScopedCoroutine};
use std::cell::Cell;
use std::cmp::Ordering;
use std::ffi::c_void;
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::panic::{AssertUnwindSafe, UnwindSafe};

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
    } else if #[cfg(windows)] {
        use windows_sys::Win32::Foundation::{EXCEPTION_ACCESS_VIOLATION, EXCEPTION_STACK_OVERFLOW};
        use windows_sys::Win32::System::Diagnostics::Debug::{AddVectoredExceptionHandler, EXCEPTION_POINTERS};
    }
}

#[repr(C)]
pub struct CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
{
    name: String,
    inner: ScopedCoroutine<'c, Param, Yield, Result<Return, &'static str>, DefaultStack>,
    state: Cell<CoroutineState<Yield, Return>>,
    local: CoroutineLocal<'c>,
}

impl<Param, Yield, Return> Drop for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
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
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + Debug + UnwindSafe,
    Return: Copy + Eq + PartialEq + Debug + UnwindSafe,
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
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + UnwindSafe,
    Return: Copy + Eq + PartialEq + UnwindSafe,
{
    #[allow(clippy::ptr_as_ptr)]
    fn init_current(current: &CoroutineImpl<'c, Param, Yield, Return>) {
        COROUTINE.with(|s| {
            s.borrow_mut()
                .push_front(std::ptr::from_ref(current) as *const c_void);
        });
    }

    fn current() -> Option<&'c Self> {
        COROUTINE.with(|s| {
            s.borrow()
                .front()
                .map(|ptr| unsafe { &*(*ptr).cast::<CoroutineImpl<'c, Param, Yield, Return>>() })
        })
    }

    fn clean_current() {
        COROUTINE.with(|s| _ = s.borrow_mut().pop_front());
    }
}

impl<Param, Yield, Return> Eq for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
}

impl<Param, Yield, Return> PartialEq<Self> for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(&other.name)
    }
}

impl<Param, Yield, Return> Ord for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.name.cmp(&other.name)
    }
}

impl<Param, Yield, Return> PartialOrd<Self> for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Param, Yield, Return> Named for CoroutineImpl<'_, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn get_name(&self) -> &str {
        &self.name
    }
}

impl<'c, Param, Yield, Return> Coroutine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Eq + PartialEq + Debug + UnwindSafe,
    Return: Copy + Eq + PartialEq + Debug + UnwindSafe,
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
        F: UnwindSafe,
        F: 'c,
        Self: Sized,
    {
        let stack = DefaultStack::new(stack_size.max(crate::common::page_size()))?;
        #[cfg(feature = "logs")]
        let co_name = name.clone().leak();
        let inner = ScopedCoroutine::with_stack(stack, move |y, p| {
            let suspender = SuspenderImpl(y);
            SuspenderImpl::<Param, Yield>::init_current(&suspender);
            let r = std::panic::catch_unwind(AssertUnwindSafe(|| f(&suspender, p))).map_err(|e| {
                let message = *e
                    .downcast_ref::<&'static str>()
                    .unwrap_or(&"coroutine failed without message");
                crate::error!("coroutine:{} finish with error:{}", co_name, message);
                message
            });
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

    fn resume_with(
        &mut self,
        arg: Self::Resume,
    ) -> std::io::Result<CoroutineState<Self::Yield, Self::Return>> {
        if let CoroutineState::Complete(r) = self.state() {
            return Ok(CoroutineState::Complete(r));
        }
        Self::setup_trap_handler();
        self.running()?;
        Self::init_current(self);
        let r = match self.inner.resume(arg) {
            CoroutineResult::Yield(y) => {
                let current = self.state();
                match current {
                    CoroutineState::Running => {
                        let timestamp = SuspenderImpl::<Yield, Param>::timestamp();
                        let new_state = CoroutineState::Suspend(y, timestamp);
                        self.suspend(y, timestamp)?;
                        new_state
                    }
                    CoroutineState::SystemCall(y, syscall, state) => {
                        CoroutineState::SystemCall(y, syscall, state)
                    }
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Other,
                            format!("{} unexpected state {current}", self.name),
                        ))
                    }
                }
            }
            CoroutineResult::Return(result) => {
                if let Ok(returns) = result {
                    self.complete(returns)?;
                    CoroutineState::Complete(returns)
                } else {
                    let message = result.unwrap_err();
                    self.error(message)?;
                    CoroutineState::Error(message)
                }
            }
        };
        Self::clean_current();
        Ok(r)
    }

    fn local(&self) -> &CoroutineLocal<'c> {
        &self.local
    }
}

impl<'c, Param, Yield, Return> StateMachine<'c> for CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe,
    Yield: Copy + Debug + Eq + PartialEq + UnwindSafe,
    Return: Copy + Debug + Eq + PartialEq + UnwindSafe,
{
    fn state(&self) -> CoroutineState<Self::Yield, Self::Return> {
        self.state.get()
    }

    fn ready(&self) -> std::io::Result<()> {
        let current = self.state();
        match current {
            CoroutineState::Created => {
                self.state.set(CoroutineState::Ready);
                return Ok(());
            }
            CoroutineState::Suspend(_, timestamp) => {
                if timestamp <= open_coroutine_timer::now() {
                    self.state.set(CoroutineState::Ready);
                    return Ok(());
                }
            }
            _ => {}
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Ready
            ),
        ))
    }

    fn running(&self) -> std::io::Result<()> {
        let current = self.state();
        match current {
            CoroutineState::Created | CoroutineState::Ready => {
                self.state.set(CoroutineState::Running);
                return Ok(());
            }
            CoroutineState::Suspend(_, timestamp) => {
                if timestamp <= open_coroutine_timer::now() {
                    self.state.set(CoroutineState::Running);
                    return Ok(());
                }
            }
            CoroutineState::SystemCall(_, _, _) => return Ok(()),
            _ => {}
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Running
            ),
        ))
    }

    fn suspend(&self, val: Self::Yield, timestamp: u64) -> std::io::Result<()> {
        let current = self.state();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Suspend(val, timestamp));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Suspend(val, timestamp)
            ),
        ))
    }

    fn syscall(
        &self,
        val: Self::Yield,
        syscall: Syscall,
        syscall_state: SyscallState,
    ) -> std::io::Result<()> {
        let current = self.state();
        match current {
            CoroutineState::Running => {
                let state = CoroutineState::SystemCall(val, syscall, syscall_state);
                self.state.set(state);
                crate::info!("{} {}->{}", self.get_name(), current, state);
                return Ok(());
            }
            CoroutineState::SystemCall(_, original_syscall, _) => {
                if original_syscall == syscall {
                    let state = CoroutineState::SystemCall(val, syscall, syscall_state);
                    self.state.set(state);
                    crate::info!("{} {}->{}", self.get_name(), current, state);
                    return Ok(());
                }
            }
            _ => {}
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::SystemCall(val, syscall, syscall_state)
            ),
        ))
    }

    fn syscall_resume(&self) -> std::io::Result<()> {
        let current = self.state();
        if let CoroutineState::SystemCall(_, _, SyscallState::Finished | SyscallState::Timeout) =
            current
        {
            let state = CoroutineState::Running;
            self.state.set(state);
            crate::info!("{} {}->{}", self.get_name(), current, state);
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Running
            ),
        ))
    }

    fn complete(&self, val: Self::Return) -> std::io::Result<()> {
        let current = self.state();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Complete(val));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Complete(val)
            ),
        ))
    }

    fn error(&self, val: &'static str) -> std::io::Result<()> {
        let current = self.state();
        if CoroutineState::Running == current {
            self.state.set(CoroutineState::Error(val));
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::Other,
            format!(
                "{} unexpected {current}->{}",
                self.name,
                CoroutineState::<Yield, Return>::Error(val)
            ),
        ))
    }
}

impl<'c, Param, Yield, Return> CoroutineImpl<'c, Param, Yield, Return>
where
    Param: UnwindSafe + 'c,
    Yield: Copy + Eq + PartialEq + UnwindSafe + 'c,
    Return: Copy + Eq + PartialEq + UnwindSafe + 'c,
{
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            #[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
            extern "C" fn trap_handler(
                _signum: libc::c_int,
                _siginfo: *mut libc::siginfo_t,
                context: *mut c_void,
            ) {
                unsafe {
                    //do not disable clippy::transmute_ptr_to_ref
                    #[allow(clippy::transmute_ptr_to_ref)]
                    let context: &mut libc::ucontext_t = std::mem::transmute(context);
                    cfg_if::cfg_if! {
                        if #[cfg(all(
                            any(target_os = "linux", target_os = "android"),
                            target_arch = "x86_64",
                        ))] {
                            let sp = context.uc_mcontext.gregs[libc::REG_RSP as usize] as usize;
                        } else if #[cfg(all(
                            any(target_os = "linux", target_os = "android"),
                            target_arch = "x86",
                        ))] {
                            let sp = context.uc_mcontext.gregs[libc::REG_ESP as usize] as usize;
                        } else if #[cfg(all(target_vendor = "apple", target_arch = "x86_64"))] {
                            let sp = (*context.uc_mcontext).__ss.__rsp as usize;
                        } else if #[cfg(all(
                                any(target_os = "linux", target_os = "android"),
                                target_arch = "aarch64",
                            ))] {
                            let sp = context.uc_mcontext.sp as usize;
                        } else if #[cfg(all(
                            any(target_os = "linux", target_os = "android"),
                            target_arch = "arm",
                        ))] {
                            let sp = context.uc_mcontext.arm_sp as usize;
                        } else if #[cfg(all(
                            any(target_os = "linux", target_os = "android"),
                            any(target_arch = "riscv64", target_arch = "riscv32"),
                        ))] {
                            let sp = context.uc_mcontext.__gregs[libc::REG_SP] as usize;
                        } else if #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))] {
                            let sp = (*context.uc_mcontext).__ss.__sp as usize;
                        } else if #[cfg(all(target_os = "linux", target_arch = "loongarch64"))] {
                            let sp = context.uc_mcontext.__gregs[3] as usize;
                        } else {
                            compile_error!("Unsupported platform");
                        }
                    }
                    if let Some(co) = Self::current() {
                        let handler = co.inner.trap_handler();
                        assert!(handler.stack_ptr_in_bounds(sp));
                        let regs = handler.setup_trap_handler(|| Err("invalid memory reference"));
                        cfg_if::cfg_if! {
                            if #[cfg(all(
                                    any(target_os = "linux", target_os = "android"),
                                    target_arch = "x86_64",
                                ))] {
                                let TrapHandlerRegs { rip, rsp, rbp, rdi, rsi } = regs;
                                context.uc_mcontext.gregs[libc::REG_RIP as usize] = rip as i64;
                                context.uc_mcontext.gregs[libc::REG_RSP as usize] = rsp as i64;
                                context.uc_mcontext.gregs[libc::REG_RBP as usize] = rbp as i64;
                                context.uc_mcontext.gregs[libc::REG_RDI as usize] = rdi as i64;
                                context.uc_mcontext.gregs[libc::REG_RSI as usize] = rsi as i64;
                            } else if #[cfg(all(
                                any(target_os = "linux", target_os = "android"),
                                target_arch = "x86",
                            ))] {
                                let TrapHandlerRegs { eip, esp, ebp, ecx, edx } = regs;
                                context.uc_mcontext.gregs[libc::REG_EIP as usize] = eip as i32;
                                context.uc_mcontext.gregs[libc::REG_ESP as usize] = esp as i32;
                                context.uc_mcontext.gregs[libc::REG_EBP as usize] = ebp as i32;
                                context.uc_mcontext.gregs[libc::REG_ECX as usize] = ecx as i32;
                                context.uc_mcontext.gregs[libc::REG_EDX as usize] = edx as i32;
                            } else if #[cfg(all(target_vendor = "apple", target_arch = "x86_64"))] {
                                let TrapHandlerRegs { rip, rsp, rbp, rdi, rsi } = regs;
                                (*context.uc_mcontext).__ss.__rip = rip;
                                (*context.uc_mcontext).__ss.__rsp = rsp;
                                (*context.uc_mcontext).__ss.__rbp = rbp;
                                (*context.uc_mcontext).__ss.__rdi = rdi;
                                (*context.uc_mcontext).__ss.__rsi = rsi;
                            } else if #[cfg(all(
                                    any(target_os = "linux", target_os = "android"),
                                    target_arch = "aarch64",
                                ))] {
                                let TrapHandlerRegs { pc, sp, x0, x1, x29, lr } = regs;
                                context.uc_mcontext.pc = pc;
                                context.uc_mcontext.sp = sp;
                                context.uc_mcontext.regs[0] = x0;
                                context.uc_mcontext.regs[1] = x1;
                                context.uc_mcontext.regs[29] = x29;
                                context.uc_mcontext.regs[30] = lr;
                            } else if #[cfg(all(
                                    any(target_os = "linux", target_os = "android"),
                                    target_arch = "arm",
                                ))] {
                                let TrapHandlerRegs {
                                    pc,
                                    r0,
                                    r1,
                                    r7,
                                    r11,
                                    r13,
                                    r14,
                                    cpsr_thumb,
                                    cpsr_endian,
                                } = regs;
                                context.uc_mcontext.arm_pc = pc;
                                context.uc_mcontext.arm_r0 = r0;
                                context.uc_mcontext.arm_r1 = r1;
                                context.uc_mcontext.arm_r7 = r7;
                                context.uc_mcontext.arm_fp = r11;
                                context.uc_mcontext.arm_sp = r13;
                                context.uc_mcontext.arm_lr = r14;
                                if cpsr_thumb {
                                    context.uc_mcontext.arm_cpsr |= 0x20;
                                } else {
                                    context.uc_mcontext.arm_cpsr &= !0x20;
                                }
                                if cpsr_endian {
                                    context.uc_mcontext.arm_cpsr |= 0x200;
                                } else {
                                    context.uc_mcontext.arm_cpsr &= !0x200;
                                }
                            } else if #[cfg(all(
                                any(target_os = "linux", target_os = "android"),
                                any(target_arch = "riscv64", target_arch = "riscv32"),
                            ))] {
                                let TrapHandlerRegs { pc, ra, sp, a0, a1, s0 } = regs;
                                context.uc_mcontext.__gregs[libc::REG_PC] = pc as libc::c_ulong;
                                context.uc_mcontext.__gregs[libc::REG_RA] = ra as libc::c_ulong;
                                context.uc_mcontext.__gregs[libc::REG_SP] = sp as libc::c_ulong;
                                context.uc_mcontext.__gregs[libc::REG_A0] = a0 as libc::c_ulong;
                                context.uc_mcontext.__gregs[libc::REG_A0 + 1] = a1 as libc::c_ulong;
                                context.uc_mcontext.__gregs[libc::REG_S0] = s0 as libc::c_ulong;
                            } else if #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))] {
                                let TrapHandlerRegs { pc, sp, x0, x1, x29, lr } = regs;
                                (*context.uc_mcontext).__ss.__pc = pc;
                                (*context.uc_mcontext).__ss.__sp = sp;
                                (*context.uc_mcontext).__ss.__x[0] = x0;
                                (*context.uc_mcontext).__ss.__x[1] = x1;
                                (*context.uc_mcontext).__ss.__fp = x29;
                                (*context.uc_mcontext).__ss.__lr = lr;
                            } else if #[cfg(all(target_os = "linux", target_arch = "loongarch64"))] {
                                let TrapHandlerRegs { pc, sp, a0, a1, fp, ra } = regs;
                                context.uc_mcontext.__pc = pc;
                                context.uc_mcontext.__gregs[1] = ra;
                                context.uc_mcontext.__gregs[3] = sp;
                                context.uc_mcontext.__gregs[4] = a0;
                                context.uc_mcontext.__gregs[5] = a1;
                                context.uc_mcontext.__gregs[22] = fp;
                            } else {
                                compile_error!("Unsupported platform");
                            }
                        }
                    }
                }
            }
        } else if #[cfg(windows)] {
            unsafe extern "system" fn trap_handler(exception_info: *mut EXCEPTION_POINTERS) -> i32 {
                match (*(*exception_info).ExceptionRecord).ExceptionCode {
                    EXCEPTION_ACCESS_VIOLATION | EXCEPTION_STACK_OVERFLOW => {}
                    _ => return 0, // EXCEPTION_CONTINUE_SEARCH
                }

                if let Some(co) = Self::current() {
                    cfg_if::cfg_if! {
                        if #[cfg(target_arch = "x86_64")] {
                            let sp = (*(*exception_info).ContextRecord).Rsp as usize;
                        } else if #[cfg(target_arch = "x86")] {
                            let sp = (*(*exception_info).ContextRecord).Esp as usize;
                        } else {
                            compile_error!("Unsupported platform");
                        }
                    }

                    let handler = co.inner.trap_handler();
                    if !handler.stack_ptr_in_bounds(sp) {
                        // EXCEPTION_CONTINUE_SEARCH
                        return 0;
                    }
                    let regs = handler.setup_trap_handler(|| Err("invalid memory reference"));

                    cfg_if::cfg_if! {
                        if #[cfg(target_arch = "x86_64")] {
                            let TrapHandlerRegs { rip, rsp, rbp, rdi, rsi } = regs;
                            (*(*exception_info).ContextRecord).Rip = rip;
                            (*(*exception_info).ContextRecord).Rsp = rsp;
                            (*(*exception_info).ContextRecord).Rbp = rbp;
                            (*(*exception_info).ContextRecord).Rdi = rdi;
                            (*(*exception_info).ContextRecord).Rsi = rsi;
                        } else if #[cfg(target_arch = "x86")] {
                            let TrapHandlerRegs { eip, esp, ebp, ecx, edx } = regs;
                            (*(*exception_info).ContextRecord).Eip = eip;
                            (*(*exception_info).ContextRecord).Esp = esp;
                            (*(*exception_info).ContextRecord).Ebp = ebp;
                            (*(*exception_info).ContextRecord).Ecx = ecx;
                            (*(*exception_info).ContextRecord).Edx = edx;
                        } else {
                            compile_error!("Unsupported platform");
                        }
                    }
                }
                // EXCEPTION_CONTINUE_EXECUTION is -1. Not to be confused with
                // ExceptionContinueExecution which has a value of 0.
                -1
            }
        }
    }

    /// handle SIGBUS and SIGSEGV
    fn setup_trap_handler() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static TRAP_HANDLER_INITED: AtomicBool = AtomicBool::new(false);
        if TRAP_HANDLER_INITED
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            #[cfg(unix)]
            {
                // install SIGSEGV & SIGBUS signal handler
                let mut set = SigSet::empty();
                set.add(Signal::SIGBUS);
                set.add(Signal::SIGSEGV);
                let sa = SigAction::new(
                    SigHandler::SigAction(Self::trap_handler),
                    SaFlags::SA_ONSTACK,
                    set,
                );
                unsafe {
                    _ = sigaction(Signal::SIGBUS, &sa).expect("install SIGBUS handler failed !");
                    _ = sigaction(Signal::SIGSEGV, &sa).expect("install SIGSEGV handler failed !");
                }
            }
            #[cfg(windows)]
            unsafe {
                if AddVectoredExceptionHandler(1, Some(Self::trap_handler)).is_null() {
                    panic!(
                        "failed to add exception handler: {}",
                        Error::last_os_error()
                    );
                }
            }
        }
    }
}
