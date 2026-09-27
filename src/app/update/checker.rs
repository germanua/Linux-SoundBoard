use chrono::Utc;
use semver::Version;

use super::downloader::{Transport, API_BODY_LIMIT, METADATA_BODY_LIMIT};
use super::release::{GitHubRelease, UpdateMetadata};
use super::state::PersistedState;
use super::verifier::{parse_manifest, verify_bytes, verify_manifest};
use super::{CheckOutcome, UpdateError, UpdateInfo};

const MANIFEST_NAME: &str = "SHA256SUMS.txt";
const SIGNATURE_NAME: &str = "SHA256SUMS.txt.minisig";
const UPDATE_METADATA_NAME: &str = "update.json";

fn releases_url() -> String {
    format!(
        "https://api.github.com/repos/{}/releases?per_page=10",
        crate::app_meta::UPDATE_REPO
    )
}

fn release_tag_url(tag: &str) -> Result<String, UpdateError> {
    validate_tag(tag)?;
    Ok(format!(
        "https://api.github.com/repos/{}/releases/tags/{tag}",
        crate::app_meta::UPDATE_REPO
    ))
}

fn validate_tag(tag: &str) -> Result<(), UpdateError> {
    if !tag.starts_with('v')
        || Version::parse(tag.trim_start_matches('v')).is_err()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(UpdateError::Metadata("unsafe release tag".into()));
    }
    Ok(())
}

fn validate_asset_name(name: &str) -> Result<(), UpdateError> {
    if name.is_empty()
        || name.contains('/')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(UpdateError::Metadata("unsafe release asset name".into()));
    }
    Ok(())
}

fn asset_download_url(release: &GitHubRelease, name: &str) -> Result<String, UpdateError> {
    validate_asset_name(name)?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| UpdateError::Metadata(format!("release asset is missing: {name}")))?;
    let prefix = format!(
        "https://api.github.com/repos/{}/releases/assets/",
        crate::app_meta::UPDATE_REPO
    );
    if !asset.url.starts_with(&prefix)
        || asset.url[prefix.len()..].is_empty()
        || !asset.url[prefix.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(UpdateError::Metadata(
            "unsafe GitHub release asset URL".into(),
        ));
    }
    Ok(asset.url.clone())
}

fn release_allowed(release: &GitHubRelease) -> bool {
    !release.draft && !release.prerelease
}

fn release_for_tag<T: Transport>(transport: &T, tag: &str) -> Result<GitHubRelease, UpdateError> {
    let response = transport.get(&release_tag_url(tag)?, None, API_BODY_LIMIT)?;
    if response.status != 200 {
        return Err(UpdateError::Network(format!(
            "GitHub release lookup returned HTTP {}",
            response.status
        )));
    }
    let release: GitHubRelease = serde_json::from_slice(&response.body)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    if release.tag_name != tag || !release_allowed(&release) {
        return Err(UpdateError::Metadata(format!(
            "release {tag} is not valid for the {} channel",
            crate::app_meta::UPDATE_CHANNEL
        )));
    }
    Ok(release)
}

pub fn check<T: Transport>(
    transport: &T,
    current: &Version,
    state: &mut PersistedState,
) -> Result<CheckOutcome, UpdateError> {
    let releases_url = releases_url();
    let response = transport.get(&releases_url, state.etag.as_deref(), API_BODY_LIMIT)?;
    if response.status == 304 {
        state.last_checked = Some(Utc::now().timestamp());
        if let Some(highest) = state
            .highest_seen_version
            .as_ref()
            .filter(|version| *version > current)
        {
            let tag = format!("v{highest}");
            let release = release_for_tag(transport, &tag)?;
            return authenticate_release(transport, &release, current)
                .map(|info| CheckOutcome::Available(Box::new(info)));
        }
        return Ok(CheckOutcome::UpToDate {
            current: current.clone(),
        });
    }
    if response.status != 200 {
        return Err(UpdateError::Network(format!(
            "GitHub release discovery returned HTTP {}",
            response.status
        )));
    }
    let mut releases: Vec<GitHubRelease> = serde_json::from_slice(&response.body)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    releases.retain(release_allowed);
    releases.sort_by_key(|release| std::cmp::Reverse(release.version()));

    let mut best: Option<UpdateInfo> = None;
    let mut network_error = None;
    for release in releases {
        let Some(version) = release.version() else {
            continue;
        };
        if version <= *current {
            continue;
        }
        match authenticate_release(transport, &release, current) {
            Ok(info) => {
                if best
                    .as_ref()
                    .map(|existing| info.metadata.version > existing.metadata.version)
                    .unwrap_or(true)
                {
                    best = Some(info);
                }
            }
            Err(UpdateError::Network(error)) => network_error = Some(error),
            Err(UpdateError::Unsupported(error)) => return Err(UpdateError::Unsupported(error)),
            Err(error) => log::warn!(
                "Ignoring unauthenticated update candidate {}: {error}",
                release.tag_name
            ),
        }
    }

    state.last_checked = Some(Utc::now().timestamp());
    state.etag = response.etag;

    if let Some(info) = best {
        if let Some(highest) = state.highest_seen_version.as_ref() {
            if info.metadata.version < *highest {
                return Err(UpdateError::Rollback(format!(
                    "authenticated release {} is older than previously authenticated release {highest}",
                    info.metadata.version
                )));
            }
        }
        if state
            .highest_seen_version
            .as_ref()
            .map(|highest| info.metadata.version > *highest)
            .unwrap_or(true)
        {
            state.highest_seen_version = Some(info.metadata.version.clone());
        }
        return Ok(CheckOutcome::Available(Box::new(info)));
    }

    if let Some(highest) = state.highest_seen_version.as_ref() {
        if highest > current {
            return Err(UpdateError::Rollback(format!(
                "previously authenticated release {highest} is no longer discoverable"
            )));
        }
    }
    if let Some(error) = network_error {
        return Err(UpdateError::Network(error));
    }
    Ok(CheckOutcome::UpToDate {
        current: current.clone(),
    })
}

