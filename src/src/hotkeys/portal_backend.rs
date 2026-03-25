use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use ashpd::WindowIdentifier;
use futures_util::StreamExt;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::runtime::Runtime;
use tokio::sync::mpsc as tokio_mpsc;

use super::backend_runtime::HotkeyBackend;
use super::{canonical_hotkey_to_portal_trigger, canonicalize_hotkey_string};

pub struct PortalBackend {
    bindings: Arc<Mutex<HashMap<String, PortalBinding>>>,
    update_tx: tokio_mpsc::UnboundedSender<PortalUpdate>,
    update_rx: Mutex<Option<tokio_mpsc::UnboundedReceiver<PortalUpdate>>>,
    started: AtomicBool,
    runtime: Arc<Runtime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PortalBinding {
    shortcut_id: String,
    preferred_trigger: String,
}

#[derive(Debug, Clone, Copy)]
enum PortalUpdate {
    SyncBindings,
}

impl PortalBackend {
    pub fn new() -> Result<Self, String> {
        let runtime = Runtime::new().map_err(|e| format!("Failed to create tokio runtime: {e}"))?;

        runtime
            .block_on(async { GlobalShortcuts::new().await })
            .map_err(|e| format!("GlobalShortcuts portal unavailable: {e}"))?;

        let (update_tx, update_rx) = tokio_mpsc::unbounded_channel();

        Ok(Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            update_tx,
            update_rx: Mutex::new(Some(update_rx)),
            started: AtomicBool::new(false),
            runtime: Arc::new(runtime),
        })
    }

    #[allow(dead_code)]
    pub fn new_for_tests() -> Self {
        let (update_tx, update_rx) = tokio_mpsc::unbounded_channel();
        Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            update_tx,
            update_rx: Mutex::new(Some(update_rx)),
            started: AtomicBool::new(false),
            runtime: Arc::new(Runtime::new().expect("tokio runtime for tests")),
        }
    }
}

impl HotkeyBackend for PortalBackend {
    fn name(&self) -> &'static str {
        "portal"
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        let canonical_hotkey = canonicalize_hotkey_string(hotkey)?;
        let preferred_trigger = canonical_hotkey_to_portal_trigger(&canonical_hotkey)?;
        let shortcut_id = shortcut_id_for(sound_id);

        let mut bindings = self.bindings.lock().unwrap();
        for (id, existing) in bindings.iter() {
            if id != sound_id && existing.preferred_trigger == preferred_trigger {
                return Err(format!("HOTKEY_CONFLICT:{id}"));
            }
        }

        bindings.insert(
            sound_id.to_string(),
            PortalBinding {
                shortcut_id,
                preferred_trigger,
            },
        );
        let _ = self.update_tx.send(PortalUpdate::SyncBindings);
        Ok(())
    }

    fn unregister(&self, sound_id: &str) -> Result<(), String> {
        self.bindings.lock().unwrap().remove(sound_id);
        let _ = self.update_tx.send(PortalUpdate::SyncBindings);
        Ok(())
    }

    fn start_listener(&self, sender: Sender<String>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let update_rx = {
            let mut guard = self.update_rx.lock().unwrap();
            guard.take()
        };

        let Some(update_rx) = update_rx else {
            warn!("Portal backend update channel missing; listener disabled");
            return;
        };

        let bindings = Arc::clone(&self.bindings);
        let runtime = Arc::clone(&self.runtime);
        thread::spawn(move || {
            runtime.block_on(async move {
                if let Err(e) = run_portal_listener(bindings, update_rx, sender).await {
                    warn!("Portal listener terminated: {}", e);
                }
            });
        });
    }
}

fn shortcut_id_for(sound_id: &str) -> String {
    format!(
        "lsb-{}",
        sound_id.replace(|ch: char| !ch.is_alphanumeric(), "-")
    )
}

fn snapshot_bindings(
    bindings: &Arc<Mutex<HashMap<String, PortalBinding>>>,
) -> Vec<(String, String, String)> {
    bindings
        .lock()
        .unwrap()
        .iter()
        .map(|(sound_id, binding)| {
            (
                sound_id.clone(),
                binding.shortcut_id.clone(),
                binding.preferred_trigger.clone(),
            )
        })
        .collect()
}

