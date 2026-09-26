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
const ALAC_M4A_MONO_44100_HEX: &str = include_str!("fixtures/alac-m4a-mono-44100.hex");
const OPUS_MP4_STEREO_48000_HEX: &str = include_str!("fixtures/opus-mp4-stereo-48000.hex");

#[derive(Clone, Copy, Debug)]
pub enum TestEncodedFixture {
    Mp3Mono44100,
    VorbisMono44100,
    FlacMono44100,
    AacAdtsMono44100,
    AacMp4Mono44100,
    AlacM4aMono44100,
    OpusMp4Stereo48000,
}

pub const TEST_OGG_OPUS_SERIAL: u32 = 0x4c53424f;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestOggOpusFinalPage {
    EndOfStream,

    NoEndOfStreamMarker,
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
    pub final_page: TestOggOpusFinalPage,
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
            final_page: TestOggOpusFinalPage::EndOfStream,
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
        TestEncodedFixture::AlacM4aMono44100 => ALAC_M4A_MONO_44100_HEX,
        TestEncodedFixture::OpusMp4Stereo48000 => OPUS_MP4_STEREO_48000_HEX,
    };
    create_encoded_file(encoded, &format!("tone.{extension}"))
}

pub fn create_test_ogg_opus_file(fixture: TestOggOpusFixture) -> PathBuf {
    let base = std::env::temp_dir().join(format!("lsb-test-audio-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).expect("create ogg opus temp dir");
    let path = base.join(format!("tone.{}", fixture.extension));
    let serial = TEST_OGG_OPUS_SERIAL;
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
        let end_info = if is_last {
            match fixture.final_page {
                TestOggOpusFinalPage::EndOfStream => PacketWriteEndInfo::EndStream,
                TestOggOpusFinalPage::NoEndOfStreamMarker => PacketWriteEndInfo::EndPage,
            }
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        writer
            .write_packet(encoded.into_boxed_slice(), serial, end_info, granule)
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

struct TestOggPage {
    serial: u32,
    header_type: u8,
    body_start: usize,
    body_len: usize,
}

fn walk_test_ogg_pages(bytes: &[u8]) -> Vec<TestOggPage> {
    let mut pages = Vec::new();
    let mut offset = 0usize;
    while offset + 27 <= bytes.len() && &bytes[offset..offset + 4] == b"OggS" {
        let serial = u32::from_le_bytes(bytes[offset + 14..offset + 18].try_into().unwrap());
        let header_type = bytes[offset + 5];
        let segment_count = bytes[offset + 26] as usize;
        let table_start = offset + 27;
        let body_start = table_start + segment_count;
        if body_start > bytes.len() {
            break;
        }
        let body_len: usize = bytes[table_start..body_start]
            .iter()
            .map(|value| usize::from(*value))
            .sum();
        if body_start + body_len > bytes.len() {
            break;
        }
        pages.push(TestOggPage {
            serial,
            header_type,
            body_start,
            body_len,
        });
        offset = body_start + body_len;
    }
    pages
}

pub fn test_ogg_has_end_of_stream_page(path: &Path) -> bool {
    let bytes = fs::read(path).expect("read Ogg fixture");
    walk_test_ogg_pages(&bytes)
        .iter()
        .any(|page| page.header_type & 0x04 != 0)
}

fn test_ogg_page_crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn test_ogg_single_packet_segments(packet_len: usize) -> Vec<u8> {
    assert!(
        packet_len < 255,
        "test fixture pages carry at most one small packet"
    );
    if packet_len == 0 {
        Vec::new()
    } else {
        vec![packet_len as u8]
    }
}

fn build_test_ogg_page(
    existing: &[u8],
    serial: u32,
    granule: u64,
    header_type: u8,
    segments: &[u8],
    body: &[u8],
    body_written: usize,
) -> Vec<u8> {
    let sequence = walk_test_ogg_pages(existing)
        .iter()
        .filter(|page| page.serial == serial)
        .count() as u32;
    let mut page = Vec::with_capacity(27 + segments.len() + body.len());
    page.extend_from_slice(b"OggS");
    page.push(0);
    page.push(header_type);
    page.extend_from_slice(&granule.to_le_bytes());
    page.extend_from_slice(&serial.to_le_bytes());
    page.extend_from_slice(&sequence.to_le_bytes());
    page.extend_from_slice(&[0; 4]);
    page.push(segments.len() as u8);
    page.extend_from_slice(segments);
    page.extend_from_slice(&body[..body_written.min(body.len())]);
    let checksum = test_ogg_page_crc32(&page);
    page[22..26].copy_from_slice(&checksum.to_le_bytes());
    page
}

pub fn append_test_ogg_page(
    path: &Path,
    serial: u32,
    granule: u64,
    header_type: u8,
    packet: &[u8],
) {
    let mut bytes = fs::read(path).expect("read Ogg fixture");
    let segments = test_ogg_single_packet_segments(packet.len());
    let page = build_test_ogg_page(
        &bytes,
        serial,
        granule,
        header_type,
        &segments,
        packet,
        packet.len(),
    );
    bytes.extend_from_slice(&page);
    fs::write(path, bytes).expect("write Ogg fixture");
}

pub fn append_truncated_test_ogg_page(
    path: &Path,
    serial: u32,
    granule: u64,
    declared_body_len: u8,
    kept_body_len: usize,
) {
    assert!(u16::from(declared_body_len) >= kept_body_len as u16);
    let mut bytes = fs::read(path).expect("read Ogg fixture");

    let page = build_test_ogg_page(
        &bytes,
        serial,
        granule,
        0,
        &[declared_body_len],
        &vec![0u8; usize::from(declared_body_len)],
        kept_body_len,
    );
    bytes.extend_from_slice(&page);
    fs::write(path, bytes).expect("write Ogg fixture");
}

pub fn corrupt_test_ogg_page_body(path: &Path, page_index: usize) {
    let mut bytes = fs::read(path).expect("read Ogg fixture");
    let page = walk_test_ogg_pages(&bytes)
        .into_iter()
        .nth(page_index)
        .unwrap_or_else(|| panic!("fixture has no page {page_index}"));
    assert!(page.body_len > 0, "page {page_index} has an empty body");
    bytes[page.body_start] ^= 0xff;
    fs::write(path, bytes).expect("write Ogg fixture");
}

pub const INVALID_TEST_OPUS_PACKET: &[u8] = &[0x03];

pub fn encode_test_opus_audio_packet(tone_offset: usize) -> Vec<u8> {
    let mut encoder = OpusEncoder::new(48_000, OpusChannels::Mono, OpusApplication::Audio)
        .expect("create opus encoder");
    let mut pcm = vec![0.0f32; 960];
    for (index, sample) in pcm.iter_mut().enumerate() {
        let frame = tone_offset * 960 + index;
        let phase = 2.0 * std::f32::consts::PI * 440.0 * frame as f32 / 48_000.0;
        *sample = phase.sin() * 0.25;
    }
    let mut encoded = vec![0; 4_000];
    let len = encoder
        .encode_float(&pcm, &mut encoded)
        .expect("encode opus frame");
    encoded.truncate(len);
    encoded
}

#[cfg(test)]
mod tests {
    use super::test_ogg_page_crc32;

    #[test]
    fn the_fixture_checksum_matches_the_vorbis_crc_parameters() {
        assert_eq!(test_ogg_page_crc32(&[61, 61, 33]), 0x9f85_8776);
    }
}
