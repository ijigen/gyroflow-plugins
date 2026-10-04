//! Experimental CinemaDNG carrier for Gyroflow Protobuf telemetry.
//!
//! This reader recognizes a private IFD0 tag (65000) containing one serialized
//! `gyroflow_proto::Main` message per DNG frame. The tag assignment is a
//! prototype convention, not a CinemaDNG or DNG standard. Only TIFF metadata
//! and the bounded tag payload are read; image strips/tiles are never decoded.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use gyroflow_core::telemetry_parser::gyroflow::{gyroflow_proto, proto_json};
use prost::Message;

const PROTOTYPE_TAG: u16 = 65000;
const MAX_PROTO_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSONL_BYTES: usize = 512 * 1024 * 1024;
const MAX_FRAMES: usize = 1_000_000;
const MAX_IFD_ENTRIES: u64 = 4096;
const MAX_DIMENSION: u32 = 16_384;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_FPS: f64 = 1000.0;

/// The telemetry stream and clip properties recovered from a numeric DNG sequence.
#[derive(Debug)]
pub struct SequenceData {
    /// Canonical Gyroflow Protobuf JSONL, with one `Main` record per frame.
    pub jsonl: Vec<u8>,
    pub frame_count: usize,
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    /// Clockwise image rotation in degrees, normalized to 0, 90, 180 or 270.
    pub rotation: i32,
}

#[derive(Clone, Copy)]
enum ByteOrder {
    Little,
    Big,
}

impl ByteOrder {
    fn u16(self, bytes: &[u8]) -> u16 {
        let bytes: [u8; 2] = bytes.try_into().expect("two bytes");
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: &[u8]) -> u32 {
        let bytes: [u8; 4] = bytes.try_into().expect("four bytes");
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }
}

/// Read a numbered DNG sequence containing `path`.
///
/// Each file must have a numeric suffix before `.dng`, the suffixes must be
/// consecutive, and every frame must contain the experimental private tag.
/// This entry point intentionally does not search other IFDs or infer telemetry
/// from ordinary CDNG metadata.
pub fn read_embedded_protobuf_sequence(path: &Path) -> Result<SequenceData, String> {
    let paths = sequence_paths(path)?;
    let mut jsonl = Vec::new();
    let mut expected_header: Option<gyroflow_proto::Header> = None;
    let mut clip_properties = None;

    for (index, frame_path) in paths.iter().enumerate() {
        let payload = read_private_tag(frame_path)
            .map_err(|error| format!("{}: {error}", frame_path.display()))?;
        let main = gyroflow_proto::Main::decode(payload.as_slice())
            .map_err(|error| format!("{}: invalid Gyroflow Protobuf: {error}", frame_path.display()))?;

        if main.magic_string != "GyroflowProtobuf" {
            return Err(format!("{}: invalid Gyroflow Protobuf magic", frame_path.display()));
        }
        if main.protocol_version != 1 {
            return Err(format!("{}: unsupported Gyroflow Protobuf version {}", frame_path.display(), main.protocol_version));
        }

        if index == 0 {
            let header = main.header.as_ref()
                .ok_or_else(|| format!("{}: first frame has no Protobuf header", frame_path.display()))?;
            if header.camera.is_none() {
                return Err(format!("{}: Protobuf header has no camera metadata", frame_path.display()));
            }
            let clip = header.clip.as_ref()
                .ok_or_else(|| format!("{}: Protobuf header has no clip metadata", frame_path.display()))?;
            if clip.frame_width == 0 || clip.frame_height == 0
                || clip.frame_width > MAX_DIMENSION || clip.frame_height > MAX_DIMENSION
                || u64::from(clip.frame_width) * u64::from(clip.frame_height) > MAX_PIXELS {
                return Err(format!("{}: invalid frame dimensions in Protobuf header", frame_path.display()));
            }
            // Keep the same file-rate precedence as the upstream JSONL parser.
            let mut fps = None;
            for rate in [clip.file_frame_rate, clip.record_frame_rate, clip.sensor_frame_rate] {
                if rate == 0.0 { continue; }
                if !rate.is_finite() || rate < 0.0 || f64::from(rate) > MAX_FPS {
                    return Err(format!("{}: invalid frame rate in Protobuf header", frame_path.display()));
                }
                fps = Some(rate as f64);
                break;
            }
            let fps = fps.ok_or_else(|| format!("{}: missing frame rate in Protobuf header", frame_path.display()))?;
            let rotation = clip.rotation_degrees.rem_euclid(360);
            if rotation % 90 != 0 {
                return Err(format!("{}: unsupported rotation {} degrees", frame_path.display(), clip.rotation_degrees));
            }
            clip_properties = Some((clip.frame_width as usize, clip.frame_height as usize, fps, rotation));
            expected_header = Some(header.clone());
        } else if let Some(header) = &main.header {
            if Some(header) != expected_header.as_ref() {
                return Err(format!("{}: Protobuf header changed within the sequence", frame_path.display()));
            }
        }

        let frame = main.frame.as_ref()
            .ok_or_else(|| format!("{}: missing Protobuf frame metadata", frame_path.display()))?;
        let expected_number = u32::try_from(index + 1)
            .map_err(|_| "DNG sequence has too many frames".to_owned())?;
        if frame.frame_number != expected_number {
            return Err(format!("{}: Protobuf frame number {} does not match expected {}", frame_path.display(), frame.frame_number, expected_number));
        }

        let line = proto_json::to_jsonl_line(&main);
        if line.is_empty() {
            return Err(format!("{}: could not serialize Protobuf metadata", frame_path.display()));
        }
        let new_len = jsonl.len().checked_add(line.len()).and_then(|len| len.checked_add(1))
            .ok_or_else(|| "CDNG Protobuf JSONL stream is too large".to_owned())?;
        if new_len > MAX_JSONL_BYTES {
            return Err("CDNG Protobuf JSONL stream is too large".to_owned());
        }
        jsonl.extend_from_slice(line.as_bytes());
        jsonl.push(b'\n');
    }

    let (width, height, fps, rotation) = clip_properties.expect("nonempty sequence");
    Ok(SequenceData { jsonl, frame_count: paths.len(), width, height, fps, rotation })
}

