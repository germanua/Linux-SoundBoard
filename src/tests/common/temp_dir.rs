//! Temporary directory management for tests.

use std::fs;
use std::path::PathBuf;

/// Temporary config directory for testing
pub struct TempConfigDir {
    path: PathBuf,
}

impl TempConfigDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lsb-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

impl Default for TempConfigDir {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TempConfigDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
