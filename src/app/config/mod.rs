mod defaults;
pub mod migration;
pub use migration::{MigrationError, CURRENT_SCHEMA_VERSION, LAST_LEGACY_SCHEMA_VERSION};
mod persistence;
mod types;

pub use defaults::*;
pub(crate) use persistence::ConfigSaveBoundary;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

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
    fn untransformed_sound_omits_source_path_when_serialized() {
        let sound = Sound::new("silence".to_string(), "/tmp/silence.wav".to_string());

        let json = serde_json::to_string(&sound).unwrap();

        assert!(!json.contains("source_path"));
    }

    #[test]
    fn config_default_uses_current_schema_version() {
        assert_eq!(Config::default().schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn auto_gain_defaults_to_dynamic() {
        assert_eq!(
            Config::default().settings.auto_gain_mode,
            AutoGainMode::Dynamic
        );
    }

    #[test]
    fn typed_settings_serialize_to_legacy_strings() {
        let cfg = Config::default();
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["settings"]["theme"], "dark");
        assert_eq!(value["settings"]["auto_gain_mode"], "dynamic");
        assert_eq!(value["settings"]["auto_gain_apply_to"], "both");
        assert_eq!(value["settings"]["play_mode"], "default");
        assert_eq!(value["settings"]["list_style"], "compact");
        assert_eq!(value["settings"]["default_source_mode"], "default");
        assert_eq!(value["settings"]["mic_latency_profile"], "balanced");
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
                    "default_source_mode": "weird",
                    "mic_latency_profile": "turbo",
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
        assert_eq!(cfg.settings.auto_gain_mode, AutoGainMode::Dynamic);
        assert_eq!(cfg.settings.auto_gain_apply_to, AutoGainApplyTo::MicOnly);
        assert_eq!(cfg.settings.play_mode, PlayMode::Default);
        assert_eq!(cfg.settings.list_style, ListStyle::Compact);
        assert_eq!(cfg.settings.default_source_mode, DefaultSourceMode::Default);
        assert_eq!(
            cfg.settings.mic_latency_profile,
            MicLatencyProfile::Balanced
        );
    }

    #[test]
    fn legacy_default_source_mode_variants_migrate_to_default() {
        // Old configs may have any of these legacy names. All migrate to the
        // single new `Default` variant since they all expressed "soundboard
        // should be the mic in some way".
        for legacy in [
            r#""auto_route_while_running""#,
            r#""temporary_default_while_running""#,
            r#""auto_while_running""#,
        ] {
            let mode: DefaultSourceMode = serde_json::from_str(legacy).unwrap();
            assert_eq!(mode, DefaultSourceMode::Default, "legacy {legacy}");
        }
        let manual: DefaultSourceMode = serde_json::from_str(r#""manual""#).unwrap();
        assert_eq!(manual, DefaultSourceMode::Manual);
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
}
