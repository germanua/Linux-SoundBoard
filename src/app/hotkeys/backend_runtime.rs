use std::any::Any;
use std::sync::mpsc::{SyncSender, TrySendError};

use super::error::HotkeyError;

pub trait HotkeyBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn validate_hotkey(&self, _hotkey: &str) -> Result<(), HotkeyError> {
        Ok(())
    }
    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), HotkeyError>;
    fn register_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
        for (sound_id, hotkey) in bindings {
            self.register(sound_id, hotkey)?;
        }
        Ok(())
    }
    fn stage_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
        self.register_many(bindings)
    }
    fn begin_staged(&self) -> Result<(), HotkeyError> {
        self.stage_many(&[])
    }
    fn commit_staged(&self) -> Result<(), HotkeyError> {
        Ok(())
    }
    fn abort_staged(&self) {}
    fn unregister(&self, sound_id: &str) -> Result<(), HotkeyError>;
    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), HotkeyError> {
        for sound_id in sound_ids {
            self.unregister(sound_id)?;
        }
        Ok(())
    }
    fn start_listener(&self, sender: SyncSender<String>);
    /// Release any resources owned by the backend (e.g. spawned daemons) before
    /// the application exits. Default is a no-op for backends with nothing to
    /// tear down.
    fn shutdown(&self) {}
    fn as_any(&self) -> &dyn Any;
}

pub(super) fn try_dispatch_hotkey(sender: &SyncSender<String>, binding_id: String) -> bool {
    match sender.try_send(binding_id) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_hotkey_queue_drops_repeat_without_blocking() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        assert!(try_dispatch_hotkey(&sender, "first".to_string()));
        assert!(!try_dispatch_hotkey(&sender, "repeat".to_string()));
        assert_eq!(receiver.recv().unwrap(), "first");
    }
}
