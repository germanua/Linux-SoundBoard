pub const CURRENT_SCHEMA_VERSION: u32 = 8;
pub const LAST_LEGACY_SCHEMA_VERSION: u32 = 7;

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("No migration path from version {from} to {to}")]
    NoMigrationPath { from: u32, to: u32 },
    #[error("Config parse error: {0}")]
    ParseError(#[from] serde_json::Error),
}

pub struct V0ToV1Migration;
pub struct V1ToV2Migration;
pub struct V2ToV3Migration;
pub struct V3ToV4Migration;
pub struct V4ToV5Migration;
pub struct V5ToV6Migration;
pub struct V6ToV7Migration;
pub struct V7ToV8Migration;

impl V0ToV1Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config.clone();
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(1));
        }
        Ok(migrated)
    }
}

impl V1ToV2Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(2));
            if let Some(settings) = obj
                .get_mut("settings")
                .and_then(|settings| settings.as_object_mut())
            {
                settings
                    .entry("default_source_mode".to_string())
                    .or_insert_with(|| serde_json::json!("auto_while_running"));
            }
        }
        Ok(migrated)
    }
}

impl V2ToV3Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(3));
            if let Some(sounds) = obj
                .get_mut("sounds")
                .and_then(|sounds| sounds.as_array_mut())
            {
                for sound in sounds {
                    if let Some(sound_obj) = sound.as_object_mut() {
                        sound_obj
                            .entry("loudness_source_fingerprint".to_string())
                            .or_insert(serde_json::Value::Null);
                    }
                }
            }
        }
        Ok(migrated)
    }
}

impl V3ToV4Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(4));
            if let Some(settings) = obj
                .get_mut("settings")
                .and_then(|settings| settings.as_object_mut())
            {
                settings
                    .entry("mic_latency_profile".to_string())
                    .or_insert_with(|| serde_json::json!("balanced"));
            }
        }
        Ok(migrated)
    }
}

impl V4ToV5Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(5));
            if let Some(settings) = obj
                .get_mut("settings")
                .and_then(|settings| settings.as_object_mut())
            {
                match settings
                    .get("default_source_mode")
                    .and_then(|mode| mode.as_str())
                {
                    Some("auto_while_running") => {
                        settings.insert(
                            "default_source_mode".to_string(),
                            serde_json::json!("temporary_default_while_running"),
                        );
                    }
                    Some(_) => {}
                    None => {
                        settings.insert(
                            "default_source_mode".to_string(),
                            serde_json::json!("auto_route_while_running"),
                        );
                    }
                }
            }
        }
        Ok(migrated)
    }
}

impl V5ToV6Migration {
    /// Force auto-gain on and broaden apply-to scope so existing users get
    /// Soundpad-style normalization across both local preview and mic output.
    /// Users who prefer it off can toggle in Settings → Loudness.
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(6));
            if let Some(settings) = obj
                .get_mut("settings")
                .and_then(|settings| settings.as_object_mut())
            {
                settings.insert("auto_gain".to_string(), serde_json::json!(true));
                settings.insert("auto_gain_apply_to".to_string(), serde_json::json!("both"));
                log::info!(
                    "Auto-gain enabled by upgrade migration; toggle in Settings → Loudness if you prefer it off."
                );
            }
        }
        Ok(migrated)
    }
}

impl V6ToV7Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(7));
            if let Some(tabs) = obj.get_mut("tabs").and_then(|tabs| tabs.as_array_mut()) {
                for tab in tabs {
                    if let Some(tab) = tab.as_object_mut() {
                        tab.entry("folder_binding".to_string())
                            .or_insert(serde_json::Value::Null);
                    }
                }
            }
        }
        Ok(migrated)
    }
}

impl V7ToV8Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config;
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(8));
            obj.remove("library_id");
        }
        Ok(migrated)
    }
}

