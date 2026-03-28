//! File link validation module
//!
//! Provides functions for checking file existence and batch validation
//! of sound source paths using bounded parallel processing.

use std::path::Path;

const VALIDATION_PARALLEL_THRESHOLD: usize = 64;
pub const STARTUP_VALIDATION_CHUNK_SIZE: usize = 32;
const VALIDATION_MAX_THREADS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Sequential,
    ParallelPool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReport {
    pub missing_ids: Vec<String>,
    pub input_count: usize,
    pub mode: ValidationMode,
    pub worker_threads: usize,
}

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
    validate_sounds_batch_with_report(sounds).missing_ids
}

pub fn validate_sounds_batch_with_report(
    sounds: &[(String, Option<String>, String)],
) -> ValidationReport {
    if sounds.is_empty() {
        return ValidationReport {
            missing_ids: Vec::new(),
            input_count: 0,
            mode: ValidationMode::Sequential,
            worker_threads: 1,
        };
    }

    if sounds.len() <= VALIDATION_PARALLEL_THRESHOLD {
        return ValidationReport {
            missing_ids: validate_sounds_sequential(sounds),
            input_count: sounds.len(),
            mode: ValidationMode::Sequential,
            worker_threads: 1,
        };
    }

    let worker_threads = bounded_validation_threads();
    let missing_ids = match rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads)
        .build()
    {
        Ok(pool) => pool.install(|| validate_sounds_parallel(sounds)),
        Err(_e) => {
            return ValidationReport {
                missing_ids: validate_sounds_sequential(sounds),
                input_count: sounds.len(),
                mode: ValidationMode::Sequential,
                worker_threads: 1,
            };
        }
    };

    ValidationReport {
        missing_ids,
        input_count: sounds.len(),
        mode: ValidationMode::ParallelPool,
        worker_threads,
    }
}

pub fn validate_sounds_chunked_with_report(
    sounds: &[(String, Option<String>, String)],
    chunk_size: usize,
) -> ValidationReport {
    if sounds.is_empty() {
        return ValidationReport {
            missing_ids: Vec::new(),
            input_count: 0,
            mode: ValidationMode::Sequential,
            worker_threads: 1,
        };
    }

    let chunk_size = chunk_size.max(1);
    let mut missing_ids = Vec::new();
    let mut used_parallel_pool = false;
    let mut max_worker_threads = 1usize;

    for chunk in sounds.chunks(chunk_size) {
        let report = validate_sounds_batch_with_report(chunk);
        if report.mode == ValidationMode::ParallelPool {
            used_parallel_pool = true;
            max_worker_threads = max_worker_threads.max(report.worker_threads);
        }
        missing_ids.extend(report.missing_ids);
    }

    ValidationReport {
        missing_ids,
        input_count: sounds.len(),
        mode: if used_parallel_pool {
            ValidationMode::ParallelPool
        } else {
            ValidationMode::Sequential
        },
        worker_threads: max_worker_threads,
    }
}

fn validate_sounds_parallel(sounds: &[(String, Option<String>, String)]) -> Vec<String> {
    use rayon::prelude::*;

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

fn validate_sounds_sequential(sounds: &[(String, Option<String>, String)]) -> Vec<String> {
    sounds
        .iter()
        .filter_map(|(id, source_path, path)| {
            let check_path = source_path.as_ref().unwrap_or(path);
            if !check_file_exists(check_path) {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect()
}

fn bounded_validation_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(1, VALIDATION_MAX_THREADS))
        .unwrap_or(1)
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
        let report = validate_sounds_batch_with_report(&sounds);
        assert!(report.missing_ids.is_empty());
        assert_eq!(report.mode, ValidationMode::Sequential);
        assert_eq!(report.worker_threads, 1);
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
        let report = validate_sounds_batch_with_report(&sounds);
        assert_eq!(report.missing_ids.len(), 2); // Both should be invalid since paths don't exist
        assert_eq!(report.mode, ValidationMode::Sequential);
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
        let report = validate_sounds_batch_with_report(&sounds);
        assert_eq!(report.missing_ids.len(), 3); // All should be invalid
        assert!(report.missing_ids.contains(&"id1".to_string()));
        assert!(report.missing_ids.contains(&"id2".to_string()));
        assert!(report.missing_ids.contains(&"id3".to_string()));
        assert_eq!(report.mode, ValidationMode::Sequential);
    }

    #[test]
    fn test_validate_sounds_batch_large_uses_bounded_parallel_pool() {
        let sounds = (0..(VALIDATION_PARALLEL_THRESHOLD + 1))
            .map(|idx| {
                (
                    format!("id{idx}"),
                    None,
                    format!("/nonexistent/file{idx}.mp3"),
                )
            })
            .collect::<Vec<_>>();

        let report = validate_sounds_batch_with_report(&sounds);
        assert_eq!(report.missing_ids.len(), sounds.len());
        assert_eq!(report.mode, ValidationMode::ParallelPool);
        assert!((1..=VALIDATION_MAX_THREADS).contains(&report.worker_threads));
    }

    #[test]
    fn test_validate_sounds_chunked_keeps_small_startup_batches_sequential() {
        let sounds = (0..(STARTUP_VALIDATION_CHUNK_SIZE + 5))
            .map(|idx| {
                (
                    format!("id{idx}"),
                    None,
                    format!("/nonexistent/file{idx}.mp3"),
                )
            })
            .collect::<Vec<_>>();

        let report = validate_sounds_chunked_with_report(&sounds, STARTUP_VALIDATION_CHUNK_SIZE);
        assert_eq!(report.input_count, sounds.len());
        assert_eq!(report.missing_ids.len(), sounds.len());
        assert_eq!(report.mode, ValidationMode::Sequential);
        assert_eq!(report.worker_threads, 1);
    }
}
