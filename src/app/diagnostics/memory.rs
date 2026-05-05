use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub vm_rss_kb: Option<u64>,
    pub vm_hwm_kb: Option<u64>,
    pub vm_data_kb: Option<u64>,
    pub vm_size_kb: Option<u64>,
    pub vm_swap_kb: Option<u64>,
    pub rss_anon_kb: Option<u64>,
    pub rss_file_kb: Option<u64>,
    pub rss_shmem_kb: Option<u64>,
    pub threads: Option<u64>,
    pub pss_kb: Option<u64>,
    pub private_clean_kb: Option<u64>,
    pub private_dirty_kb: Option<u64>,
    pub shared_clean_kb: Option<u64>,
    pub shared_dirty_kb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPhase {
    pub name: String,
    pub timestamp_secs: u64,
    pub snapshot: MemorySnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<AppMemoryInventory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppMemoryInventory {
    pub sound_count: usize,
    pub tab_count: usize,
    pub folder_count: usize,
    pub sound_string_bytes: usize,
    pub tab_string_bytes: usize,
    pub folder_string_bytes: usize,
    pub settings_string_bytes: usize,
    pub config_string_bytes: usize,
    pub estimated_sound_row_payload_bytes: usize,
    pub estimated_navigation_payload_bytes: usize,
    pub estimated_ui_store_bytes: usize,
    pub ui_row_count_estimate: usize,
    pub hotkey_binding_count: usize,
    pub validation_batch_size: usize,
    pub validation_mode: String,
    pub validation_worker_threads: usize,
    pub work_kind: String,
    pub work_item_count: usize,
    pub pool_thread_count: usize,
    pub live_timer_count: usize,
    pub hotkey_status: String,
    pub playback_registry_count: usize,
    pub thread_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReport {
    pub phases: Vec<MemoryPhase>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub top_file_mappings: Vec<FileMappingSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMappingSummary {
    pub path: String,
    pub rss_kb: u64,
    pub pss_kb: u64,
    pub private_clean_kb: u64,
    pub private_dirty_kb: u64,
    pub shared_clean_kb: u64,
    pub shared_dirty_kb: u64,
    pub region_count: usize,
}

static MEMORY_REPORT: Mutex<Option<MemoryReport>> = Mutex::new(None);
static RUNTIME_INVENTORY: LazyLock<Mutex<RuntimeInventory>> =
    LazyLock::new(|| Mutex::new(RuntimeInventory::default()));

#[derive(Debug, Clone, Default)]
struct RuntimeInventory {
    validation_batch_size: usize,
    validation_mode: String,
    validation_worker_threads: usize,
    work_kind: String,
    work_item_count: usize,
    pool_thread_count: usize,
    live_timer_count: usize,
    hotkey_status: String,
    playback_registry_count: usize,
}

fn parse_kb_value(line: &str) -> Option<u64> {
    line.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
}

fn parse_u64_value(line: &str) -> Option<u64> {
    line.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
}

fn parse_report_enabled_env() -> bool {
    std::env::var("LSB_MEMORY_REPORT")
        .ok()
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
                true
            } else if matches!(normalized.as_str(), "0" | "false" | "no" | "off" | "") {
                false
            } else {
                normalized.parse::<bool>().unwrap_or(false)
            }
        })
        .unwrap_or(false)
}

fn parse_smaps_numeric_value(line: &str) -> u64 {
    parse_kb_value(line).unwrap_or(0)
}

fn canonical_mapping_path(raw_path: &str) -> String {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        "[anonymous]".to_string()
    } else {
        trimmed.to_string()
    }
}

