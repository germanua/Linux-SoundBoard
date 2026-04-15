use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct TimerRegistry {
    inner: Rc<RefCell<Vec<glib::SourceId>>>,
}

impl TimerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, source_id: glib::SourceId) {
        self.inner.borrow_mut().push(source_id);
    }

    pub fn remove_all(&self) {
        for source_id in self.inner.borrow_mut().drain(..) {
            if !remove_source_id_safe(source_id) {
                log::debug!("Skipping removal of inactive SourceId during shutdown cleanup");
            }
        }
    }

    pub fn count(&self) -> usize {
        self.inner.borrow().len()
    }
}

pub fn remove_source_id_safe(source_id: glib::SourceId) -> bool {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| source_id.remove())).is_ok()
}
