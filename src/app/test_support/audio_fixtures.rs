use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use opus::{Application as OpusApplication, Channels as OpusChannels, Encoder as OpusEncoder};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_TEST_TONE_DURATION_MS: u32 = 200;

const MP3_MONO_44100_HEX: &str = include_str!("fixtures/mp3-mono-44100.hex");
const LIBVORBIS_MONO_44100_HEX: &str = include_str!("fixtures/libvorbis-mono-44100.ogg.hex");
const LIBVORBIS_STEREO_48000_HEX: &str = include_str!("fixtures/libvorbis-stereo-48000.ogg.hex");
const FLAC_MONO_44100_HEX: &str = include_str!("fixtures/flac-mono-44100.hex");
const AAC_ADTS_MONO_44100_HEX: &str = include_str!("fixtures/aac-adts-mono-44100.hex");
const AAC_MP4_MONO_44100_HEX: &str = include_str!("fixtures/aac-mp4-mono-44100.hex");

#[derive(Clone, Copy, Debug)]
pub enum TestEncodedFixture {
    Mp3Mono44100,
    VorbisMono44100,
    FlacMono44100,
    AacAdtsMono44100,
    AacMp4Mono44100,
}

#[derive(Clone, Copy, Debug)]
pub struct TestOggOpusFixture {
    pub extension: &'static str,
    pub channels: u16,
    pub input_rate: u32,
    pub pre_skip: u16,
    pub output_gain_q8: i16,
    pub channel_mapping_family: u8,
    pub packet_count: usize,
    pub final_granule: Option<u64>,
}

impl Default for TestOggOpusFixture {
    fn default() -> Self {
        Self {
            extension: "ogg",
            channels: 1,
            input_rate: 48_000,
            pre_skip: 0,
            output_gain_q8: 0,
            channel_mapping_family: 0,
            packet_count: 2,
            final_granule: None,
        }
    }
}

#[derive(Clone, Copy)]
pub enum TestVorbisFixture {
    Mono44100,
    Stereo48000,
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

pub fn create_test_vorbis_file(fixture: TestVorbisFixture) -> PathBuf {
    let encoded = match fixture {
        TestVorbisFixture::Mono44100 => LIBVORBIS_MONO_44100_HEX,
        TestVorbisFixture::Stereo48000 => LIBVORBIS_STEREO_48000_HEX,
    };
    create_encoded_file(encoded, "tone-libvorbis.ogg")
}

pub fn create_test_encoded_file(fixture: TestEncodedFixture, extension: &str) -> PathBuf {
    let encoded = match fixture {
        TestEncodedFixture::Mp3Mono44100 => MP3_MONO_44100_HEX,
        TestEncodedFixture::VorbisMono44100 => LIBVORBIS_MONO_44100_HEX,
        TestEncodedFixture::FlacMono44100 => FLAC_MONO_44100_HEX,
        TestEncodedFixture::AacAdtsMono44100 => AAC_ADTS_MONO_44100_HEX,
        TestEncodedFixture::AacMp4Mono44100 => AAC_MP4_MONO_44100_HEX,
    };
    create_encoded_file(encoded, &format!("tone.{extension}"))
}

pub fn create_test_ogg_opus_file(fixture: TestOggOpusFixture) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lsb-test-audio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).expect("create ogg opus temp dir");
    let path = base.join(format!("tone.{}", fixture.extension));
    let serial = 0x4c53424f;
    let mut writer = PacketWriter::new(Vec::new());
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(fixture.channels as u8);
    head.extend_from_slice(&fixture.pre_skip.to_le_bytes());
    head.extend_from_slice(&fixture.input_rate.to_le_bytes());
    head.extend_from_slice(&fixture.output_gain_q8.to_le_bytes());
    head.push(fixture.channel_mapping_family);
    writer
        .write_packet(
            head.into_boxed_slice(),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .expect("write opus head");

    let vendor = b"linux-soundboard-test";
    let mut tags = b"OpusTags".to_vec();
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes());
    writer
        .write_packet(
            tags.into_boxed_slice(),
            serial,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .expect("write opus tags");

    let opus_channels = match fixture.channels {
        1 => OpusChannels::Mono,
        2 => OpusChannels::Stereo,
        channels => panic!("test Opus encoder supports one or two channels, got {channels}"),
    };
    let mut encoder = OpusEncoder::new(48_000, opus_channels, OpusApplication::Audio)
        .expect("create opus encoder");
    let frame_samples = 960usize;
    for packet_index in 0..fixture.packet_count {
        let mut pcm = vec![0.0f32; frame_samples * fixture.channels as usize];
        for (sample_index, sample) in pcm.iter_mut().enumerate() {
            let frame = packet_index * frame_samples + sample_index / fixture.channels as usize;
            let phase = 2.0 * std::f32::consts::PI * 440.0 * frame as f32 / 48_000.0;
            *sample = phase.sin() * 0.25;
        }
        let mut encoded = vec![0; 4_000];
        let len = encoder
            .encode_float(&pcm, &mut encoded)
            .expect("encode opus frame");
        encoded.truncate(len);
        let is_last = packet_index + 1 == fixture.packet_count;
        let granule = if is_last {
            fixture
                .final_granule
                .unwrap_or((fixture.packet_count * frame_samples) as u64)
        } else {
            ((packet_index + 1) * frame_samples) as u64
        };
        writer
            .write_packet(
                encoded.into_boxed_slice(),
                serial,
                if is_last {
                    PacketWriteEndInfo::EndStream
                } else {
                    PacketWriteEndInfo::NormalPacket
                },
                granule,
            )
            .expect("write opus packet");
    }

    fs::write(&path, writer.into_inner()).expect("write ogg opus fixture");
    path
}

fn create_encoded_file(encoded: &str, file_name: &str) -> PathBuf {
    let bytes = decode_hex_fixture(encoded);
    let base = std::env::temp_dir().join(format!("lsb-test-audio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).expect("create temp audio dir");
    let path = base.join(file_name);
    fs::write(&path, bytes).expect("write encoded audio test fixture");
    path
}

fn decode_hex_fixture(encoded: &str) -> Vec<u8> {
    let digits = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    assert_eq!(digits.len() % 2, 0, "hex fixture must contain byte pairs");

    digits
        .chunks_exact(2)
        .map(|pair| (decode_hex_nibble(pair[0]) << 4) | decode_hex_nibble(pair[1]))
        .collect()
}

fn decode_hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid hex digit in test fixture"),
    }
}

pub fn cleanup_test_audio_path(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}
