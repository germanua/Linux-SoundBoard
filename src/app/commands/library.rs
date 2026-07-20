use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;

use crate::audio::file_link::{
    check_file_exists, validate_sounds_batch_with_report, validate_sounds_chunked_with_report,
    ValidationMode, ValidationReport, STARTUP_VALIDATION_CHUNK_SIZE,
};
use crate::audio::scanner;
use crate::config::{Config, FolderTabBinding, LoudnessAnalysisState, Sound};
use crate::hotkeys::HotkeyManager;

use super::shared::{
    adaptive_audio_analysis_plan, build_sound_with_metadata, compute_sound_source_fingerprint,
    default_sound_import_dir, dispatch_async_result, probe_duration_ms,
    unregister_hotkeys_best_effort, with_config, with_config_mut, with_saved_config,
    ERR_FILE_DOES_NOT_EXIST, ERR_SOUND_ALREADY_EXISTS, ERR_UNSUPPORTED_AUDIO_FILE,
};
use super::{CommandError, LoudnessCoordinators};

fn maybe_schedule_missing_loudness_backfill(
    config: &Arc<Mutex<Config>>,
    coords: &LoudnessCoordinators,
) {
    match crate::commands::trigger_missing_loudness_analysis(
        Arc::clone(config),
        false,
        None,
        coords,
    ) {
        Ok(crate::commands::MissingLoudnessAnalysisTrigger::Started) => {
            log::debug!("Scheduled background loudness backfill after library update");
        }
        Ok(_) => {}
        Err(err) => {
            log::warn!("Failed to schedule background loudness backfill: {}", err);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FingerprintRefreshOutcome {
    changed: bool,
    invalidated: bool,
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

#[derive(Debug)]
struct NewSound {
    root_folder: String,
    sound: Sound,
}

#[derive(Debug)]
struct FingerprintUpdate {
    sound: Sound,
    invalidated: bool,
}

#[derive(Debug)]
struct GeneratedTabPlan {
    binding: FolderTabBinding,
    default_name: String,
    sound_paths: Vec<String>,
}

#[derive(Debug, Default)]
struct TabReconcileSummary {
    changed: bool,
    created: usize,
    removed: usize,
    memberships_added: usize,
}

fn refresh_existing_sound_source_fingerprint(sound: &mut Sound) -> FingerprintRefreshOutcome {
    let current_fingerprint = compute_sound_source_fingerprint(&sound.path, sound.duration_ms);

    let Some(mut fingerprint) = current_fingerprint else {
        return FingerprintRefreshOutcome {
            changed: false,
            invalidated: false,
        };
    };

    if sound.loudness_source_fingerprint.is_none() {
        sound.loudness_source_fingerprint = Some(fingerprint);
        return FingerprintRefreshOutcome {
            changed: true,
            invalidated: false,
        };
    }

    if sound.loudness_source_fingerprint.as_deref() == Some(fingerprint.as_str()) {
        return FingerprintRefreshOutcome {
            changed: false,
            invalidated: false,
        };
    }

    let refreshed_duration = probe_duration_ms(&sound.path);
    if sound.duration_ms != refreshed_duration {
        sound.duration_ms = refreshed_duration;
    }

    if let Some(recomputed) = compute_sound_source_fingerprint(&sound.path, sound.duration_ms) {
        fingerprint = recomputed;
    }

    sound.loudness_source_fingerprint = Some(fingerprint);
    sound.loudness_lufs = None;
    sound.loudness_true_peak_dbtp = None;
    sound.loudness_analysis_state = LoudnessAnalysisState::Pending;
    sound.loudness_confidence = None;
    FingerprintRefreshOutcome {
        changed: true,
        invalidated: true,
    }
}

fn effective_source_path(sound: &Sound) -> &str {
    sound.source_path.as_deref().unwrap_or(&sound.path)
}

fn insert_unique_sounds(config: &mut Config, candidates: Vec<Sound>) -> Vec<Sound> {
    let mut existing_paths = config
        .sounds
        .iter()
        .map(|sound| sound.path.clone())
        .collect::<HashSet<_>>();
    let inserted = candidates
        .into_iter()
        .filter(|sound| existing_paths.insert(sound.path.clone()))
        .collect::<Vec<_>>();
    config.sounds.extend(inserted.iter().cloned());
    inserted
}

fn build_generated_tab_plans(scan: &scanner::AudioScan) -> Vec<GeneratedTabPlan> {
    let mut paths_by_binding = BTreeMap::<FolderTabBinding, Vec<String>>::new();
    for subfolder in &scan.subfolders {
        paths_by_binding
            .entry(FolderTabBinding {
                root_folder: subfolder.root_folder.clone(),
                relative_subfolder: subfolder.relative_subfolder.clone(),
            })
            .or_default();
    }
    for file in &scan.files {
        let Some(relative_subfolder) = &file.top_level_subfolder else {
            continue;
        };
        paths_by_binding
            .entry(FolderTabBinding {
                root_folder: file.root_folder.clone(),
                relative_subfolder: relative_subfolder.clone(),
            })
            .or_default()
            .push(file.path.clone());
    }

    let mut name_counts = HashMap::<String, usize>::new();
    for binding in paths_by_binding.keys() {
        *name_counts
            .entry(binding.relative_subfolder.clone())
            .or_default() += 1;
    }

    paths_by_binding
        .into_iter()
        .map(|(binding, mut sound_paths)| {
            sound_paths.sort();
            sound_paths.dedup();
            let default_name = if name_counts
                .get(&binding.relative_subfolder)
                .copied()
                .unwrap_or_default()
                > 1
            {
                let root_name = Path::new(&binding.root_folder)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| binding.root_folder.clone());
                format!("{} ({root_name})", binding.relative_subfolder)
            } else {
                binding.relative_subfolder.clone()
            };
            GeneratedTabPlan {
                binding,
                default_name,
                sound_paths,
            }
        })
        .collect()
}

fn path_belongs_to_binding(path: &str, binding: &FolderTabBinding) -> bool {
    let Ok(relative) = Path::new(path).strip_prefix(&binding.root_folder) else {
        return false;
    };
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return false;
    };
    components.next().is_some() && first.as_os_str().to_string_lossy() == binding.relative_subfolder
}

fn reconcile_generated_tabs(
    config: &mut Config,
    plans: &[GeneratedTabPlan],
) -> TabReconcileSummary {
    let mut summary = TabReconcileSummary::default();
    let active_bindings = plans
        .iter()
        .map(|plan| plan.binding.clone())
        .collect::<HashSet<_>>();

    config.tabs.retain_mut(|tab| {
        let Some(binding) = tab.folder_binding.as_ref() else {
            return true;
        };
        if active_bindings.contains(binding) {
            return true;
        }
        summary.changed = true;
        if tab.sound_ids.is_empty() {
            summary.removed += 1;
            false
        } else {
            tab.folder_binding = None;
            true
        }
    });

    let sound_paths = config
        .sounds
        .iter()
        .map(|sound| (sound.id.clone(), sound.path.clone()))
        .collect::<HashMap<_, _>>();
    let sound_ids_by_path = sound_paths
        .iter()
        .map(|(id, path)| (path.clone(), id.clone()))
        .collect::<HashMap<_, _>>();
    let mut next_order = config
        .tabs
        .iter()
        .map(|tab| tab.order)
        .max()
        .unwrap_or(0)
        .saturating_add(1);

    for plan in plans {
        let desired_ids = plan
            .sound_paths
            .iter()
            .filter_map(|path| sound_ids_by_path.get(path).cloned())
            .collect::<Vec<_>>();
        let desired_set = desired_ids.iter().cloned().collect::<HashSet<_>>();

        let tab_index = config
            .tabs
            .iter()
            .position(|tab| tab.folder_binding.as_ref() == Some(&plan.binding));
        let tab = if let Some(index) = tab_index {
            &mut config.tabs[index]
        } else {
            let mut tab = crate::config::SoundTab::new(plan.default_name.clone(), next_order);
            next_order = next_order.saturating_add(1);
            tab.folder_binding = Some(plan.binding.clone());
            let index = config.tabs.len();
            config.tabs.push(tab);
            summary.changed = true;
            summary.created += 1;
            &mut config.tabs[index]
        };

        let before = tab.sound_ids.len();
        tab.sound_ids.retain(|id| {
            sound_paths.get(id).is_some_and(|path| {
                !path_belongs_to_binding(path, &plan.binding) || desired_set.contains(id)
            })
        });
        if tab.sound_ids.len() != before {
            summary.changed = true;
        }
        let mut existing_ids = tab.sound_ids.iter().cloned().collect::<HashSet<_>>();
        for id in desired_ids {
            if existing_ids.insert(id.clone()) {
                tab.sound_ids.push(id);
                summary.changed = true;
                summary.memberships_added += 1;
            }
        }
    }

    summary
}

fn detach_or_remove_tabs_for_root(config: &mut Config, root_folder: &str) {
    config.tabs.retain_mut(|tab| {
        let belongs_to_root = tab
            .folder_binding
            .as_ref()
            .is_some_and(|binding| binding.root_folder == root_folder);
        if !belongs_to_root {
            return true;
        }
        if tab.sound_ids.is_empty() {
            false
        } else {
            tab.folder_binding = None;
            true
        }
    });
}

fn apply_fingerprint_update(current: &mut Sound, refreshed: &Sound) -> bool {
    let changed = current.duration_ms != refreshed.duration_ms
        || current.loudness_source_fingerprint != refreshed.loudness_source_fingerprint
        || current.loudness_lufs != refreshed.loudness_lufs
        || current.loudness_true_peak_dbtp != refreshed.loudness_true_peak_dbtp
        || current.loudness_analysis_state != refreshed.loudness_analysis_state
        || current.loudness_confidence != refreshed.loudness_confidence;
    if changed {
        current.duration_ms = refreshed.duration_ms;
        current.loudness_source_fingerprint = refreshed.loudness_source_fingerprint.clone();
        current.loudness_lufs = refreshed.loudness_lufs;
        current.loudness_true_peak_dbtp = refreshed.loudness_true_peak_dbtp;
        current.loudness_analysis_state = refreshed.loudness_analysis_state;
        current.loudness_confidence = refreshed.loudness_confidence;
    }
    changed
}

pub fn add_sound(
    name: String,
    path: String,
    config: Arc<Mutex<Config>>,
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

    let duplicate = with_config(&config, |cfg| cfg.sounds.iter().any(|s| s.path == path))?;
    if duplicate {
        return Err(CommandError::Invalid(ERR_SOUND_ALREADY_EXISTS.to_string()));
    }

    let sound = build_sound_with_metadata(name, path);
    let sound_clone = sound.clone();
    with_config_mut(&config, move |cfg| {
        cfg.add_sound(sound);
        cfg.save().map_err(CommandError::config_save)
    })??;
    maybe_schedule_missing_loudness_backfill(&config, coords);
    Ok(sound_clone)
}

pub fn rename_sound(
    id: String,
    name: String,
    config: Arc<Mutex<Config>>,
) -> Result<Sound, CommandError> {
    let new_name = name.trim().to_string();
    if new_name.is_empty() {
        return Err(CommandError::Invalid("Name cannot be empty".to_string()));
    }
    let sound = with_config_mut(&config, |cfg| {
        if cfg.get_sound(&id).is_none() {
            return Err(CommandError::SoundNotFound);
        }
        cfg.set_sound_name(&id, new_name);
        cfg.save().map_err(CommandError::config_save)?;
        cfg.get_sound(&id)
            .cloned()
            .ok_or(CommandError::SoundNotFound)
    })??;
    Ok(sound)
}

pub fn remove_sound(
    id: String,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), CommandError> {
    remove_sounds(vec![id], config, hotkeys)
}

#[derive(Debug, Default)]
struct SoundRemovalPlan {
    existing_ids: Vec<String>,
    hotkey_ids: Vec<String>,
}

fn build_sound_removal_plan(ids: &[String], config: &Config) -> SoundRemovalPlan {
    if ids.is_empty() {
        return SoundRemovalPlan::default();
    }

    let requested_ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut existing_ids = Vec::new();
    let mut hotkey_ids = Vec::new();
    for sound in &config.sounds {
        if !requested_ids.contains(sound.id.as_str()) {
            continue;
        }

        existing_ids.push(sound.id.clone());
        if sound.hotkey.is_some() {
            hotkey_ids.push(sound.id.clone());
        }
    }

    SoundRemovalPlan {
        existing_ids,
        hotkey_ids,
    }
}

pub fn remove_sounds(
    ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), CommandError> {
    let plan = with_config_mut(&config, |cfg| {
        let plan = build_sound_removal_plan(&ids, cfg);
        if plan.existing_ids.is_empty() {
            return Ok(plan);
        }

        let mut candidate = cfg.clone();
        candidate.remove_sounds(&plan.existing_ids);
        candidate.save().map_err(CommandError::config_save)?;
        *cfg = candidate;
        Ok(plan)
    })??;
    if plan.existing_ids.is_empty() {
        return Ok(());
    }

    unregister_hotkeys_best_effort(&hotkeys, &plan.hotkey_ids, "remove_sounds");
    Ok(())
}

pub fn remove_sounds_async<F>(
    ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "remove_sounds",
        move || remove_sounds(ids, config, hotkeys),
        on_complete,
    )
}

pub fn add_sound_folder(folder: String, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    if !Path::new(&folder).is_dir() {
        return Err(CommandError::Invalid("Folder does not exist".to_string()));
    }
    with_saved_config(&config, |cfg| {
        cfg.add_sound_folder(folder);
    })
}

pub fn add_sound_folder_async<F>(
    folder: String,
    config: Arc<Mutex<Config>>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "add_sound_folder",
        move || add_sound_folder(folder, config),
        on_complete,
    )
}