pub fn run_migrations(
    config: serde_json::Value,
    from_version: u32,
) -> Result<serde_json::Value, MigrationError> {
    if from_version == CURRENT_SCHEMA_VERSION {
        return Ok(config);
    }
    if from_version == 0 {
        let migrated = V0ToV1Migration::migrate(config)?;
        let migrated = V1ToV2Migration::migrate(migrated)?;
        let migrated = V2ToV3Migration::migrate(migrated)?;
        let migrated = V3ToV4Migration::migrate(migrated)?;
        let migrated = V4ToV5Migration::migrate(migrated)?;
        let migrated = V5ToV6Migration::migrate(migrated)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 1 {
        let migrated = V1ToV2Migration::migrate(config)?;
        let migrated = V2ToV3Migration::migrate(migrated)?;
        let migrated = V3ToV4Migration::migrate(migrated)?;
        let migrated = V4ToV5Migration::migrate(migrated)?;
        let migrated = V5ToV6Migration::migrate(migrated)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 2 {
        let migrated = V2ToV3Migration::migrate(config)?;
        let migrated = V3ToV4Migration::migrate(migrated)?;
        let migrated = V4ToV5Migration::migrate(migrated)?;
        let migrated = V5ToV6Migration::migrate(migrated)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 3 {
        let migrated = V3ToV4Migration::migrate(config)?;
        let migrated = V4ToV5Migration::migrate(migrated)?;
        let migrated = V5ToV6Migration::migrate(migrated)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 4 {
        let migrated = V4ToV5Migration::migrate(config)?;
        let migrated = V5ToV6Migration::migrate(migrated)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 5 {
        let migrated = V5ToV6Migration::migrate(config)?;
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(migrated)?);
    }
    if from_version == 6 {
        return V7ToV8Migration::migrate(V6ToV7Migration::migrate(config)?);
    }
    if from_version == 7 {
        return V7ToV8Migration::migrate(config);
    }
    Err(MigrationError::NoMigrationPath {
        from: from_version,
        to: CURRENT_SCHEMA_VERSION,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base_config_with_settings(settings: serde_json::Value) -> serde_json::Value {
        json!({
            "schema_version": 0,
            "sound_folders": [],
            "sounds": [],
            "tabs": [],
            "settings": settings,
        })
    }

    #[test]
    fn v0_to_v1_sets_schema_version() {
        let migrated = V0ToV1Migration::migrate(base_config_with_settings(json!({}))).unwrap();
        assert_eq!(migrated["schema_version"], json!(1));
    }

    #[test]
    fn v1_to_v2_adds_default_source_mode_when_missing() {
        let migrated = V1ToV2Migration::migrate(base_config_with_settings(json!({}))).unwrap();
        assert_eq!(migrated["schema_version"], json!(2));
        assert_eq!(
            migrated["settings"]["default_source_mode"],
            json!("auto_while_running")
        );
    }

    #[test]
    fn v2_to_v3_adds_source_fingerprint_field_when_missing() {
        let migrated = V2ToV3Migration::migrate(json!({
            "schema_version": 2,
            "sound_folders": [],
            "sounds": [
                {
                    "id": "1",
                    "name": "Test",
                    "path": "/tmp/test.wav",
                    "source_path": "/tmp/test.wav",
                    "hotkey": null,
                    "duration_ms": 1000,
                    "volume": 100,
                    "enabled": true,
                    "loudness_lufs": -14.0,
                    "loudness_analysis_state": "refined",
                    "loudness_confidence": 1.0
                }
            ],
            "tabs": [],
            "settings": {
                "default_source_mode": "manual"
            }
        }))
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(3));
        assert!(migrated["sounds"][0]
            .get("loudness_source_fingerprint")
            .is_some());
    }

    #[test]
    fn v1_to_v2_preserves_existing_default_source_mode() {
        let migrated = V1ToV2Migration::migrate(base_config_with_settings(json!({
            "default_source_mode": "auto_while_running",
        })))
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(2));
        assert_eq!(
            migrated["settings"]["default_source_mode"],
            json!("auto_while_running")
        );
    }

    #[test]
    fn v4_to_v5_migrates_legacy_auto_mode_to_temporary_default() {
        let migrated = V4ToV5Migration::migrate(base_config_with_settings(json!({
            "default_source_mode": "auto_while_running"
        })))
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(5));
        assert_eq!(
            migrated["settings"]["default_source_mode"],
            json!("temporary_default_while_running")
        );
    }

    #[test]
    fn v4_to_v5_adds_auto_route_mode_when_missing() {
        let migrated = V4ToV5Migration::migrate(base_config_with_settings(json!({}))).unwrap();

        assert_eq!(migrated["schema_version"], json!(5));
        assert_eq!(
            migrated["settings"]["default_source_mode"],
            json!("auto_route_while_running")
        );
    }

    #[test]
    fn run_migrations_v0_to_current_applies_all_steps() {
        let migrated = run_migrations(base_config_with_settings(json!({})), 0).unwrap();

        assert_eq!(migrated["schema_version"], json!(CURRENT_SCHEMA_VERSION));
        assert_eq!(
            migrated["settings"]["default_source_mode"],
            json!("temporary_default_while_running")
        );
        assert_eq!(
            migrated["settings"]["mic_latency_profile"],
            json!("balanced")
        );
        assert_eq!(migrated["settings"]["auto_gain"], json!(true));
        assert_eq!(migrated["settings"]["auto_gain_apply_to"], json!("both"));
    }

    #[test]
    fn run_migrations_v2_to_current_applies_source_fingerprint_and_latency_profile_steps() {
        let migrated = run_migrations(
            json!({
                "schema_version": 2,
                "sound_folders": [],
                "sounds": [],
                "tabs": [],
                "settings": {
                    "default_source_mode": "manual"
                }
            }),
            2,
        )
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(CURRENT_SCHEMA_VERSION));
        assert_eq!(
            migrated["settings"]["mic_latency_profile"],
            json!("balanced")
        );
    }

    #[test]
    fn v5_to_v6_forces_auto_gain_on_and_apply_to_both() {
        let migrated = V5ToV6Migration::migrate(base_config_with_settings(json!({
            "auto_gain": false,
            "auto_gain_apply_to": "mic_only",
        })))
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(6));
        assert_eq!(migrated["settings"]["auto_gain"], json!(true));
        assert_eq!(migrated["settings"]["auto_gain_apply_to"], json!("both"));
    }

    #[test]
    fn v6_to_v7_marks_existing_tabs_as_unbound() {
        let migrated = V6ToV7Migration::migrate(json!({
            "schema_version": 6,
            "sound_folders": ["/sounds"],
            "sounds": [],
            "tabs": [{
                "id": "manual",
                "name": "Manual",
                "sound_ids": ["sound-1"],
                "order": 4
            }],
            "settings": {}
        }))
        .unwrap();

        assert_eq!(migrated["schema_version"], json!(7));
        assert_eq!(migrated["tabs"][0]["folder_binding"], json!(null));
        assert_eq!(migrated["tabs"][0]["id"], json!("manual"));
        assert_eq!(migrated["tabs"][0]["name"], json!("Manual"));
        assert_eq!(migrated["tabs"][0]["sound_ids"], json!(["sound-1"]));
        assert_eq!(migrated["tabs"][0]["order"], json!(4));
    }

    #[test]
    fn run_migrations_rejects_unknown_version() {
        let err = run_migrations(base_config_with_settings(json!({})), 99).unwrap_err();
        match err {
            MigrationError::NoMigrationPath { from, to } => {
                assert_eq!(from, 99);
                assert_eq!(to, CURRENT_SCHEMA_VERSION);
            }
            _ => panic!("unexpected migration error variant"),
        }
    }

    #[test]
    fn run_migrations_tolerates_malformed_settings_payload() {
        let config = base_config_with_settings(json!("not-an-object"));
        let migrated = run_migrations(config, 1).unwrap();

        assert_eq!(migrated["schema_version"], json!(CURRENT_SCHEMA_VERSION));
        assert_eq!(migrated["settings"], json!("not-an-object"));
    }

    #[test]
    fn run_migrations_noop_when_already_current() {
        let config = json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "sound_folders": [],
            "sounds": [],
            "tabs": [],
            "settings": {
                "default_source_mode": "manual",
            },
        });

        let migrated = run_migrations(config.clone(), CURRENT_SCHEMA_VERSION).unwrap();
        assert_eq!(migrated, config);
    }
}
