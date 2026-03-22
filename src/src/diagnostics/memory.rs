use log::info;

#[derive(Default, Debug, Clone)]
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

    Some(snapshot)
}

pub fn log_memory_snapshot(tag: &str) {
    if let Some(m) = read_memory_snapshot() {
        info!(
            "RAM [{}] VmRSS={}kB VmHWM={}kB VmData={}kB VmSize={}kB VmSwap={}kB RssAnon={}kB RssFile={}kB RssShmem={}kB Threads={}",
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
        );
    } else {
        info!("RAM [{}] unable to read /proc/self/status", tag);
    }
}
