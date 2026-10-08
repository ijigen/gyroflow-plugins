//! Hand a parsed CinemaDNG take to the stock app as a self-contained project.
use std::path::{Path, PathBuf};
use std::io::Write;
use ciborium::value::Value;
use crate::{cdng, StabilizationManager, PluginResult, filesystem, gyroflow_core};

/// Keep integer timestamp keys intact while adapting the newer (fx, fy) field
/// to Gyroflow 1.6.3's scalar. Newer cores accept that scalar too. SIGMA fp's
/// fitted lenses have equal fx and fy; refuse an anisotropic projection rather
/// than silently lose its vertical focal length.
fn compatible_metadata(value: &mut Value) -> PluginResult<()> {
    let Value::Map(fields) = value else { return Err("Invalid project metadata".into()) };
    for (key, val) in fields {
        if key.as_text() != Some("lens_params") { continue; }
        let Value::Map(lenses) = val else { return Err("Invalid per-frame lens metadata".into()) };
        for (_, lens) in lenses {
            let Value::Map(fields) = lens else { return Err("Invalid lens metadata".into()) };
            for (key, focal) in fields {
                if key.as_text() != Some("pixel_focal_length") { continue; }
                if let Value::Array(pair) = focal {
                    if pair.len() != 2 || pair[0] != pair[1] {
                        return Err("This lens has unequal horizontal and vertical focal lengths; the stock 1.6.3 project format cannot represent them".into());
                    }
                    *focal = pair[0].clone();
                }
            }
        }
    }
    Ok(())
}

fn project_data(stab: &StabilizationManager, source: &Path) -> PluginResult<String> {
    let paths = cdng::sequence_paths(source)?;
    let first = paths.first().ok_or("Empty DNG sequence")?;
    let stem = first.file_stem().and_then(|s| s.to_str()).ok_or("Invalid DNG name")?;
    let prefix = stem.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &stem[prefix.len()..];
    let start: i32 = digits.parse()?;
    let extension = first.extension().and_then(|s| s.to_str()).ok_or("Invalid DNG extension")?;
    let pattern = first.with_file_name(format!("{prefix}%0{}d.{extension}", digits.len()));
    let data = stab.export_gyroflow_data(gyroflow_core::GyroflowProjectType::WithGyroData, "{}", None)?;
    let mut project: serde_json::Value = serde_json::from_str(&data)?;
    let encoded = project["gyro_source"]["file_metadata"].as_str().ok_or("Project has no embedded telemetry")?;
    let mut metadata: Value = gyroflow_core::util::decompress_from_base91_cbor(encoded)?;
    compatible_metadata(&mut metadata)?;
    project["gyro_source"]["file_metadata"] = gyroflow_core::util::compress_to_base91_cbor(&metadata)
        .ok_or("Unable to compress project metadata")?.into();
    project["videofile"] = filesystem::path_to_url(&pattern.to_string_lossy()).into();
    // The app's UI probes a separate gyro source before importing the project.
    // Match the sequence URL so it uses the embedded metadata instead of
    // trying to parse the private DNG carrier as a separate telemetry file.
    project["gyro_source"]["filepath"] = project["videofile"].clone();
    project["image_sequence_start"] = start.into();
    project["image_sequence_fps"] = stab.params.read().fps.into();
    // The core made a bookmark for the single source frame, before we changed
    // videofile to a sequence pattern. Let the app use that absolute pattern.
    project.as_object_mut().unwrap().remove("videofile_bookmark");
    project["gyro_source"].as_object_mut().unwrap().remove("filepath_bookmark");
    if !StabilizationManager::project_has_motion_data(serde_json::to_string(&project)?.as_bytes()) {
        return Err("Project has no usable embedded motion data".into());
    }
    Ok(serde_json::to_string_pretty(&project)?)
}

pub(crate) fn save_project(stab: &StabilizationManager, source: &Path) -> PluginResult<PathBuf> {
    save_project_in(stab, source, &gyroflow_core::settings::data_dir().join("fpsup-handoffs"))
}

