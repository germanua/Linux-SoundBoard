//! File link validation module
//!
//! Provides functions for checking file existence and batch validation
//! of sound source paths using parallel processing.

use rayon::prelude::*;
use std::path::Path;

/// Check if a single file exists at the given path
pub fn check_file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Validate multiple sounds in parallel, returns IDs of sounds with missing source files
///
/// # Arguments
/// * `sounds` - A slice of tuples containing (sound_id, source_path, path)
///   - source_path: Original file location (for drag & drop imports)
///   - path: Current file path (fallback for legacy sounds)
///
/// # Returns
/// A vector of sound IDs where the source file is missing
pub fn validate_sounds_batch(sounds: &[(String, Option<String>, String)]) -> Vec<String> {
    sounds
        .par_iter()
        .filter_map(|(id, source_path, path)| {
            // Check source_path first, fallback to path
            let check_path = source_path.as_ref().unwrap_or(path);
            if !check_file_exists(check_path) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_check_file_exists() {
        let temp_path = std::env::temp_dir().join(format!(
            "linux-soundboard-file-link-{}.tmp",
            std::process::id()
        ));
        fs::write(&temp_path, b"ok").unwrap();
        assert!(check_file_exists(temp_path.to_string_lossy().as_ref()));
        let _ = fs::remove_file(&temp_path);

        // Non-existent file
        assert!(!check_file_exists("/nonexistent/path/to/file.mp3"));
    }

    #[test]
    fn test_validate_sounds_batch_empty() {
        let sounds: Vec<(String, Option<String>, String)> = vec![];
        let result = validate_sounds_batch(&sounds);
        assert!(result.is_empty());
    }

    #[test]
    fn test_validate_sounds_batch_with_valid_path_fallback() {
        // When source_path is None, it should check the path field
        // These paths don't exist, so they should be invalid
        let sounds = vec![
            (
                "id1".to_string(),
                None,
                "/nonexistent/file1.mp3".to_string(),
            ),
            (
                "id2".to_string(),
                None,
                "/nonexistent/file2.mp3".to_string(),
            ),
        ];
        let result = validate_sounds_batch(&sounds);
        assert_eq!(result.len(), 2); // Both should be invalid since paths don't exist
    }

    #[test]
    fn test_validate_sounds_batch_with_missing() {
        let sounds = vec![
            (
                "id1".to_string(),
                Some("/nonexistent/file1.mp3".to_string()),
                "/any/path.mp3".to_string(),
            ),
            (
                "id2".to_string(),
                Some("/nonexistent/file2.mp3".to_string()),
                "/any/path.mp3".to_string(),
            ),
            (
                "id3".to_string(),
                None,
                "/nonexistent/file3.mp3".to_string(),
            ),
        ];
        let result = validate_sounds_batch(&sounds);
        assert_eq!(result.len(), 3); // All should be invalid
        assert!(result.contains(&"id1".to_string()));
        assert!(result.contains(&"id2".to_string()));
        assert!(result.contains(&"id3".to_string()));
    }
}
