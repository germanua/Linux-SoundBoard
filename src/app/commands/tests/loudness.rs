use super::*;

#[test]
fn test_trigger_missing_loudness_analysis_backfills_existing_sounds() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config.sounds.push(Sound::new(
        "Startup Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    ));
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let result =
        commands::trigger_missing_loudness_analysis(Arc::clone(&config), false, None, &coords);

    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::Started)
    ));
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_missing_loudness_analysis_skips_unavailable_sounds() {
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new(
        "Unavailable Sound".to_string(),
        "/tmp/missing.wav".to_string(),
    );
    sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
    sound.loudness_lufs = None;
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();
    let result = commands::trigger_missing_loudness_analysis(config, false, None, &coords);

    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds)
    ));
    assert_eq!(coords.backfill.start_count(), 0);
}

#[test]
fn test_trigger_missing_loudness_analysis_starts_refinement_for_estimated_sounds() {
    let audio_path = create_test_audio_file("wav");

    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let mut sound = Sound::new(
        "Estimated Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.loudness_lufs = Some(-16.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.5);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let result =
        commands::trigger_missing_loudness_analysis(Arc::clone(&config), false, None, &coords);
    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds)
    ));
    assert_eq!(coords.backfill.start_count(), 0);
    assert_eq!(coords.refinement.start_count(), 1);

    wait_for_coords_idle(&coords);

    let cfg = config.lock();
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert_ne!(
        stored.loudness_analysis_state,
        LoudnessAnalysisState::Unavailable
    );
    assert!(stored.loudness_confidence.unwrap_or(0.0) >= 0.5);
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_estimated_loudness_refinement_force_refines_high_confidence_sound() {
    let audio_path = create_test_audio_file_with_duration("wav", 3_000);

    let mut config = create_test_config();
    config.settings.auto_gain = false;
    let mut sound = Sound::new(
        "Estimated High Confidence".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.loudness_lufs = Some(-16.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.98);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let result =
        commands::trigger_estimated_loudness_refinement(Arc::clone(&config), true, &coords);
    assert!(matches!(
        result,
        Ok(commands::EstimatedLoudnessRefinementTrigger::Started)
    ));
    assert_eq!(coords.refinement.start_count(), 1);

    wait_for_coords_idle(&coords);

    let cfg = config.lock();
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert_eq!(
        stored.loudness_analysis_state,
        LoudnessAnalysisState::Refined
    );
    assert_eq!(stored.loudness_confidence, Some(1.0));
    assert!(stored.loudness_lufs.is_some());
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_estimated_loudness_refinement_skips_high_confidence_without_force() {
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new("High Confidence".to_string(), "/tmp/high.wav".to_string());
    sound.loudness_lufs = Some(-15.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.95);
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();
    let result = commands::trigger_estimated_loudness_refinement(config, false, &coords);

    assert!(matches!(
        result,
        Ok(commands::EstimatedLoudnessRefinementTrigger::SkippedNoCandidates)
    ));
    assert_eq!(coords.refinement.start_count(), 0);
}

#[test]
fn test_set_auto_gain_schedules_loudness_backfill_for_missing_sounds() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Backfill Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    ));
    let config = Arc::new(Mutex::new(config));
    let player = create_test_audio_player();
    let coords = commands::LoudnessCoordinators::new();

    let result = commands::set_auto_gain(true, Arc::clone(&config), player, &coords);

    assert!(result.is_ok());
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_add_sound_backfills_loudness_when_auto_gain_is_enabled() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    commands::add_sound(
        "Added Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
        Arc::clone(&config),
        &coords,
    )
    .expect("add sound succeeds");

    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_files_to_tab_backfills_loudness_when_auto_gain_is_enabled() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let imported = commands::import_files_to_tab(
        vec![audio_path.to_string_lossy().to_string()],
        None,
        Arc::clone(&config),
        &coords,
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_dropped_files_backfills_loudness_when_auto_gain_is_enabled() {
    let audio_path = create_test_audio_file("mp3");
    let target_root =
        std::env::temp_dir().join(format!("lsb-import-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&target_root).expect("create target dir");

    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config
        .sound_folders
        .push(target_root.to_string_lossy().to_string());
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let imported = commands::import_dropped_files(
        vec![audio_path.to_string_lossy().to_string()],
        config,
        &coords,
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
    let _ = fs::remove_dir_all(&target_root);
}

#[test]
fn test_refresh_sounds_backfills_loudness_for_new_library_files() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();
    let coords = commands::LoudnessCoordinators::new();

    let summary =
        commands::refresh_sounds(Arc::clone(&config), hotkeys, &coords).expect("refresh succeeds");

    assert_eq!(summary.added, 1);
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_update_sound_source_invalidates_loudness_and_schedules_backfill() {
    let new_audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new("Source Test".to_string(), "/tmp/original.mp3".to_string());
    sound.loudness_lufs = Some(-14.0);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));
    let coords = commands::LoudnessCoordinators::new();

    let updated = commands::update_sound_source(
        sound_id.clone(),
        new_audio_path.to_string_lossy().to_string(),
        Arc::clone(&config),
        &coords,
    )
    .expect("update succeeds");

    assert!(updated.loudness_lufs.is_none());
    assert_eq!(
        updated.loudness_analysis_state,
        LoudnessAnalysisState::Pending
    );
    assert!(updated.loudness_confidence.is_none());
    assert!(updated.loudness_source_fingerprint.is_some());
    assert_eq!(coords.backfill.start_count(), 1);
    wait_for_coords_idle(&coords);

    let cfg = config.lock();
    let stored = cfg.get_sound(&sound_id).expect("updated sound exists");
    assert!(stored.loudness_lufs.is_none());
    assert_ne!(
        stored.loudness_analysis_state,
        LoudnessAnalysisState::Estimated
    );
    assert!(stored.loudness_confidence.is_none());
    assert!(stored.loudness_source_fingerprint.is_some());
    drop(cfg);

    cleanup_test_audio_path(&new_audio_path);
}

#[test]
fn test_refresh_sounds_invalidates_stale_loudness_fingerprint() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = false;

    let mut sound = Sound::new(
        "Fingerprint Drift".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.duration_ms = commands::probe_duration_ms(&sound.path);
    sound.loudness_lufs = Some(-14.5);
    sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
    sound.loudness_confidence = Some(1.0);
    sound.loudness_source_fingerprint = Some("stale-fingerprint".to_string());

    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let sound_id = sound.id.clone();
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let summary = commands::refresh_sounds(
        Arc::clone(&config),
        hotkeys,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("refresh succeeds");
    assert_eq!(summary.added, 0);
    assert_eq!(summary.refreshed, 1);
    assert_eq!(summary.invalidated, 1);

    let cfg = config.lock();
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert!(stored.loudness_lufs.is_none());
    assert_eq!(
        stored.loudness_analysis_state,
        LoudnessAnalysisState::Pending
    );
    assert!(stored.loudness_confidence.is_none());
    assert!(stored.loudness_source_fingerprint.is_some());
    assert_ne!(
        stored.loudness_source_fingerprint.as_deref(),
        Some("stale-fingerprint")
    );
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}
