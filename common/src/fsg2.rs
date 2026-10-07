//! Decoder for FSG2 payload kind 2: the SIGMA fp's per-frame gyro record.
//!
//! The camera copies raw sensor counts into each DNG's MakerNote tail and does
//! no arithmetic; everything that turns counts into Gyroflow Protobuf units and
//! timestamps happens here. Times are never stored: a sample's time is its
//! global index (counted from the start of the take) times the sample period,
//! and events and the frame mark are positions in that same sample stream.
//!
//! Payload layout, all integers little-endian:
//!
//! ```text
//!   0 u8   fmt_version       1
//!   1 u8   orientation       0 = "xyz"
//!   2 u16  flags             see FLAG_*
//!   4 u32  clip_id           TickTimer at record start; equal across a take
//!   8 u32  frame_seq         native frame number, 0-based
//!  12 u32  sample_period_ps  gyro sample period in picoseconds
//!  16 f32  gscale            rad/s per gyro count
//!  20 f32  ascale            g per accelerometer count
//!  24 u32  readout_ns        rolling-shutter readout of the running mode
//!  28 u16  mode_id           sensor mode, for diagnostics only
//!  30 u16  crop_x, crop_y, crop_w, crop_h   readout window, sensor pixels
//!  38 u16  frame_mark        sample position of the frame hook, from base_idx
//!  40 u32  base_idx          global index of the first sample in this block
//!  44 u16  n_samples
//!  46 u16  n_events
//!  48      samples           n_samples x i16 x, y, z (raw counts)
//!          events            n_events x { u16 pos, u8 type, u8 len, value }
//!          lens table        only with FLAG_LENS_TABLE, LENS_TABLE_BYTES
//! ```

use gyroflow_core::telemetry_parser::gyroflow::gyroflow_proto;

use crate::lensfit;

pub const FMT_VERSION: u8 = 1;
pub const HEADER_BYTES: usize = 48;

pub const FLAG_DATA_LOST: u16 = 1 << 0;
pub const FLAG_TRUNCATED: u16 = 1 << 1;
// Informational: the take's first and last blocks. The reader relies on
// clip_id and file order instead, so these are not checked.
#[allow(dead_code)]
pub const FLAG_FIRST_FRAME: u16 = 1 << 2;
#[allow(dead_code)]
pub const FLAG_LAST_FRAME: u16 = 1 << 3;
pub const FLAG_OIS_ON: u16 = 1 << 4;
pub const FLAG_EIS_ON: u16 = 1 << 5;
pub const FLAG_LENS_TABLE: u16 = 1 << 6;

pub const EVENT_LEVEL: u8 = 1;
pub const EVENT_FOCUS: u8 = 2;

/// calib_focal u32 + five u32 focus support points + 5 x 3 x 4 f64 coefficients.
#[cfg_attr(not(test), allow(dead_code))]
pub const LENS_TABLE_BYTES: usize = 4 + 5 * 4 + 5 * 3 * 4 * 8;

/// IMX410 active area and pitch: 35.9 mm across 6000 pixels.
const SENSOR_PIXEL_WIDTH: u32 = 6000;
const SENSOR_PIXEL_HEIGHT: u32 = 4000;
const PIXEL_PITCH_NM: u32 = 5983;
const STANDARD_GRAVITY: f64 = 9.80665;

/// Constant from the frame hook to the readout of the first sensor row, in
/// microseconds: the first row is read out this long before the hook runs.
/// Measured 2026-10-07 by matching the frames' optical-flow rotation against
/// the gyro (A001_044 FHD 59.94p, readout 10.556 ms: -13.28 ms; A001_042 OG3K
/// 29.97p, readout 12.435 ms: -13.42 ms; both exposure 4 ms, yaw correlation
/// 0.97 and 0.9996).
pub const HOOK_TO_READOUT_US: f64 = -13_350.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub pos: u16,
    pub kind: u8,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LensTable {
    /// Focal length the correction data was measured at, in tenths of a mm.
    pub calib_focal_tenths_mm: u32,
    /// Focus support points, 2^24 / object distance in mm (0 = infinity).
    pub axis: [u32; 5],
    /// Five focus nodes x three colour planes x four radial coefficients.
    pub nodes: [[[f64; 4]; 3]; 5],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub orientation: u8,
    pub flags: u16,
    pub clip_id: u32,
    pub frame_seq: u32,
    pub sample_period_ps: u32,
    pub gscale: f32,
    pub ascale: f32,
    pub readout_ns: u32,
    pub mode_id: u16,
    pub crop: [u16; 4],
    pub frame_mark: u16,
    pub base_idx: u32,
    pub samples: Vec<[i16; 3]>,
    pub events: Vec<Event>,
    pub lens_table: Option<LensTable>,
}

struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize, what: &str) -> Result<&'a [u8], String> {
        let bytes = self.at.checked_add(len)
            .and_then(|end| self.data.get(self.at..end))
            .ok_or_else(|| format!("kind 2 payload truncated in {what}"))?;
        self.at += len;
        Ok(bytes)
    }
    fn u8(&mut self, what: &str) -> Result<u8, String> { Ok(self.take(1, what)?[0]) }
    fn u16(&mut self, what: &str) -> Result<u16, String> { Ok(u16::from_le_bytes(self.take(2, what)?.try_into().unwrap())) }
    fn i16(&mut self, what: &str) -> Result<i16, String> { Ok(i16::from_le_bytes(self.take(2, what)?.try_into().unwrap())) }
    fn u32(&mut self, what: &str) -> Result<u32, String> { Ok(u32::from_le_bytes(self.take(4, what)?.try_into().unwrap())) }
    fn f32(&mut self, what: &str) -> Result<f32, String> { Ok(f32::from_le_bytes(self.take(4, what)?.try_into().unwrap())) }
    fn f64(&mut self, what: &str) -> Result<f64, String> { Ok(f64::from_le_bytes(self.take(8, what)?.try_into().unwrap())) }
}

pub fn parse(payload: &[u8]) -> Result<Record, String> {
    let mut c = Cursor { data: payload, at: 0 };
    let version = c.u8("header")?;
    if version != FMT_VERSION {
        return Err(format!("unsupported kind 2 format version {version}"));
    }
    let orientation = c.u8("header")?;
    if orientation != 0 {
        return Err(format!("unknown kind 2 orientation code {orientation}"));
    }
    let flags = c.u16("header")?;
    let clip_id = c.u32("header")?;
    let frame_seq = c.u32("header")?;
    let sample_period_ps = c.u32("header")?;
    let gscale = c.f32("header")?;
    let ascale = c.f32("header")?;
    let readout_ns = c.u32("header")?;
    let mode_id = c.u16("header")?;
    let crop = [c.u16("header")?, c.u16("header")?, c.u16("header")?, c.u16("header")?];
    let frame_mark = c.u16("header")?;
    let base_idx = c.u32("header")?;
    let n_samples = c.u16("header")? as usize;
    let n_events = c.u16("header")? as usize;
    debug_assert_eq!(c.at, HEADER_BYTES);

    if sample_period_ps == 0 {
        return Err("kind 2 sample period is zero".to_owned());
    }
    if !(gscale.is_finite() && gscale > 0.0) || !(ascale.is_finite() && ascale > 0.0) {
        return Err("kind 2 gyro/accelerometer scale is not a positive number".to_owned());
    }

    let mut samples = Vec::with_capacity(n_samples);
    for _ in 0..n_samples {
        samples.push([c.i16("samples")?, c.i16("samples")?, c.i16("samples")?]);
    }
    let mut events = Vec::with_capacity(n_events);
    for _ in 0..n_events {
        let pos = c.u16("events")?;
        let kind = c.u8("events")?;
        let len = c.u8("events")? as usize;
        let value = c.take(len, "events")?.to_vec();
        let expected = match kind {
            EVENT_LEVEL => Some(6),
            EVENT_FOCUS => Some(4),
            _ => None, // unknown kinds are skipped by length
        };
        if expected.is_some_and(|n| n != len) {
            return Err(format!("kind 2 event type {kind} has length {len}"));
        }
        events.push(Event { pos, kind, value });
    }
    let lens_table = if flags & FLAG_LENS_TABLE != 0 {
        let calib_focal_tenths_mm = c.u32("lens table")?;
        let mut axis = [0_u32; 5];
        for value in &mut axis { *value = c.u32("lens table")?; }
        let mut nodes = [[[0_f64; 4]; 3]; 5];
        for node in &mut nodes {
            for plane in node.iter_mut() {
                for coefficient in plane.iter_mut() { *coefficient = c.f64("lens table")?; }
            }
        }
        Some(LensTable { calib_focal_tenths_mm, axis, nodes })
    } else {
        None
    };
    if c.at != payload.len() {
        return Err(format!("kind 2 payload has {} unexpected trailing bytes", payload.len() - c.at));
    }
    Ok(Record {
        orientation, flags, clip_id, frame_seq, sample_period_ps, gscale, ascale, readout_ns,
        mode_id, crop, frame_mark, base_idx, samples, events, lens_table,
    })
}

/// Per-frame values the DNG itself already carries, read from its EXIF/DNG tags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DngInfo {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub software: Option<String>,
    pub lens_model: Option<String>,
    /// DefaultCropSize: the size of the developed picture.
    pub frame_size: Option<(u32, u32)>,
    pub frame_rate: Option<f64>,
    pub exposure_s: Option<f64>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    pub focal_length_mm: Option<f64>,
    pub subject_distance_m: Option<f64>,
    /// Green-plane kr0..kr3 of the frame's WarpRectilinear opcode: the camera's
    /// distortion, already interpolated for this frame's focus.
    pub warp_rectilinear: Option<[f64; 4]>,
}

