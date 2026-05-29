use super::*;
use crate::config::{Config, Sound, SoundTab};

#[test]
fn test_parse_proc_status() {
    let fixture = "Name:\ttest\nVmRSS:\t12345 kB\nVmHWM:\t23456 kB\nVmData:\t34567 kB\nVmSize:\t45678 kB\nVmSwap:\t0 kB\nRssAnon:\t11111 kB\nRssFile:\t2222 kB\nRssShmem:\t333 kB\nThreads:\t5\n";

    let mut snapshot = MemorySnapshot::default();
    for line in fixture.lines() {
        if line.starts_with("VmRSS:") {
            snapshot.vm_rss_kb = parse_kb_value(line);
        } else if line.starts_with("VmHWM:") {
            snapshot.vm_hwm_kb = parse_kb_value(line);
        } else if line.starts_with("VmData:") {
            snapshot.vm_data_kb = parse_kb_value(line);
        } else if line.starts_with("VmSize:") {
            snapshot.vm_size_kb = parse_kb_value(line);
        } else if line.starts_with("VmSwap:") {
            snapshot.vm_swap_kb = parse_kb_value(line);
        } else if line.starts_with("RssAnon:") {
            snapshot.rss_anon_kb = parse_kb_value(line);
        } else if line.starts_with("RssFile:") {
            snapshot.rss_file_kb = parse_kb_value(line);
        } else if line.starts_with("RssShmem:") {
            snapshot.rss_shmem_kb = parse_kb_value(line);
        } else if line.starts_with("Threads:") {
            snapshot.threads = parse_u64_value(line);
        }
    }

    assert_eq!(snapshot.vm_rss_kb, Some(12345));
    assert_eq!(snapshot.vm_hwm_kb, Some(23456));
    assert_eq!(snapshot.vm_data_kb, Some(34567));
    assert_eq!(snapshot.vm_size_kb, Some(45678));
    assert_eq!(snapshot.vm_swap_kb, Some(0));
    assert_eq!(snapshot.rss_anon_kb, Some(11111));
    assert_eq!(snapshot.rss_file_kb, Some(2222));
    assert_eq!(snapshot.rss_shmem_kb, Some(333));
    assert_eq!(snapshot.threads, Some(5));
}

#[test]
fn test_parse_smaps_rollup() {
    let fixture = "12345678-12345678 ---p 00000000 00:00 0                          [rollup]\nPss:\t5678 kB\nPrivate_Clean:\t1234 kB\nPrivate_Dirty:\t2345 kB\nShared_Clean:\t3456 kB\nShared_Dirty:\t4567 kB\n";

    let mut snapshot = MemorySnapshot::default();
    for line in fixture.lines() {
        if line.starts_with("Pss:") {
            snapshot.pss_kb = parse_kb_value(line);
        } else if line.starts_with("Private_Clean:") {
            snapshot.private_clean_kb = parse_kb_value(line);
        } else if line.starts_with("Private_Dirty:") {
            snapshot.private_dirty_kb = parse_kb_value(line);
        } else if line.starts_with("Shared_Clean:") {
            snapshot.shared_clean_kb = parse_kb_value(line);
        } else if line.starts_with("Shared_Dirty:") {
            snapshot.shared_dirty_kb = parse_kb_value(line);
        }
    }

    assert_eq!(snapshot.pss_kb, Some(5678));
    assert_eq!(snapshot.private_clean_kb, Some(1234));
    assert_eq!(snapshot.private_dirty_kb, Some(2345));
    assert_eq!(snapshot.shared_clean_kb, Some(3456));
    assert_eq!(snapshot.shared_dirty_kb, Some(4567));
}

#[test]
fn test_parse_report_enabled_env_accepts_common_truthy_and_falsey_values() {
    let key = "LSB_MEMORY_REPORT";
    let original = std::env::var(key).ok();

    std::env::set_var(key, "1");
    assert!(parse_report_enabled_env());
    std::env::set_var(key, "yes");
    assert!(parse_report_enabled_env());
    std::env::set_var(key, "true");
    assert!(parse_report_enabled_env());
    std::env::set_var(key, "0");
    assert!(!parse_report_enabled_env());
    std::env::set_var(key, "no");
    assert!(!parse_report_enabled_env());
    std::env::set_var(key, "false");
    assert!(!parse_report_enabled_env());

    if let Some(value) = original {
        std::env::set_var(key, value);
    } else {
        std::env::remove_var(key);
    }
}

#[test]
fn test_is_smaps_mapping_header() {
    assert!(is_smaps_mapping_header(
        "7f1234567000-7f1234568000 r--p 00000000 00:00 0 /usr/lib/libexample.so"
    ));
    assert!(!is_smaps_mapping_header("Rss:                120 kB"));
}

#[test]
fn test_app_inventory_byte_estimation() {
    let mut config = Config::default();

    let mut sound1 = Sound::new("TestSound1".to_string(), "/path/to/sound1.wav".to_string());
    sound1.hotkey = Some("Ctrl+1".to_string());

    let mut sound2 = Sound::new("TestSound2".to_string(), "/path/to/sound2.wav".to_string());
    sound2.hotkey = Some("Ctrl+2".to_string());

    config.sounds.push(sound1);
    config.sounds.push(sound2);

    let mut tab1 = SoundTab::new("Tab1".to_string(), 0);
    tab1.sound_ids.push("sound-id-1".to_string());
    tab1.sound_ids.push("sound-id-2".to_string());

    config.tabs.push(tab1);
    config.sound_folders.push("/home/user/sounds".to_string());

    config.settings.mic_source = Some("alsa_input.usb".to_string());
    config.settings.control_hotkeys.play_pause = Some("Ctrl+Space".to_string());
    config.settings.control_hotkeys.stop_all = Some("Ctrl+S".to_string());

    let inventory = build_app_inventory(&config);

    assert_eq!(inventory.sound_count, 2);
    assert_eq!(inventory.tab_count, 1);
    assert_eq!(inventory.folder_count, 1);
    assert!(inventory.sound_string_bytes > 0);
    assert!(inventory.tab_string_bytes > 0);
    assert!(inventory.settings_string_bytes > 0);
    assert_eq!(inventory.hotkey_binding_count, 4);
    assert_eq!(inventory.ui_row_count_estimate, 3);
}

#[test]
fn test_record_phase() {
    record_phase("test_phase", None);

    let guard = MEMORY_REPORT.lock();
    assert!(guard.is_some());
    if let Some(ref report) = *guard {
        assert!(
            report.phases.iter().any(|phase| phase.name == "test_phase"),
            "expected report to contain test_phase, got {:?}",
            report
                .phases
                .iter()
                .map(|phase| phase.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn test_memory_snapshot_serialization() {
    let snapshot = MemorySnapshot {
        vm_rss_kb: Some(12345),
        pss_kb: Some(5678),
        threads: Some(4),
        ..Default::default()
    };

    let json = serde_json::to_string(&snapshot).unwrap();
    let deserialized: MemorySnapshot = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.vm_rss_kb, Some(12345));
    assert_eq!(deserialized.pss_kb, Some(5678));
    assert_eq!(deserialized.threads, Some(4));
}
