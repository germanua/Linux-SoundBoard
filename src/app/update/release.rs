use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppImageMetadata {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMetadata {
    pub schema: u32,
    pub version: Version,
    pub tag: String,
    pub channel: String,
    pub published_at: String,
    pub minimum_updater_version: Version,
    pub critical: bool,
    pub requires_helper_update: bool,
    pub appimage: AppImageMetadata,
    pub summary: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub assets: Vec<GitHubAsset>,
}

impl GitHubRelease {
    pub fn version(&self) -> Option<Version> {
        Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)).ok()
    }
}
