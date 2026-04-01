use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::config::ControlHotkeyAction;

pub fn extract_prebound_hotkeys(config: &crate::config::Config) -> Vec<(String, String)> {
    let mut prebound: Vec<(String, String)> = config
        .sounds
        .iter()
        .filter_map(|sound| {
            sound
                .hotkey
                .as_ref()
                .map(|hotkey| (sound.id.clone(), hotkey.clone()))
        })
        .collect();

    for meta in ControlHotkeyAction::all() {
        if let Some(hotkey) = config.settings.control_hotkeys.get_cloned(meta.action) {
            prebound.push((meta.action.binding_id().to_string(), hotkey));
        }
    }

    prebound
}

pub fn init_hotkeys(
    prebound_hotkeys: Vec<(String, String)>,
) -> Result<
    (
        crate::hotkeys::HotkeyManager,
        std::sync::mpsc::Receiver<String>,
    ),
    String,
> {
    let (hotkey_sender, hotkey_receiver) = mpsc::channel();
    let _hotkey_receiver_arc = Arc::new(Mutex::new(hotkey_receiver));

    let manager = crate::hotkeys::HotkeyManager::new_blocking(hotkey_sender, &prebound_hotkeys);

    Ok((manager, mpsc::channel().1))
}