pub fn remove_sound_folder(
    folder: String,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), CommandError> {
    let folder_path = Path::new(&folder);
    let (sounds_to_remove, hotkey_ids) = with_config_mut(&config, |cfg| {
        let sounds_to_remove = cfg
            .sounds
            .iter()
            .filter(|sound| Path::new(effective_source_path(sound)).starts_with(folder_path))
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        let hotkey_ids = cfg
            .sounds
            .iter()
            .filter(|sound| sounds_to_remove.contains(&sound.id) && sound.hotkey.is_some())
            .map(|sound| sound.id.clone())
            .collect::<Vec<_>>();

        let mut candidate = cfg.clone();
        candidate.remove_sounds(&sounds_to_remove);
        detach_or_remove_tabs_for_root(&mut candidate, &folder);
        candidate.remove_sound_folder(&folder);
        candidate.save().map_err(CommandError::config_save)?;
        *cfg = candidate;
        Ok((sounds_to_remove, hotkey_ids))
    })??;

    log::info!(
        "Removing {} sounds from folder: {}",
        sounds_to_remove.len(),
        folder
    );

    unregister_hotkeys_best_effort(&hotkeys, &hotkey_ids, "remove_sound_folder");
    Ok(())
}

pub fn remove_sound_folder_async<F>(
    folder: String,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "remove_sound_folder",
        move || remove_sound_folder(folder, config, hotkeys),
        on_complete,
    )
}

