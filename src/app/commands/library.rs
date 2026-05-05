use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::audio::file_link::{
    check_file_exists, validate_sounds_batch_with_report, validate_sounds_chunked_with_report,
    ValidationMode, ValidationReport, STARTUP_VALIDATION_CHUNK_SIZE,
};
use crate::audio::scanner;
use crate::config::{Config, LoudnessAnalysisState, Sound, SoundTab};
use crate::hotkeys::HotkeyManager;

use super::shared::{
    adaptive_audio_analysis_plan, build_sound_with_metadata, compute_sound_source_fingerprint,
    default_sound_import_dir, probe_duration_ms, unregister_hotkeys_best_effort, with_config,
    with_config_mut, with_saved_config, ERR_FILE_DOES_NOT_EXIST, ERR_SOUND_ALREADY_EXISTS,
    ERR_SOUND_NOT_FOUND, ERR_UNSUPPORTED_AUDIO_FILE,
};

fn maybe_schedule_missing_loudness_backfill(config: &Arc<Mutex<Config>>) {
    match crate::commands::trigger_missing_loudness_analysis(Arc::clone(config), false, None) {
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

pub fn add_sound(name: String, path: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    if !Path::new(&path).exists() {
        return Err(ERR_FILE_DOES_NOT_EXIST.to_string());
    }
    if !scanner::is_audio_file(&path) {
        return Err(ERR_UNSUPPORTED_AUDIO_FILE.to_string());
    }

    let duplicate = with_config(&config, |cfg| cfg.sounds.iter().any(|s| s.path == path))?;
    if duplicate {
        return Err(ERR_SOUND_ALREADY_EXISTS.to_string());
    }

    let sound = build_sound_with_metadata(name, path);
    let sound_clone = sound.clone();
    with_config_mut(&config, move |cfg| {
        cfg.add_sound(sound);
        cfg.save().map_err(|e| e.to_string())
    })??;
    maybe_schedule_missing_loudness_backfill(&config);
    Ok(sound_clone)
}

pub fn rename_sound(id: String, name: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    let new_name = name.trim().to_string();
    if new_name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    let sound = with_config_mut(&config, |cfg| {
        if cfg.get_sound(&id).is_none() {
            return Err(ERR_SOUND_NOT_FOUND.to_string());
        }
        cfg.set_sound_name(&id, new_name);
        cfg.save().map_err(|e| e.to_string())?;
        cfg.get_sound(&id)
            .cloned()
            .ok_or_else(|| ERR_SOUND_NOT_FOUND.to_string())
    })??;
    Ok(sound)
}

pub fn remove_sound(
    id: String,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), String> {
    remove_sounds(vec![id], config, hotkeys)
}

#[derive(Debug, Default)]
struct SoundRemovalPlan {
    existing_ids: Vec<String>,
    hotkey_ids: Vec<String>,
}

fn build_sound_removal_plan(
    ids: &[String],
    config: &Arc<Mutex<Config>>,
) -> Result<SoundRemovalPlan, String> {
    if ids.is_empty() {
        return Ok(SoundRemovalPlan::default());
    }

    let requested_ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    with_config(config, |cfg| {
        let mut existing_ids = Vec::new();
        let mut hotkey_ids = Vec::new();
        for sound in &cfg.sounds {
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
    })
}

pub fn remove_sounds(
    ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), String> {
    let plan = build_sound_removal_plan(&ids, &config)?;
    if plan.existing_ids.is_empty() {
        return Ok(());
    }

    unregister_hotkeys_best_effort(&hotkeys, &plan.hotkey_ids, "remove_sounds");

    with_config_mut(&config, |cfg| {
        cfg.remove_sounds(&plan.existing_ids);
        cfg.save().map_err(|e| e.to_string())
    })?
}

pub fn add_sound_folder(folder: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    if !Path::new(&folder).is_dir() {
        return Err("Folder does not exist".to_string());
    }
    with_saved_config(&config, |cfg| {
        cfg.add_sound_folder(folder);
    })
}

pub fn remove_sound_folder(
    folder: String,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), String> {
    let folder_path = Path::new(&folder);

    log::info!(
        "Cancelling loudness analysis before removing folder: {}",
        folder
    );
    crate::commands::cancel_loudness_analysis();

    let sounds_to_remove: Vec<String> = with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .filter(|sound| Path::new(effective_source_path(sound)).starts_with(folder_path))
            .map(|s| s.id.clone())
            .collect()
    })?;

    log::info!(
        "Removing {} sounds from folder: {}",
        sounds_to_remove.len(),
        folder
    );

    unregister_hotkeys_best_effort(&hotkeys, &sounds_to_remove, "remove_sound_folder");

    with_saved_config(&config, |cfg| {
        cfg.remove_sound_folder(&folder);
        cfg.remove_sounds(&sounds_to_remove);
    })
}

pub fn refresh_sounds(
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<Vec<Sound>, String> {
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:start");
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("refresh_sounds:start", &cfg);
    } else {
        crate::diagnostics::record_phase("refresh_sounds:start", None);
    }
    #[derive(Debug)]
    struct RefreshWork {
        new_sounds: Vec<Sound>,
        removed_ids: Vec<String>,
        removed_paths: HashSet<String>,
    }

    let (folders, existing_paths, known_sounds) = with_config(&config, |cfg| {
        let folders = cfg.sound_folders.clone();
        let existing_paths = cfg
            .sounds
            .iter()
            .map(|s| s.path.clone())
            .collect::<HashSet<_>>();
        let known_sounds = cfg
            .sounds
            .iter()
            .map(|s| (s.id.clone(), s.path.clone()))
            .collect::<Vec<_>>();
        (folders, existing_paths, known_sounds)
    })?;

    let work: RefreshWork = {
        let files = scanner::scan_folders(&folders);

        let mut seen_new_paths = HashSet::new();
        let new_files = files
            .into_iter()
            .filter(|f| !existing_paths.contains(&f.path))
            .filter(|f| seen_new_paths.insert(f.path.clone()))
            .collect::<Vec<_>>();

        let build_sound = |file: &scanner::AudioFile| {
            build_sound_with_metadata(file.name.clone(), file.path.clone())
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
        if let Ok(cfg) = config.lock() {
            crate::diagnostics::record_phase_with_config(
                "refresh_sounds:before_metadata_pool",
                &cfg,
            );
        } else {
            crate::diagnostics::record_phase("refresh_sounds:before_metadata_pool", None);
        }
        let new_sounds: Vec<Sound> = if new_files.is_empty() {
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
        let removed: Vec<(String, String)> = known_sounds
            .iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .cloned()
            .collect();

        RefreshWork {
            removed_ids: removed.iter().map(|(id, _)| id.clone()).collect(),
            removed_paths: removed.into_iter().map(|(_, path)| path).collect(),
            new_sounds,
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:after_metadata_pool");
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("refresh_sounds:after_metadata_pool", &cfg);
    } else {
        crate::diagnostics::record_phase("refresh_sounds:after_metadata_pool", None);
    }

    unregister_hotkeys_best_effort(&hotkeys, &work.removed_ids, "refresh_sounds");

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let mut refreshed_existing = 0usize;
    let mut invalidated_existing = 0usize;
    for sound in &mut cfg.sounds {
        if work.removed_paths.contains(&sound.path) {
            continue;
        }
        if !Path::new(&sound.path).exists() {
            continue;
        }
        let refresh = refresh_existing_sound_source_fingerprint(sound);
        if refresh.changed {
            refreshed_existing += 1;
        }
        if refresh.invalidated {
            invalidated_existing += 1;
        }
    }

    if invalidated_existing > 0 {
        log::info!(
            "Refresh invalidated loudness metadata for {} sound(s) due to source fingerprint drift",
            invalidated_existing
        );
    }

    let should_schedule_backfill = !work.new_sounds.is_empty() || invalidated_existing > 0;

    if work.new_sounds.is_empty() && work.removed_paths.is_empty() && refreshed_existing == 0 {
        crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:end:no_changes");
        crate::diagnostics::record_phase_with_config("library:refresh_complete", &cfg);
        crate::diagnostics::clear_work_runtime();
        return Ok(cfg.sounds.clone());
    }
    for sound in work.new_sounds {
        cfg.add_sound(sound);
    }
    if !work.removed_paths.is_empty() {
        cfg.sounds.retain(|s| !work.removed_paths.contains(&s.path));
    }
    cfg.save().map_err(|e| e.to_string())?;
    let sounds = cfg.sounds.clone();
    crate::diagnostics::record_phase_with_config("library:refresh_complete", &cfg);
    crate::diagnostics::clear_work_runtime();
    drop(cfg);
    if should_schedule_backfill {
        maybe_schedule_missing_loudness_backfill(&config);
    }
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:end:saved");
    Ok(sounds)
}

pub fn import_dropped_files(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    let (target_folder, existing_paths, added_default_folder): (String, HashSet<String>, bool) =
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

    fs::create_dir_all(&target_folder)
        .map_err(|e| format!("Failed to create target folder '{target_folder}': {e}"))?;

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
                cfg.save().map_err(|e| e.to_string())
            })??;
        }
        return Ok(imported);
    }

    let imported_clones = imported.clone();
    with_config_mut(&config, move |cfg| {
        for sound in imported {
            cfg.add_sound(sound);
        }
        cfg.save().map_err(|e| e.to_string())
    })??;
    maybe_schedule_missing_loudness_backfill(&config);
    Ok(imported_clones)
}