fn build_shortcut_requests(snapshot: &[(String, String, String)]) -> Vec<NewShortcut> {
    snapshot
        .iter()
        .map(|(sound_id, shortcut_id, preferred_trigger)| {
            NewShortcut::new(shortcut_id.clone(), format!("Play {}", sound_id))
                .preferred_trigger(Some(preferred_trigger.as_str()))
        })
        .collect()
}

async fn synchronize_portal_bindings(
    shortcuts: &GlobalShortcuts<'_>,
    session: &ashpd::desktop::Session<'_, GlobalShortcuts<'_>>,
    bindings: &Arc<Mutex<HashMap<String, PortalBinding>>>,
) -> Result<(), String> {
    let snapshot = snapshot_bindings(bindings);
    let new_shortcuts = build_shortcut_requests(&snapshot);

    let request = shortcuts
        .bind_shortcuts(session, &new_shortcuts, &WindowIdentifier::default())
        .await
        .map_err(|e| format!("Portal bind request failed: {e}"))?;
    let response = request
        .response()
        .map_err(|e| format!("Portal bind response failed: {e}"))?;

    let bound: HashSet<String> = response
        .shortcuts()
        .iter()
        .map(|shortcut| shortcut.id().to_string())
        .collect();

    let mut missing = Vec::new();
    for (sound_id, shortcut_id, _) in &snapshot {
        if !bound.contains(shortcut_id) {
            missing.push(sound_id.clone());
        }
    }

    if missing.is_empty() {
        info!("Portal shortcuts synchronized: {}", snapshot.len());
    } else {
        warn!(
            "Portal skipped {} shortcut(s): {}",
            missing.len(),
            missing.join(", ")
        );
    }
    Ok(())
}

async fn run_portal_listener(
    bindings: Arc<Mutex<HashMap<String, PortalBinding>>>,
    mut update_rx: tokio_mpsc::UnboundedReceiver<PortalUpdate>,
    sender: Sender<String>,
) -> Result<(), String> {
    let shortcuts = GlobalShortcuts::new()
        .await
        .map_err(|e| format!("Failed to create GlobalShortcuts proxy: {e}"))?;

    let session = shortcuts
        .create_session()
        .await
        .map_err(|e| format!("Failed to create portal global-shortcuts session: {e}"))?;

    synchronize_portal_bindings(&shortcuts, &session, &bindings).await?;

    let mut activated_stream = shortcuts
        .receive_activated()
        .await
        .map_err(|e| format!("Failed to subscribe to portal activations: {e}"))?;

    info!("Portal listener active");

    loop {
        tokio::select! {
            update = update_rx.recv() => {
                if matches!(update, Some(PortalUpdate::SyncBindings)) {
                    if let Err(err) = synchronize_portal_bindings(&shortcuts, &session, &bindings).await {
                        warn!("Portal sync failed: {}", err);
                    }
                } else {
                    return Ok(());
                }
            }
            activated = activated_stream.next() => {
                let Some(activated) = activated else {
                    return Err("Portal activation stream ended".to_string());
                };

                let shortcut_id = activated.shortcut_id();
                let match_id = {
                    let guard = bindings.lock().unwrap();
                    guard
                        .iter()
                        .find(|(_, binding)| binding.shortcut_id == shortcut_id)
                        .map(|(id, _)| id.clone())
                };

                if let Some(sound_id) = match_id {
                    debug!("Portal hotkey triggered: {}", sound_id);
                    let _ = sender.send(sound_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PortalBackend;
    use crate::hotkeys::backend_runtime::HotkeyBackend;

    #[test]
    fn conflicts_on_duplicate_hotkey() {
        let backend = PortalBackend::new_for_tests();
        backend.register("s1", "Ctrl+KeyA").unwrap();
        let err = backend.register("s2", "Ctrl+KeyA").unwrap_err();
        assert_eq!(err, "HOTKEY_CONFLICT:s1");
    }

    #[test]
    fn shortcut_id_sanitization() {
        assert_eq!(super::shortcut_id_for("a/b:c"), "lsb-a-b-c");
    }
}
