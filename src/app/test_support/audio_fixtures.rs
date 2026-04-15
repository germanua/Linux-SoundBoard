use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_TEST_TONE_DURATION_MS: u32 = 200;

pub fn build_test_wave_payload() -> Vec<u8> {
    build_test_wave_payload_with_duration(DEFAULT_TEST_TONE_DURATION_MS)
}

pub fn build_test_wave_payload_with_duration(duration_ms: u32) -> Vec<u8> {
    let sample_rate = 44_100_u32;
    let channels = 2_u16;
    let bits_per_sample = 16_u16;
    let sample_count = ((sample_rate as u64 * duration_ms.max(1) as u64) / 1000).max(1) as u32;
    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let block_align = channels as usize * bytes_per_sample;
    let byte_rate = sample_rate as usize * block_align;
    let mut pcm = Vec::with_capacity(sample_count as usize * block_align);

    for frame in 0..sample_count {
        let phase = 2.0_f32 * std::f32::consts::PI * 440.0 * frame as f32 / sample_rate as f32;
        let sample = (phase.sin() * 12_000.0) as i16;
        for _ in 0..channels {
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
    }

    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;

    let mut bytes = Vec::with_capacity(44 + pcm.len());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(byte_rate as u32).to_le_bytes());
    bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&pcm);
    bytes
}

pub fn create_test_audio_file(ext: &str) -> PathBuf {
    create_test_audio_file_with_duration(ext, DEFAULT_TEST_TONE_DURATION_MS)
}

pub fn create_test_audio_file_with_duration(ext: &str, duration_ms: u32) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lsb-test-audio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).expect("create temp audio dir");
    let path = base.join(format!("tone.{ext}"));
    fs::write(&path, build_test_wave_payload_with_duration(duration_ms))
        .expect("write test audio payload");
    path
}

pub fn cleanup_test_audio_path(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}