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
use crate::config::{Config, Sound, SoundTab};
use crate::hotkeys::HotkeyManager;

use super::shared::{
    bounded_audio_analysis_threads, build_sound_with_metadata, default_sound_import_dir,
    probe_duration_ms, with_config_mut, with_saved_config,
};

pub fn add_sound(name: String, path: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    if !Path::new(&path).exists() {
        return Err("File does not exist".to_string());
    }
    if !scanner::is_audio_file(&path) {
        return Err("Not a supported audio file".to_string());
    }

    {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        if cfg.sounds.iter().any(|s| s.path == path) {
            return Err("Sound already exists".to_string());
        }
    }

    let sound = build_sound_with_metadata(name, path);

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let sound_clone = sound.clone();
    cfg.add_sound(sound);
    cfg.save().map_err(|e| e.to_string())?;
    drop(cfg);
    Ok(sound_clone)
}

pub fn rename_sound(id: String, name: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    let new_name = name.trim().to_string();
    if new_name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    let mut config = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    if config.get_sound(&id).is_none() {
        return Err("Sound not found".to_string());
    }
    config.set_sound_name(&id, new_name);
    config.save().map_err(|e| e.to_string())?;
    config
        .get_sound(&id)
        .cloned()
        .ok_or_else(|| "Sound not found".to_string())
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
    let cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;

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

    Ok(SoundRemovalPlan {
        existing_ids,
        hotkey_ids,
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

    if !plan.hotkey_ids.is_empty() {
        if let Ok(mut manager) = hotkeys.lock() {
            let _ = manager.unregister_hotkeys_blocking(&plan.hotkey_ids);
        } else {
            log::warn!(
                "Hotkeys lock poisoned, skipping unregister for {} sound(s)",
                plan.hotkey_ids.len()
            );
        }
    }

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    cfg.remove_sounds(&plan.existing_ids);
    cfg.save().map_err(|e| e.to_string())
}

pub fn add_sound_folder(folder: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    if !Path::new(&folder).is_dir() {
        return Err("Folder does not exist".to_string());
    }
    with_saved_config(&config, |cfg| {
        cfg.add_sound_folder(folder);
    })
}

pub fn remove_sound_folder(folder: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    with_saved_config(&config, |cfg| {
        cfg.remove_sound_folder(&folder);
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

    let (folders, existing_paths, known_sounds) = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
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
    };

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
        let analysis_threads = bounded_audio_analysis_threads();
        let pool_threads = if new_files.is_empty() {
            1
        } else {
            analysis_threads
        };
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

    if !work.removed_ids.is_empty() {
        if let Ok(mut manager) = hotkeys.lock() {
            let _ = manager.unregister_hotkeys_blocking(&work.removed_ids);
        } else {
            log::warn!(
                "Hotkeys lock poisoned, skipping unregister for {} sounds",
                work.removed_ids.len()
            );
        }
    }

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    if work.new_sounds.is_empty() && work.removed_paths.is_empty() {
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
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:end:saved");
    Ok(sounds)
}

pub fn import_dropped_files(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    let (target_folder, existing_paths, added_default_folder): (String, HashSet<String>, bool) = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
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
    };

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

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let imported_clones = imported.clone();
    for sound in imported {
        cfg.add_sound(sound);
    }
    cfg.save().map_err(|e| e.to_string())?;
    drop(cfg);
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
    let existing_paths: HashSet<String> = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        cfg.sounds.iter().map(|s| s.path.clone()).collect()
    };

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

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let mut sound_ids = Vec::new();
    for sound in &new_sounds {
        cfg.add_sound(sound.clone());
        sound_ids.push(sound.id.clone());
    }

    if let Some(tab_id) = tab_id {
        cfg.add_sounds_to_tab(&tab_id, sound_ids);
    }

    cfg.save().map_err(|e| e.to_string())?;
    drop(cfg);

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
    let config = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;

    let sounds: Vec<(String, Option<String>, String)> = config
        .sounds
        .iter()
        .map(|s| (s.id.clone(), s.source_path.clone(), s.path.clone()))
        .collect();

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
    let config = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;

    Ok(config
        .sounds
        .iter()
        .map(|s| (s.id.clone(), s.source_path.clone(), s.path.clone()))
        .collect())
}

pub fn validate_sources_for_startup(
    sounds: &[(String, Option<String>, String)],
) -> ValidationReport {
    validate_sounds_chunked_with_report(sounds, STARTUP_VALIDATION_CHUNK_SIZE)
}

pub fn validate_single_source(id: String, config: Arc<Mutex<Config>>) -> Result<bool, String> {
    let config = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let sound = config.sounds.iter().find(|s| s.id == id);

    match sound {
        Some(s) => match &s.source_path {
            Some(path) => Ok(check_file_exists(path)),
            None => Ok(check_file_exists(&s.path)),
        },
        None => Err("Sound not found".to_string()),
    }
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
        return Err("Not a supported audio file".to_string());
    }

    let mut config = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    let sound = config.sounds.iter_mut().find(|s| s.id == id);

    match sound {
        Some(s) => {
            s.source_path = Some(new_path.clone());
            s.path = new_path;
            s.duration_ms = probe_duration_ms(&s.path);
            let updated_sound = s.clone();
            config.save().map_err(|e| e.to_string())?;
            Ok(updated_sound)
        }
        None => Err("Sound not found".to_string()),
    }
}

fn _keep_soundtab_in_module_tree(_: &SoundTab) {}
