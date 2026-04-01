pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error("No migration path from version {from} to {to}")]
    NoMigrationPath { from: u32, to: u32 },
    #[error("Config parse error: {0}")]
    ParseError(#[from] serde_json::Error),
}

pub struct V0ToV1Migration;

impl V0ToV1Migration {
    pub fn migrate(config: serde_json::Value) -> Result<serde_json::Value, MigrationError> {
        let mut migrated = config.clone();
        if let Some(obj) = migrated.as_object_mut() {
            obj.insert("schema_version".to_string(), serde_json::json!(1));
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
    if from_version == 0 && CURRENT_SCHEMA_VERSION == 1 {
        return V0ToV1Migration::migrate(config);
    }
    Err(MigrationError::NoMigrationPath {
        from: from_version,
        to: CURRENT_SCHEMA_VERSION,
    })
}
