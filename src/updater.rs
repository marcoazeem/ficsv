//! Self-update from GitHub Releases.
//!
//! * `check` asks the GitHub API for the latest release and compares versions.
//! * `install` downloads this platform's zip, swaps the app in place and returns the
//!   path to relaunch. macOS replaces the whole `FI CSV.app` bundle; Windows replaces the
//!   running `.exe` (via `self_replace`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const REPO: &str = "marcoazeem/ficsv";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub notes_url: String,
    pub asset_url: String,
    pub asset_size: u64,
}

/// Version of this build. `FICSV_UPDATE_TEST_VERSION` overrides it (for testing updates).
pub fn current_version() -> String {
    std::env::var("FICSV_UPDATE_TEST_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// Parse "v1.2.3" / "1.2.3" into comparable numbers.
pub fn parse_version(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

/// Release asset built for this machine (names match .github/workflows/build.yml).
pub fn asset_name() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("ficsv-macos-arm64.zip")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("ficsv-macos-x64.zip")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("ficsv-windows-x64.zip")
    } else {
        None
    }
}

/// Extract the update for this platform from a GitHub "latest release" JSON document.
pub fn parse_release(json: &str, asset: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("bad response from GitHub: {e}"))?;
    let tag = v["tag_name"].as_str().ok_or("release has no tag")?;
    let notes_url = v["html_url"].as_str().unwrap_or_default().to_string();
    let a = v["assets"]
        .as_array()
        .and_then(|list| list.iter().find(|a| a["name"].as_str() == Some(asset)))
        .ok_or_else(|| format!("release {tag} has no {asset}"))?;
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        notes_url,
        asset_url: a["browser_download_url"].as_str().ok_or("asset has no download URL")?.to_string(),
        asset_size: a["size"].as_u64().unwrap_or(0),
    })
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(std::time::Duration::from_secs(10)).timeout_read(std::time::Duration::from_secs(60)).build()
}

/// Ask GitHub for the latest release. `Ok(None)` = already up to date.
pub fn check() -> Result<Option<Release>, String> {
    check_against(&current_version())
}

/// Like `check`, comparing against an explicit installed version.
pub fn check_against(current: &str) -> Result<Option<Release>, String> {
    let asset = asset_name().ok_or("automatic updates aren't available on this platform")?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = agent()
        .get(&url)
        .set("User-Agent", "fi-csv-updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("couldn't reach GitHub: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let rel = parse_release(&body, asset)?;
    Ok(is_newer(&rel.version, current).then_some(rel))
}

/// The installed app to replace: the `.app` bundle on macOS, the `.exe` on Windows.
pub fn install_target() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if cfg!(target_os = "macos") {
        // …/FI CSV.app/Contents/MacOS/ficsv
        let bundle = exe.ancestors().nth(3).map(Path::to_path_buf).ok_or("unexpected app location")?;
        if bundle.extension().and_then(|e| e.to_str()) != Some("app") {
            return Err("updates only work when running the installed app (not a development build)".into());
        }
        Ok(bundle)
    } else {
        Ok(exe)
    }
}

/// Download to `dest`, reporting bytes received through `progress`.
fn download(url: &str, dest: &Path, progress: &AtomicU64) -> Result<(), String> {
    let resp = agent().get(url).set("User-Agent", "fi-csv-updater").call().map_err(|e| format!("download failed: {e}"))?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download failed: {e}"))?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?;
        progress.fetch_add(n as u64, Ordering::Relaxed);
    }
    Ok(())
}

/// Download and install `rel`. Returns the path to relaunch.
pub fn install(rel: &Release, progress: &AtomicU64) -> Result<PathBuf, String> {
    install_to(rel, install_target()?, progress)
}

