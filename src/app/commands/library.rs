use parking_lot::Mutex;
use std::collections::{BTreeMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rayon::prelude::*;

use crate::audio::file_link::check_file_exists;
use crate::audio::scanner;
use crate::config::{Config, LoudnessAnalysisState, Sound};
use crate::library_store::{
    FolderRecord, LibraryBatch, LibraryScope, LibraryStore, RootRecord, SoundLocationRecord,
    SoundRecord, MAX_BATCH_ROWS,
};

use super::shared::{
    build_sound_with_metadata, compute_sound_source_fingerprint, dispatch_async_result,
    probe_duration_ms, ERR_FILE_DOES_NOT_EXIST, ERR_UNSUPPORTED_AUDIO_FILE,
};
use super::{CommandError, LoudnessCoordinators};

const STORE_SCAN_METADATA_BATCH: usize = 32;

fn maybe_schedule_missing_loudness_backfill_with_store(
    config: &Arc<Mutex<Config>>,
    library: &LibraryStore,
    coords: &LoudnessCoordinators,
) {
    match crate::commands::trigger_missing_loudness_analysis_with_store(
        Arc::clone(config),
        library.clone(),
        false,
        None,
        coords,
    ) {
        Ok(crate::commands::MissingLoudnessAnalysisTrigger::Started) => {
            log::info!("Scheduled missing loudness analysis after library update");
        }
        Ok(_) => {}
        Err(error) => log::warn!("Failed to schedule loudness analysis: {error}"),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    pub added: usize,
    pub removed: usize,
    pub refreshed: usize,
    pub invalidated: usize,
    pub tabs_created: usize,
    pub tabs_removed: usize,
    pub tab_memberships_added: usize,
}

#[derive(Default)]
struct StoreScanBatch {
    root: String,
    generation: i64,
    folders: Vec<FolderRecord>,
    folder_paths: HashSet<String>,
    sounds: Vec<SoundRecord>,
    rows: usize,
}

impl StoreScanBatch {
    fn reset_for(&mut self, root: String, generation: i64) {
        self.root = root;
        self.generation = generation;
        self.folders.clear();
        self.folder_paths.clear();
        self.sounds.clear();
        self.rows = 0;
    }

    fn flush(&mut self, library: &LibraryStore) -> Result<(), CommandError> {
        if self.folders.is_empty() && self.sounds.is_empty() {
            return Ok(());
        }
        library
            .apply_root_scan_batch(
                &self.root,
                self.generation,
                std::mem::take(&mut self.folders),
                std::mem::take(&mut self.sounds),
            )
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
        self.folder_paths.clear();
        self.rows = 0;
        Ok(())
    }

    fn push_file(
        &mut self,
        file: scanner::AudioFile,
        sound: Sound,
        position: usize,
        library: &LibraryStore,
    ) -> Result<(), CommandError> {
        let parent = Path::new(&file.relative_path)
            .parent()
            .filter(|path| path.components().next().is_some())
            .map(|path| path.to_string_lossy().into_owned());
        // Match the store's per-component folder row count.
        let folder_rows = parent
            .as_ref()
            .map(|path| Path::new(path).components().count().max(1))
            .unwrap_or(0);
        let already_in_batch = parent
            .as_ref()
            .is_some_and(|path| self.folder_paths.contains(path));

        // Reserve the folder row repeated after each flush.
        let rows_after_flush = 2_usize.saturating_add(folder_rows);
        if rows_after_flush > MAX_BATCH_ROWS {
            return Err(CommandError::Library(format!(
                "folder nesting exceeds the {MAX_BATCH_ROWS}-row scan transaction limit"
            )));
        }
        let required_rows = if already_in_batch {
            2
        } else {
            rows_after_flush
        };
        if self.rows.saturating_add(required_rows) > MAX_BATCH_ROWS {
            self.flush(library)?;
        }

        if let Some(relative_path) = parent.as_ref() {
            if self.folder_paths.insert(relative_path.clone()) {
                let path = Path::new(relative_path);
                self.folders.push(FolderRecord {
                    root_path: self.root.clone(),
                    relative_path: relative_path.clone(),
                    parent_relative_path: path
                        .parent()
                        .filter(|parent| parent.components().next().is_some())
                        .map(|parent| parent.to_string_lossy().into_owned()),
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| relative_path.clone()),
                    position: 0,
                });
                self.rows = self.rows.saturating_add(folder_rows);
            }
        }

        self.sounds.push(SoundRecord {
            sound,
            general_position: position,
            locations: vec![SoundLocationRecord {
                root_path: self.root.clone(),
                folder_relative_path: parent,
                relative_path: file.relative_path,
            }],
        });
        self.rows = self.rows.saturating_add(2);
        Ok(())
    }
}

fn build_scanned_metadata_batch(
    files: Vec<scanner::AudioFile>,
    cancelled: &AtomicBool,
    pool: Option<&rayon::ThreadPool>,
) -> Vec<(scanner::AudioFile, Sound)> {
    let build = || {
        files
            .into_par_iter()
            .map(|file| {
                if cancelled.load(Ordering::Relaxed) {
                    return None;
                }
                let sound = build_sound_with_metadata(file.name.clone(), file.path.clone());
                (!cancelled.load(Ordering::Relaxed)).then_some((file, sound))
            })
            .collect::<Vec<_>>()
    };
    let built = match pool {
        Some(pool) => pool.install(build),
        None => build(),
    };
    built.into_iter().flatten().collect()
}

pub fn add_sound_with_store(
    name: String,
    path: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    coords: &LoudnessCoordinators,
) -> Result<Sound, CommandError> {
    if !Path::new(&path).exists() {
        return Err(CommandError::Invalid(ERR_FILE_DOES_NOT_EXIST.to_string()));
    }
    if !scanner::is_audio_file(&path) {
        return Err(CommandError::Invalid(
            ERR_UNSUPPORTED_AUDIO_FILE.to_string(),
        ));
    }
    let position = library
        .count(LibraryScope::General, "")
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    let sound = build_sound_with_metadata(name, path);
    library
        .apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
            sound: sound.clone(),
            general_position: position,
            locations: Vec::new(),
        }]))
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    maybe_schedule_missing_loudness_backfill_with_store(&config, &library, coords);
    Ok(sound)
}

