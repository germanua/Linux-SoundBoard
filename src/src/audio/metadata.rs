//! Shared audio metadata probing helpers.

use std::path::Path;
use std::time::Duration;

use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::FormatOptions;
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

pub fn probe_duration_ms(path: &str) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
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
    let track = format.default_track().or_else(|| {
        format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
    })?;

    let params = &track.codec_params;
    let (time_base, n_frames) = (params.time_base?, params.n_frames?);
    time_to_ms(time_base.calc_time(n_frames))
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
