use std::any::Any;
use std::sync::mpsc::Sender;

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
    fn unregister(&self, sound_id: &str) -> Result<(), HotkeyError>;
    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), HotkeyError> {
        for sound_id in sound_ids {
            self.unregister(sound_id)?;
        }
        Ok(())
    }
    fn start_listener(&self, sender: Sender<String>);
    /// Release any resources owned by the backend (e.g. spawned daemons) before
    /// the application exits. Default is a no-op for backends with nothing to
    /// tear down.
    fn shutdown(&self) {}
    fn as_any(&self) -> &dyn Any;
}
