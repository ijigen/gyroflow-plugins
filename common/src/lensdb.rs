//! Picking a SIGMA fp lens profile out of Gyroflow's bundled database by name,
//! for lenses the camera knows nothing about (no electronic contacts).

use gyroflow_core::lens_profile_database::LensProfileDatabase;
use std::sync::OnceLock;

fn database() -> &'static LensProfileDatabase {
    static DB: OnceLock<LensProfileDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = LensProfileDatabase::default();
        db.load_all();
        db
    })
}

/// Letters and digits only, lower case: "Helios-44-2" -> "helios442".
fn squash(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// The best SIGMA fp (not fp L) profile whose lens name holds every word of
/// `query`, preferring one calibrated at the width nearest `frame_width`.
/// Returns (a label for the user, the profile as JSON).
pub fn find_fp_profile(query: &str, frame_width: usize) -> Option<(String, String)> {
    let words: Vec<String> = query.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(squash).collect();
    if words.is_empty() {
        return None;
    }
    let db = database();
    let mut best: Option<(usize, String, String)> = None;
    // `find` scans the whole database, so narrow by the file name first
    // (profiles live under "Sigma/...fp..." in the database).
    for name in db.get_all_filenames() {
        let lower = name.to_lowercase();
        if !lower.contains("sigma") || !lower.contains("fp") {
            continue;
        }
        let Some(profile) = db.find(&name) else { continue };
        if squash(&profile.camera_brand) != "sigma" || !matches!(squash(&profile.camera_model).as_str(), "fp" | "sigmafp") {
            continue;
        }
        if profile.fisheye_params.distortion_coeffs.len() < 4 {
            continue;
        }
        let lens = squash(&profile.lens_model);
        if !words.iter().all(|w| lens.contains(w.as_str())) {
            continue;
        }
        let distance = profile.calib_dimension.w.abs_diff(frame_width);
        let label = format!("{} ({}, {}x{}, by {})", profile.lens_model.trim(), profile.camera_model.trim(),
            profile.calib_dimension.w, profile.calib_dimension.h, profile.calibrated_by.trim());
        // The database is a hash map: break ties by name so a search always picks the same one.
        if best.as_ref().is_some_and(|(d, l, _)| (*d, l.as_str()) <= (distance, label.as_str())) {
            continue;
        }
        let Ok(json) = profile.get_json() else { continue };
        best = Some((distance, label, json));
    }
    best.map(|(_, label, json)| (label, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_manual_lens_for_the_fp_by_its_words() {
        let (label, json) = find_fp_profile("helios 44", 3840).expect("the bundled database has Helios 44 profiles for the fp");
        assert!(squash(&label).contains("helios44"), "{label}");
        assert!(json.contains("fisheye_params"));
        assert!(!squash(&label).contains("fpl"), "not an fp L profile: {label}");
        assert_eq!(find_fp_profile("helios 44", 3840).unwrap().0, label, "the same pick every time");
        assert!(squash(&find_fp_profile("canon fd 50", 3008).unwrap().0).contains("50"));
        assert!(find_fp_profile("no such lens zzz", 3840).is_none());
        assert!(find_fp_profile("  ", 3840).is_none());
    }
}