pub fn refresh_sounds(
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    coords: &LoudnessCoordinators,
) -> Result<RefreshSummary, CommandError> {
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:start");
    crate::diagnostics::record_phase_with_config("refresh_sounds:start", &config.lock());
    #[derive(Debug)]
    struct RefreshWork {
        new_sounds: Vec<NewSound>,
        removed_sounds: Vec<(String, String)>,
        fingerprint_updates: Vec<FingerprintUpdate>,
        tab_plans: Vec<GeneratedTabPlan>,
    }

    let (folders, existing_paths, known_sounds) = with_config(&config, |cfg| {
        let folders = cfg.sound_folders.clone();
        let existing_paths = cfg
            .sounds
            .iter()
            .map(|s| s.path.clone())
            .collect::<HashSet<_>>();
        let known_sounds = cfg.sounds.clone();
        (folders, existing_paths, known_sounds)
    })?;

    let work: RefreshWork = {
        let scan = scanner::scan_folders(&folders);

        let new_files = scan
            .files
            .iter()
            .filter(|f| !existing_paths.contains(&f.path))
            .cloned()
            .collect::<Vec<_>>();

        let build_sound = |file: &scanner::AudioFile| NewSound {
            root_folder: file.root_folder.clone(),
            sound: build_sound_with_metadata(file.name.clone(), file.path.clone()),
        };
        let analysis_plan = adaptive_audio_analysis_plan(new_files.len());
        let analysis_threads = analysis_plan.threads;
        let pool_threads = if new_files.is_empty() {
            1
        } else {
            analysis_threads
        };
        if analysis_plan.throttled {
            log::info!(
                "Adaptive refresh metadata throttling applied: threads={} base={} rss={}kB process_threads={}",
                analysis_plan.threads,
                analysis_plan.base_threads,
                analysis_plan.rss_kb.unwrap_or(0),
                analysis_plan.process_threads.unwrap_or(0)
            );
        }
        crate::diagnostics::set_work_runtime("refresh_metadata", new_files.len(), pool_threads);
        crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:before_metadata_pool");
        crate::diagnostics::record_phase_with_config(
            "refresh_sounds:before_metadata_pool",
            &config.lock(),
        );
        let new_sounds: Vec<NewSound> = if new_files.is_empty() {
            Vec::new()
        } else {
            match rayon::ThreadPoolBuilder::new()
                .num_threads(analysis_threads)
                .build()
            {
                Ok(pool) => pool.install(|| new_files.par_iter().map(build_sound).collect()),
                Err(e) => {
                    log::warn!(
                        "Failed to build bounded refresh-analysis pool ({} threads): {}. Falling back to sequential metadata build.",
                        analysis_threads,
                        e
                    );
                    new_files.iter().map(build_sound).collect()
                }
            }
        };
        let removed_sounds = known_sounds
            .iter()
            .filter(|sound| !Path::new(&sound.path).exists())
            .map(|sound| (sound.id.clone(), sound.path.clone()))
            .collect::<Vec<_>>();
        let removed_ids = removed_sounds
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<HashSet<_>>();
        let fingerprint_updates = known_sounds
            .into_iter()
            .filter(|sound| !removed_ids.contains(sound.id.as_str()))
            .filter_map(|mut sound| {
                let refresh = refresh_existing_sound_source_fingerprint(&mut sound);
                refresh.changed.then_some(FingerprintUpdate {
                    sound,
                    invalidated: refresh.invalidated,
                })
            })
            .collect();

        RefreshWork {
            new_sounds,
            removed_sounds,
            fingerprint_updates,
            tab_plans: build_generated_tab_plans(&scan),
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:after_metadata_pool");
    crate::diagnostics::record_phase_with_config(
        "refresh_sounds:after_metadata_pool",
        &config.lock(),
    );

    let mut cfg = config.lock();
    let active_roots = cfg.sound_folders.iter().cloned().collect::<HashSet<_>>();
    let removed_ids = work
        .removed_sounds
        .iter()
        .filter_map(|(id, path)| {
            cfg.get_sound(id)
                .is_some_and(|sound| sound.path == *path && !Path::new(path).exists())
                .then_some(id.clone())
        })
        .collect::<Vec<_>>();
    cfg.remove_sounds(&removed_ids);

    let mut refreshed_existing = 0;
    let mut invalidated_existing = 0;
    for update in &work.fingerprint_updates {
        let Some(current) = cfg.get_sound_mut(&update.sound.id) else {
            continue;
        };
        if current.path != update.sound.path {
            continue;
        }
        if apply_fingerprint_update(current, &update.sound) {
            refreshed_existing += 1;
            if update.invalidated {
                invalidated_existing += 1;
            }
        }
    }

    let mut added_count = 0;
    let mut current_paths = cfg
        .sounds
        .iter()
        .map(|sound| sound.path.clone())
        .collect::<HashSet<_>>();
    for NewSound { root_folder, sound } in work.new_sounds {
        if active_roots.contains(&root_folder)
            && Path::new(&sound.path).exists()
            && current_paths.insert(sound.path.clone())
        {
            cfg.sounds.push(sound);
            added_count += 1;
        }
    }

    let active_tab_plans = work
        .tab_plans
        .into_iter()
        .filter(|plan| active_roots.contains(&plan.binding.root_folder))
        .collect::<Vec<_>>();
    let tabs = reconcile_generated_tabs(&mut cfg, &active_tab_plans);
    let removed_count = removed_ids.len();
    let summary = RefreshSummary {
        added: added_count,
        removed: removed_count,
        refreshed: refreshed_existing,
        invalidated: invalidated_existing,
        tabs_created: tabs.created,
        tabs_removed: tabs.removed,
        tab_memberships_added: tabs.memberships_added,
    };

    let should_schedule_backfill = added_count > 0 || invalidated_existing > 0;
    let changed = added_count > 0 || removed_count > 0 || refreshed_existing > 0 || tabs.changed;
    if changed {
        if let Err(err) = cfg.save() {
            crate::diagnostics::clear_work_runtime();
            return Err(CommandError::config_save(err));
        }
    }
    crate::diagnostics::record_phase_with_config("library:refresh_complete", &cfg);
    crate::diagnostics::clear_work_runtime();
    drop(cfg);

    unregister_hotkeys_best_effort(&hotkeys, &removed_ids, "refresh_sounds");
    if should_schedule_backfill {
        maybe_schedule_missing_loudness_backfill(&config, coords);
    }
    if invalidated_existing > 0 {
        log::info!(
            "Refresh invalidated loudness metadata for {} sound(s) due to source fingerprint drift",
            invalidated_existing
        );
    }
    crate::diagnostics::memory::log_memory_snapshot(if changed {
        "refresh_sounds:end:saved"
    } else {
        "refresh_sounds:end:no_changes"
    });
    Ok(summary)
}

pub fn refresh_sounds_async<F>(
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<RefreshSummary, CommandError>) + 'static,
{
    dispatch_async_result(
        "refresh_sounds",
        move || refresh_sounds(config, hotkeys, &coords),
        on_complete,
    )
}

pub fn import_dropped_files(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
    coords: &LoudnessCoordinators,
) -> Result<Vec<Sound>, CommandError> {
    let (target_folder, mut existing_paths, added_default_folder): (String, HashSet<String>, bool) =
        with_config(&config, |cfg| {
            let added_default_folder = cfg.sound_folders.is_empty();
            let existing = cfg
                .sounds
                .iter()
                .map(|s| s.path.clone())
                .collect::<HashSet<_>>();

            let target_folder = if cfg.sound_folders.is_empty() {
                let default_folder = default_sound_import_dir(dirs::audio_dir(), dirs::home_dir())
                    .to_string_lossy()
                    .to_string();
                default_folder
            } else {
                cfg.sound_folders[0].clone()
            };

            (target_folder, existing, added_default_folder)
        })?;

    let mut imported = Vec::new();

    fs::create_dir_all(&target_folder).map_err(|e| {
        CommandError::Io(format!(
            "Failed to create target folder '{target_folder}': {e}"
        ))
    })?;

    for path in paths {
        if !scanner::is_audio_file(&path) || !Path::new(&path).exists() {
            continue;
        }

        let source = Path::new(&path);
        let Some(filename) = source.file_name() else {
            continue;
        };

        let dest = Path::new(&target_folder).join(filename);
        let dest_str = dest.to_string_lossy().to_string();

        if existing_paths.contains(&dest_str) {
            continue;
        }

        if fs::copy(source, &dest).is_ok() {
            existing_paths.insert(dest_str.clone());
            let name = dest
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".to_string());
            imported.push(build_sound_with_metadata(name, dest_str));
        }
    }

    if imported.is_empty() {
        if added_default_folder {
            with_config_mut(&config, |cfg| {
                cfg.add_sound_folder(target_folder.clone());
                cfg.save().map_err(CommandError::config_save)
            })??;
        }
        return Ok(imported);
    }

    let imported = with_config_mut(&config, move |cfg| {
        let inserted = insert_unique_sounds(cfg, imported);
        if inserted.is_empty() {
            return Ok(inserted);
        }
        cfg.save().map_err(CommandError::config_save)?;
        Ok(inserted)
    })??;
    if !imported.is_empty() {
        maybe_schedule_missing_loudness_backfill(&config, coords);
    }
    Ok(imported)
}

pub fn import_files_as_links(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
    coords: &LoudnessCoordinators,
) -> Result<Vec<Sound>, CommandError> {
    import_files_to_tab(paths, None, config, coords)
}

pub fn import_files_to_tab(
    paths: Vec<String>,
    tab_id: Option<String>,
    config: Arc<Mutex<Config>>,
    coords: &LoudnessCoordinators,
) -> Result<Vec<Sound>, CommandError> {
    let mut existing_paths: HashSet<String> = with_config(&config, |cfg| {
        cfg.sounds.iter().map(|s| s.path.clone()).collect()
    })?;

    let mut new_sounds = Vec::new();

    for path in paths {
        if !scanner::is_audio_file(&path) {
            continue;
        }
        if !check_file_exists(&path) {
            continue;
        }
        if !existing_paths.insert(path.clone()) {
            continue;
        }

        let name = Path::new(&path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Unknown")
            .to_string();

        new_sounds.push(build_sound_with_metadata(name, path));
    }

    if new_sounds.is_empty() {
        return Ok(new_sounds);
    }

    let new_sounds = with_config_mut(&config, move |cfg| {
        let inserted = insert_unique_sounds(cfg, new_sounds);
        if inserted.is_empty() {
            return Ok(inserted);
        }

        if let Some(tab_id) = tab_id.as_deref() {
            cfg.add_sounds_to_tab(
                tab_id,
                inserted.iter().map(|sound| sound.id.clone()).collect(),
            );
        }

        cfg.save().map_err(CommandError::config_save)?;
        Ok(inserted)
    })??;

    if !new_sounds.is_empty() {
        maybe_schedule_missing_loudness_backfill(&config, coords);
    }

    Ok(new_sounds)
}

pub fn import_files_to_tab_async<F>(
    paths: Vec<String>,
    tab_id: Option<String>,
    config: Arc<Mutex<Config>>,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Vec<Sound>, CommandError>) + 'static,
{
    dispatch_async_result(
        "import_files_to_tab",
        move || import_files_to_tab(paths, tab_id, config, &coords),
        on_complete,
    )
}

pub fn validate_all_sources(config: Arc<Mutex<Config>>) -> Result<Vec<String>, CommandError> {
    let sounds = source_validation_inputs(config)?;
    let report = validate_sounds_batch_with_report(&sounds);
    crate::diagnostics::set_validation_runtime(
        report.input_count,
        match report.mode {
            ValidationMode::Sequential => "sequential",
            ValidationMode::ParallelPool => "bounded_parallel",
        },
        report.worker_threads,
    );

    Ok(report.missing_ids)
}

pub fn validate_all_sources_chunked(
    config: Arc<Mutex<Config>>,
    chunk_size: usize,
) -> Result<Vec<String>, CommandError> {
    let sounds = source_validation_inputs(config)?;

    let report = crate::audio::file_link::validate_sounds_chunked_with_report(&sounds, chunk_size);
    crate::diagnostics::set_validation_runtime(
        report.input_count,
        match report.mode {
            ValidationMode::Sequential => "sequential",
            ValidationMode::ParallelPool => "bounded_parallel",
        },
        report.worker_threads,
    );

    Ok(report.missing_ids)
}

pub fn source_validation_inputs(
    config: Arc<Mutex<Config>>,
) -> Result<Vec<(String, Option<String>, String)>, CommandError> {
    with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .map(|s| (s.id.clone(), s.source_path.clone(), s.path.clone()))
            .collect()
    })
}

pub fn validate_sources_for_startup(
    sounds: &[(String, Option<String>, String)],
) -> ValidationReport {
    validate_sounds_chunked_with_report(sounds, STARTUP_VALIDATION_CHUNK_SIZE)
}

pub fn validate_single_source(
    id: String,
    config: Arc<Mutex<Config>>,
) -> Result<bool, CommandError> {
    let exists = with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .find(|s| s.id == id)
            .map(|s| check_file_exists(effective_source_path(s)))
    })?;

    exists.ok_or(CommandError::SoundNotFound)
}

