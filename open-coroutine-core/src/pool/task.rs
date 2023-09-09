use crate::coroutine::Named;
use std::cell::Cell;
use std::fmt::{Debug, Formatter};
use std::panic::UnwindSafe;

/// A trait implemented for describing task.
/// Note: the param and the result is raw pointer.
pub trait Task: Named + UnwindSafe {
    /// Create a new `Task` instance.
    fn new(
        name: String,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'static,
        param: Option<usize>,
    ) -> Self;

    /// Set a param for this task.
    fn set_param(&self, param: usize) -> Option<usize>;

    /// Get param from this task.
    fn get_param(&self) -> Option<usize>;

    /// exec the task
    ///
    /// # Errors
    /// if an exception occurred while executing this task.
    fn run<'e>(self) -> Result<Option<usize>, &'e str>;
}

#[repr(C)]
#[allow(clippy::type_complexity, box_pointers, missing_docs)]
pub struct TaskImpl {
    name: String,
    func: Box<dyn FnOnce(Option<usize>) -> Option<usize> + UnwindSafe>,
    param: Cell<Option<usize>>,
}

impl Debug for TaskImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("name", &self.name)
            .field("param", &self.param)
            .finish_non_exhaustive()
    }
}

impl Named for TaskImpl {
    fn get_name(&self) -> &str {
        &self.name
    }
}

impl Task for TaskImpl {
    #[allow(box_pointers)]
    fn new(
        name: String,
        func: impl FnOnce(Option<usize>) -> Option<usize> + UnwindSafe + 'static,
        param: Option<usize>,
    ) -> Self {
        TaskImpl {
            name,
            func: Box::new(func),
            param: Cell::new(param),
        }
    }

    fn set_param(&self, param: usize) -> Option<usize> {
        self.param.replace(Some(param))
    }

    fn get_param(&self) -> Option<usize> {
        self.param.get()
    }

    #[allow(box_pointers)]
    fn run<'e>(self) -> Result<Option<usize>, &'e str> {
        let paran = self.get_param();
        std::panic::catch_unwind(|| (self.func)(paran))
            .map_err(|e| *e.downcast_ref::<&'static str>().unwrap())
    }
}
