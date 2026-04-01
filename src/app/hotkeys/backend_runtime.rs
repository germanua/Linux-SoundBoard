use std::any::Any;
use std::sync::mpsc::Sender;

pub trait HotkeyBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn validate_hotkey(&self, _hotkey: &str) -> Result<(), String> {
        Ok(())
    }
    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String>;
    fn unregister(&self, sound_id: &str) -> Result<(), String>;
    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), String> {
        for sound_id in sound_ids {
            self.unregister(sound_id)?;
        }
        Ok(())
    }
    fn start_listener(&self, sender: Sender<String>);
    /// Downcast support.
    fn as_any(&self) -> &dyn Any;
}
