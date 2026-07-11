use log::info;
use rayon::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "ogg", "flac", "m4a", "aac", "mp4"];

#[derive(Debug, Clone)]
pub struct AudioFile {
    pub path: String,
    pub name: String,
    pub root_folder: String,
    pub relative_path: String,
    pub top_level_subfolder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScannedSubfolder {
    pub root_folder: String,
    pub relative_subfolder: String,
}

#[derive(Debug, Clone, Default)]
pub struct AudioScan {
    pub files: Vec<AudioFile>,
    pub subfolders: Vec<ScannedSubfolder>,
}

pub fn scan_folder(folder: &str) -> AudioScan {
    let mut scan = AudioScan::default();

    let path = Path::new(folder);
    if !path.exists() || !path.is_dir() {
        return scan;
    }

    info!("Scanning folder: {}", folder);

    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                if entry.path().is_dir() {
                    scan.subfolders.push(ScannedSubfolder {
                        root_folder: folder.to_string(),
                        relative_subfolder: entry.file_name().to_string_lossy().to_string(),
                    });
                }
            }
        }
        Err(err) => log::warn!("Failed to enumerate sound folder '{folder}': {err}"),
    }

    for entry in WalkDir::new(folder)
        .follow_links(true)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                log::warn!("Skipping unreadable entry while scanning '{folder}': {err}");
                None
            }
        })
    {
        let file_path = entry.path();

        if file_path.is_file() {
            if let Some(ext) = file_path.extension() {
                let ext_lower = ext.to_string_lossy().to_lowercase();
                if AUDIO_EXTENSIONS.contains(&ext_lower.as_str()) {
                    let name = file_path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Unknown".to_string());
                    let relative = file_path.strip_prefix(path).unwrap_or(file_path);
                    let mut components = relative.components();
                    let first = components.next();
                    let top_level_subfolder = if components.next().is_some() {
                        first.map(|part| part.as_os_str().to_string_lossy().to_string())
                    } else {
                        None
                    };

                    scan.files.push(AudioFile {
                        path: file_path.to_string_lossy().to_string(),
                        name,
                        root_folder: folder.to_string(),
                        relative_path: relative.to_string_lossy().to_string(),
                        top_level_subfolder,
                    });
                }
            }
        }
    }

    scan.files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    scan.subfolders.sort();
    scan.subfolders.dedup();
    info!("Found {} audio files in {}", scan.files.len(), folder);
    scan
}

pub fn scan_folders(folders: &[String]) -> AudioScan {
    let mut roots = folders.to_vec();
    roots.sort();
    roots.dedup();

    let scans = roots
        .par_iter()
        .map(|folder| scan_folder(folder))
        .collect::<Vec<_>>();
    let mut combined = AudioScan::default();
    for scan in scans {
        combined.files.extend(scan.files);
        combined.subfolders.extend(scan.subfolders);
    }

    combined.files.sort_by(|a, b| {
        a.root_folder
            .cmp(&b.root_folder)
            .then(a.relative_path.cmp(&b.relative_path))
            .then(a.path.cmp(&b.path))
    });
    let mut seen_paths = HashSet::new();
    combined
        .files
        .retain(|file| seen_paths.insert(file.path.clone()));
    combined.subfolders.sort();
    combined.subfolders.dedup();
    combined
}

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

        let scan = scan_folder(&dir.to_string_lossy());

        fs::remove_dir_all(&dir).expect("cleanup test dir");
        assert_eq!(scan.files.len(), 1);
        assert_eq!(scan.files[0].name, "clip");
        assert_eq!(scan.files[0].path, mp4_path.to_string_lossy());
    }

    #[test]
    fn scan_folders_preserves_folder_relationships_and_deduplicates_overlaps() {
        let root = test_dir();
        let nested = root.join("Меми").join("Nested");
        fs::create_dir_all(&nested).expect("create nested test tree");
        let root_file = root.join("root.mp3");
        let nested_file = nested.join("clip.ogg");
        fs::write(&root_file, []).expect("write root audio placeholder");
        fs::write(&nested_file, []).expect("write nested audio placeholder");

        let scan = scan_folders(&[
            nested.parent().unwrap().to_string_lossy().to_string(),
            root.to_string_lossy().to_string(),
        ]);

        assert_eq!(scan.files.len(), 2);
        let root_audio = scan
            .files
            .iter()
            .find(|file| file.path == root_file.to_string_lossy())
            .unwrap();
        assert_eq!(root_audio.root_folder, root.to_string_lossy());
        assert_eq!(root_audio.relative_path, "root.mp3");
        assert_eq!(root_audio.top_level_subfolder, None);
        let nested_audio = scan
            .files
            .iter()
            .find(|file| file.path == nested_file.to_string_lossy())
            .unwrap();
        assert_eq!(nested_audio.relative_path, "Меми/Nested/clip.ogg");
        assert_eq!(nested_audio.top_level_subfolder.as_deref(), Some("Меми"));
        assert!(scan.subfolders.iter().any(|folder| {
            folder.root_folder == root.to_string_lossy() && folder.relative_subfolder == "Меми"
        }));

        fs::remove_dir_all(root).expect("cleanup test tree");
    }
}
