mod apply;
mod checker;
pub mod downloader;
mod release;
mod staging;
pub(crate) mod state;
mod verifier;

use semver::Version;
use thiserror::Error;

pub use downloader::DownloadProgress;
pub use release::UpdateMetadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub metadata: UpdateMetadata,
    pub appimage_url: String,
    pub appimage_sha256: String,
    pub manifest: Vec<u8>,
    pub signature: String,
    pub metadata_json: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate { current: Version },
    Available(Box<UpdateInfo>),
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("release verification failed: {0}")]
    Verification(String),
    #[error("invalid update metadata: {0}")]
    Metadata(String),
    #[error("updater state error: {0}")]
    State(String),
    #[error("rollback protection triggered: {0}")]
    Rollback(String),
    #[error("update requires a newer updater: {0}")]
    Unsupported(String),
    #[error("update cancelled")]
    Cancelled,
}

pub fn can_apply_in_app() -> bool {
    std::env::var_os("APPIMAGE").is_some()
}

pub fn automatic_checks_enabled() -> bool {
    state::load()
        .map(|state| state.automatic_checks_enabled)
        .unwrap_or(true)
}

pub fn set_automatic_checks_enabled(enabled: bool) -> Result<(), UpdateError> {
    let mut persisted = state::load()?;
    persisted.automatic_checks_enabled = enabled;
    state::save(&persisted)
}

pub fn launch_staged_update(path: &std::path::Path) -> Result<(), UpdateError> {
    apply::launch(path)
}

pub fn apply_from_args() -> Option<Result<(), UpdateError>> {
    apply::parse_apply_args()
        .map(|args| args.and_then(|(pid, ticks, previous)| apply::run(pid, ticks, previous)))
}

pub fn mark_update_healthy() {
    apply::mark_healthy_from_env();
}

pub fn take_pending_notice() -> Result<Option<state::PendingNotice>, UpdateError> {
    let mut persisted = state::load()?;
    let notice = persisted.pending_notice.take();
    if notice.is_some() {
        state::save(&persisted)?;
    }
    Ok(notice)
}

pub fn stage_controlled<F>(
    info: &UpdateInfo,
    cancelled: &std::sync::atomic::AtomicBool,
    on_progress: F,
) -> Result<std::path::PathBuf, UpdateError>
where
    F: Fn(DownloadProgress),
{
    if !can_apply_in_app() {
        return Err(UpdateError::Unsupported(
            "one-click updates require an AppImage installation".into(),
        ));
    }
    if info.metadata.requires_helper_update {
        return Err(UpdateError::Unsupported(
            "this release changes the privileged Wayland helper and requires the authenticated installer".into(),
        ));
    }
    staging::stage(info, cancelled, on_progress)
}

pub fn check_automatic_if_due() -> Result<Option<CheckOutcome>, UpdateError> {
    if !can_apply_in_app() {
        return Ok(None);
    }
    let current = Version::parse(crate::app_meta::APP_VERSION)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    let mut persisted = state::load()?;
    if !persisted.automatic_checks_enabled {
        return Ok(None);
    }
    let now = chrono::Utc::now().timestamp();
    if persisted
        .last_checked
        .is_some_and(|last| now.saturating_sub(last) < 24 * 60 * 60)
    {
        return Ok(None);
    }
    let result = checker::check(&downloader::HttpTransport, &current, &mut persisted)?;
    state::save(&persisted)?;
    Ok(Some(result))
}

pub fn check_now() -> Result<CheckOutcome, UpdateError> {
    let current = Version::parse(crate::app_meta::APP_VERSION)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    let mut persisted = state::load()?;
    let result = checker::check(&downloader::HttpTransport, &current, &mut persisted);
    if result.is_ok() {
        state::save(&persisted)?;
    }
    result
}
