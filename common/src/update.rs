//! Updating the fpSup OpenFX plugin from the fork's GitHub releases, and going
//! back.
//!
//! Releases are tagged `fpsup-vX.Y.Z`; this build is `RELEASE`. Each release
//! carries the platform zips and a `SHA256SUMS` file. Installing any release
//! (newer or older) first moves the installed bundle into a backup folder, so
//! "roll back" can put the previous one back without the network, and every
//! swap is itself undoable. Nothing is installed without the user pressing a
//! button; the check only reports.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// This build. Bump it with each `fpsup-vX.Y.Z` tag.
pub const RELEASE: &str = "0.1.6";

/// A release number, compared part by part: 0.1.10 is after 0.1.9.
pub type Version = (u32, u32, u32);

/// "0.1.0" (also "0.1", "2", with or without "fpsup-v" or "v") -> (0, 1, 0).
pub fn parse_version(text: &str) -> Option<Version> {
    let t = text.trim();
    let t = t.strip_prefix(TAG_PREFIX).or_else(|| t.strip_prefix('v')).unwrap_or(t);
    let mut parts = t.split('.').map(|p| p.parse::<u32>());
    let v = (parts.next()?.ok()?, parts.next().unwrap_or(Ok(0)).ok()?, parts.next().unwrap_or(Ok(0)).ok()?);
    parts.next().is_none().then_some(v)
}

pub fn show(v: Version) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

fn this_build() -> Version {
    parse_version(RELEASE).expect("RELEASE is a version")
}
pub const REPO: &str = "ijigen/gyroflow-plugins";
const TAG_PREFIX: &str = "fpsup-v";
const BUNDLE: &str = "fpSupGyroflow.ofx.bundle";
const CHECK_EVERY_S: u64 = 24 * 3600;

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub number: Version,
    pub tag: String,
    /// (asset name, download url)
    pub assets: Vec<(String, String)>,
}

/// `fpsup-v0.1.2` -> (0, 1, 2); other tags (the upstream plugin's) -> None.
pub fn release_number(tag: &str) -> Option<Version> {
    tag.strip_prefix(TAG_PREFIX).and_then(parse_version)
}

/// The fpSup releases in a GitHub `/releases` listing, newest first.
pub fn parse_releases(json: &str) -> Result<Vec<Release>, String> {
    let list: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("release list: {e}"))?;
    let mut out: Vec<Release> = list.as_array().ok_or("release list is not an array")?.iter().filter_map(|r| {
        if r["draft"].as_bool() == Some(true) {
            return None;
        }
        let tag = r["tag_name"].as_str()?.to_owned();
        let number = release_number(&tag)?;
        let assets = r["assets"].as_array()?.iter().filter_map(|a| {
            Some((a["name"].as_str()?.to_owned(), a["browser_download_url"].as_str()?.to_owned()))
        }).collect();
        Some(Release { number, tag, assets })
    }).collect();
    out.sort_by(|a, b| b.number.cmp(&a.number));
    Ok(out)
}

/// The zip for this platform.
pub fn platform_asset() -> &'static str {
    if cfg!(target_os = "macos") { "fpSupGyroflow-OpenFX-macos.zip" }
    else if cfg!(target_os = "windows") { "fpSupGyroflow-OpenFX-windows.zip" }
    else { "fpSupGyroflow-OpenFX-linux.zip" }
}

/// The digest listed for `name` in a `sha256sum` style file.
pub fn digest_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        let file = file.rsplit(['/', '\\']).next().unwrap_or(file);
        (file == name && digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())).then(|| digest.to_ascii_lowercase())
    })
}

pub fn install_dir() -> PathBuf {
    if cfg!(target_os = "macos") { PathBuf::from("/Library/OFX/Plugins") }
    else if cfg!(target_os = "windows") { PathBuf::from(r"C:\Program Files\Common Files\OFX\Plugins") }
    else { PathBuf::from("/usr/OFX/Plugins") }
}