fn sequence_paths(first: &Path) -> Result<Vec<PathBuf>, String> {
    if !first.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("dng")) {
        return Err(format!("{}: expected a DNG file", first.display()));
    }
    let stem = first.file_stem().and_then(|stem| stem.to_str())
        .ok_or_else(|| format!("{}: DNG filename is not UTF-8", first.display()))?;
    let prefix_len = stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (prefix, digits) = stem.split_at(prefix_len);
    if digits.is_empty() {
        return Err(format!("{}: DNG filename needs a numeric frame suffix", first.display()));
    }
    let first_number: u64 = digits.parse()
        .map_err(|_| format!("{}: DNG frame number is too large", first.display()))?;
    let parent = first.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut numbered = BTreeMap::<u64, PathBuf>::new();
    for entry in fs::read_dir(parent).map_err(|error| format!("{}: {error}", parent.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", parent.display()))?;
        let path = entry.path();
        if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("dng")) {
            continue;
        }
        let Some(other_stem) = path.file_stem().and_then(|name| name.to_str()) else { continue };
        let Some(other_digits) = other_stem.strip_prefix(prefix) else { continue };
        if other_digits.is_empty() || !other_digits.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let number: u64 = other_digits.parse()
            .map_err(|_| format!("{}: DNG frame number is too large", path.display()))?;
        if numbered.insert(number, path).is_some() {
            return Err(format!("duplicate DNG frame number {number} in {}", parent.display()));
        }
        if numbered.len() > MAX_FRAMES {
            return Err("DNG sequence has too many frames".to_owned());
        }
    }

    let first_path = numbered.get(&first_number)
        .ok_or_else(|| format!("{}: selected DNG frame does not exist", first.display()))?;
    if first_path.file_name() != first.file_name() {
        return Err(format!("{}: selected DNG frame does not exist", first.display()));
    }
    let mut expected = *numbered.first_key_value().expect("selected frame exists").0;
    let mut paths = Vec::with_capacity(numbered.len());
    for (number, path) in numbered {
        if number != expected {
            return Err(format!("DNG sequence is missing frame {expected} before {}", path.display()));
        }
        if !path.is_file() {
            return Err(format!("{}: DNG frame is not a regular file", path.display()));
        }
        paths.push(path);
        expected = expected.checked_add(1)
            .ok_or_else(|| "DNG frame number overflow".to_owned())?;
    }
    Ok(paths)
}