pub fn import_files_as_links(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    import_files_to_tab(paths, None, config)
}

pub fn import_files_to_tab(
    paths: Vec<String>,
    tab_id: Option<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    let existing_paths: HashSet<String> = with_config(&config, |cfg| {
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
        if existing_paths.contains(&path) {
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

    with_config_mut(&config, |cfg| {
        let mut sound_ids = Vec::new();
        for sound in &new_sounds {
            cfg.add_sound(sound.clone());
            sound_ids.push(sound.id.clone());
        }

        if let Some(tab_id) = tab_id.as_deref() {
            cfg.add_sounds_to_tab(tab_id, sound_ids);
        }

        cfg.save().map_err(|e| e.to_string())
    })??;

    maybe_schedule_missing_loudness_backfill(&config);

    Ok(new_sounds)
}

pub fn validate_all_sources(config: Arc<Mutex<Config>>) -> Result<Vec<String>, String> {
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
) -> Result<Vec<String>, String> {
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
) -> Result<Vec<(String, Option<String>, String)>, String> {
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

pub fn validate_single_source(id: String, config: Arc<Mutex<Config>>) -> Result<bool, String> {
    let exists = with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .find(|s| s.id == id)
            .map(|s| check_file_exists(effective_source_path(s)))
    })?;

    exists.ok_or_else(|| ERR_SOUND_NOT_FOUND.to_string())
}

pub fn update_sound_source(
    id: String,
    new_path: String,
    config: Arc<Mutex<Config>>,
) -> Result<Sound, String> {
    if !check_file_exists(&new_path) {
        return Err("New file path does not exist".to_string());
    }

    if !scanner::is_audio_file(&new_path) {
        return Err(ERR_UNSUPPORTED_AUDIO_FILE.to_string());
    }

    let updated_sound = with_config_mut(&config, |cfg| {
        let sound = cfg.sounds.iter_mut().find(|s| s.id == id);

        match sound {
            Some(s) => {
                s.source_path = Some(new_path.clone());
                s.path = new_path;
                s.duration_ms = probe_duration_ms(&s.path);
                s.loudness_source_fingerprint =
                    compute_sound_source_fingerprint(&s.path, s.duration_ms);
                s.loudness_lufs = None;
                s.loudness_analysis_state = crate::config::LoudnessAnalysisState::Pending;
                s.loudness_confidence = None;
                let updated_sound = s.clone();
                cfg.save().map_err(|e| e.to_string())?;
                Ok(updated_sound)
            }
            None => Err(ERR_SOUND_NOT_FOUND.to_string()),
        }
    })??;

    maybe_schedule_missing_loudness_backfill(&config);
    Ok(updated_sound)
}

fn _keep_soundtab_in_module_tree(_: &SoundTab) {}
