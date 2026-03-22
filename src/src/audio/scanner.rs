//! Audio file scanner using walkdir

use log::info;
use rayon::prelude::*;
use std::path::Path;
use walkdir::WalkDir;

/// Supported audio file extensions
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "ogg", "flac", "m4a", "aac"];

/// Represents a discovered audio file
#[derive(Debug, Clone)]
pub struct AudioFile {
    pub path: String,
    pub name: String,
}

/// Scan a folder for audio files
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

/// Scan multiple folders for audio files
pub fn scan_folders(folders: &[String]) -> Vec<AudioFile> {
    folders
        .par_iter()
        .flat_map_iter(|folder| scan_folder(folder).into_iter())
        .collect()
}

/// Check if a file is a supported audio file
pub fn is_audio_file(path: &str) -> bool {
    let path = Path::new(path);

    if let Some(ext) = path.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        AUDIO_EXTENSIONS.contains(&ext_lower.as_str())
    } else {
        false
    }
}