fn read_top_file_mappings(limit: usize) -> Vec<FileMappingSummary> {
    let content = match std::fs::read_to_string("/proc/self/smaps") {
        Ok(content) => content,
        Err(_) => return Vec::new(),
    };

    #[derive(Default)]
    struct MappingAccumulator {
        rss_kb: u64,
        pss_kb: u64,
        private_clean_kb: u64,
        private_dirty_kb: u64,
        shared_clean_kb: u64,
        shared_dirty_kb: u64,
        region_count: usize,
    }

    let mut mappings: HashMap<String, MappingAccumulator> = HashMap::new();
    let mut current_path: Option<String> = None;

    for line in content.lines() {
        if is_smaps_mapping_header(line) {
            let path = line
                .split_whitespace()
                .skip(5)
                .collect::<Vec<_>>()
                .join(" ");
            let path = canonical_mapping_path(&path);
            current_path = Some(path.clone());
            mappings.entry(path).or_default().region_count += 1;
            continue;
        }

        let Some(path) = current_path.as_ref() else {
            continue;
        };

        let Some(entry) = mappings.get_mut(path) else {
            continue;
        };

        if line.starts_with("Rss:") {
            entry.rss_kb += parse_smaps_numeric_value(line);
        } else if line.starts_with("Pss:") {
            entry.pss_kb += parse_smaps_numeric_value(line);
        } else if line.starts_with("Private_Clean:") {
            entry.private_clean_kb += parse_smaps_numeric_value(line);
        } else if line.starts_with("Private_Dirty:") {
            entry.private_dirty_kb += parse_smaps_numeric_value(line);
        } else if line.starts_with("Shared_Clean:") {
            entry.shared_clean_kb += parse_smaps_numeric_value(line);
        } else if line.starts_with("Shared_Dirty:") {
            entry.shared_dirty_kb += parse_smaps_numeric_value(line);
        }
    }

    let mut summaries = mappings
        .into_iter()
        .filter(|(path, acc)| path != "[anonymous]" && acc.rss_kb > 0)
        .map(|(path, acc)| FileMappingSummary {
            path,
            rss_kb: acc.rss_kb,
            pss_kb: acc.pss_kb,
            private_clean_kb: acc.private_clean_kb,
            private_dirty_kb: acc.private_dirty_kb,
            shared_clean_kb: acc.shared_clean_kb,
            shared_dirty_kb: acc.shared_dirty_kb,
            region_count: acc.region_count,
        })
        .collect::<Vec<_>>();

    summaries.sort_by(|a, b| {
        b.rss_kb
            .cmp(&a.rss_kb)
            .then_with(|| b.pss_kb.cmp(&a.pss_kb))
            .then_with(|| a.path.cmp(&b.path))
    });
    summaries.truncate(limit);
    summaries
}

fn is_smaps_mapping_header(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    let Some(range) = parts.next() else {
        return false;
    };
    let Some(perms) = parts.next() else {
        return false;
    };

    range.contains('-')
        && perms.len() == 4
        && perms
            .chars()
            .all(|c| matches!(c, 'r' | 'w' | 'x' | 'p' | 's' | '-'))
}

