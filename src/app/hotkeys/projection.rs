use parking_lot::{Condvar, Mutex};
use std::sync::Arc;

use crate::library_store::{LibraryStore, PAGE_SIZE};

use super::{HotkeyError, HotkeyManager};

#[derive(Default)]
struct ProjectionGateState {
    requested: u64,
    completed: u64,
    running: bool,
    error: Option<String>,
}

#[derive(Default)]
struct ProjectionGate {
    state: Mutex<ProjectionGateState>,
    changed: Condvar,
}

impl ProjectionGate {
    fn reconcile(&self, mut project: impl FnMut() -> Result<(), String>) -> Result<(), String> {
        let generation = {
            let mut state = self.state.lock();
            state.requested = state.requested.saturating_add(1);
            let generation = state.requested;
            if state.running {
                while state.completed < generation {
                    self.changed.wait(&mut state);
                }
                return state.error.clone().map_or(Ok(()), Err);
            }
            state.running = true;
            generation
        };

        let mut target = generation;
        loop {
            let result = project();
            let mut state = self.state.lock();
            state.error = result.err();
            state.completed = target;
            if state.requested == target {
                state.running = false;
                self.changed.notify_all();
                return state.error.clone().map_or(Ok(()), Err);
            }
            target = state.requested;
        }
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.state.lock().requested
    }
}

#[derive(Clone)]
pub struct HotkeyProjectionCoordinator {
    library: LibraryStore,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    gate: Arc<ProjectionGate>,
}

impl HotkeyProjectionCoordinator {
    pub fn new(library: LibraryStore, hotkeys: Arc<Mutex<HotkeyManager>>) -> Self {
        Self {
            library,
            hotkeys,
            gate: Arc::new(ProjectionGate::default()),
        }
    }

    pub fn reconcile_blocking(&self) -> Result<(), String> {
        self.gate.reconcile(|| {
            project_current_bindings(&self.library, &mut self.hotkeys.lock())
                .map(|count| log::info!("Projected {count} persisted hotkey binding(s)"))
                .map_err(|error| error.to_string())
        })
    }
}

fn project_current_bindings(
    library: &LibraryStore,
    hotkeys: &mut HotkeyManager,
) -> Result<usize, HotkeyError> {
    let mut after = None;
    let mut finished = false;
    hotkeys.project_hotkey_pages_blocking(|| {
        if finished {
            return Ok(None);
        }
        let page = library
            .hotkey_bindings_after(after.as_deref())
            .recv()
            .map_err(|error| HotkeyError::Io(error.to_string()))?;
        let count = page.bindings.len();
        after = page
            .bindings
            .last()
            .map(|binding| binding.binding_id.clone());
        finished = count < PAGE_SIZE;
        Ok((!page.bindings.is_empty()).then(|| {
            page.bindings
                .into_iter()
                .map(|binding| (binding.binding_id, binding.accelerator))
                .collect()
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::ProjectionGate;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn edit_during_projection_runs_one_fresh_generation() {
        let gate = Arc::new(ProjectionGate::default());
        let runs = Arc::new(AtomicUsize::new(0));
        let leader_gate = Arc::clone(&gate);
        let leader_runs = Arc::clone(&runs);
        let leader = std::thread::spawn(move || {
            leader_gate.reconcile(|| {
                let run = leader_runs.fetch_add(1, Ordering::SeqCst);
                if run == 0 {
                    while leader_gate.generation() < 2 {
                        std::thread::yield_now();
                    }
                }
                Ok(())
            })
        });

        while gate.generation() < 1 {
            std::thread::yield_now();
        }
        let follower_gate = Arc::clone(&gate);
        let follower = std::thread::spawn(move || follower_gate.reconcile(|| Ok(())));

        leader
            .join()
            .expect("leader thread")
            .expect("leader result");
        follower
            .join()
            .expect("follower thread")
            .expect("follower result");
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