pub fn rename_sound_with_store(
    id: String,
    name: String,
    library: LibraryStore,
) -> Result<Sound, CommandError> {
    let new_name = name.trim().to_string();
    if new_name.is_empty() {
        return Err(CommandError::Invalid("Name cannot be empty".to_string()));
    }
    let mut sound = library
        .sound_by_id(&id)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?
        .ok_or(CommandError::SoundNotFound)?;
    sound.name = new_name;
    library
        .update_sound(sound.clone())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(sound)
}

pub fn rename_sound_with_store_async<F>(
    id: String,
    name: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Sound, CommandError>) + 'static,
{
    dispatch_async_result(
        "rename_sound",
        move || rename_sound_with_store(id, name, library),
        on_complete,
    )
}

pub fn remove_sounds_with_store(
    ids: Vec<String>,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    let mut removed = Vec::new();
    for id in &ids {
        if library
            .delete_sound(id)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?
        {
            removed.push(id.clone());
        }
    }
    if !removed.is_empty() {
        projection
            .reconcile_blocking()
            .map_err(CommandError::HotkeyProjection)?;
    }
    Ok(())
}

pub fn remove_sounds_with_store_async<F>(
    ids: Vec<String>,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "remove_sounds",
        move || remove_sounds_with_store(ids, library, projection),
        on_complete,
    )
}

pub fn add_sound_folder_with_store(
    folder: String,
    library: LibraryStore,
) -> Result<(), CommandError> {
    if !Path::new(&folder).is_dir() {
        return Err(CommandError::Invalid("Folder does not exist".to_string()));
    }
    let position = library
        .roots(0)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?
        .total;
    library
        .apply_batch(LibraryBatch::Roots(vec![RootRecord {
            path: folder,
            position,
        }]))
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))
}

pub fn add_sound_folder_with_store_async<F>(
    folder: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "add_sound_folder",
        move || add_sound_folder_with_store(folder, library),
        on_complete,
    )
}

pub fn remove_sound_folder_with_store(
    folder: String,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    library
        .remove_root(&folder)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
}

pub fn remove_sound_folder_with_store_async<F>(
    folder: String,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "remove_sound_folder",
        move || remove_sound_folder_with_store(folder, library, projection),
        on_complete,
    )
}

pub fn refresh_sounds_with_store(
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
    coords: &LoudnessCoordinators,
) -> Result<RefreshSummary, CommandError> {
    refresh_sounds_with_store_cancellable(
        config,
        library,
        projection,
        coords,
        &AtomicBool::new(false),
    )
}

