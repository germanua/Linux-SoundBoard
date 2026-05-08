//! Audio duration probing helpers.

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::{FormatOptions, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

fn time_to_ms(time: Time) -> Option<u64> {
    if !time.frac.is_finite() || time.frac.is_sign_negative() {
        return None;
    }

    let duration =
        Duration::from_secs(time.seconds).checked_add(Duration::from_secs_f64(time.frac))?;
    let millis = duration.as_millis() as u64;
    (millis > 0).then_some(millis)
}

/// Estimate duration from file size and codec hints.
fn estimate_duration_from_file(path: &str, sample_rate: u32, channels: u8) -> Option<u64> {
    let metadata = File::open(path).ok()?.metadata().ok()?;
    let file_size_bytes = metadata.len();

    let estimated_bitrate = match Path::new(path)
        .extension()?
        .to_str()?
        .to_lowercase()
        .as_str()
    {
        "mp3" => 192_000,
        "ogg" | "opus" => 128_000,
        "flac" => 800_000,
        "wav" => {
            if file_size_bytes > 44 {
                let data_bytes = file_size_bytes - 44;
                let bytes_per_sample = 2;
                let total_samples = data_bytes / (bytes_per_sample * channels as u64);
                let duration_secs = total_samples / sample_rate as u64;
                return Some(duration_secs * 1000);
            }
            return None;
        }
        "aac" | "m4a" | "mp4" => 192_000,
        _ => 192_000,
    };

    let duration_secs = (file_size_bytes * 8) / estimated_bitrate;
    let duration_ms = duration_secs * 1000;

    if duration_ms < 100 || duration_ms > 24 * 60 * 60 * 1000 {
        return None;
    }

    Some(duration_ms)
}

fn is_strict_audio_container(path: &str) -> bool {
    matches!(
        Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "aac" | "m4a" | "mp4")
    )
}

fn is_audio_track(track: &Track) -> bool {
    track.codec_params.codec != CODEC_TYPE_NULL && track.codec_params.sample_rate.is_some()
}

pub fn probe_duration_ms(path: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let _file_size = file.metadata().ok()?.len();
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let format_opts = FormatOptions {
        enable_gapless: true,
        ..Default::default()
    };

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &MetadataOptions::default())
        .ok()?;

    let format = probed.format;
    let strict_audio_container = is_strict_audio_container(path);
    let track = format
        .tracks()
        .iter()
        .find(|track| is_audio_track(track))
        .or_else(|| {
            (!strict_audio_container)
                .then(|| {
                    format
                        .default_track()
                        .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                })
                .flatten()
        })
        .or_else(|| {
            (!strict_audio_container)
                .then(|| {
                    format
                        .tracks()
                        .iter()
                        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                })
                .flatten()
        })?;

    let params = &track.codec_params;

    if let (Some(time_base), Some(n_frames)) = (params.time_base, params.n_frames) {
        if let Some(duration) = time_to_ms(time_base.calc_time(n_frames)) {
            return Some(duration);
        }
    }

    let sample_rate = params.sample_rate.unwrap_or(44100);
    let channels = params.channels.map(|c| c.count() as u8).unwrap_or(2);

    estimate_duration_from_file(path, sample_rate, channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_to_ms_keeps_fractional_precision() {
        assert_eq!(time_to_ms(Time::new(12, 0.9)), Some(12_900));
    }

    #[test]
    fn time_to_ms_rejects_non_finite_fraction() {
        assert_eq!(time_to_ms(Time::new(12, f64::NAN)), None);
    }
}