fn read_smaps_rollup() -> Option<MemorySnapshot> {
    let content = std::fs::read_to_string("/proc/self/smaps_rollup").ok()?;
    let mut snapshot = MemorySnapshot::default();

    for line in content.lines() {
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

    Some(snapshot)
}

pub fn read_memory_snapshot() -> Option<MemorySnapshot> {
    let content = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut snapshot = MemorySnapshot::default();

    for line in content.lines() {
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

    if let Some(smaps) = read_smaps_rollup() {
        snapshot.pss_kb = smaps.pss_kb;
        snapshot.private_clean_kb = smaps.private_clean_kb;
        snapshot.private_dirty_kb = smaps.private_dirty_kb;
        snapshot.shared_clean_kb = smaps.shared_clean_kb;
        snapshot.shared_dirty_kb = smaps.shared_dirty_kb;
    }

    Some(snapshot)
}

pub fn log_memory_snapshot(tag: &str) {
    if let Some(m) = read_memory_snapshot() {
        info!(
            "RAM [{}] VmRSS={}kB VmHWM={}kB VmData={}kB VmSize={}kB VmSwap={}kB RssAnon={}kB RssFile={}kB RssShmem={}kB Threads={} Pss={}kB PrivClean={}kB PrivDirty={}kB ShClean={}kB ShDirty={}kB",
            tag,
            m.vm_rss_kb.unwrap_or(0),
            m.vm_hwm_kb.unwrap_or(0),
            m.vm_data_kb.unwrap_or(0),
            m.vm_size_kb.unwrap_or(0),
            m.vm_swap_kb.unwrap_or(0),
            m.rss_anon_kb.unwrap_or(0),
            m.rss_file_kb.unwrap_or(0),
            m.rss_shmem_kb.unwrap_or(0),
            m.threads.unwrap_or(0),
            m.pss_kb.unwrap_or(0),
            m.private_clean_kb.unwrap_or(0),
            m.private_dirty_kb.unwrap_or(0),
            m.shared_clean_kb.unwrap_or(0),
            m.shared_dirty_kb.unwrap_or(0),
        );
    } else {
        info!("RAM [{}] unable to read /proc/self/status", tag);
    }
}

pub fn estimate_config_bytes(config: &crate::config::Config) -> usize {
    let mut total = 0;

    for sound in &config.sounds {
        total += sound.id.len();
        total += sound.name.len();
        total += sound.path.len();
        if let Some(ref source_path) = sound.source_path {
            total += source_path.len();
        }
        if let Some(ref hotkey) = sound.hotkey {
            total += hotkey.len();
        }
    }

    for tab in &config.tabs {
        total += tab.id.len();
        total += tab.name.len();
        for sound_id in &tab.sound_ids {
            total += sound_id.len();
        }
    }

    for folder in &config.sound_folders {
        total += folder.len();
    }

    if let Some(ref mic_source) = config.settings.mic_source {
        total += mic_source.len();
    }

    let hotkeys = &config.settings.control_hotkeys;
    if let Some(ref hk) = hotkeys.play_pause {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.stop_all {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.previous_sound {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.next_sound {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.mute_headphones {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.mute_real_mic {
        total += hk.len();
    }
    if let Some(ref hk) = hotkeys.cycle_play_mode {
        total += hk.len();
    }

    total
}

pub fn estimate_ui_bytes(config: &crate::config::Config) -> usize {
    let sound_rows = config.sounds.len() * 200;
    let tab_rows = config.tabs.len() * 100;
    sound_rows + tab_rows
}

pub fn build_app_inventory(config: &crate::config::Config) -> AppMemoryInventory {
    let sound_count = config.sounds.len();
    let tab_count = config.tabs.len();
    let folder_count = config.sound_folders.len();

    let mut sound_string_bytes = 0;
    for sound in &config.sounds {
        sound_string_bytes += sound.id.len();
        sound_string_bytes += sound.name.len();
        sound_string_bytes += sound.path.len();
        if let Some(ref source_path) = sound.source_path {
            sound_string_bytes += source_path.len();
        }
        if let Some(ref hotkey) = sound.hotkey {
            sound_string_bytes += hotkey.len();
        }
    }

    let mut tab_string_bytes = 0;
    for tab in &config.tabs {
        tab_string_bytes += tab.id.len();
        tab_string_bytes += tab.name.len();
        for sound_id in &tab.sound_ids {
            tab_string_bytes += sound_id.len();
        }
    }

    let folder_string_bytes = config
        .sound_folders
        .iter()
        .map(|folder| folder.len())
        .sum::<usize>();

    let mut settings_string_bytes = 0;
    if let Some(ref mic_source) = config.settings.mic_source {
        settings_string_bytes += mic_source.len();
    }
    let hotkeys = &config.settings.control_hotkeys;
    if let Some(ref hk) = hotkeys.play_pause {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.stop_all {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.previous_sound {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.next_sound {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.mute_headphones {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.mute_real_mic {
        settings_string_bytes += hk.len();
    }
    if let Some(ref hk) = hotkeys.cycle_play_mode {
        settings_string_bytes += hk.len();
    }

    let config_string_bytes =
        sound_string_bytes + tab_string_bytes + folder_string_bytes + settings_string_bytes;

    let estimated_sound_row_payload_bytes = config.sounds.iter().fold(0usize, |total, sound| {
        total
            + std::mem::size_of::<String>() * 2
            + std::mem::size_of::<Option<u64>>()
            + std::mem::size_of::<Option<String>>()
            + sound.id.len()
            + sound.name.len()
            + sound.hotkey.as_ref().map_or(0, String::len)
    });

    let estimated_navigation_payload_bytes = config.sounds.iter().fold(0usize, |total, sound| {
        total + std::mem::size_of::<String>() * 2 + sound.id.len() + sound.name.len()
    });

    let estimated_ui_store_bytes =
        estimated_sound_row_payload_bytes + estimated_navigation_payload_bytes + (tab_count * 128);

    let ui_row_count_estimate = sound_count + tab_count;

    let mut hotkey_binding_count = 0;
    for sound in &config.sounds {
        if sound.hotkey.is_some() {
            hotkey_binding_count += 1;
        }
    }
    if hotkeys.play_pause.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.stop_all.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.previous_sound.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.next_sound.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.mute_headphones.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.mute_real_mic.is_some() {
        hotkey_binding_count += 1;
    }
    if hotkeys.cycle_play_mode.is_some() {
        hotkey_binding_count += 1;
    }

    let thread_count = read_memory_snapshot().and_then(|s| s.threads).unwrap_or(0);
    let runtime = RUNTIME_INVENTORY
        .lock()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    AppMemoryInventory {
        sound_count,
        tab_count,
        folder_count,
        sound_string_bytes,
        tab_string_bytes,
        folder_string_bytes,
        settings_string_bytes,
        config_string_bytes,
        estimated_sound_row_payload_bytes,
        estimated_navigation_payload_bytes,
        estimated_ui_store_bytes,
        ui_row_count_estimate,
        hotkey_binding_count,
        validation_batch_size: runtime.validation_batch_size,
        validation_mode: runtime.validation_mode,
        validation_worker_threads: runtime.validation_worker_threads,
        work_kind: runtime.work_kind,
        work_item_count: runtime.work_item_count,
        pool_thread_count: runtime.pool_thread_count,
        live_timer_count: runtime.live_timer_count,
        hotkey_status: runtime.hotkey_status,
        playback_registry_count: runtime.playback_registry_count,
        thread_count,
    }
}

pub fn set_validation_runtime(batch_size: usize, mode: &str, worker_threads: usize) {
    if let Ok(mut runtime) = RUNTIME_INVENTORY.lock() {
        runtime.validation_batch_size = batch_size;
        runtime.validation_mode = mode.to_string();
        runtime.validation_worker_threads = worker_threads;
    }
}

pub fn set_work_runtime(kind: &str, item_count: usize, pool_threads: usize) {
    if let Ok(mut runtime) = RUNTIME_INVENTORY.lock() {
        runtime.work_kind = kind.to_string();
        runtime.work_item_count = item_count;
        runtime.pool_thread_count = pool_threads;
    }
}

pub fn clear_work_runtime() {
    set_work_runtime("", 0, 0);
}

pub fn set_timer_count(timer_count: usize) {
    if let Ok(mut runtime) = RUNTIME_INVENTORY.lock() {
        runtime.live_timer_count = timer_count;
    }
}

pub fn set_hotkey_status(status: &str) {
    if let Ok(mut runtime) = RUNTIME_INVENTORY.lock() {
        runtime.hotkey_status = status.to_string();
    }
}

pub fn set_playback_registry_count(count: usize) {
    if let Ok(mut runtime) = RUNTIME_INVENTORY.lock() {
        runtime.playback_registry_count = count;
    }
}

pub fn record_phase_with_config(name: &str, config: &crate::config::Config) {
    record_phase(name, Some(build_app_inventory(config)));
}

pub fn record_phase(name: &str, inventory: Option<AppMemoryInventory>) {
    let snapshot = match read_memory_snapshot() {
        Some(s) => s,
        None => {
            warn!("Failed to read memory snapshot for phase: {}", name);
            return;
        }
    };

    let timestamp_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let phase = MemoryPhase {
        name: name.to_string(),
        timestamp_secs,
        snapshot: snapshot.clone(),
        inventory,
    };

    if let Ok(mut guard) = MEMORY_REPORT.lock() {
        if guard.is_none() {
            *guard = Some(MemoryReport {
                phases: vec![],
                top_file_mappings: vec![],
            });
        }
        if let Some(ref mut report) = *guard {
            report.phases.push(phase.clone());
        }
    }

    info!(
        "RAM_PHASE [{}] RSS={}kB PSS={}kB PrivDirty={}kB RssFile={}kB RssShmem={}kB ShDirty={}kB Threads={}{}",
        phase.name,
        snapshot.vm_rss_kb.unwrap_or(0),
        snapshot.pss_kb.unwrap_or(0),
        snapshot.private_dirty_kb.unwrap_or(0),
        snapshot.rss_file_kb.unwrap_or(0),
        snapshot.rss_shmem_kb.unwrap_or(0),
        snapshot.shared_dirty_kb.unwrap_or(0),
        snapshot.threads.unwrap_or(0),
        phase
            .inventory
            .as_ref()
            .map(|inv| format!(
                " timers={} hotkeys=\"{}\" playback={} validation={}({},{}) work={}({},{})",
                inv.live_timer_count,
                inv.hotkey_status,
                inv.playback_registry_count,
                inv.validation_batch_size,
                inv.validation_mode,
                inv.validation_worker_threads,
                if inv.work_kind.is_empty() {
                    "none"
                } else {
                    &inv.work_kind
                },
                inv.work_item_count,
                inv.pool_thread_count
            ))
            .unwrap_or_default()
    );
}

pub fn write_memory_report() -> Result<(), Box<dyn std::error::Error>> {
    if !parse_report_enabled_env() {
        return Ok(());
    }

    let mut report = match MEMORY_REPORT.lock() {
        Ok(guard) => match &*guard {
            Some(r) => r.clone(),
            None => return Ok(()),
        },
        Err(_) => return Err("Failed to lock memory report".into()),
    };

    report.top_file_mappings = read_top_file_mappings(8);

    let output_path = std::env::var("LSB_MEMORY_REPORT_PATH")
        .unwrap_or_else(|_| "/tmp/lsb_memory_report.json".to_string());

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&output_path, json)?;
    info!("Memory report written to: {}", output_path);

    let text_path = output_path.replace(".json", ".txt");
    let mut text = String::new();
    text.push_str("Linux Soundboard Memory Report\n");
    text.push_str("==============================\n\n");

    for phase in &report.phases {
        text.push_str(&format!("Phase: {}\n", phase.name));
        text.push_str(&format!("Timestamp: {}\n", phase.timestamp_secs));
        let s = &phase.snapshot;
        text.push_str(&format!("  VmRSS: {} kB\n", s.vm_rss_kb.unwrap_or(0)));
        text.push_str(&format!("  VmHWM: {} kB\n", s.vm_hwm_kb.unwrap_or(0)));
        text.push_str(&format!("  VmData: {} kB\n", s.vm_data_kb.unwrap_or(0)));
        text.push_str(&format!("  Pss: {} kB\n", s.pss_kb.unwrap_or(0)));
        text.push_str(&format!(
            "  Private_Dirty: {} kB\n",
            s.private_dirty_kb.unwrap_or(0)
        ));
        text.push_str(&format!("  RssFile: {} kB\n", s.rss_file_kb.unwrap_or(0)));
        text.push_str(&format!("  Threads: {}\n", s.threads.unwrap_or(0)));
        text.push_str(&format!(
            "  Summary: RSS={} kB, PSS={} kB, Private_Dirty={} kB, RssFile={} kB\n",
            s.vm_rss_kb.unwrap_or(0),
            s.pss_kb.unwrap_or(0),
            s.private_dirty_kb.unwrap_or(0),
            s.rss_file_kb.unwrap_or(0)
        ));
        text.push_str(&format!(
            "  Interpretation: private/process-owned ~= Private_Dirty={} kB, shared-or-file-backed ~= RssFile={} kB, proportional share ~= PSS={} kB\n",
            s.private_dirty_kb.unwrap_or(0),
            s.rss_file_kb.unwrap_or(0),
            s.pss_kb.unwrap_or(0)
        ));

        if let Some(ref inv) = phase.inventory {
            text.push_str("  Inventory:\n");
            text.push_str(&format!("    Sounds: {}\n", inv.sound_count));
            text.push_str(&format!("    Tabs: {}\n", inv.tab_count));
            text.push_str(&format!("    Folders: {}\n", inv.folder_count));
            text.push_str(&format!(
                "    Sound strings: {} bytes\n",
                inv.sound_string_bytes
            ));
            text.push_str(&format!(
                "    Tab strings: {} bytes\n",
                inv.tab_string_bytes
            ));
            text.push_str(&format!(
                "    Folder strings: {} bytes\n",
                inv.folder_string_bytes
            ));
            text.push_str(&format!(
                "    Settings strings: {} bytes\n",
                inv.settings_string_bytes
            ));
            text.push_str(&format!(
                "    Config strings total: {} bytes\n",
                inv.config_string_bytes
            ));
            text.push_str(&format!(
                "    Estimated sound row payload: {} bytes\n",
                inv.estimated_sound_row_payload_bytes
            ));
            text.push_str(&format!(
                "    Estimated navigation payload: {} bytes\n",
                inv.estimated_navigation_payload_bytes
            ));
            text.push_str(&format!(
                "    Estimated UI store total: {} bytes\n",
                inv.estimated_ui_store_bytes
            ));
            text.push_str(&format!(
                "    Hotkey bindings: {}\n",
                inv.hotkey_binding_count
            ));
            text.push_str(&format!(
                "    Validation batch: {} ({}, {} threads)\n",
                inv.validation_batch_size, inv.validation_mode, inv.validation_worker_threads
            ));
            text.push_str(&format!(
                "    Work: {} (items={}, pool_threads={})\n",
                if inv.work_kind.is_empty() {
                    "none"
                } else {
                    &inv.work_kind
                },
                inv.work_item_count,
                inv.pool_thread_count
            ));
            text.push_str(&format!("    Live timers: {}\n", inv.live_timer_count));
            text.push_str(&format!("    Hotkey status: {}\n", inv.hotkey_status));
            text.push_str(&format!(
                "    Playback registry count: {}\n",
                inv.playback_registry_count
            ));
        }
        text.push_str("\n");
    }

    if !report.top_file_mappings.is_empty() {
        text.push_str("Top File-Backed Mappings\n");
        text.push_str("========================\n");
        text.push_str("These are typically shared libraries, themes, fonts, graphics stacks, and other mapped files.\n\n");
        for mapping in &report.top_file_mappings {
            text.push_str(&format!(
                "- {}: RSS={} kB, PSS={} kB, Private={} kB, Shared={} kB, Regions={}\n",
                mapping.path,
                mapping.rss_kb,
                mapping.pss_kb,
                mapping.private_clean_kb + mapping.private_dirty_kb,
                mapping.shared_clean_kb + mapping.shared_dirty_kb,
                mapping.region_count
            ));
        }
        text.push('\n');
    }

    std::fs::write(&text_path, text)?;
    info!("Memory report text summary written to: {}", text_path);

    Ok(())
}

#[cfg(test)]
mod tests {
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
    fn test_estimate_config_bytes() {
        let mut config = Config::default();

        let sound = Sound::new("Test".to_string(), "/path/test.wav".to_string());
        config.sounds.push(sound);

        let bytes = estimate_config_bytes(&config);
        assert!(bytes > 0);

        assert!(bytes >= "Test".len() + "/path/test.wav".len());
    }

    #[test]
    fn test_record_phase() {
        record_phase("test_phase", None);

        if let Ok(guard) = MEMORY_REPORT.lock() {
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
    }

    #[test]
    fn test_memory_snapshot_serialization() {
        let mut snapshot = MemorySnapshot::default();
        snapshot.vm_rss_kb = Some(12345);
        snapshot.pss_kb = Some(5678);
        snapshot.threads = Some(4);

        let json = serde_json::to_string(&snapshot).unwrap();
        let deserialized: MemorySnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.vm_rss_kb, Some(12345));
        assert_eq!(deserialized.pss_kb, Some(5678));
        assert_eq!(deserialized.threads, Some(4));
    }
}
