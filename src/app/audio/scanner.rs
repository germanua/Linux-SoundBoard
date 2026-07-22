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
    pub relative_subfolders: Vec<String>,
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
            let relative_subfolders = relative
                .parent()
                .into_iter()
                .flat_map(Path::ancestors)
                .filter(|ancestor| ancestor.components().next().is_some())
                .map(|ancestor| ancestor.to_string_lossy().into_owned())
                .collect();
            let file = AudioFile {
                path: file_path.to_string_lossy().into_owned(),
                name: file_path
                    .file_stem()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Unknown".to_string()),
                root_folder: folder.clone(),
                relative_path: relative.to_string_lossy().into_owned(),
                relative_subfolders,
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

#[cfg(test)]
fn scan_folder(folder: &str) -> AudioScan {
    let mut scan = AudioScan::default();
    let cancelled = AtomicBool::new(false);
    visit_audio_files(&[folder.to_string()], &cancelled, |file| {
        scan.subfolders
            .extend(
                file.relative_subfolders
                    .iter()
                    .map(|relative_subfolder| ScannedSubfolder {
                        root_folder: file.root_folder.clone(),
                        relative_subfolder: relative_subfolder.clone(),
                    }),
            );
        scan.files.push(file);
        ControlFlow::Continue(())
    });

    scan.files
        .sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    scan.subfolders.sort();
    scan.subfolders.dedup();
    info!("Found {} audio files in {}", scan.files.len(), folder);
    scan
}

pub fn scan_folders(folders: &[String]) -> AudioScan {
    let mut combined = AudioScan::default();
    let cancelled = AtomicBool::new(false);
    visit_audio_files(folders, &cancelled, |file| {
        combined
            .subfolders
            .extend(
                file.relative_subfolders
                    .iter()
                    .map(|relative_subfolder| ScannedSubfolder {
                        root_folder: file.root_folder.clone(),
                        relative_subfolder: relative_subfolder.clone(),
                    }),
            );
        combined.files.push(file);
        ControlFlow::Continue(())
    });

    combined.files.sort_by(|a, b| {
        a.root_folder
            .cmp(&b.root_folder)
            .then(a.relative_path.cmp(&b.relative_path))
            .then(a.path.cmp(&b.path))
    });
    let mut seen_paths = HashSet::new();
    combined.files.retain(|file| {
        seen_paths
            .insert(fs::canonicalize(&file.path).unwrap_or_else(|_| PathBuf::from(&file.path)))
    });
    combined.subfolders.sort();
    combined.subfolders.dedup();
    combined
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
    fn scan_folder_imports_opus_files() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create test dir");
        let opus_path = dir.join("clip.OPUS");
        fs::write(&opus_path, []).expect("write opus placeholder");

        let scan = scan_folder(&dir.to_string_lossy());

        fs::remove_dir_all(&dir).expect("cleanup test dir");
        assert_eq!(scan.files.len(), 1);
        assert_eq!(scan.files[0].path, opus_path.to_string_lossy());
    }

    #[test]
    fn scan_folder_omits_subfolders_without_supported_audio() {
        let root = test_dir();
        let with_audio = root.join("With Audio").join("Nested");
        let empty = root.join("Empty");
        let unsupported = root.join("Documents");
        fs::create_dir_all(&with_audio).expect("create audio subfolder");
        fs::create_dir_all(&empty).expect("create empty subfolder");
        fs::create_dir_all(&unsupported).expect("create unsupported subfolder");
        fs::write(with_audio.join("clip.OPUS"), []).expect("write supported audio");
        fs::write(unsupported.join("notes.txt"), []).expect("write unsupported file");

        let scan = scan_folder(&root.to_string_lossy());

        assert_eq!(
            scan.subfolders,
            [
                ScannedSubfolder {
                    root_folder: root.to_string_lossy().to_string(),
                    relative_subfolder: "With Audio".to_string(),
                },
                ScannedSubfolder {
                    root_folder: root.to_string_lossy().to_string(),
                    relative_subfolder: "With Audio/Nested".to_string(),
                },
            ]
        );
        fs::remove_dir_all(root).expect("cleanup test tree");
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
        assert!(root_audio.relative_subfolders.is_empty());
        let nested_audio = scan
            .files
            .iter()
            .find(|file| file.path == nested_file.to_string_lossy())
            .unwrap();
        assert_eq!(nested_audio.relative_path, "Меми/Nested/clip.ogg");
        assert_eq!(nested_audio.relative_subfolders, ["Меми/Nested", "Меми"]);
        assert!(scan.subfolders.iter().any(|folder| {
            folder.root_folder == root.to_string_lossy() && folder.relative_subfolder == "Меми"
        }));

        fs::remove_dir_all(root).expect("cleanup test tree");
    }

    #[test]
    fn scan_folders_parent_root_owns_configured_child() {
        let root = test_dir();
        let alerts = root.join("Alerts");
        fs::create_dir_all(&alerts).expect("create test tree");
        let sound = alerts.join("alert.mp3");
        fs::write(&sound, []).expect("write sound");

        let scan = scan_folders(&[
            alerts.to_string_lossy().to_string(),
            root.to_string_lossy().to_string(),
        ]);

        assert_eq!(scan.files.len(), 1);
        assert_eq!(scan.files[0].root_folder, root.to_string_lossy());
        assert_eq!(scan.subfolders.len(), 1);
        assert_eq!(scan.subfolders[0].root_folder, root.to_string_lossy());
        assert_eq!(scan.subfolders[0].relative_subfolder, "Alerts");

        fs::remove_dir_all(root).expect("cleanup test tree");
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

    #[cfg(unix)]
    #[test]
    fn scan_folders_deduplicates_symbolic_link_roots() {
        use std::os::unix::fs::symlink;

        let base = test_dir();
        let root = base.join("actual");
        let alias = base.join("alias");
        let alerts = root.join("Alerts");
        fs::create_dir_all(&alerts).expect("create test tree");
        fs::write(alerts.join("alert.mp3"), []).expect("write sound");
        symlink(&root, &alias).expect("create root alias");

        let scan = scan_folders(&[
            root.to_string_lossy().to_string(),
            alias.to_string_lossy().to_string(),
        ]);

        assert_eq!(scan.files.len(), 1);
        assert_eq!(scan.subfolders.len(), 1);
        assert_eq!(scan.files[0].root_folder, scan.subfolders[0].root_folder);

        fs::remove_file(alias).expect("remove root alias");
        fs::remove_dir_all(base).expect("cleanup test tree");
    }
}
