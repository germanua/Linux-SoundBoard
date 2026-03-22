use std::sync::mpsc::Sender;

pub trait HotkeyBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String>;
    fn unregister(&self, sound_id: &str) -> Result<(), String>;
    fn start_listener(&self, sender: Sender<String>);
}
