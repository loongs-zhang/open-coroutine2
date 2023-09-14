use crate::blocker::Blocker;
use crate::coroutine::constants::CoroutineState;
use crate::coroutine::suspender::SimpleSuspender;
use crate::coroutine::{Coroutine, Current, StateMachine};
use crate::pool::{CoroutinePool, CoroutinePoolImpl};
use crate::scheduler::{Listener, SchedulableCoroutine, SchedulableSuspender};
use nix::sys::pthread::{pthread_kill, pthread_self, Pthread};
use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use open_coroutine_timer::TimerList;
use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::fmt::Debug;
use std::io::{Error, ErrorKind};
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct TaskNode {
    pthread: Pthread,
    coroutine: *const c_void,
}

trait Monitor {
    /// Get a global `WorkStealQueue` instance.
    fn get_instance<'m>() -> &'m Self;

    /// Start this monitor.
    ///
    /// # Errors
    /// if install signal handler failed.
    /// if start monitor thread failed.
    fn start(&self) -> std::io::Result<()>;

    /// Stop this monitor.
    fn stop(&self);

    /// Submit task to this monitor.
    ///
    /// # Errors
    /// see `start`.
    fn submit(&self, timestamp: u64, coroutine: &SchedulableCoroutine) -> std::io::Result<()>;

    /// Remove the task from this monitor.
    fn remove(&self, timestamp: u64, coroutine: &SchedulableCoroutine);
}

#[allow(box_pointers)]
#[derive(Debug)]
struct MonitorImpl {
    cpu: usize,
    tasks: UnsafeCell<TimerList<TaskNode>>,
    run: AtomicBool,
    monitor: UnsafeCell<MaybeUninit<JoinHandle<()>>>,
    blocker: Box<dyn Blocker>,
}

impl Default for MonitorImpl {
    #[allow(box_pointers)]
    fn default() -> Self {
        let blocker = Box::new(crate::blocker::SleepBlocker {});
        MonitorImpl {
            cpu: 0,
            tasks: UnsafeCell::new(TimerList::default()),
            run: AtomicBool::default(),
            monitor: UnsafeCell::new(MaybeUninit::uninit()),
            blocker,
        }
    }
}

extern "C" fn sigurg_handler(_: libc::c_int) {
    if let Ok(mut set) = SigSet::thread_get_mask() {
        //删除对SIGURG信号的屏蔽，使信号处理函数即使在处理中，也可以再次进入信号处理函数
        set.remove(Signal::SIGURG);
        set.thread_set_mask()
            .expect("Failed to remove SIGURG signal mask!");
        if let Some(suspender) = SchedulableSuspender::current() {
            suspender.suspend();
        }
    }
}

impl Monitor for MonitorImpl {
    #[allow(unsafe_code, trivial_casts, box_pointers)]
    fn get_instance<'m>() -> &'m Self {
        static MONITOR: AtomicUsize = AtomicUsize::new(0);
        let mut ret = MONITOR.load(Ordering::Relaxed);
        if ret == 0 {
            let ptr: &'m mut MonitorImpl = Box::leak(Box::default());
            ret = ptr as *mut MonitorImpl as usize;
            MONITOR.store(ret, Ordering::Relaxed);
        }
        unsafe { &*(ret as *mut MonitorImpl) }
    }