fn authenticate_release<T: Transport>(
    transport: &T,
    release: &GitHubRelease,
    current: &Version,
) -> Result<UpdateInfo, UpdateError> {
    let tag = &release.tag_name;
    validate_tag(tag)?;
    let version = release
        .version()
        .ok_or_else(|| UpdateError::Metadata("invalid release version".into()))?;
    let manifest = transport.get(
        &asset_download_url(release, MANIFEST_NAME)?,
        None,
        METADATA_BODY_LIMIT,
    )?;
    let signature = transport.get(
        &asset_download_url(release, SIGNATURE_NAME)?,
        None,
        METADATA_BODY_LIMIT,
    )?;
    let metadata_response = transport.get(
        &asset_download_url(release, UPDATE_METADATA_NAME)?,
        None,
        METADATA_BODY_LIMIT,
    )?;
    for response in [&manifest, &signature, &metadata_response] {
        if response.status != 200 {
            return Err(UpdateError::Network(format!(
                "release asset returned HTTP {}",
                response.status
            )));
        }
    }

    let signature_text = std::str::from_utf8(&signature.body)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    verify_manifest(&manifest.body, signature_text, tag)?;
    let entries = parse_manifest(&manifest.body)?;
    verify_bytes(UPDATE_METADATA_NAME, &metadata_response.body, &entries)?;
    let metadata: UpdateMetadata = serde_json::from_slice(&metadata_response.body)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    validate_metadata(&metadata, tag, &version, current)?;

    if metadata.appimage.size == 0
        || metadata.appimage.size > super::downloader::ARTIFACT_BODY_LIMIT
    {
        return Err(UpdateError::Metadata(
            "invalid AppImage size in update metadata".into(),
        ));
    }
    let appimage_sha256 = entries
        .get(&metadata.appimage.name)
        .cloned()
        .ok_or_else(|| {
            UpdateError::Verification("AppImage is missing from the signed manifest".into())
        })?;
    let appimage_url = asset_download_url(release, &metadata.appimage.name)?;

    Ok(UpdateInfo {
        appimage_url,
        metadata,
        appimage_sha256,
        manifest: manifest.body,
        signature: signature_text.to_string(),
        metadata_json: metadata_response.body,
    })
}