fn state_dir() -> PathBuf {
    gyroflow_core::settings::data_dir().join("fpsup-plugin")
}

pub fn backup_dir() -> PathBuf {
    state_dir().join("backups")
}

/// Backups, newest first: folders named `<unix seconds>-v<release>`.
pub fn backups() -> Vec<(u64, String, PathBuf)> {
    backups_in(&backup_dir())
}

fn backups_in(dir: &Path) -> Vec<(u64, String, PathBuf)> {
    let mut out: Vec<(u64, String, PathBuf)> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let (secs, label) = name.split_once('-')?;
        Some((secs.parse().ok()?, label.to_owned(), e.path()))
    }).filter(|(_, _, p)| p.join(BUNDLE).is_dir()).collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut response = ureq::get(url)
        .header("User-Agent", "fpSupGyroflow-updater")
        .header("Accept", "application/vnd.github+json")
        .call().map_err(|e| format!("{url}: {e}"))?;
    response.body_mut().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))
}

/// A release file, with the system's curl: ureq read GitHub's file server at
/// ~36 KB/s (480 s for the 17 MB macOS zip, 2026-10-08), freezing the editor.
fn download(url: &str, to: &Path) -> Result<(), String> {
    let curl = if cfg!(target_os = "windows") { "curl.exe" } else { "curl" };
    run(Command::new(curl).args(["-fsSL", "--retry", "2", "--max-time", "300", "-o"]).arg(to).arg(url))
}

pub fn fetch_releases() -> Result<Vec<Release>, String> {
    let body = get(&format!("https://api.github.com/repos/{REPO}/releases?per_page=50"), 4 << 20)?;
    parse_releases(&String::from_utf8_lossy(&body))
}

fn status_line(latest: Result<Option<Version>, String>) -> String {
    match latest {
        Ok(Some(n)) if n > this_build() => format!("fpSup v{RELEASE}: v{} is available (Install)", show(n)),
        Ok(_) => format!("fpSup v{RELEASE}: up to date"),
        Err(e) => format!("fpSup v{RELEASE}: update check failed ({e})"),
    }
}

/// Install in the background: the download can take a minute and the editor
/// must not wait for it. The outcome goes to a system notification and is kept
/// for the status line (shown when an instance is next created).
/// false when an install is already running (a second press is ignored).
pub fn install_in_background(number: Option<Version>) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::SeqCst) {
        return false;
    }
    std::thread::spawn(move || {
        let result = install(number).unwrap_or_else(|e| format!("Install failed: {e}"));
        let _ = std::fs::create_dir_all(state_dir());
        let _ = std::fs::write(state_dir().join("last_install"), &result);
        notify(&result);
        BUSY.store(false, Ordering::SeqCst);
    });
    true
}

/// A system notification (macOS); elsewhere the status line has it.
fn notify(text: &str) {
    if cfg!(target_os = "macos") {
        let t = text.replace('\\', "\\\\").replace('"', "\\\"");
        let _ = Command::new("osascript").args(["-e", &format!("display notification \"{t}\" with title \"Gyroflow (fpSup)\"")]).status();
    }
}

/// The last check's answer, without the network.
pub fn cached_status() -> String {
    if let Ok(last) = std::fs::read_to_string(state_dir().join("last_install")) {
        let _ = std::fs::remove_file(state_dir().join("last_install"));
        if !last.trim().is_empty() {
            return last;
        }
    }
    let latest = std::fs::read_to_string(state_dir().join("last_check")).ok()
        .and_then(|s| s.trim().split_once(' ').map(|(_, n)| parse_version(n)));
    match latest {
        Some(n) => status_line(Ok(n)),
        None => format!("fpSup v{RELEASE}: not checked yet"),
    }
}

