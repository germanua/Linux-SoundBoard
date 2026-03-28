//! Configuration management module.

mod defaults;
mod persistence;
mod types;

pub use defaults::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_for_persistence_drops_non_finite_loudness() {
        let mut cfg = Config::default();
        let mut sound = Sound::new("silence".to_string(), "/tmp/silence.wav".to_string());
        sound.loudness_lufs = Some(f64::NEG_INFINITY);
        cfg.sounds.push(sound);

        cfg.sanitize_for_persistence();

        assert_eq!(cfg.sounds[0].loudness_lufs, None);
        assert!(serde_json::to_string(&cfg).is_ok());
    }

    #[test]
    fn sanitize_for_persistence_clamps_invalid_target_lufs() {
        let mut cfg = Config::default();
        cfg.settings.auto_gain_target_lufs = f64::NAN;
        cfg.sanitize_for_persistence();
        assert_eq!(cfg.settings.auto_gain_target_lufs, -14.0);

        cfg.settings.auto_gain_target_lufs = 7.0;
        cfg.sanitize_for_persistence();
        assert_eq!(cfg.settings.auto_gain_target_lufs, 0.0);
    }

    #[test]
    fn sanitize_for_persistence_disables_multiple_playback() {
        let mut cfg = Config::default();
        cfg.settings.allow_multiple_playbacks = true;

        cfg.sanitize_for_persistence();

        assert!(!cfg.settings.allow_multiple_playbacks);
    }

    #[test]
    fn typed_settings_serialize_to_legacy_strings() {
        let cfg = Config::default();
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["settings"]["theme"], "dark");
        assert_eq!(value["settings"]["auto_gain_mode"], "static");
        assert_eq!(value["settings"]["auto_gain_apply_to"], "mic_only");
        assert_eq!(value["settings"]["play_mode"], "default");
        assert_eq!(value["settings"]["list_style"], "compact");
    }

    #[test]
    fn typed_settings_deserialize_invalid_values_to_defaults() {
        let cfg: Config = serde_json::from_str(
            r#"{
                "sound_folders": [],
                "sounds": [],
                "tabs": [],
                "settings": {
                    "theme": "weird",
                    "local_volume": 80,
                    "local_mute": false,
                    "mic_volume": 100,
                    "allow_multiple_playbacks": true,
                    "mic_passthrough": true,
                    "mic_source": null,
                    "skip_delete_confirm": false,
                    "auto_gain": false,
                    "auto_gain_mode": "weird",
                    "auto_gain_target_lufs": -14.0,
                    "auto_gain_apply_to": "odd",
                    "auto_gain_lookahead_ms": 30,
                    "auto_gain_attack_ms": 6,
                    "auto_gain_release_ms": 150,
                    "control_hotkeys": {},
                    "play_mode": "nope",
                    "list_style": "wide"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(cfg.settings.theme, Theme::Dark);
        assert_eq!(cfg.settings.auto_gain_mode, AutoGainMode::Static);
        assert_eq!(cfg.settings.auto_gain_apply_to, AutoGainApplyTo::MicOnly);
        assert_eq!(cfg.settings.play_mode, PlayMode::Default);
        assert_eq!(cfg.settings.list_style, ListStyle::Compact);
    }

    #[test]
    fn control_hotkey_metadata_is_consistent() {
        for meta in ControlHotkeyAction::all() {
            assert_eq!(ControlHotkeyAction::from_id(meta.id), Some(meta.action));
            assert_eq!(
                ControlHotkeyAction::from_binding_id(meta.binding_id),
                Some(meta.action)
            );
            assert_eq!(meta.action.id(), meta.id);
            assert_eq!(meta.action.binding_id(), meta.binding_id);
        }
    }

    #[test]
    fn remove_sounds_from_tab_batch_removes_present_and_ignores_missing() {
        let mut cfg = Config::default();
        let mut tab = SoundTab::new("Custom".to_string(), 1);
        tab.id = "custom-a".to_string();
        tab.sound_ids = vec![
            "sound-1".to_string(),
            "sound-2".to_string(),
            "sound-3".to_string(),
        ];
        cfg.tabs.push(tab);

        let removed = cfg.remove_sounds_from_tab(
            "custom-a",
            &[
                "sound-2".to_string(),
                "missing-id".to_string(),
                "sound-2".to_string(),
            ],
        );

        assert!(removed);
        let tab = cfg.get_tab("custom-a").unwrap();
        assert_eq!(tab.sound_ids, vec!["sound-1", "sound-3"]);
    }

    #[test]
    fn remove_sounds_from_tab_batch_fails_when_tab_missing() {
        let mut cfg = Config::default();
        let removed = cfg.remove_sounds_from_tab("missing-tab", &["sound-1".to_string()]);
        assert!(!removed);
    }

    #[test]
    fn remove_sounds_batch_removes_sounds_and_tab_membership() {
        let mut cfg = Config::default();

        let mut sound_a = Sound::new("A".to_string(), "/tmp/a.wav".to_string());
        sound_a.id = "sound-a".to_string();
        let mut sound_b = Sound::new("B".to_string(), "/tmp/b.wav".to_string());
        sound_b.id = "sound-b".to_string();
        let mut sound_c = Sound::new("C".to_string(), "/tmp/c.wav".to_string());
        sound_c.id = "sound-c".to_string();
        cfg.sounds = vec![sound_a, sound_b, sound_c];

        let mut tab = SoundTab::new("Custom".to_string(), 1);
        tab.sound_ids = vec![
            "sound-a".to_string(),
            "sound-b".to_string(),
            "sound-c".to_string(),
        ];
        cfg.tabs.push(tab);

        cfg.remove_sounds(&[
            "sound-b".to_string(),
            "missing-id".to_string(),
            "sound-c".to_string(),
        ]);

        let remaining_ids = cfg
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(remaining_ids, vec!["sound-a"]);
        assert_eq!(cfg.tabs[0].sound_ids, vec!["sound-a"]);
    }
}