/// Turns a take's kind 2 records, in file order, into Gyroflow Protobuf messages.
///
/// Samples are deduplicated by global index, so guard samples that two
/// neighbouring blocks share are emitted once and a dropped block leaves a gap
/// rather than a shift. The accelerometer is sample-and-hold from level events.
/// How a take is turned into Gyroflow's messages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Place frames on a regular grid fitted to the frame hooks instead of at
    /// each hook. The sensor exposes on a fixed clock, but the hook runs when
    /// the recording task gets to it, a few milliseconds late by a varying
    /// amount (rms 1.8 ms at 29.97p, 2.9 ms at 59.94p on 2026-10-06 takes).
    pub regular_frame_timing: bool,
}

impl Default for Options {
    fn default() -> Self { Self { regular_frame_timing: true } }
}

pub fn to_messages(frames: &[(Record, DngInfo)]) -> Result<Vec<gyroflow_proto::Main>, String> {
    to_messages_with(frames, &Options::default())
}

/// Each frame's hook position, in samples from the take's start: as recorded,
/// or on the line through them whose slope is the nominal samples per frame.
/// The line keeps the hooks' mean, so HOOK_TO_READOUT_US still applies. Falls
/// back to the recorded positions when the hooks do not follow the frame rate
/// (fewer than three frames, or a fitted slope off by more than 1 %).
pub fn frame_positions(frames: &[(Record, DngInfo)], fps: f64, regular: bool) -> Vec<f64> {
    let hooks: Vec<f64> = frames.iter().map(|(r, _)| r.base_idx as f64 + r.frame_mark as f64).collect();
    if !regular || frames.len() < 3 || !(fps > 0.0) {
        return hooks;
    }
    let period_s = frames[0].0.sample_period_ps as f64 * 1e-12;
    if !(period_s > 0.0) {
        return hooks;
    }
    let per_frame = 1.0 / (fps * period_s);
    let seqs: Vec<f64> = frames.iter().map(|(r, _)| r.frame_seq as f64).collect();
    let n = seqs.len() as f64;
    let (ms, mh) = (seqs.iter().sum::<f64>() / n, hooks.iter().sum::<f64>() / n);
    let sxx: f64 = seqs.iter().map(|s| (s - ms).powi(2)).sum();
    let sxy: f64 = seqs.iter().zip(&hooks).map(|(s, h)| (s - ms) * (h - mh)).sum();
    if !(sxx > 0.0) || ((sxy / sxx) / per_frame - 1.0).abs() > 0.01 {
        log::warn!("Frame hooks do not follow {fps:.3} fps; keeping each frame at its hook.");
        return hooks;
    }
    let intercept = mh - per_frame * ms;
    seqs.iter().map(|s| intercept + per_frame * s).collect()
}