fn validate_metadata(
    metadata: &UpdateMetadata,
    release_tag: &str,
    release_version: &Version,
    current: &Version,
) -> Result<(), UpdateError> {
    if metadata.schema != 1 {
        return Err(UpdateError::Metadata(format!(
            "unsupported update metadata schema {}",
            metadata.schema
        )));
    }
    if metadata.channel != crate::app_meta::UPDATE_CHANNEL {
        return Err(UpdateError::Metadata(format!(
            "update metadata channel '{}' does not match '{}'",
            metadata.channel,
            crate::app_meta::UPDATE_CHANNEL
        )));
    }
    if metadata.tag != release_tag || metadata.version != *release_version {
        return Err(UpdateError::Verification(
            "release tag/version binding mismatch".into(),
        ));
    }
    if metadata.version <= *current {
        return Err(UpdateError::Rollback(
            "candidate is not newer than the running version".into(),
        ));
    }
    if current < &metadata.minimum_updater_version {
        return Err(UpdateError::Unsupported(format!(
            "update {} requires updater {} or newer",
            metadata.version, metadata.minimum_updater_version
        )));
    }
    let expected_name = format!(
        "{}-{}-{}.AppImage",
        crate::app_meta::APP_BINARY,
        metadata.version,
        std::env::consts::ARCH
    );
    if metadata.appimage.name != expected_name {
        return Err(UpdateError::Metadata(format!(
            "unexpected AppImage name '{}'",
            metadata.appimage.name
        )));
    }
    if metadata.summary.len() > 20 || metadata.summary.iter().any(|item| item.len() > 500) {
        return Err(UpdateError::Metadata(
            "update summary exceeds limits".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::downloader::HttpResponse;
    use crate::update::release::GitHubAsset;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MockTransport {
        responses: HashMap<String, HttpResponse>,
    }

    impl Transport for MockTransport {
        fn get(
            &self,
            url: &str,
            _etag: Option<&str>,
            _limit: u64,
        ) -> Result<HttpResponse, UpdateError> {
            self.responses
                .get(url)
                .cloned()
                .ok_or_else(|| UpdateError::Network(format!("missing mock response for {url}")))
        }
    }

    fn release(tag: &str, draft: bool, prerelease: bool) -> GitHubRelease {
        GitHubRelease {
            tag_name: tag.to_string(),
            draft,
            prerelease,
            assets: vec![],
        }
    }

    #[test]
    fn release_sorting_is_semantic() {
        let mut releases = [
            release("v2.9.0", false, false),
            release("v2.10.0", false, false),
            release("v9.0.0-rc.1", false, true),
        ];
        releases.sort_by_key(|release| std::cmp::Reverse(release.version()));
        assert_eq!(releases[0].tag_name, "v9.0.0-rc.1");
    }

    #[test]
    fn release_filter_accepts_only_published_stable_releases() {
        assert!(release_allowed(&release("v2.4.7", false, false)));
        assert!(!release_allowed(&release("v2.4.7-rc.1", false, true)));
        assert!(!release_allowed(&release("v2.4.7", true, false)));
    }

    #[test]
    fn metadata_rejects_version_or_tag_mismatch() {
        let version = Version::parse("9.9.9").unwrap();
        let metadata = UpdateMetadata {
            schema: 1,
            version: version.clone(),
            tag: "v9.9.8".into(),
            channel: crate::app_meta::UPDATE_CHANNEL.into(),
            published_at: "2026-09-25T00:00:00Z".into(),
            minimum_updater_version: Version::parse("0.0.1").unwrap(),
            critical: false,
            requires_helper_update: false,
            appimage: super::super::release::AppImageMetadata {
                name: format!(
                    "{}-{}-{}.AppImage",
                    crate::app_meta::APP_BINARY,
                    version,
                    std::env::consts::ARCH
                ),
                size: 100,
            },
            summary: vec![],
        };
        assert!(validate_metadata(
            &metadata,
            "v9.9.9",
            &version,
            &Version::parse("1.0.0").unwrap()
        )
        .is_err());
    }

    #[test]
    fn unsigned_higher_release_cannot_override_current_state() {
        let tag = "v999.0.0";
        let prerelease = false;
        let body = serde_json::to_vec(&serde_json::json!([{
            "tag_name": tag,
            "draft": false,
            "prerelease": prerelease,
            "assets": []
        }]))
        .unwrap();
        let mut transport = MockTransport::default();
        transport.responses.insert(
            releases_url(),
            HttpResponse {
                status: 200,
                etag: None,
                body,
            },
        );
        let mut state = PersistedState::default();
        let result = check(&transport, &Version::parse("2.4.6").unwrap(), &mut state).unwrap();
        assert!(matches!(result, CheckOutcome::UpToDate { .. }));
        assert!(state.highest_seen_version.is_none());
    }

    #[test]
    fn asset_urls_must_be_api_assets_from_the_configured_repository() {
        let mut candidate = release("v2.4.7", false, false);
        candidate.assets.push(GitHubAsset {
            name: "payload.AppImage".into(),
            url: format!(
                "https://api.github.com/repos/{}/releases/assets/1234",
                crate::app_meta::UPDATE_REPO
            ),
        });
        assert!(asset_download_url(&candidate, "payload.AppImage").is_ok());
        candidate.assets[0].url = "https://example.com/payload".into();
        assert!(asset_download_url(&candidate, "payload.AppImage").is_err());
        assert!(asset_download_url(&candidate, "../payload").is_err());
    }
}
