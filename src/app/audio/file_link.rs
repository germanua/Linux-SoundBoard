//! Validate sound source paths.

use std::path::Path;

pub fn check_file_exists(path: &str) -> bool {
    Path::new(path).exists()
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

        assert!(!check_file_exists("/nonexistent/path/to/file.mp3"));
    }
}