fn read_exact_at(file: &mut File, file_len: u64, offset: u64, out: &mut [u8]) -> Result<(), String> {
    let len = u64::try_from(out.len()).map_err(|_| "TIFF range is too large".to_owned())?;
    if offset.checked_add(len).is_none_or(|end| end > file_len) {
        return Err("TIFF offset or length exceeds file bounds".to_owned());
    }
    file.seek(SeekFrom::Start(offset)).map_err(|error| error.to_string())?;
    file.read_exact(out).map_err(|error| error.to_string())
}

fn read_private_tag(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let file_len = file.metadata().map_err(|error| error.to_string())?.len();
    let mut header = [0_u8; 8];
    read_exact_at(&mut file, file_len, 0, &mut header)?;
    let byte_order = if &header[..2] == b"II" {
        ByteOrder::Little
    } else if &header[..2] == b"MM" {
        ByteOrder::Big
    } else {
        return Err("not a TIFF/DNG file".to_owned());
    };
    match byte_order.u16(&header[2..4]) {
        42 => {}
        43 => return Err("BigTIFF is not supported".to_owned()),
        _ => return Err("not a classic TIFF/DNG file".to_owned()),
    }
    let ifd_offset = u64::from(byte_order.u32(&header[4..8]));
    let mut count_bytes = [0_u8; 2];
    read_exact_at(&mut file, file_len, ifd_offset, &mut count_bytes)?;
    let entry_count = u64::from(byte_order.u16(&count_bytes));
    if entry_count > MAX_IFD_ENTRIES {
        return Err("TIFF IFD has too many entries".to_owned());
    }
    let entries_offset = ifd_offset.checked_add(2).ok_or("TIFF IFD offset overflow")?;
    let entries_len = entry_count.checked_mul(12).ok_or("TIFF IFD length overflow")?;
    if entries_offset.checked_add(entries_len).and_then(|end| end.checked_add(4)).is_none_or(|end| end > file_len) {
        return Err("TIFF IFD exceeds file bounds".to_owned());
    }

    let mut found = None;
    for index in 0..entry_count {
        let mut entry = [0_u8; 12];
        read_exact_at(&mut file, file_len, entries_offset + index * 12, &mut entry)?;
        if byte_order.u16(&entry[..2]) != PROTOTYPE_TAG {
            continue;
        }
        if found.is_some() {
            return Err("duplicate experimental Protobuf tag in IFD0".to_owned());
        }
        let kind = byte_order.u16(&entry[2..4]);
        if kind != 1 && kind != 7 {
            return Err("experimental Protobuf tag must be TIFF BYTE or UNDEFINED".to_owned());
        }
        let count = usize::try_from(byte_order.u32(&entry[4..8]))
            .map_err(|_| "Protobuf tag payload length is too large".to_owned())?;
        if count == 0 || count > MAX_PROTO_BYTES {
            return Err("Protobuf tag payload length is invalid or too large".to_owned());
        }
        let mut payload = vec![0_u8; count];
        if count <= 4 {
            payload.copy_from_slice(&entry[8..8 + count]);
        } else {
            let offset = u64::from(byte_order.u32(&entry[8..12]));
            read_exact_at(&mut file, file_len, offset, &mut payload)?;
        }
        found = Some(payload);
    }
    found.ok_or_else(|| "experimental Protobuf tag 65000 is absent from IFD0".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("gyroflow-cdng-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn frame(&self, number: u32) -> PathBuf {
            self.0.join(format!("shot_{number:04}.dng"))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    fn message(number: u32, header: bool) -> gyroflow_proto::Main {
        let clip = gyroflow_proto::header::ClipMetadata {
            frame_width: 1920,
            frame_height: 1080,
            file_frame_rate: 23.976,
            rotation_degrees: -90,
            ..Default::default()
        };
        gyroflow_proto::Main {
            magic_string: "GyroflowProtobuf".to_owned(),
            protocol_version: 1,
            header: header.then(|| gyroflow_proto::Header {
                camera: Some(Default::default()),
                clip: Some(clip),
            }),
            frame: Some(gyroflow_proto::FrameMetadata { frame_number: number, ..Default::default() }),
        }
    }

    fn write_dng(path: &Path, main: &gyroflow_proto::Main, order: ByteOrder, kind: u16) {
        let payload = main.encode_to_vec();
        let mut bytes = Vec::new();
        let put_u16 = |out: &mut Vec<u8>, n: u16| match order {
            ByteOrder::Little => out.extend_from_slice(&n.to_le_bytes()),
            ByteOrder::Big => out.extend_from_slice(&n.to_be_bytes()),
        };
        let put_u32 = |out: &mut Vec<u8>, n: u32| match order {
            ByteOrder::Little => out.extend_from_slice(&n.to_le_bytes()),
            ByteOrder::Big => out.extend_from_slice(&n.to_be_bytes()),
        };
        bytes.extend_from_slice(match order { ByteOrder::Little => b"II", ByteOrder::Big => b"MM" });
        put_u16(&mut bytes, 42);
        put_u32(&mut bytes, 8);
        put_u16(&mut bytes, 1);
        put_u16(&mut bytes, PROTOTYPE_TAG);
        put_u16(&mut bytes, kind);
        put_u32(&mut bytes, payload.len() as u32);
        put_u32(&mut bytes, 26);
        put_u32(&mut bytes, 0);
        bytes.extend_from_slice(&payload);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn reads_little_and_big_endian_sequence_as_jsonl() {
        let dir = TestDir::new();
        write_dng(&dir.frame(1), &message(1, true), ByteOrder::Little, 1);
        write_dng(&dir.frame(2), &message(2, false), ByteOrder::Big, 7);
        let sequence = read_embedded_protobuf_sequence(&dir.frame(1)).unwrap();
        assert_eq!(sequence.frame_count, 2);
        assert_eq!((sequence.width, sequence.height, sequence.rotation), (1920, 1080, 270));
        assert!((sequence.fps - 23.976).abs() < 0.001);
        let lines: Vec<_> = sequence.jsonl.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert!(std::str::from_utf8(lines[0]).unwrap().contains("GyroflowProtobuf"));
        assert!(std::str::from_utf8(lines[1]).unwrap().contains("frameNumber"));

        let from_second = read_embedded_protobuf_sequence(&dir.frame(2)).unwrap();
        assert_eq!(from_second.frame_count, 2);
        assert_eq!(from_second.jsonl, sequence.jsonl);
    }

    #[test]
    fn core_consumes_two_frame_sequence_as_jsonl() {
        let dir = TestDir::new();
        let mut first = message(1, true);
        first.frame.as_mut().unwrap().start_timestamp_us = 1_000_000.0;
        first.frame.as_mut().unwrap().end_timestamp_us = 1_005_000.0;
        let mut second = message(2, false);
        second.frame.as_mut().unwrap().start_timestamp_us = 1_041_708.0;
        second.frame.as_mut().unwrap().end_timestamp_us = 1_046_708.0;
        write_dng(&dir.frame(1), &first, ByteOrder::Little, 7);
        write_dng(&dir.frame(2), &second, ByteOrder::Little, 7);

        let sequence = read_embedded_protobuf_sequence(&dir.frame(2)).unwrap();
        let metadata = gyroflow_core::telemetry_parser::util::VideoMetadata {
            width: sequence.width,
            height: sequence.height,
            fps: sequence.fps,
            duration_s: sequence.frame_count as f64 / sequence.fps,
            rotation: sequence.rotation,
        };
        let virtual_path = dir.0.join("embedded-telemetry.jsonl");
        let virtual_url = gyroflow_core::filesystem::path_to_url(&virtual_path.to_string_lossy());
        let mut stream = std::io::Cursor::new(sequence.jsonl.as_slice());
        let manager = gyroflow_core::StabilizationManager::default();
        manager.load_video_file(&mut stream, sequence.jsonl.len(), &virtual_url, Some(metadata), true).unwrap();
        assert_eq!(manager.gyro.read().file_metadata.read().per_frame_time_offsets.len(), 2);
    }

    #[test]
    fn rejects_gap_in_dng_numbering() {
        let dir = TestDir::new();
        write_dng(&dir.frame(1), &message(1, true), ByteOrder::Little, 7);
        write_dng(&dir.frame(3), &message(2, false), ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&dir.frame(1)).unwrap_err().contains("missing frame 2"));
    }

    #[test]
    fn accepts_numeric_suffix_that_grows_in_width() {
        let dir = TestDir::new();
        let first = dir.frame(9999);
        let second = dir.frame(10000);
        write_dng(&first, &message(1, true), ByteOrder::Little, 7);
        write_dng(&second, &message(2, false), ByteOrder::Little, 7);
        assert_eq!(read_embedded_protobuf_sequence(&second).unwrap().frame_count, 2);
    }

    #[test]
    fn rejects_protobuf_frame_gap() {
        let dir = TestDir::new();
        write_dng(&dir.frame(1), &message(1, true), ByteOrder::Little, 7);
        write_dng(&dir.frame(2), &message(3, false), ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&dir.frame(1)).unwrap_err().contains("frame number 3"));
    }

    #[test]
    fn rejects_out_of_bounds_payload_without_reading_pixels() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        write_dng(&path, &message(1, true), ByteOrder::Little, 7);
        let mut bytes = fs::read(&path).unwrap();
        bytes[18..22].copy_from_slice(&(MAX_PROTO_BYTES as u32).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("exceeds file bounds"));
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        let mut main = message(1, true);
        main.magic_string = "other".to_owned();
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("invalid Gyroflow Protobuf magic"));
        main.magic_string = "GyroflowProtobuf".to_owned();
        main.protocol_version = 2;
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("unsupported Gyroflow Protobuf version"));
    }

    #[test]
    fn rejects_missing_clip_and_invalid_rate() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        let mut main = message(1, true);
        main.header.as_mut().unwrap().clip = None;
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("no clip metadata"));

        main.header.as_mut().unwrap().clip = Some(gyroflow_proto::header::ClipMetadata {
            frame_width: 1920,
            frame_height: 1080,
            file_frame_rate: f32::NAN,
            record_frame_rate: 24.0,
            ..Default::default()
        });
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("invalid frame rate"));
    }

    #[test]
    fn rejects_implausible_clip_properties() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        let mut main = message(1, true);
        let clip = main.header.as_mut().unwrap().clip.as_mut().unwrap();
        clip.frame_width = MAX_DIMENSION + 1;
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("invalid frame dimensions"));

        let clip = main.header.as_mut().unwrap().clip.as_mut().unwrap();
        clip.frame_width = 12_000;
        clip.frame_height = 12_000;
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("invalid frame dimensions"));

        let clip = main.header.as_mut().unwrap().clip.as_mut().unwrap();
        clip.frame_width = 1920;
        clip.frame_height = 1080;
        clip.file_frame_rate = 1001.0;
        write_dng(&path, &main, ByteOrder::Little, 7);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("invalid frame rate"));
    }

    #[test]
    fn rejects_wrong_tiff_tag_type() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        write_dng(&path, &message(1, true), ByteOrder::Big, 2);
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("BYTE or UNDEFINED"));
    }

    #[test]
    fn rejects_big_tiff() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        fs::write(&path, b"II\x2b\0\x08\0\0\0").unwrap();
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("BigTIFF"));
    }

    #[test]
    fn rejects_excessive_ifd_entry_count() {
        let dir = TestDir::new();
        let path = dir.frame(1);
        write_dng(&path, &message(1, true), ByteOrder::Little, 7);
        let mut bytes = fs::read(&path).unwrap();
        bytes[8..10].copy_from_slice(&((MAX_IFD_ENTRIES + 1) as u16).to_le_bytes());
        fs::write(&path, bytes).unwrap();
        assert!(read_embedded_protobuf_sequence(&path).unwrap_err().contains("too many entries"));
    }
}
