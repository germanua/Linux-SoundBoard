#![cfg(test)]

mod common;
mod config_tests;

fn process_pss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()?
        .lines()
        .find(|line| line.starts_with("Pss:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[test]
#[ignore = "release-scale gate for the 156,000-sound library"]
#[allow(clippy::print_stderr)]
fn benchmark_large_config_serialization() {
    let config = common::ConfigBuilder::new()
        .with_generated_sounds(156_000)
        .with_partitioned_tabs(8)
        .build();
    let pss_kib = process_pss_kib().expect("read process memory after building fixture");
    let cfg = config.lock();

    let started = std::time::Instant::now();
    serde_json::to_writer_pretty(std::io::sink(), &*cfg).expect("serialize large config");
    let elapsed = started.elapsed();

    eprintln!(
        "large-config sounds={} tabs={} serialize_ms={} pss_kib={}",
        cfg.sounds.len(),
        cfg.tabs.len(),
        elapsed.as_millis(),
        pss_kib,
    );
    assert!(elapsed <= std::time::Duration::from_millis(100));
    assert!(pss_kib < 102_400);
}