pub fn to_messages_with(frames: &[(Record, DngInfo)], options: &Options) -> Result<Vec<gyroflow_proto::Main>, String> {
    let (first, first_dng) = frames.first().ok_or("no kind 2 frames")?;
    let (width, height) = first_dng.frame_size
        .ok_or("first DNG has no DefaultCropSize; cannot size the clip")?;
    let fps = first_dng.frame_rate.filter(|fps| *fps > 0.0)
        .ok_or("first DNG has no FrameRate tag")?;

    for (index, (record, _)) in frames.iter().enumerate() {
        if record.clip_id != first.clip_id {
            return Err(format!("frame {} belongs to another take (clip id {:#x}, expected {:#x})", index + 1, record.clip_id, first.clip_id));
        }
        if record.sample_period_ps != first.sample_period_ps || record.gscale != first.gscale || record.ascale != first.ascale {
            return Err(format!("frame {}: sample period or scale changed within the take", index + 1));
        }
    }

    let any = |flag: u16| frames.iter().any(|(record, _)| record.flags & flag != 0);
    if any(FLAG_OIS_ON) {
        log::warn!("Lens OIS was on during this take; Gyroflow cannot undo it, so stabilisation will fight it.");
    }
    if any(FLAG_EIS_ON) {
        log::warn!("In-camera electronic stabilisation was on during this take; its warp is not recorded.");
    }
    for (index, (record, _)) in frames.iter().enumerate() {
        if record.flags & FLAG_TRUNCATED != 0 {
            log::warn!("Frame {}: the camera truncated the gyro block to fit the MakerNote.", index + 1);
        }
    }

    let period_us = first.sample_period_ps as f64 / 1.0e6;
    let gyro_dps = first.gscale as f64 * 180.0 / std::f64::consts::PI;
    let accel_ms2 = first.ascale as f64 * STANDARD_GRAVITY;
    // Accelerometer axes are a quarter turn from the gyro's: native Y is gyro
    // X, native X negated is gyro Y (fpSup gcsv_rows.S rule 2).
    let accel_in_gyro_axes = |raw: [i16; 3]| -> [f32; 3] {
        [(raw[1] as f64 * accel_ms2) as f32, (-(raw[0] as f64) * accel_ms2) as f32, (raw[2] as f64 * accel_ms2) as f32]
    };

    // Hold the first level reading of the take until the first one arrives,
    // so no sample carries an all-zero accelerometer vector.
    let mut held_accel = frames.iter()
        .flat_map(|(record, _)| record.events.iter())
        .find(|event| event.kind == EVENT_LEVEL)
        .map(|event| accel_in_gyro_axes(level_counts(event)))
        .unwrap_or([0.0; 3]);

    let header = gyroflow_proto::Header {
        camera: Some(gyroflow_proto::header::CameraMetadata {
            camera_brand: first_dng.make.clone().unwrap_or_else(|| "SIGMA".to_owned()),
            camera_model: first_dng.model.clone().unwrap_or_else(|| "fp".to_owned()),
            camera_serial_number: first_dng.serial.clone(),
            firmware_version: first_dng.software.clone(),
            lens_brand: String::new(),
            lens_model: first_dng.lens_model.clone().unwrap_or_default(),
            pixel_pitch_x_nm: PIXEL_PITCH_NM,
            pixel_pitch_y_nm: PIXEL_PITCH_NM,
            sensor_pixel_width: SENSOR_PIXEL_WIDTH,
            sensor_pixel_height: SENSOR_PIXEL_HEIGHT,
            imu_orientation: Some("xyz".to_owned()),
            ..Default::default()
        }),
        clip: Some(gyroflow_proto::header::ClipMetadata {
            frame_width: width,
            frame_height: height,
            duration_us: frames.len() as f64 * 1.0e6 / fps,
            record_frame_rate: fps as f32,
            sensor_frame_rate: fps as f32,
            file_frame_rate: fps as f32,
            imu_sample_rate: (1.0e12 / first.sample_period_ps as f64).round() as u32,
            frame_readout_time_us: first.readout_ns as f64 / 1000.0,
            frame_readout_direction: gyroflow_proto::header::clip_metadata::ReadoutDirection::TopToBottom as i32,
            pixel_aspect_ratio: 1.0,
            ..Default::default()
        }),
    };

    // The take's lens table, normally on its first frame; frames without one in
    // reach fall back to their own WarpRectilinear opcode.
    let lens_table = frames.iter().find_map(|(record, _)| record.lens_table.as_ref());

    let positions = frame_positions(frames, fps, options.regular_frame_timing);
    let mut next_global: u64 = 0;
    let mut messages = Vec::with_capacity(frames.len());
    for (index, (record, dng)) in frames.iter().enumerate() {
        let base = record.base_idx as u64;
        let time_of = |position: u64| (base + position) as f64 * period_us;
        let start = positions[index] * period_us + HOOK_TO_READOUT_US;

        // Level events in this block, by position, to hold the accelerometer.
        let mut levels: Vec<(u64, [f32; 3])> = record.events.iter()
            .filter(|event| event.kind == EVENT_LEVEL)
            .map(|event| (event.pos as u64, accel_in_gyro_axes(level_counts(event))))
            .collect();
        levels.sort_by_key(|(pos, _)| *pos);
        let mut level_iter = levels.into_iter().peekable();

        let mut imu = Vec::new();
        if record.flags & FLAG_DATA_LOST == 0 {
            for (position, raw) in record.samples.iter().enumerate() {
                let position = position as u64;
                // An event sits after the samples that preceded it: it applies
                // from the next sample on.
                while level_iter.peek().is_some_and(|(pos, _)| *pos <= position) {
                    held_accel = level_iter.next().unwrap().1;
                }
                let global = base + position;
                if global < next_global { continue; }
                next_global = global + 1;
                imu.push(gyroflow_proto::ImuData {
                    sample_timestamp_us: Some(time_of(position)),
                    gyroscope_x: (raw[0] as f64 * gyro_dps) as f32,
                    gyroscope_y: (raw[1] as f64 * gyro_dps) as f32,
                    gyroscope_z: (raw[2] as f64 * gyro_dps) as f32,
                    accelerometer_x: held_accel[0],
                    accelerometer_y: held_accel[1],
                    accelerometer_z: held_accel[2],
                    ..Default::default()
                });
            }
        }
        for (_, value) in level_iter { held_accel = value; }

        let lens = lens_data(lens_table, record, dng, width, height);
        let frame = gyroflow_proto::FrameMetadata {
            start_timestamp_us: start,
            end_timestamp_us: start + record.readout_ns as f64 / 1000.0,
            frame_number: u32::try_from(index + 1).map_err(|_| "too many frames".to_owned())?,
            iso: dng.iso,
            exposure_time_us: dng.exposure_s.map(|s| s * 1.0e6),
            crop_x: Some(record.crop[0] as f32),
            crop_y: Some(record.crop[1] as f32),
            crop_width: Some(record.crop[2] as f32),
            crop_height: Some(record.crop[3] as f32),
            lens: if lens == gyroflow_proto::LensData::default() { vec![] } else { vec![lens] },
            imu,
            ..Default::default()
        };
        messages.push(gyroflow_proto::Main {
            magic_string: "GyroflowProtobuf".to_owned(),
            protocol_version: 1,
            header: (index == 0).then(|| header.clone()),
            frame: Some(frame),
        });
    }
    Ok(messages)
}