pub fn update_sound_source(
    id: String,
    new_path: String,
    config: Arc<Mutex<Config>>,
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

    let updated_sound = with_config_mut(&config, |cfg| {
        let sound = cfg.sounds.iter_mut().find(|s| s.id == id);

        match sound {
            Some(s) => {
                s.path = new_path;
                s.source_path = None;
                s.duration_ms = probe_duration_ms(&s.path);
                s.loudness_source_fingerprint =
                    compute_sound_source_fingerprint(&s.path, s.duration_ms);
                s.loudness_lufs = None;
                s.loudness_true_peak_dbtp = None;
                s.loudness_analysis_state = crate::config::LoudnessAnalysisState::Pending;
                s.loudness_confidence = None;
                let updated_sound = s.clone();
                cfg.save().map_err(CommandError::config_save)?;
                Ok(updated_sound)
            }
            None => Err(CommandError::SoundNotFound),
        }
    })??;

    maybe_schedule_missing_loudness_backfill(&config, coords);
    Ok(updated_sound)
}

pub fn update_sound_source_async<F>(
    id: String,
    new_path: String,
    config: Arc<Mutex<Config>>,
    coords: LoudnessCoordinators,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Sound, CommandError>) + 'static,
{
    dispatch_async_result(
        "update_sound_source",
        move || update_sound_source(id, new_path, config, &coords),
        on_complete,
    )
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    #[test]
    fn batch_insert_returns_only_sounds_added_under_final_lock() {
        let mut config = Config::default();
        let existing = Sound::new("Existing".to_string(), "/tmp/existing.wav".to_string());
        config.sounds.push(existing);
        let duplicate = Sound::new("Duplicate".to_string(), "/tmp/existing.wav".to_string());
        let added = Sound::new("Added".to_string(), "/tmp/added.wav".to_string());
        let added_id = added.id.clone();

        let inserted = insert_unique_sounds(&mut config, vec![duplicate, added]);

        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].id, added_id);
        assert_eq!(config.sounds.len(), 2);
        assert_eq!(config.sounds[1].id, added_id);
    }
}