    fn start(&self) -> std::io::Result<()> {
        if self
            .run
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // install SIGURG signal handler
            let mut set = SigSet::empty();
            set.add(Signal::SIGURG);
            let sa = SigAction::new(
                SigHandler::Handler(sigurg_handler),
                SaFlags::SA_RESTART,
                set,
            );
            unsafe { _ = sigaction(Signal::SIGURG, &sa)? };

            // start the monitor thread
            let monitor = unsafe { &mut *self.monitor.get() };
            *monitor = MaybeUninit::new(
                std::thread::Builder::new()
                    .name("open-coroutine-monitor".to_string())
                    .spawn(|| {
                        let monitor = Self::get_instance();
                        // thread per core
                        _ = core_affinity::set_for_current(core_affinity::CoreId {
                            id: monitor.cpu,
                        });
                        let mut pool = CoroutinePoolImpl::new(
                            String::from("open-coroutine-monitor"),
                            crate::coroutine::DEFAULT_STACK_SIZE,
                            1,
                            1,
                            0,
                            crate::blocker::DelayBlocker {},
                        );
                        let tasks = unsafe { &*monitor.tasks.get() };
                        while monitor.run.load(Ordering::Acquire) || !tasks.is_empty() {
                            //只遍历，不删除，如果抢占调度失败，会在1ms后不断重试，相当于主动检测
                            for (exec_time, entry) in tasks.iter() {
                                if open_coroutine_timer::now() < *exec_time {
                                    break;
                                }
                                for node in entry.iter() {
                                    _ = pool.submit(
                                        None,
                                        |_| {
                                            let coroutine = unsafe {
                                                &*(node.coroutine.cast::<SchedulableCoroutine>())
                                            };
                                            if CoroutineState::Running == coroutine.state() {
                                                //只对陷入重度计算的协程发送信号抢占，对陷入执行系统调用的协程
                                                //不发送信号(如果发送信号，会打断系统调用，进而降低总体性能)
                                                if pthread_kill(node.pthread, Signal::SIGURG)
                                                    .is_err()
                                                {
                                                    todo!("log");
                                                };
                                            }
                                            None
                                        },
                                        None,
                                    );
                                }
                                _ = pool.try_schedule();
                            }
                            //monitor线程不执行协程计算任务，每次循环至少wait 1ms
                            #[allow(box_pointers)]
                            monitor.blocker.block(Duration::from_millis(1));
                        }
                    })
                    .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?,
            );
        }
        Ok(())
    }

    fn stop(&self) {
        self.run.store(false, Ordering::Release);
    }

    fn submit(&self, timestamp: u64, coroutine: &SchedulableCoroutine) -> std::io::Result<()> {
        self.start()?;
        let tasks = unsafe { &mut *self.tasks.get() };
        tasks.insert(
            timestamp,
            TaskNode {
                pthread: pthread_self(),
                coroutine: (coroutine as *const SchedulableCoroutine).cast::<c_void>(),
            },
        );
        Ok(())
    }

    fn remove(&self, timestamp: u64, coroutine: &SchedulableCoroutine) {
        let tasks = unsafe { &mut *self.tasks.get() };
        if let Some(entry) = tasks.get_entry(&timestamp) {
            if !entry.is_empty() {
                _ = entry.remove(&TaskNode {
                    pthread: pthread_self(),
                    coroutine: (coroutine as *const SchedulableCoroutine).cast::<c_void>(),
                });
            }
            if entry.is_empty() {
                _ = tasks.remove(&timestamp);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct MonitorListener {}

const MONITOR_TIMESTAMP: &str = "MONITOR_TIMESTAMP";

impl Listener for MonitorListener {
    fn on_resume(&self, timeout_time: u64, coroutine: &SchedulableCoroutine) {
        let timestamp =
            open_coroutine_timer::get_timeout_time(Duration::from_millis(10)).min(timeout_time);
        _ = coroutine.local().put(MONITOR_TIMESTAMP, timestamp);
        MonitorImpl::get_instance()
            .submit(timestamp, coroutine)
            .expect("Submit task to monitor failed !");
    }

    fn on_suspend(&self, _: u64, coroutine: &SchedulableCoroutine) {
        if let Some(timestamp) = coroutine.local().get(MONITOR_TIMESTAMP) {
            MonitorImpl::get_instance().remove(*timestamp, coroutine);
        }
    }

    fn on_complete(&self, _: u64, coroutine: &SchedulableCoroutine) {
        if let Some(timestamp) = coroutine.local().get(MONITOR_TIMESTAMP) {
            MonitorImpl::get_instance().remove(*timestamp, coroutine);
        }
    }

    fn on_error(&self, _: u64, coroutine: &SchedulableCoroutine, _: &str) {
        if let Some(timestamp) = coroutine.local().get(MONITOR_TIMESTAMP) {
            MonitorImpl::get_instance().remove(*timestamp, coroutine);
        }
    }
}

#[allow(box_pointers)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::prelude::JoinHandleExt;

    static SIGNALED: AtomicBool = AtomicBool::new(false);

    extern "C" fn handler(_: libc::c_int) {
        SIGNALED.store(true, Ordering::Relaxed);
    }

    #[test]
    fn test() -> std::io::Result<()> {
        let mut set = SigSet::empty();
        set.add(Signal::SIGUSR1);
        let sa = SigAction::new(SigHandler::Handler(handler), SaFlags::SA_RESTART, set);
        unsafe { _ = sigaction(Signal::SIGUSR1, &sa)? };

        SIGNALED.store(false, Ordering::Relaxed);
        let handle = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(2));
        });
        std::thread::sleep(Duration::from_secs(1));
        pthread_kill(handle.as_pthread_t(), Signal::SIGUSR1)?;
        handle
            .join()
            .map_err(|e| Error::new(ErrorKind::Other, format!("{e:?}")))?;
        assert!(SIGNALED.load(Ordering::Relaxed));
        Ok(())
    }

    /// This test can be run locally, but is not stable enough
    /// in extreme scenarios when in non release mode.
    #[cfg(not(debug_assertions))]
    #[test]
    fn preemptive_schedule() -> std::io::Result<()> {
        use crate::scheduler::{Scheduler, SchedulerImpl};
        use std::sync::{Arc, Condvar, Mutex};
        static TEST_FLAG1: AtomicBool = AtomicBool::new(true);
        static TEST_FLAG2: AtomicBool = AtomicBool::new(true);
        let pair = Arc::new((Mutex::new(true), Condvar::new()));
        let pair2 = Arc::clone(&pair);
        let _: JoinHandle<std::io::Result<()>> = std::thread::Builder::new()
            .name("test_preemptive_schedule".to_string())
            .spawn(move || {
                let mut scheduler = SchedulerImpl::default();
                _ = scheduler.submit(
                    |_, _| {
                        while TEST_FLAG1.load(Ordering::Acquire) {
                            _ = unsafe { libc::usleep(10_000) };
                        }
                    },
                    None,
                );
                _ = scheduler.submit(
                    |_, _| {
                        while TEST_FLAG2.load(Ordering::Acquire) {
                            _ = unsafe { libc::usleep(10_000) };
                        }
                        TEST_FLAG1.store(false, Ordering::Release);
                    },
                    None,
                );
                _ = scheduler.submit(|_, _| TEST_FLAG2.store(false, Ordering::Release), None);
                scheduler.try_schedule()?;

                let (lock, cvar) = &*pair2;
                let mut pending = lock.lock().unwrap();
                *pending = false;
                // notify the condvar that the value has changed.
                cvar.notify_one();
                Ok(())
            })
            .expect("failed to spawn thread");

        // wait for the thread to start up
        let (lock, cvar) = &*pair;
        let result = cvar
            .wait_timeout_while(
                lock.lock().unwrap(),
                Duration::from_millis(3000),
                |&mut pending| pending,
            )
            .unwrap();
        if result.1.timed_out() {
            Err(Error::new(ErrorKind::Other, "preemptive schedule failed"))
        } else {
            assert!(
                !TEST_FLAG1.load(Ordering::Acquire),
                "preemptive schedule failed"
            );
            Ok(())
        }
    }
}