fn refresh_sounds_with_store_cancellable(
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
    coords: &LoudnessCoordinators,
    cancelled: &AtomicBool,
) -> Result<RefreshSummary, CommandError> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(CommandError::Library(
            "sound folder refresh was cancelled".to_string(),
        ));
    }
    let mut folders = Vec::new();
    let mut root_page = 0_usize;
    loop {
        let page = library
            .roots(root_page)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
        folders.extend(page.roots.into_iter().map(|root| root.path));
        if folders.len() >= page.total {
            break;
        }
        root_page = root_page.saturating_add(1);
    }
    let before = library
        .count(LibraryScope::General, "")
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;

    let mut roots = Vec::new();
    let mut seen_roots = HashSet::new();
    for folder in folders {
        if seen_roots.insert(folder.clone()) {
            roots.push(folder);
        }
    }

    let mut generations = BTreeMap::new();
    for (position, root) in roots.iter().enumerate() {
        match library.begin_root_scan(root, position).recv() {
            Ok(generation) => {
                generations.insert(root.clone(), generation);
            }
            Err(error) => {
                for (started_root, generation) in &generations {
                    let _ = library.cancel_root_scan(started_root, *generation).recv();
                }
                return Err(CommandError::Library(error.to_string()));
            }
        }
    }

    let mut batch = StoreScanBatch::default();
    let mut next_position = 0_usize;
    let mut scan_error = None;
    let metadata_threads = std::thread::available_parallelism()
        .map(|threads| threads.get().min(2))
        .unwrap_or(1);
    let metadata_pool = if metadata_threads > 1 {
        match rayon::ThreadPoolBuilder::new()
            .num_threads(metadata_threads)
            .thread_name(|index| format!("scan-metadata-{index}"))
            .build()
        {
            Ok(pool) => Some(pool),
            Err(error) => {
                log::warn!(
                    "Failed to start bounded scan metadata workers: {error}. Falling back to sequential metadata probing."
                );
                None
            }
        }
    } else {
        None
    };
    let mut pending_files = Vec::with_capacity(STORE_SCAN_METADATA_BATCH);
    let progress = {
        let mut process_pending = |pending: &mut Vec<scanner::AudioFile>| {
            let files = std::mem::take(pending);
            for (file, sound) in
                build_scanned_metadata_batch(files, cancelled, metadata_pool.as_ref())
            {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(CommandError::Library(
                        "sound folder refresh was cancelled".to_string(),
                    ));
                }
                let Some(&generation) = generations.get(&file.root_folder) else {
                    return Err(CommandError::Library(format!(
                        "scanner returned an unknown root: {}",
                        file.root_folder
                    )));
                };
                if batch.root != file.root_folder {
                    batch.flush(&library)?;
                    batch.reset_for(file.root_folder.clone(), generation);
                }
                batch.push_file(file, sound, next_position, &library)?;
                next_position = next_position.saturating_add(1);
            }
            Ok(())
        };
        let progress = scanner::visit_audio_files(&roots, cancelled, |file| {
            pending_files.push(file);
            if pending_files.len() < STORE_SCAN_METADATA_BATCH {
                return ControlFlow::Continue(());
            }
            if let Err(error) = process_pending(&mut pending_files) {
                scan_error = Some(error);
                return ControlFlow::Break(());
            }
            ControlFlow::Continue(())
        });
        if scan_error.is_none() {
            scan_error = process_pending(&mut pending_files).err();
        }
        progress
    };
    if scan_error.is_none() {
        scan_error = batch.flush(&library).err();
    }
    if progress.cancelled || progress.stopped_early || scan_error.is_some() {
        for (root, generation) in &generations {
            let _ = library.cancel_root_scan(root, *generation).recv();
        }
        return Err(scan_error.unwrap_or_else(|| {
            CommandError::Library("sound folder refresh was cancelled".to_string())
        }));
    }

    for (root, generation) in &generations {
        if !library
            .finish_root_scan(root, *generation)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?
        {
            return Err(CommandError::Library(format!(
                "sound folder refresh for '{root}' was superseded"
            )));
        }
    }

    let after = library
        .count(LibraryScope::General, "")
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)?;
    maybe_schedule_missing_loudness_backfill_with_store(&config, &library, coords);
    crate::ui_event_bridge::post_loudness_status_refresh();
    Ok(RefreshSummary {
        added: after.saturating_sub(before),
        removed: before.saturating_sub(after),
        refreshed: progress.files,
        ..RefreshSummary::default()
    })
}

pub fn refresh_sounds_with_store_async<F>(
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    projection: crate::hotkeys::HotkeyProjectionCoordinator,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<Arc<AtomicBool>, CommandError>
where
    F: FnOnce(Result<RefreshSummary, CommandError>) + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    dispatch_async_result(
        "refresh_sounds",
        move || {
            refresh_sounds_with_store_cancellable(
                config,
                library,
                projection,
                &coords,
                &worker_cancelled,
            )
        },
        on_complete,
    )?;
    Ok(cancelled)
}