/// One frame's lens: focal length, aperture, focus distance, and - when the
/// camera's correction data reaches this frame - the camera matrix and fisheye
/// coefficients fitted for this frame's focus, so breathing and distortion
/// follow a focus pull.
fn lens_data(table: Option<&LensTable>, record: &Record, dng: &DngInfo, width: u32, height: u32) -> gyroflow_proto::LensData {
    let distance_mm = dng.subject_distance_m.map(|m| m * 1000.0);
    let (kr, focal_mm) = match table {
        Some(table) => (
            lensfit::interpolate(&table.axis, &table.nodes, distance_mm),
            lensfit::focal_mm(Some(table.calib_focal_tenths_mm), dng.focal_length_mm),
        ),
        None => (dng.warp_rectilinear, lensfit::focal_mm(None, dng.focal_length_mm)),
    };
    let mut lens = gyroflow_proto::LensData {
        focal_length_mm: focal_mm.or(dng.focal_length_mm).map(|v| v as f32),
        f_number: dng.f_number.map(|v| v as f32),
        focus_distance_mm: distance_mm.map(|mm| mm as f32),
        ..Default::default()
    };
    let fitted = focal_mm.zip(kr).and_then(|(focal_mm, kr)| {
        lensfit::fit(kr, width, height, lensfit::focal_px(width, focal_mm, record.crop[2]))
    });
    if let Some((coefficients, fx)) = fitted {
        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let fx = fx as f32;
        lens.camera_intrinsic_matrix = vec![fx, 0.0, cx, 0.0, fx, cy, 0.0, 0.0, 1.0];
        lens.distortion = Some(gyroflow_proto::lens_data::Distortion::OpencvFisheye(gyroflow_proto::OpenCvFisheye {
            coefficients: coefficients.iter().map(|v| *v as f32).collect(),
        }));
    }
    lens
}

fn level_counts(event: &Event) -> [i16; 3] {
    let v = &event.value;
    [i16::from_le_bytes([v[0], v[1]]), i16::from_le_bytes([v[2], v[3]]), i16::from_le_bytes([v[4], v[5]])]
}