/// Install `rel` over `target` (an `.app` bundle on macOS, an `.exe` elsewhere).
pub fn install_to(rel: &Release, target: PathBuf, progress: &AtomicU64) -> Result<PathBuf, String> {
    let work = std::env::temp_dir().join(format!("fi-csv-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let zip_path = work.join("update.zip");
    download(&rel.asset_url, &zip_path, progress)?;
    if rel.asset_size > 0 {
        let got = std::fs::metadata(&zip_path).map(|m| m.len()).unwrap_or(0);
        if got != rel.asset_size {
            return Err(format!("download incomplete ({got} of {} bytes)", rel.asset_size));
        }
    }
    let result = if cfg!(target_os = "macos") { install_macos(&zip_path, &target) } else { install_windows(&zip_path, &target, &work) };
    let _ = std::fs::remove_dir_all(&work);
    result.map(|_| target)
}

fn install_macos(zip_path: &Path, bundle: &Path) -> Result<(), String> {
    let parent = bundle.parent().ok_or("app has no parent folder")?;
    // Unpack next to the installed app so the final swap is an atomic rename on one volume.
    let staging = parent.join(".fi-csv-update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("can't write to {}: {e}", parent.display()))?;
    let ok = std::process::Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(zip_path)
        .arg(&staging)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("couldn't unpack the update".into());
    }
    let new_app = std::fs::read_dir(&staging)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("app"))
        .ok_or("update doesn't contain an app")?;
    let old = parent.join(".fi-csv-old.app");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(bundle, &old).map_err(|e| format!("can't replace the app: {e}"))?;
    if let Err(e) = std::fs::rename(&new_app, bundle) {
        let _ = std::fs::rename(&old, bundle); // roll back
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("can't install the update: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

fn install_windows(zip_path: &Path, _exe: &Path, work: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("bad update archive: {e}"))?;
    let idx = (0..archive.len())
        .find(|&i| archive.by_index(i).map(|f| f.name().to_ascii_lowercase().ends_with(".exe")).unwrap_or(false))
        .ok_or("update doesn't contain an .exe")?;
    let new_exe = work.join("new.exe");
    {
        let mut entry = archive.by_index(idx).map_err(|e| e.to_string())?;
        let mut out = std::fs::File::create(&new_exe).map_err(|e| e.to_string())?;
        std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
    }
    // Windows can't overwrite a running .exe; self_replace moves it aside first.
    self_replace::self_replace(&new_exe).map_err(|e| format!("can't replace the app: {e}"))
}

/// Start the freshly installed app.
pub fn relaunch(target: &Path) -> Result<(), String> {
    let r = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open").arg("-n").arg(target).spawn()
    } else {
        std::process::Command::new(target).spawn()
    };
    r.map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real network test: FICSV_UPDATE_E2E=1 cargo test updater_e2e -- --ignored --nocapture
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn updater_e2e() {
        if std::env::var("FICSV_UPDATE_E2E").is_err() {
            return;
        }
        // Pretend to be an old install and update a throwaway bundle from the live release.
        let rel = check_against("0.0.1").expect("check").expect("a newer release must exist");
        println!("latest release {} ({} bytes)", rel.version, rel.asset_size);
        let dir = std::env::temp_dir().join(format!("fi-csv-e2e-{}", std::process::id()));
        let app = dir.join("FI CSV.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), "old").unwrap();
        let progress = AtomicU64::new(0);
        let t = std::time::Instant::now();
        let target = install_to(&rel, app.clone(), &progress).expect("install");
        println!("installed in {:?}, downloaded {} bytes", t.elapsed(), progress.load(Ordering::Relaxed));
        let plist = std::fs::read_to_string(target.join("Contents/Info.plist")).unwrap();
        assert!(plist.contains(&format!("<string>{}</string>", rel.version)), "new Info.plist expected");
        assert!(target.join("Contents/MacOS/ficsv").is_file());
        assert!(!dir.join(".fi-csv-old.app").exists() && !dir.join(".fi-csv-update").exists(), "temp folders cleaned up");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn parses_github_release() {
        let json = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/x/y/releases/tag/v0.3.0",
            "assets":[{"name":"ficsv-windows-x64.zip","size":10,"browser_download_url":"https://e/w.zip"},
                      {"name":"ficsv-macos-arm64.zip","size":20,"browser_download_url":"https://e/m.zip"}]}"#;
        let r = parse_release(json, "ficsv-macos-arm64.zip").unwrap();
        assert_eq!(r.version, "0.3.0");
        assert_eq!(r.asset_url, "https://e/m.zip");
        assert_eq!(r.asset_size, 20);
        assert!(parse_release(json, "ficsv-macos-x64.zip").is_err());
    }
}
