//! Audio file scanner.

use log::info;
use rayon::prelude::*;
use std::path::Path;
use walkdir::WalkDir;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "ogg", "flac", "m4a", "aac", "mp4"];

#[derive(Debug, Clone)]
pub struct AudioFile {
    pub path: String,
    pub name: String,
}

pub fn scan_folder(folder: &str) -> Vec<AudioFile> {
    let mut files = Vec::new();

    let path = Path::new(folder);
    if !path.exists() || !path.is_dir() {
        return files;
    }

    info!("Scanning folder: {}", folder);

    for entry in WalkDir::new(folder)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext_lower = ext.to_string_lossy().to_lowercase();
                if AUDIO_EXTENSIONS.contains(&ext_lower.as_str()) {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Unknown".to_string());

                    files.push(AudioFile {
                        path: path.to_string_lossy().to_string(),
                        name,
                    });
                }
            }
        }
    }

    info!("Found {} audio files in {}", files.len(), folder);
    files
}

/// Scan multiple folders in parallel.
pub fn scan_folders(folders: &[String]) -> Vec<AudioFile> {
    folders
        .par_iter()
        .flat_map_iter(|folder| scan_folder(folder).into_iter())
        .collect()
}

/// Check whether a path has a supported audio extension.
pub fn is_audio_file(path: &str) -> bool {
    let path = Path::new(path);

    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        AUDIO_EXTENSIONS.contains(&ext_lower.as_str())
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lsb-scanner-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn is_audio_file_accepts_mp4_case_insensitive() {
        assert!(is_audio_file("/tmp/sound.mp4"));
        assert!(is_audio_file("/tmp/sound.MP4"));
    }

    #[test]
    fn is_audio_file_rejects_unsupported_extensions() {
        assert!(!is_audio_file("/tmp/video.mkv"));
        assert!(!is_audio_file("/tmp/no-extension"));
    }

    #[test]
    fn scan_folder_imports_mp4_files() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create test dir");
        let mp4_path = dir.join("clip.mp4");
        let txt_path = dir.join("notes.txt");
        fs::write(&mp4_path, []).expect("write mp4 placeholder");
        fs::write(txt_path, []).expect("write unsupported placeholder");

        let files = scan_folder(&dir.to_string_lossy());

        fs::remove_dir_all(&dir).expect("cleanup test dir");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "clip");
        assert_eq!(files[0].path, mp4_path.to_string_lossy());
    }
}