/// Reference encoder: the byte layout the camera writes. Used by the tests and
/// kept next to the decoder so the two cannot drift apart.
#[cfg_attr(not(test), allow(dead_code))]
pub fn encode(record: &Record) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + record.samples.len() * 6);
    out.push(FMT_VERSION);
    out.push(record.orientation);
    out.extend_from_slice(&record.flags.to_le_bytes());
    out.extend_from_slice(&record.clip_id.to_le_bytes());
    out.extend_from_slice(&record.frame_seq.to_le_bytes());
    out.extend_from_slice(&record.sample_period_ps.to_le_bytes());
    out.extend_from_slice(&record.gscale.to_le_bytes());
    out.extend_from_slice(&record.ascale.to_le_bytes());
    out.extend_from_slice(&record.readout_ns.to_le_bytes());
    out.extend_from_slice(&record.mode_id.to_le_bytes());
    for v in record.crop { out.extend_from_slice(&v.to_le_bytes()); }
    out.extend_from_slice(&record.frame_mark.to_le_bytes());
    out.extend_from_slice(&record.base_idx.to_le_bytes());
    out.extend_from_slice(&(record.samples.len() as u16).to_le_bytes());
    out.extend_from_slice(&(record.events.len() as u16).to_le_bytes());
    for sample in &record.samples {
        for v in sample { out.extend_from_slice(&v.to_le_bytes()); }
    }
    for event in &record.events {
        out.extend_from_slice(&event.pos.to_le_bytes());
        out.push(event.kind);
        out.push(event.value.len() as u8);
        out.extend_from_slice(&event.value);
    }
    if let Some(table) = &record.lens_table {
        out.extend_from_slice(&table.calib_focal_tenths_mm.to_le_bytes());
        for v in table.axis { out.extend_from_slice(&v.to_le_bytes()); }
        for node in &table.nodes {
            for plane in node {
                for v in plane { out.extend_from_slice(&v.to_le_bytes()); }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD_PS: u32 = 400_085_400;
    const GSCALE: f32 = 0.000137923;
    const ASCALE: f32 = 0.0009765625;

    pub fn record(base_idx: u32, frame_seq: u32, samples: Vec<[i16; 3]>) -> Record {
        Record {
            orientation: 0,
            flags: 0,
            clip_id: 0x1234_5678,
            frame_seq,
            sample_period_ps: PERIOD_PS,
            gscale: GSCALE,
            ascale: ASCALE,
            readout_ns: 21_325_000,
            mode_id: 123,
            crop: [64, 1840, 5872, 3304],
            frame_mark: 2,
            base_idx,
            samples,
            events: vec![],
            lens_table: None,
        }
    }

    fn level(pos: u16, raw: [i16; 3]) -> Event {
        Event { pos, kind: EVENT_LEVEL, value: raw.iter().flat_map(|v| v.to_le_bytes()).collect() }
    }

    fn dng() -> DngInfo {
        DngInfo {
            make: Some("SIGMA".into()),
            model: Some("SIGMA fp".into()),
            lens_model: Some("28mm F1.4 DG HSM | Art 019".into()),
            frame_size: Some((1920, 1080)),
            frame_rate: Some(30000.0 / 1001.0),
            exposure_s: Some(1.0 / 60.0),
            iso: Some(100),
            f_number: Some(1.4),
            focal_length_mm: Some(28.0),
            subject_distance_m: Some(0.403),
            ..Default::default()
        }
    }

    fn lens_table() -> LensTable {
        let mut nodes = [[[0.0; 4]; 3]; 5];
        for (n, node) in nodes.iter_mut().enumerate() {
            for (p, plane) in node.iter_mut().enumerate() {
                *plane = [1.0 + n as f64 * 1e-3, -0.0148 + p as f64 * 1e-4, 0.0052, -0.0098];
            }
        }
        LensTable { calib_focal_tenths_mm: 394, axis: [0, 18641, 37283, 49637, 55924], nodes }
    }

    #[test]
    fn encode_parse_round_trip_with_events_and_lens_table() {
        let mut original = record(1000, 7, vec![[1, -2, 3], [-32768, 32767, 0]]);
        original.flags = FLAG_FIRST_FRAME | FLAG_LENS_TABLE;
        original.events = vec![level(1, [10, -20, 1024]), Event { pos: 0, kind: EVENT_FOCUS, value: 9000_u32.to_le_bytes().to_vec() }];
        original.lens_table = Some(lens_table());
        let bytes = encode(&original);
        assert_eq!(bytes.len(), HEADER_BYTES + 2 * 6 + (4 + 6) + (4 + 4) + LENS_TABLE_BYTES);
        assert_eq!(parse(&bytes).unwrap(), original);
    }

    #[test]
    fn header_offsets_match_the_spec() {
        let bytes = encode(&record(0xAABB_CCDD, 0, vec![]));
        assert_eq!(&bytes[12..16], &PERIOD_PS.to_le_bytes());
        assert_eq!(&bytes[16..20], &GSCALE.to_le_bytes());
        assert_eq!(&bytes[24..28], &21_325_000_u32.to_le_bytes());
        assert_eq!(&bytes[38..40], &2_u16.to_le_bytes());
        assert_eq!(&bytes[40..44], &0xAABB_CCDD_u32.to_le_bytes());
        assert_eq!(bytes.len(), HEADER_BYTES);
    }

    #[test]
    fn unknown_event_types_are_skipped_by_length() {
        let mut original = record(0, 0, vec![[1, 2, 3]]);
        original.events = vec![Event { pos: 0, kind: 77, value: vec![1, 2, 3, 4, 5, 6, 7] }, level(0, [0, 0, 1024])];
        assert_eq!(parse(&encode(&original)).unwrap().events.len(), 2);
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        let good = encode(&record(0, 0, vec![[1, 2, 3]]));
        let mut bad_version = good.clone();
        bad_version[0] = 9;
        assert!(parse(&bad_version).unwrap_err().contains("format version 9"));
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(parse(&trailing).unwrap_err().contains("trailing"));
        assert!(parse(&good[..good.len() - 1]).unwrap_err().contains("truncated in samples"));
        let mut wrong_level = record(0, 0, vec![]);
        wrong_level.events = vec![Event { pos: 0, kind: EVENT_LEVEL, value: vec![0; 4] }];
        assert!(parse(&encode(&wrong_level)).unwrap_err().contains("type 1 has length 4"));
        let mut no_table = record(0, 0, vec![]);
        no_table.flags = FLAG_LENS_TABLE;
        assert!(parse(&encode(&no_table)).unwrap_err().contains("truncated in lens table"));
        let mut zero_period = record(0, 0, vec![]);
        zero_period.sample_period_ps = 0;
        assert!(parse(&encode(&zero_period)).unwrap_err().contains("period is zero"));
    }

    #[test]
    fn regular_timing_puts_jittered_hooks_back_on_the_frame_clock() {
        // 29.97p: 83.40 samples a frame at 400.0854 us. Hooks land late by 0..30.
        let per_frame = 1.0 / (30000.0 / 1001.0 * PERIOD_PS as f64 * 1e-12);
        let late = [0u32, 30, 5, 22, 1, 17, 9, 28, 3, 12];
        let frames: Vec<_> = late.iter().enumerate().map(|(k, d)| {
            let hook = (250.0 + per_frame * k as f64).round() as u32 + d;
            let mut r = record(hook - 4, k as u32, vec![[0, 0, 0]; 4]);
            r.frame_mark = 4;
            (r, dng())
        }).collect();
        let fps = 30000.0 / 1001.0;
        let regular = frame_positions(&frames, fps, true);
        let steps: Vec<f64> = regular.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps.iter().all(|s| (s - per_frame).abs() < 1e-9), "{steps:?}");
        let hooks = frame_positions(&frames, fps, false);
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
        assert!((mean(&regular) - mean(&hooks)).abs() < 1e-9, "the line keeps the hooks' mean");
        assert_eq!(hooks[1], (250.0 + per_frame).round() + 30.0);
        // A dropped stretch (frame_seq jumps) stays on the same clock.
        let mut gap = frames.clone();
        gap.drain(3..6);
        let g = frame_positions(&gap, fps, true);
        assert!((g[3] - g[2] - 4.0 * per_frame).abs() < 1e-9);
        // Hooks that do not follow the frame rate are left as they are.
        assert_eq!(frame_positions(&frames, 25.0, true), hooks);
        // The messages use the line.
        let messages = to_messages_with(&frames, &Options { regular_frame_timing: true }).unwrap();
        let period_us = PERIOD_PS as f64 / 1e6;
        let t1 = messages[1].frame.as_ref().unwrap().start_timestamp_us;
        assert!((t1 - (regular[1] * period_us + HOOK_TO_READOUT_US)).abs() < 1e-6);
    }

    #[test]
    fn timing_and_units_come_from_positions_and_scales() {
        let frames = vec![(record(100, 0, vec![[131, 0, -131]; 4]), dng())];
        let messages = to_messages(&frames).unwrap();
        let frame = messages[0].frame.as_ref().unwrap();
        let period_us = PERIOD_PS as f64 / 1e6;
        assert!((frame.start_timestamp_us - (102.0 * period_us + HOOK_TO_READOUT_US)).abs() < 1e-6);
        assert!((frame.end_timestamp_us - frame.start_timestamp_us - 21_325.0).abs() < 1e-6);
        assert!((frame.imu[3].sample_timestamp_us.unwrap() - 103.0 * period_us).abs() < 1e-6);
        let dps = 131.0 * GSCALE as f64 * 180.0 / std::f64::consts::PI;
        assert!((frame.imu[0].gyroscope_x as f64 - dps).abs() < 1e-4);
        assert!((frame.imu[0].gyroscope_z as f64 + dps).abs() < 1e-4);
        assert_eq!(frame.exposure_time_us.map(|v| v.round()), Some(16667.0));
        let header = messages[0].header.as_ref().unwrap();
        let clip = header.clip.as_ref().unwrap();
        assert_eq!((clip.frame_width, clip.frame_height, clip.imu_sample_rate), (1920, 1080, 2499));
        assert_eq!(header.camera.as_ref().unwrap().imu_orientation.as_deref(), Some("xyz"));
    }

    #[test]
    fn overlapping_guards_are_emitted_once_and_a_lost_block_leaves_a_gap() {
        // Frame 1 covers 0..10, frame 2 re-sends 8..9 as guards and runs to 15,
        // frame 3 lost its data, frame 4 resumes at 30.
        let mut lost = record(15, 2, vec![]);
        lost.flags = FLAG_DATA_LOST;
        let frames = vec![
            (record(0, 0, vec![[1, 0, 0]; 10]), dng()),
            (record(8, 1, vec![[2, 0, 0]; 7]), dng()),
            (lost, dng()),
            (record(30, 3, vec![[3, 0, 0]; 5]), dng()),
        ];
        let messages = to_messages(&frames).unwrap();
        let counts: Vec<usize> = messages.iter().map(|m| m.frame.as_ref().unwrap().imu.len()).collect();
        assert_eq!(counts, vec![10, 5, 0, 5]);
        let period_us = PERIOD_PS as f64 / 1e6;
        let first_of_frame_2 = messages[1].frame.as_ref().unwrap().imu[0].sample_timestamp_us.unwrap();
        assert!((first_of_frame_2 - 10.0 * period_us).abs() < 1e-6);
        let start = |k: usize| messages[k].frame.as_ref().unwrap().start_timestamp_us;
        assert!(start(1) < start(2) && start(2) < start(3), "a lost block still moves the timeline on");
        assert!(messages.iter().skip(1).all(|m| m.header.is_none()));
    }

    #[test]
    fn accelerometer_is_held_and_turned_into_gyro_axes() {
        let mut first = record(0, 0, vec![[0, 0, 0]; 4]);
        first.events = vec![level(2, [100, 200, 1024])];
        let mut second = record(4, 1, vec![[0, 0, 0]; 2]);
        second.events = vec![level(1, [-50, 0, 1000])];
        let messages = to_messages(&[(first, dng()), (second, dng())]).unwrap();
        let g = ASCALE as f64 * STANDARD_GRAVITY;
        let acc = |m: usize, i: usize| {
            let s = &messages[m].frame.as_ref().unwrap().imu[i];
            [s.accelerometer_x as f64, s.accelerometer_y as f64, s.accelerometer_z as f64]
        };
        // Before the take's first reading, that reading is held backwards.
        let first_reading = [200.0 * g, -100.0 * g, 1024.0 * g];
        let close = |a: [f64; 3], b: [f64; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        assert!(close(acc(0, 0), first_reading));
        assert!(close(acc(0, 3), first_reading));
        assert!(close(acc(1, 0), first_reading));
        assert!(close(acc(1, 1), [0.0, 50.0 * g, 1000.0 * g]));
    }

    // The LUMIX S 40/F2's green plane (fpSup gyro/test_distfit.py).
    fn lumix_40_table() -> LensTable {
        let green = [
            [0.987878098, 0.000230792, -0.001804660, 0.012710940],
            [0.995612771, -0.003803673, -0.003186222, 0.011056404],
            [0.999854447, -0.010596089, 0.003693395, 0.003874420],
            [0.999856513, -0.016680599, 0.013089761, -0.003420052],
            [0.999845119, -0.020184787, 0.018855619, -0.007281209],
        ];
        LensTable { calib_focal_tenths_mm: 400, axis: [0, 18641, 37283, 49637, 55924], nodes: green.map(|kr| [kr, kr, kr]) }
    }

    fn lens_of(message: &gyroflow_proto::Main) -> &gyroflow_proto::LensData {
        &message.frame.as_ref().unwrap().lens[0]
    }

    fn fisheye(lens: &gyroflow_proto::LensData) -> Vec<f32> {
        match &lens.distortion {
            Some(gyroflow_proto::lens_data::Distortion::OpencvFisheye(c)) => c.coefficients.clone(),
            other => panic!("expected OpenCV fisheye, got {other:?}"),
        }
    }

    #[test]
    fn lens_follows_a_focus_pull_from_the_first_frames_table() {
        let mut first = record(0, 0, vec![[0, 0, 0]]);
        first.flags |= FLAG_LENS_TABLE;
        first.lens_table = Some(lumix_40_table());
        first.crop[2] = 6000;
        let mut second = record(1, 1, vec![[0, 0, 0]]);
        second.crop[2] = 6000;
        let at = |metres: Option<f64>| DngInfo { frame_size: Some((1936, 1090)), focal_length_mm: Some(40.0), subject_distance_m: metres, ..dng() };
        let messages = to_messages(&[(first, at(None)), (second, at(Some(0.364)))]).unwrap();

        // Golden values from fpSup's Python mirror of the camera's fit.
        let infinity = lens_of(&messages[0]);
        assert!((infinity.camera_intrinsic_matrix[0] as f64 - 2130.954872120334).abs() < 1e-2);
        assert_eq!(infinity.camera_intrinsic_matrix[0], infinity.camera_intrinsic_matrix[4]);
        assert_eq!((infinity.camera_intrinsic_matrix[2], infinity.camera_intrinsic_matrix[5]), (968.0, 545.0));
        assert!((fisheye(infinity)[0] as f64 - 0.3434034356654717).abs() < 1e-6);
        assert_eq!(infinity.focal_length_mm, Some(40.0));

        let near = lens_of(&messages[1]);
        assert!((near.camera_intrinsic_matrix[0] as f64 - 2156.792268728652).abs() < 1e-2);
        assert!((fisheye(near)[0] as f64 - 0.2772084741693018).abs() < 1e-6);
        assert!((near.focus_distance_mm.unwrap() - 364.0).abs() < 1e-3);
    }

    #[test]
    fn without_a_table_each_frame_uses_its_own_warp_opcode() {
        let opcode = DngInfo { warp_rectilinear: Some([1.000334, -0.01484, 0.00518, -0.009761]), ..dng() };
        let messages = to_messages(&[(record(0, 0, vec![]), opcode)]).unwrap();
        let lens = lens_of(&messages[0]);
        assert_eq!(fisheye(lens).len(), 4);
        // The breathing term rides on the 28 mm focal: 1920 x 28 / 35.9 x kr0.
        let focal = 1920.0 * 28.0 / (35.9 * 5872.0 / 6000.0) * 1.000334;
        assert!((lens.camera_intrinsic_matrix[0] as f64 - focal).abs() < 1e-2);

        let plain = to_messages(&[(record(0, 0, vec![]), dng())]).unwrap();
        let lens = lens_of(&plain[0]);
        assert!(lens.distortion.is_none() && lens.camera_intrinsic_matrix.is_empty());
        assert_eq!(lens.focal_length_mm, Some(28.0));
    }

    #[test]
    fn a_take_cannot_mix_clips() {
        let mut other = record(10, 1, vec![]);
        other.clip_id = 1;
        let error = to_messages(&[(record(0, 0, vec![]), dng()), (other, dng())]).unwrap_err();
        assert!(error.contains("another take"), "{error}");
    }

    #[test]
    fn frame_size_and_rate_are_required_from_the_dng() {
        let mut no_rate = dng();
        no_rate.frame_rate = None;
        assert!(to_messages(&[(record(0, 0, vec![]), no_rate)]).unwrap_err().contains("FrameRate"));
        let mut no_size = dng();
        no_size.frame_size = None;
        assert!(to_messages(&[(record(0, 0, vec![]), no_size)]).unwrap_err().contains("DefaultCropSize"));
    }
}