fn save_project_in(stab: &StabilizationManager, source: &Path, root: &Path) -> PluginResult<PathBuf> {
    let data = project_data(stab, source)?;
    // The editor will keep referring to this project after the app closes or
    // the computer restarts, so it must not live in the system temporary folder.
    std::fs::create_dir_all(root)?;
    let folder = root.join(format!("fpsup-gyroflow-{:032x}", fastrand::u128(..)));
    std::fs::create_dir(&folder)?;
    let name = source.file_stem().and_then(|s| s.to_str()).ok_or("Invalid DNG name")?;
    let path = folder.join(format!("{name}.gyroflow"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(data.as_bytes())?;
    log::info!("CinemaDNG handoff project: {} ({} bytes)", path.display(), data.len());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::{GyroflowPluginParams, Params, TimeType};
    use gyroflow_core::gyro_source::{FileMetadata, LensParams, TimeIMU};

    #[derive(serde::Deserialize)]
    struct OldLens { pixel_focal_length: Option<f32> }
    #[derive(serde::Deserialize)]
    struct OldMetadata {
        raw_imu: Vec<TimeIMU>,
        lens_params: BTreeMap<i64, OldLens>,
        per_frame_time_offsets: Vec<f64>,
    }

    struct ProjectParams { path: String, embedded: String }
    impl GyroflowPluginParams for ProjectParams {
        fn get_string(&self, param: Params) -> PluginResult<String> {
            Ok(match param { Params::ProjectPath => &self.path, Params::ProjectData => &self.embedded, _ => panic!("Unexpected parameter") }.clone())
        }
        fn set_string(&mut self, param: Params, value: &str) -> PluginResult<()> {
            match param { Params::ProjectPath => self.path = value.into(), Params::ProjectData => self.embedded = value.into(), _ => panic!("Unexpected parameter") }
            Ok(())
        }
        fn set_enabled(&mut self, _: Params, _: bool) -> PluginResult<()> { unreachable!() }
        fn set_label(&mut self, _: Params, _: &str) -> PluginResult<()> { unreachable!() }
        fn set_hint(&mut self, _: Params, _: &str) -> PluginResult<()> { unreachable!() }
        fn set_f64(&mut self, _: Params, _: f64) -> PluginResult<()> { unreachable!() }
        fn get_f64(&self, _: Params) -> PluginResult<f64> { unreachable!() }
        fn get_f64_at_time(&self, _: Params, _: TimeType) -> PluginResult<f64> { unreachable!() }
        fn set_bool(&mut self, _: Params, _: bool) -> PluginResult<()> { unreachable!() }
        fn get_bool(&self, _: Params) -> PluginResult<bool> { unreachable!() }
        fn get_bool_at_time(&self, _: Params, _: TimeType) -> PluginResult<bool> { unreachable!() }
        fn set_i32(&mut self, _: Params, _: i32) -> PluginResult<()> { unreachable!() }
        fn get_i32(&self, _: Params) -> PluginResult<i32> { unreachable!() }
        fn is_keyframed(&self, _: Params) -> bool { unreachable!() }
        fn get_keyframes(&self, _: Params) -> Vec<(TimeType, f64)> { unreachable!() }
        fn clear_keyframes(&mut self, _: Params) -> PluginResult<()> { unreachable!() }
        fn set_f64_at_time(&mut self, _: Params, _: TimeType, _: f64) -> PluginResult<()> { unreachable!() }
    }

    #[test]
    fn restoring_a_portable_project_preserves_its_only_embedded_copy() {
        let mut instance = crate::GyroflowPluginBaseInstance::default();
        let cache = crate::Mutex::new(crate::LruCache::new(std::num::NonZeroUsize::new(1).unwrap()));
        let mut params = ProjectParams { path: "/missing/project.gyroflow".into(), embedded: "only-copy".into() };
        instance.param_changed(&mut params, &cache, Params::ProjectPath, false).unwrap();
        assert_eq!(params.embedded, "only-copy");
        instance.param_changed(&mut params, &cache, Params::ReloadProject, true).unwrap();
        assert_eq!(params.embedded, "only-copy");
    }

    #[test]
    fn explicit_reload_discards_the_stale_embedded_copy_when_the_file_exists() {
        let path = std::env::temp_dir().join(format!("fpsup-reload-{:032x}.gyroflow", fastrand::u128(..)));
        std::fs::write(&path, "newly-saved-project").unwrap();
        let mut instance = crate::GyroflowPluginBaseInstance::default();
        let cache = crate::Mutex::new(crate::LruCache::new(std::num::NonZeroUsize::new(1).unwrap()));
        let mut params = ProjectParams { path: path.to_string_lossy().into(), embedded: "stale-project".into() };
        instance.param_changed(&mut params, &cache, Params::ReloadProject, true).unwrap();
        assert!(params.embedded.is_empty());
        assert!(instance.reload_values_from_project);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newly-saved-project");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn metadata_keeps_samples_timing_and_integer_keys_for_old_and_new_readers() {
        let metadata = FileMetadata {
            raw_imu: vec![TimeIMU { timestamp_ms: 0.4, gyro: Some([1.0, -2.0, 3.0]), accl: Some([0.0, 0.0, 9.80665]), ..Default::default() }],
            lens_params: [(123456, LensParams { pixel_focal_length: Some((1500.0, 1500.0)), ..Default::default() })].into(),
            per_frame_time_offsets: vec![0.9, 0.8],
            ..Default::default()
        };
        let encoded = gyroflow_core::util::compress_to_base91_cbor(&metadata).unwrap();
        let mut value: Value = gyroflow_core::util::decompress_from_base91_cbor(&encoded).unwrap();
        compatible_metadata(&mut value).unwrap();
        let encoded = gyroflow_core::util::compress_to_base91_cbor(&value).unwrap();
        let old: OldMetadata = gyroflow_core::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert_eq!(old.raw_imu[0].gyro, Some([1.0, -2.0, 3.0]));
        assert_eq!(old.raw_imu[0].timestamp_ms, 0.4);
        assert_eq!(old.lens_params[&123456].pixel_focal_length, Some(1500.0));
        assert_eq!(old.per_frame_time_offsets, metadata.per_frame_time_offsets);
        let new: FileMetadata = gyroflow_core::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert_eq!(new.lens_params[&123456].pixel_focal_length, Some((1500.0, 1500.0)));
    }

    #[test]
    fn unequal_focal_axes_are_not_silently_collapsed() {
        let metadata = FileMetadata {
            lens_params: [(0, LensParams { pixel_focal_length: Some((1500.0, 1400.0)), ..Default::default() })].into(),
            ..Default::default()
        };
        let encoded = gyroflow_core::util::compress_to_base91_cbor(&metadata).unwrap();
        let mut value: Value = gyroflow_core::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert!(compatible_metadata(&mut value).is_err());
    }

    #[test]
    fn reload_evicts_old_project_even_when_a_render_still_uses_it() {
        let mut instance = crate::GyroflowPluginBaseInstance::default();
        let cache = crate::Mutex::new(crate::LruCache::new(std::num::NonZeroUsize::new(2).unwrap()));
        let rendering = std::sync::Arc::new(StabilizationManager::default());
        instance.managers.put("edited-project".into(), rendering.clone());
        cache.lock().put("edited-project".into(), rendering.clone());
        cache.lock().put("other-clip".into(), std::sync::Arc::new(StabilizationManager::default()));
        instance.reload_stab(&cache);
        assert!(!cache.lock().contains("edited-project"));
        assert!(cache.lock().contains("other-clip"));
        assert_eq!(std::sync::Arc::strong_count(&rendering), 1);
    }

    /// Export the same data as the button, then import it without re-reading DNG
    /// telemetry. This also supplies a project for testing the installed app.
    #[test]
    #[ignore]
    fn real_dng_project_roundtrip() {
        let source = PathBuf::from(std::env::var("FP_DNG").expect("FP_DNG"));
        let sequence = cdng::read_embedded_protobuf_sequence(&source).unwrap();
        let video = gyroflow_core::telemetry_parser::util::VideoMetadata {
            width: sequence.width, height: sequence.height, fps: sequence.fps,
            duration_s: sequence.frame_count as f64 / sequence.fps, rotation: sequence.rotation,
        };
        let stab = StabilizationManager::default();
        let mut stream = std::io::Cursor::new(&sequence.jsonl);
        let virtual_url = filesystem::path_to_url("/tmp/fpsup-handoff.jsonl");
        stab.load_video_file(&mut stream, sequence.jsonl.len(), &virtual_url, Some(video), true).unwrap();
        stab.input_file.write().url = filesystem::path_to_url(&source.to_string_lossy());
        stab.gyro.write().file_url = stab.input_file.read().url.clone();
        let started = std::time::Instant::now();
        let path = save_project_in(&stab, &source, &std::env::temp_dir()).unwrap();
        let export_elapsed = started.elapsed();
        let data = std::fs::read(&path).unwrap();
        let project: serde_json::Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(project["gyro_source"]["filepath"], project["videofile"]);
        assert_eq!(project["image_sequence_start"], 1);
        assert!(project["videofile"].as_str().unwrap().contains("%2506d.DNG"));
        let restored = StabilizationManager::default();
        let mut is_preset = false;
        restored.import_gyroflow_data(&data, true, Some(&filesystem::path_to_url(&path.to_string_lossy())), |_|(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)), &mut is_preset, false).unwrap();
        let gyro = stab.gyro.read();
        let original = gyro.file_metadata.read();
        let restored_gyro = restored.gyro.read();
        let loaded = restored_gyro.file_metadata.read();
        assert_eq!(loaded.raw_imu.len(), original.raw_imu.len());
        assert_eq!(loaded.per_frame_time_offsets, original.per_frame_time_offsets);
        assert_eq!(loaded.lens_params.len(), original.lens_params.len());
        assert_eq!(serde_json::to_value(&loaded.raw_imu).unwrap(), serde_json::to_value(&original.raw_imu).unwrap());
        assert_eq!(loaded.raw_imu.last().unwrap().timestamp_ms, original.raw_imu.last().unwrap().timestamp_ms);
        let old: OldMetadata = gyroflow_core::util::decompress_from_base91_cbor(
            serde_json::from_slice::<serde_json::Value>(&data).unwrap()["gyro_source"]["file_metadata"].as_str().unwrap()).unwrap();
        assert_eq!(old.raw_imu.len(), original.raw_imu.len());
        assert_eq!(old.lens_params.len(), original.lens_params.len());
        println!("HANDOFF {}: {} samples, {} frames, {} lens records; cached export took {:?}", path.display(), loaded.raw_imu.len(),
            loaded.per_frame_time_offsets.len(), loaded.lens_params.len(), export_elapsed);
    }
}