pub fn import_files_to_tab_with_store(
    paths: Vec<String>,
    tab_id: Option<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    coords: &LoudnessCoordinators,
) -> Result<usize, CommandError> {
    let mut position = library
        .count(LibraryScope::General, "")
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    let mut imported = 0_usize;
    let mut membership_position = if let Some(tab_id) = tab_id.as_ref() {
        library
            .count(LibraryScope::ManualTab(tab_id.clone()), "")
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?
    } else {
        0
    };
    let mut accepted_paths = HashSet::new();
    let mut sounds = Vec::with_capacity(MAX_BATCH_ROWS / 2);
    let mut memberships = Vec::with_capacity(MAX_BATCH_ROWS / 2);

    let flush = |sounds: &mut Vec<SoundRecord>,
                 memberships: &mut Vec<crate::library_store::ManualMembershipRecord>|
     -> Result<(), CommandError> {
        if !sounds.is_empty() {
            library
                .apply_batch(LibraryBatch::Sounds(std::mem::take(sounds)))
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?;
        }
        if !memberships.is_empty() {
            library
                .apply_batch(LibraryBatch::ManualMemberships(std::mem::take(memberships)))
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?;
        }
        Ok(())
    };

    for path in paths {
        if !scanner::is_audio_file(&path)
            || !check_file_exists(&path)
            || !accepted_paths.insert(path.clone())
        {
            continue;
        }
        if library
            .sound_by_path(&path)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?
            .is_some()
        {
            continue;
        }
        let name = Path::new(&path)
            .file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Unknown".to_string());
        let sound = build_sound_with_metadata(name, path);
        if let Some(tab_id) = tab_id.as_ref() {
            memberships.push(crate::library_store::ManualMembershipRecord {
                tab_public_id: tab_id.clone(),
                sound_public_id: sound.id.clone(),
                position: membership_position,
            });
            membership_position = membership_position.saturating_add(1);
        }
        sounds.push(SoundRecord {
            sound,
            general_position: position,
            locations: Vec::new(),
        });
        imported = imported.saturating_add(1);
        position = position.saturating_add(1);
        if sounds.len() >= MAX_BATCH_ROWS / 2 {
            flush(&mut sounds, &mut memberships)?;
        }
    }
    flush(&mut sounds, &mut memberships)?;
    maybe_schedule_missing_loudness_backfill_with_store(&config, &library, coords);
    Ok(imported)
}

pub fn import_files_to_tab_with_store_async<F>(
    paths: Vec<String>,
    tab_id: Option<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<usize, CommandError>) + 'static,
{
    dispatch_async_result(
        "import_files_to_tab",
        move || import_files_to_tab_with_store(paths, tab_id, config, library, &coords),
        on_complete,
    )
}

pub fn update_sound_source_with_store(
    id: String,
    new_path: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    coords: &LoudnessCoordinators,
) -> Result<Sound, CommandError> {
    if !check_file_exists(&new_path) {
        return Err(CommandError::Invalid(
            "New file path does not exist".to_string(),
        ));
    }
    if !scanner::is_audio_file(&new_path) {
        return Err(CommandError::Invalid(
            ERR_UNSUPPORTED_AUDIO_FILE.to_string(),
        ));
    }
    let mut sound = library
        .sound_by_id(&id)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?
        .ok_or(CommandError::SoundNotFound)?;
    sound.path = new_path;
    sound.source_path = None;
    sound.duration_ms = probe_duration_ms(&sound.path);
    sound.loudness_source_fingerprint =
        compute_sound_source_fingerprint(&sound.path, sound.duration_ms);
    sound.loudness_lufs = None;
    sound.loudness_true_peak_dbtp = None;
    sound.loudness_analysis_state = LoudnessAnalysisState::Pending;
    sound.loudness_confidence = None;
    library
        .update_sound(sound.clone())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    maybe_schedule_missing_loudness_backfill_with_store(&config, &library, coords);
    Ok(sound)
}

pub fn update_sound_source_with_store_async<F>(
    id: String,
    new_path: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Sound, CommandError>) + 'static,
{
    dispatch_async_result(
        "update_sound_source",
        move || update_sound_source_with_store(id, new_path, config, library, &coords),
        on_complete,
    )
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn bounded_metadata_batch_preserves_scan_order_and_honours_cancellation() {
        let scanned_file = |name: &str| scanner::AudioFile {
            path: format!("/missing/{name}.wav"),
            name: name.to_string(),
            root_folder: "/missing".to_string(),
            relative_path: format!("{name}.wav"),
        };
        let files = ["first", "second", "third"]
            .into_iter()
            .map(scanned_file)
            .collect();
        let cancelled = AtomicBool::new(false);

        let built = build_scanned_metadata_batch(files, &cancelled, None);
        assert_eq!(
            built
                .iter()
                .map(|(_, sound)| sound.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );

        cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            build_scanned_metadata_batch(vec![scanned_file("cancelled")], &cancelled, None)
                .is_empty()
        );
    }
}
