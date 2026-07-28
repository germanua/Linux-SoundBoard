use log::info;
use std::collections::HashSet;
use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use walkdir::WalkDir;

pub(crate) const AUDIO_EXTENSIONS: &[&str] =
    &["wav", "mp3", "ogg", "opus", "flac", "m4a", "aac", "mp4"];

#[derive(Debug, Clone)]
pub struct AudioFile {
    pub path: String,
    pub name: String,
    pub root_folder: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScannedSubfolder {
    pub root_folder: String,
    pub relative_subfolder: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioVisitProgress {
    pub files: usize,
    pub cancelled: bool,
    pub stopped_early: bool,
}

#[derive(Debug)]
struct ScanRoot {
    configured: String,
    canonical: Option<PathBuf>,
}

fn scan_roots(folders: &[String]) -> Vec<String> {
    let mut roots = folders
        .iter()
        .cloned()
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|configured| ScanRoot {
            canonical: fs::canonicalize(&configured).ok(),
            configured,
        })
        .collect::<Vec<_>>();
    roots.sort_by(|a, b| match (&a.canonical, &b.canonical) {
        (Some(a_path), Some(b_path)) => a_path
            .components()
            .count()
            .cmp(&b_path.components().count())
            .then(a_path.cmp(b_path))
            .then(a.configured.cmp(&b.configured)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.configured.cmp(&b.configured),
    });

    let mut owners = Vec::new();
    roots
        .into_iter()
        .filter_map(|root| {
            let Some(canonical) = root.canonical.as_ref() else {
                return Some(root.configured);
            };
            if let Some(owner) = owners
                .iter()
                .find(|owner: &&PathBuf| canonical.starts_with(owner))
            {
                info!(
                    "Skipping overlapping sound folder '{}' owned by '{}'",
                    root.configured,
                    owner.display()
                );
                None
            } else {
                owners.push(canonical.clone());
                Some(root.configured)
            }
        })
        .collect()
}

pub(crate) fn visit_audio_files<F>(
    folders: &[String],
    cancelled: &AtomicBool,
    mut visitor: F,
) -> AudioVisitProgress
where
    F: FnMut(AudioFile) -> ControlFlow<()>,
{
    let mut progress = AudioVisitProgress::default();
    for folder in scan_roots(folders) {
        if cancelled.load(Ordering::Relaxed) {
            progress.cancelled = true;
            break;
        }
        let root = Path::new(&folder);
        if !root.is_dir() {
            continue;
        }
        info!("Scanning folder: {folder}");
        for entry in WalkDir::new(root)
            .follow_links(true)
            .sort_by_file_name()
            .into_iter()
        {
            if cancelled.load(Ordering::Relaxed) {
                progress.cancelled = true;
                return progress;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    log::warn!("Skipping unreadable entry while scanning '{folder}': {err}");
                    continue;
                }
            };
            let file_path = entry.path();
            if !file_path.is_file() || !is_audio_path(file_path) {
                continue;
            }
            let relative = file_path.strip_prefix(root).unwrap_or(file_path);
            let file = AudioFile {
                path: file_path.to_string_lossy().into_owned(),
                name: file_path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Unknown".to_string()),
                root_folder: folder.clone(),
                relative_path: relative.to_string_lossy().into_owned(),
            };
            progress.files += 1;
            if visitor(file).is_break() {
                progress.stopped_early = true;
                return progress;
            }
        }
    }
    progress
}

pub fn is_audio_file(path: &str) -> bool {
    is_audio_path(Path::new(path))
}

fn is_audio_path(path: &Path) -> bool {
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
    fn is_audio_file_accepts_opus_case_insensitive() {
        assert!(is_audio_file("/tmp/sound.opus"));
        assert!(is_audio_file("/tmp/sound.OPUS"));
    }

    #[test]
    fn is_audio_file_accepts_wav_case_insensitive() {
        assert!(is_audio_file("/tmp/sound.wav"));
        assert!(is_audio_file("/tmp/sound.WAV"));
    }

    #[test]
    fn is_audio_file_rejects_unsupported_extensions() {
        assert!(!is_audio_file("/tmp/video.mkv"));
        assert!(!is_audio_file("/tmp/no-extension"));
    }

    #[test]
    fn streaming_scan_is_deterministic_and_stops_at_callback_boundary() {
        let root = test_dir();
        fs::create_dir_all(&root).expect("create test tree");
        fs::write(root.join("c.mp3"), []).expect("write sound");
        fs::write(root.join("a.mp3"), []).expect("write sound");
        fs::write(root.join("b.mp3"), []).expect("write sound");
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let mut visited = Vec::new();

        let progress =
            visit_audio_files(&[root.to_string_lossy().to_string()], &cancelled, |file| {
                visited.push(file.name);
                if visited.len() == 2 {
                    std::ops::ControlFlow::Break(())
                } else {
                    std::ops::ControlFlow::Continue(())
                }
            });

        assert_eq!(visited, ["a", "b"]);
        assert_eq!(progress.files, 2);
        assert!(progress.stopped_early);
        fs::remove_dir_all(root).expect("cleanup test tree");
    }

    #[test]
    fn streaming_scan_honours_preexisting_cancellation_without_walking() {
        let root = test_dir();
        fs::create_dir_all(&root).expect("create test tree");
        fs::write(root.join("sound.mp3"), []).expect("write sound");
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let mut visited = 0;

        let progress = visit_audio_files(&[root.to_string_lossy().to_string()], &cancelled, |_| {
            visited += 1;
            std::ops::ControlFlow::Continue(())
        });

        assert_eq!(visited, 0);
        assert_eq!(progress.files, 0);
        assert!(progress.cancelled);
        fs::remove_dir_all(root).expect("cleanup test tree");
    }
}