/// A status line for the plugin: checks the network at most once a day unless
/// `force`, remembering the answer in between.
pub fn check(force: bool) -> String {
    let stamp = state_dir().join("last_check");
    let cached = std::fs::read_to_string(&stamp).ok().and_then(|s| {
        let (when, latest) = s.trim().split_once(' ')?;
        Some((when.parse::<u64>().ok()?, parse_version(latest)))
    });
    let latest = match cached {
        Some((when, latest)) if !force && now_s().saturating_sub(when) < CHECK_EVERY_S => Ok(latest),
        _ => fetch_releases().map(|r| r.first().map(|r| r.number)).inspect(|latest| {
            let _ = std::fs::create_dir_all(state_dir());
            let _ = std::fs::write(&stamp, format!("{} {}", now_s(), latest.map_or("none".to_owned(), show)));
        }),
    };
    status_line(latest)
}

fn sha256_of(path: &Path) -> Result<String, String> {
    let output = if cfg!(target_os = "windows") {
        Command::new("powershell").args(["-NoProfile", "-Command", &format!("(Get-FileHash -Algorithm SHA256 '{}').Hash", path.display())]).output()
    } else if cfg!(target_os = "macos") {
        Command::new("shasum").args(["-a", "256"]).arg(path).output()
    } else {
        Command::new("sha256sum").arg(path).output()
    }.map_err(|e| format!("sha256: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let digest = text.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if digest.len() == 64 { Ok(digest) } else { Err(format!("sha256: {}", String::from_utf8_lossy(&output.stderr))) }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(format!("{cmd:?}: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

/// Swap `new_bundle` into the plugin folder; the installed one goes to a new
/// backup folder labelled `label`. On macOS a refused move is retried with an
/// administrator prompt; on Windows the move always runs elevated.
fn swap_in(new_bundle: &Path, label: &str) -> Result<(), String> {
    swap_in_at(&install_dir(), &backup_dir(), new_bundle, label)
}

fn swap_in_at(plugins: &Path, backups: &Path, new_bundle: &Path, label: &str) -> Result<(), String> {
    let installed = plugins.join(BUNDLE);
    let mut stamp = now_s();
    while backups.join(format!("{stamp}-{label}")).exists() { stamp += 1; }   // two swaps in one second
    let backup = backups.join(format!("{stamp}-{label}"));
    std::fs::create_dir_all(&backup).map_err(|e| format!("{}: {e}", backup.display()))?;
    let saved = backup.join(BUNDLE);
    if cfg!(target_os = "windows") {
        let script = format!(
            "if (Test-Path '{i}') {{ Move-Item -Force '{i}' '{s}' }}; Move-Item -Force '{n}' '{i}'",
            i = installed.display(), s = saved.display(), n = new_bundle.display());
        return run(Command::new("powershell").args(["-NoProfile", "-Command",
            &format!("Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile','-Command',\"{}\"", script.replace('"', "`\""))]));
    }
    // mv, not rename: the temporary folder may sit on another volume.
    let plain = (|| -> Result<(), String> {
        if installed.exists() { run(Command::new("mv").arg(&installed).arg(&saved))?; }
        run(Command::new("mv").arg(new_bundle).arg(&installed))
    })();
    match plain {
        Ok(()) => Ok(()),
        Err(e) if cfg!(target_os = "macos") => {
            let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
            let shell = format!("{{ [ ! -e {i} ] || mv {i} {s}; }} && mv {n} {i}", i = q(&installed), s = q(&saved), n = q(new_bundle));
            run(Command::new("osascript").args(["-e",
                &format!("do shell script \"{}\" with administrator privileges", shell.replace('\\', "\\\\").replace('"', "\\\""))]))
                .map_err(|e2| format!("{e}; with administrator rights: {e2}"))
        }
        Err(e) => Err(format!("{}: {e} (install it by hand from {})", installed.display(), new_bundle.display())),
    }
}

/// Install release `number` (any, newer or older), or the newest when `None`.
/// The running editor keeps the loaded copy; the new one loads on restart.
pub fn install(number: Option<Version>) -> Result<String, String> {
    let releases = fetch_releases()?;
    let release = match number {
        Some(n) => releases.iter().find(|r| r.number == n).ok_or_else(|| format!("no release fpsup-v{}", show(n)))?,
        None => releases.first().ok_or("no fpSup release yet")?,
    };
    let url_of = |name: &str| release.assets.iter().find(|(a, _)| a == name).map(|(_, u)| u.clone());
    let zip_name = platform_asset();
    let zip_url = url_of(zip_name).ok_or_else(|| format!("{} has no {zip_name}", release.tag))?;
    let sums_url = url_of("SHA256SUMS").ok_or_else(|| format!("{} has no SHA256SUMS", release.tag))?;
    let work = std::env::temp_dir().join(format!("fpsup-update-{}", now_s()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let sums = work.join("SHA256SUMS");
    download(&sums_url, &sums)?;
    let want = digest_for(&std::fs::read_to_string(&sums).map_err(|e| e.to_string())?, zip_name)
        .ok_or_else(|| format!("SHA256SUMS lists no {zip_name}"))?;
    let zip = work.join(zip_name);
    download(&zip_url, &zip)?;
    let got = sha256_of(&zip)?;
    if got != want {
        return Err(format!("{zip_name}: SHA-256 {got} is not the release's {want}"));
    }
    let unpacked = work.join("unpacked");
    if cfg!(target_os = "windows") {
        run(Command::new("powershell").args(["-NoProfile", "-Command",
            &format!("Expand-Archive -Force '{}' '{}'", zip.display(), unpacked.display())]))?;
    } else if cfg!(target_os = "macos") {
        run(Command::new("ditto").args(["-x", "-k"]).arg(&zip).arg(&unpacked))?;
    } else {
        run(Command::new("unzip").arg("-q").arg(&zip).arg("-d").arg(&unpacked))?;
    }
    let bundle = find_bundle(&unpacked).ok_or_else(|| format!("{zip_name} holds no {BUNDLE}"))?;
    if cfg!(target_os = "macos") && run(Command::new("codesign").args(["--verify", "--deep"]).arg(&bundle)).is_err() {
        run(Command::new("codesign").args(["--force", "--deep", "-s", "-"]).arg(&bundle))?;
    }
    swap_in(&bundle, &format!("v{RELEASE}"))?;
    Ok(format!("Installed {} (was v{RELEASE}); restart the editor to load it", release.tag))
}

fn find_bundle(dir: &Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == BUNDLE) && path.is_dir() {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_bundle(&path) { return Some(found); }
        }
    }
    None
}

/// Put the newest backup back in place; what was installed becomes a backup.
pub fn roll_back() -> Result<String, String> {
    roll_back_at(&install_dir(), &backup_dir())
}

fn roll_back_at(plugins: &Path, backup_root: &Path) -> Result<String, String> {
    let (_, label, folder) = backups_in(backup_root).into_iter().next().ok_or("no backup to roll back to")?;
    let work = std::env::temp_dir().join(format!("fpsup-rollback-{}", now_s()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let staged = work.join(BUNDLE);
    if cfg!(target_os = "windows") {
        std::fs::rename(folder.join(BUNDLE), &staged).map_err(|e| format!("{}: {e}", folder.display()))?;
    } else {
        run(Command::new("mv").arg(folder.join(BUNDLE)).arg(&staged))?;
    }
    let _ = std::fs::remove_dir(&folder);
    swap_in_at(plugins, backup_root, &staged, &format!("v{RELEASE}"))?;
    Ok(format!("Rolled back to {label} (was v{RELEASE}); restart the editor to load it"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_are_read_newest_first_and_others_ignored() {
        let json = r#"[
            {"tag_name": "fpsup-v0.1.2", "draft": false, "assets": [{"name": "SHA256SUMS", "browser_download_url": "https://x/s"}]},
            {"tag_name": "v2.1.1", "draft": false, "assets": []},
            {"tag_name": "fpsup-v0.1.10", "draft": false, "assets": [{"name": "fpSupGyroflow-OpenFX-macos.zip", "browser_download_url": "https://x/m"}]},
            {"tag_name": "fpsup-v0.1.11", "draft": true, "assets": []}
        ]"#;
        let r = parse_releases(json).unwrap();
        assert_eq!(r.iter().map(|r| r.number).collect::<Vec<_>>(), vec![(0, 1, 10), (0, 1, 2)]);
        assert_eq!(r[0].assets[0].0, "fpSupGyroflow-OpenFX-macos.zip");
        assert_eq!(release_number("fpsup-v0.2.0"), Some((0, 2, 0)));
        assert_eq!(release_number("fpsup-vx"), None);
        assert_eq!(release_number("v2.1.1"), None, "the upstream plugin's tags are not ours");
        assert_eq!(parse_version("0.1"), Some((0, 1, 0)));
        assert_eq!(parse_version("fpsup-v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(parse_version("0.1.10") > parse_version("0.1.9"));
        assert_eq!(this_build(), parse_version(RELEASE).unwrap());
        assert!(parse_releases("{}").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn install_keeps_a_backup_and_roll_back_swaps_back_and_forth() {
        let root = std::env::temp_dir().join(format!("fpsup-update-test-{}", fastrand::u64(..)));
        let (plugins, backups) = (root.join("Plugins"), root.join("backups"));
        let bundle = |dir: &Path, mark: &str| {
            std::fs::create_dir_all(dir.join(BUNDLE).join("Contents")).unwrap();
            std::fs::write(dir.join(BUNDLE).join("Contents/mark"), mark).unwrap();
        };
        let mark = || std::fs::read_to_string(plugins.join(BUNDLE).join("Contents/mark")).unwrap();
        bundle(&plugins, "old");
        let incoming = root.join("incoming");
        bundle(&incoming, "new");
        swap_in_at(&plugins, &backups, &incoming.join(BUNDLE), "v1").unwrap();
        assert_eq!(mark(), "new");
        assert_eq!(backups_in(&backups).len(), 1);
        roll_back_at(&plugins, &backups).unwrap();
        assert_eq!(mark(), "old", "rolled back");
        roll_back_at(&plugins, &backups).unwrap();
        assert_eq!(mark(), "new", "and the roll back itself can be undone");
        assert_eq!(backups_in(&backups).len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore]
    fn the_release_zip_downloads_fast() {
        // network: the newest release's zip for this platform, in well under a minute
        let r = fetch_releases().unwrap();
        let url = r[0].assets.iter().find(|(n, _)| n == platform_asset()).unwrap().1.clone();
        let to = std::env::temp_dir().join(format!("fpsup-dl-test-{}", now_s()));
        let t = std::time::Instant::now();
        download(&url, &to).unwrap();
        let n = std::fs::metadata(&to).unwrap().len();
        println!("{n} bytes in {:?}", t.elapsed());
        assert!(n > 1 << 20 && t.elapsed().as_secs() < 60);
        let _ = std::fs::remove_file(&to);
    }

    #[test]
    fn the_digest_is_taken_for_the_named_file_only() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  fpSupGyroflow-OpenFX-linux.zip\n{b} *./fpSupGyroflow-OpenFX-macos.zip\nshort  x.zip\n");
        assert_eq!(digest_for(&sums, "fpSupGyroflow-OpenFX-macos.zip"), Some("b".repeat(64)));
        assert_eq!(digest_for(&sums, "fpSupGyroflow-OpenFX-linux.zip"), Some(a));
        assert_eq!(digest_for(&sums, "x.zip"), None);
        assert_eq!(digest_for(&sums, "missing.zip"), None);
    }
}
