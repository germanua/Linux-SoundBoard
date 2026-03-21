use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::audio::file_link::{check_file_exists, validate_sounds_batch};
use crate::audio::scanner;
use crate::config::{Config, Sound, SoundTab};
use crate::hotkeys::HotkeyManager;

use super::shared::{
    bounded_audio_analysis_threads, build_sound_with_metadata, default_sound_import_dir,
    probe_duration_ms, with_saved_config,
};

#[allow(dead_code)]
pub fn add_sound(name: String, path: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    if !Path::new(&path).exists() {
        return Err("File does not exist".to_string());
    }
    if !scanner::is_audio_file(&path) {
        return Err("Not a supported audio file".to_string());
    }

    {
        let cfg = config.lock().unwrap();
        if cfg.sounds.iter().any(|s| s.path == path) {
            return Err("Sound already exists".to_string());
        }
    }

    let sound = build_sound_with_metadata(name, path);

    let mut cfg = config.lock().unwrap();
    let sound_clone = sound.clone();
    cfg.add_sound(sound);
    cfg.save().map_err(|e| e.to_string())?;
    Ok(sound_clone)
}

pub fn rename_sound(id: String, name: String, config: Arc<Mutex<Config>>) -> Result<Sound, String> {
    let new_name = name.trim().to_string();
    if new_name.is_empty() {
        return Err("Name cannot be empty".to_string());
    }
    let mut config = config.lock().unwrap();
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
    {
        let manager = hotkeys.lock().unwrap();
        let _ = manager.unregister_hotkey_blocking(&id);
    }
    let mut config = config.lock().unwrap();
    config.remove_sound(&id);
    config.save().map_err(|e| e.to_string())
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
    #[derive(Debug)]
    struct RefreshWork {
        new_sounds: Vec<Sound>,
        removed_ids: Vec<String>,
        removed_paths: HashSet<String>,
    }

    let (folders, existing_paths, known_sounds) = {
        let cfg = config.lock().unwrap();
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

        let build_metadata = |file: &scanner::AudioFile| {
            build_sound_with_metadata(file.name.clone(), file.path.clone())
        };
        let analysis_threads = bounded_audio_analysis_threads();
        crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:before_metadata_pool");
        let new_sounds: Vec<Sound> = match rayon::ThreadPoolBuilder::new()
            .num_threads(analysis_threads)
            .build()
        {
            Ok(pool) => pool.install(|| new_files.par_iter().map(build_metadata).collect()),
            Err(e) => {
                log::warn!(
                    "Failed to build bounded refresh-analysis pool ({} threads): {}. Falling back to sequential metadata build.",
                    analysis_threads,
                    e
                );
                new_files.iter().map(build_metadata).collect()
            }
        };

        let removed: Vec<(String, String)> = known_sounds
            .into_par_iter()
            .filter(|(_, path)| !Path::new(path).exists())
            .collect();

        RefreshWork {
            removed_ids: removed.iter().map(|(id, _)| id.clone()).collect(),
            removed_paths: removed.into_iter().map(|(_, path)| path).collect(),
            new_sounds,
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:after_metadata_pool");

    if !work.removed_ids.is_empty() {
        let manager = hotkeys.lock().unwrap();
        for id in &work.removed_ids {
            let _ = manager.unregister_hotkey_blocking(id);
        }
    }

    let mut cfg = config.lock().unwrap();
    if work.new_sounds.is_empty() && work.removed_paths.is_empty() {
        crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:end:no_changes");
        return Ok(cfg.sounds.clone());
    }
    for sound in work.new_sounds {
        cfg.add_sound(sound);
    }
    if !work.removed_paths.is_empty() {
        cfg.sounds.retain(|s| !work.removed_paths.contains(&s.path));
    }
    cfg.save().map_err(|e| e.to_string())?;
    crate::diagnostics::memory::log_memory_snapshot("refresh_sounds:end:saved");
    Ok(cfg.sounds.clone())
}

#[allow(dead_code)]
pub fn import_dropped_files(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    let (target_folder, existing_paths, added_default_folder): (String, HashSet<String>, bool) = {
        let mut cfg = config.lock().unwrap();
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
            cfg.sound_folders.push(default_folder.clone());
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
            config.lock().unwrap().save().map_err(|e| e.to_string())?;
        }
        return Ok(imported);
    }

    let mut cfg = config.lock().unwrap();
    let imported_clones = imported.clone();
    for sound in imported {
        cfg.add_sound(sound);
    }
    cfg.save().map_err(|e| e.to_string())?;
    Ok(imported_clones)
}

pub fn import_files_as_links(
    paths: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<Vec<Sound>, String> {
    let existing_paths: HashSet<String> = {
        let cfg = config.lock().unwrap();
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

    let mut cfg = config.lock().unwrap();
    for sound in &new_sounds {
        cfg.add_sound(sound.clone());
    }
    cfg.save().map_err(|e| e.to_string())?;

    Ok(new_sounds)
}

pub fn validate_all_sources(config: Arc<Mutex<Config>>) -> Result<Vec<String>, String> {
    let config = config.lock().unwrap();

    let sounds: Vec<(String, Option<String>, String)> = config
        .sounds
        .iter()
        .map(|s| (s.id.clone(), s.source_path.clone(), s.path.clone()))
        .collect();

    Ok(validate_sounds_batch(&sounds))
}

#[allow(dead_code)]
pub fn validate_single_source(id: String, config: Arc<Mutex<Config>>) -> Result<bool, String> {
    let config = config.lock().unwrap();
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

    let mut config = config.lock().unwrap();
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

#[allow(dead_code)]
fn _keep_soundtab_in_module_tree(_: &SoundTab) {}
