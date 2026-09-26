use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::UpdateError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingNotice {
    Updated { version: Version },
    RestoredAfterFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistedState {
    #[serde(default = "default_automatic_checks_enabled")]
    pub automatic_checks_enabled: bool,
    pub last_checked: Option<i64>,
    pub etag: Option<String>,
    pub highest_seen_version: Option<Version>,
    pub skipped_version: Option<Version>,
    pub staged_version: Option<Version>,
    pub pending_notice: Option<PendingNotice>,
}

fn default_automatic_checks_enabled() -> bool {
    true
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            automatic_checks_enabled: true,
            last_checked: None,
            etag: None,
            highest_seen_version: None,
            skipped_version: None,
            staged_version: None,
            pending_notice: None,
        }
    }
}

pub fn root() -> Result<PathBuf, UpdateError> {
    let root = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
        .ok_or_else(|| UpdateError::State("could not resolve user state directory".into()))?;
    let app = root.join(crate::app_meta::STATE_DIR_NAME);
    ensure_private_directory(&app)?;
    Ok(app)
}

pub fn state_path() -> Result<PathBuf, UpdateError> {
    Ok(root()?.join("update-state.json"))
}

fn ensure_private_directory(path: &Path) -> Result<(), UpdateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(UpdateError::State("unsafe updater state path".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|error| UpdateError::State(error.to_string()))?;
        }
        Err(error) => return Err(UpdateError::State(error.to_string())),
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|error| UpdateError::State(error.to_string()))?;
    if !metadata.file_type().is_dir() || metadata.uid() != nix::unistd::getuid().as_raw() {
        return Err(UpdateError::State("unsafe updater state directory".into()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| UpdateError::State(error.to_string()))
}

pub fn load() -> Result<PersistedState, UpdateError> {
    let path = state_path()?;
    load_from(&path)
}

fn load_from(path: &Path) -> Result<PersistedState, UpdateError> {
    if !path.exists() {
        return Ok(PersistedState::default());
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|error| UpdateError::State(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.uid() != nix::unistd::getuid().as_raw() {
        return Err(UpdateError::State("unsafe updater state file".into()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let bytes = fs::read(path).map_err(|error| UpdateError::State(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| UpdateError::State(error.to_string()))
}

pub fn save(state: &PersistedState) -> Result<(), UpdateError> {
    let path = state_path()?;
    save_to(&path, state)
}

fn save_to(path: &Path, state: &PersistedState) -> Result<(), UpdateError> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::State("missing state parent".into()))?;
    ensure_private_directory(parent)?;
    let tmp = parent.join(format!(".update-state-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|error| UpdateError::State(error.to_string()))?;
    file.write_all(&bytes)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    file.sync_all()
        .map_err(|error| UpdateError::State(error.to_string()))?;
    fs::rename(&tmp, path).map_err(|error| UpdateError::State(error.to_string()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| UpdateError::State(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updater_state_round_trips_privately() {
        let root = std::env::temp_dir().join(format!("lsb-update-state-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state.json");
        let state = PersistedState {
            last_checked: Some(123),
            highest_seen_version: Some(Version::parse("2.4.7").unwrap()),
            ..PersistedState::default()
        };
        save_to(&path, &state).unwrap();
        assert_eq!(load_from(&path).unwrap(), state);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn older_state_without_new_fields_loads_with_defaults() {
        let root =
            std::env::temp_dir().join(format!("lsb-update-old-state-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state.json");
        fs::write(
            &path,
            br#"{"automatic_checks_enabled":true,"last_checked":null,"etag":null,"highest_seen_version":null,"skipped_version":null,"staged_version":null}"#,
        )
        .unwrap();
        let state = load_from(&path).unwrap();
        assert!(state.pending_notice.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlinked_state_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("lsb-update-link-state-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let target = root.join("target.json");
        let link = root.join("state.json");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, &link).unwrap();
        assert!(load_from(&link).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
