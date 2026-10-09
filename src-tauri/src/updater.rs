//! Updates from BiWrite's GitHub releases. Every release is listed,
//! pre-releases included (branch builds are published as pre-releases), so
//! any version can be installed, older ones too. The installer for this
//! platform is downloaded and checked against the SHA-256 that GitHub
//! reports. On macOS the app bundle is then replaced in place; on Windows
//! the installer runs after BiWrite quits.

use std::cmp::Ordering;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::error::{CommandError, CommandResult};
use crate::state::AppState;

const RELEASES: &str = "https://api.github.com/repos/zzccppp/biwrite/releases?per_page=40";
pub const EVENT_UPDATE: &str = "update-progress";
/// Largest installer accepted.
const MAX_INSTALLER_BYTES: u64 = 600 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    name: Option<String>,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
    published_at: Option<String>,
    body: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    /// `sha256:<hex>`, when GitHub has computed it.
    digest: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Newer,
    Current,
    Older,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetView {
    pub name: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseView {
    pub tag: String,
    pub name: String,
    pub version: String,
    pub prerelease: bool,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub notes: String,
    pub relation: Relation,
    /// The installer for this platform, if the release has one.
    pub asset: Option<AssetView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatesView {
    pub current: String,
    pub releases: Vec<ReleaseView>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    tag: String,
    received: u64,
    total: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledView {
    pub version: String,
    /// The new version is in place; it starts with the next launch.
    pub relaunch: bool,
    /// BiWrite quits so the installer can run.
    pub quitting: bool,
    /// Where the installer was saved (when BiWrite could not install it).
    pub file: Option<String>,
}

#[derive(Default)]
pub struct UpdateState {
    /// The releases last listed, by tag (with their download URL).
    listed: Mutex<Vec<(GhRelease, Option<GhAsset>)>>,
    /// The app bundle replaced by an update (macOS), for relaunching.
    installed: Mutex<Option<PathBuf>>,
}

impl UpdateState {
    fn listed(&self) -> MutexGuard<'_, Vec<(GhRelease, Option<GhAsset>)>> {
        self.listed.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::Settings(message.into())
}

// ── Versions ─────────────────────────────────────────────────────────

/// `v0.2.0-beta.1` → `(0, 2, 0, ["beta", "1"])`.
fn parse_version(v: &str) -> Option<(u64, u64, u64, Vec<String>)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, pre.split('.').map(str::to_owned).collect()),
        None => (v, Vec::new()),
    };
    let mut parts = core.split('.').map(|p| p.parse::<u64>());
    let major = parts.next()?.ok()?;
    let minor = parts.next().unwrap_or(Ok(0)).ok()?;
    let patch = parts.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch, pre))
}

/// Semantic-version order: a pre-release comes before its release.
fn compare_versions(a: &str, b: &str) -> Ordering {
    let (Some(a), Some(b)) = (parse_version(a), parse_version(b)) else {
        return a.cmp(b);
    };
    let core = (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2));
    if core != Ordering::Equal {
        return core;
    }
    match (a.3.is_empty(), b.3.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in a.3.iter().zip(&b.3) {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            a.3.len().cmp(&b.3.len())
        }
    }
}

/// The installer for this platform among a release's assets.
fn pick_asset(assets: &[GhAsset]) -> Option<&GhAsset> {
    let find = |pred: &dyn Fn(&str) -> bool| assets.iter().find(|a| pred(&a.name.to_lowercase()));
    let arm = cfg!(target_arch = "aarch64");
    if cfg!(target_os = "macos") {
        find(&|n| n.ends_with("_universal.dmg"))
            .or_else(|| {
                find(&|n| n.ends_with(".dmg") && n.contains(if arm { "aarch64" } else { "x64" }))
            })
            .or_else(|| find(&|n| n.ends_with(".dmg")))
    } else if cfg!(windows) {
        find(&|n| n.ends_with("-setup.exe") && n.contains(if arm { "arm64" } else { "x64" }))
            .or_else(|| find(&|n| n.ends_with("-setup.exe")))
            .or_else(|| find(&|n| n.ends_with(".msi")))
    } else {
        find(&|n| n.ends_with(".appimage")).or_else(|| find(&|n| n.ends_with(".deb")))
    }
}

fn sha256_of(digest: Option<&str>) -> Option<String> {
    digest
        .and_then(|d| d.strip_prefix("sha256:"))
        .map(str::to_ascii_lowercase)
}

fn view(r: &GhRelease, asset: Option<&GhAsset>) -> ReleaseView {
    let version = r.tag_name.trim_start_matches('v').to_owned();
    let relation = match compare_versions(&version, env!("CARGO_PKG_VERSION")) {
        Ordering::Greater => Relation::Newer,
        Ordering::Equal => Relation::Current,
        Ordering::Less => Relation::Older,
    };
    ReleaseView {
        tag: r.tag_name.clone(),
        name: r
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| r.tag_name.clone()),
        version,
        prerelease: r.prerelease,
        date: r
            .published_at
            .as_deref()
            .and_then(|d| d.get(..10))
            .unwrap_or("")
            .to_owned(),
        notes: r.body.clone().unwrap_or_default(),
        relation,
        asset: asset.map(|a| AssetView {
            name: a.name.clone(),
            size: a.size,
            sha256: sha256_of(a.digest.as_deref()),
        }),
    }
}

fn client() -> CommandResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("BiWrite/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| fail(e.to_string()))
}

// ── Commands ─────────────────────────────────────────────────────────

/// Every published release, newest first.
#[tauri::command]
pub async fn list_releases(state: State<'_, AppState>) -> CommandResult<UpdatesView> {
    let body = client()?
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| fail(format!("GitHub: {e}")))?
        .bytes()
        .await
        .map_err(|e| fail(format!("GitHub: {e}")))?;
    let mut releases: Vec<GhRelease> =
        serde_json::from_slice(&body).map_err(|e| fail(format!("GitHub: {e}")))?;
    releases.retain(|r| !r.draft && parse_version(&r.tag_name).is_some());
    releases.sort_by(|a, b| compare_versions(&b.tag_name, &a.tag_name));
    let views = releases
        .iter()
        .map(|r| view(r, pick_asset(&r.assets)))
        .collect();
    *state.updates.listed() = releases
        .into_iter()
        .map(|r| {
            let asset = pick_asset(&r.assets).cloned();
            (r, asset)
        })
        .collect();
    Ok(UpdatesView {
        current: env!("CARGO_PKG_VERSION").to_owned(),
        releases: views,
    })
}

/// Download release `tag`'s installer, check it, and install it.
#[tauri::command]
pub async fn install_release(
    app: AppHandle,
    state: State<'_, AppState>,
    tag: String,
) -> CommandResult<InstalledView> {
    let (release, asset) = state
        .updates
        .listed()
        .iter()
        .find(|(r, _)| r.tag_name == tag)
        .cloned()
        .ok_or_else(|| fail(format!("{tag} is not among the listed releases")))?;
    let asset = asset.ok_or_else(|| fail(format!("{tag} has no installer for this computer")))?;
    if asset.size > MAX_INSTALLER_BYTES {
        return Err(fail("the installer is unexpectedly large"));
    }
    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| fail(e.to_string()))?
        .join("updates");
    std::fs::create_dir_all(&dir).map_err(|e| CommandError::io(&dir, e))?;
    let file = dir.join(safe_name(&asset.name));
    download(&app, &tag, &asset, &file).await?;
    let version = release.tag_name.trim_start_matches('v').to_owned();
    log::info!("downloaded BiWrite {version} ({})", asset.name);
    install(&app, &state, &file, version).await
}

/// Start the version installed by `install_release` (macOS).
#[tauri::command]
pub async fn relaunch(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    if state.is_dirty() {
        return Err(fail("Save or discard the changes first."));
    }
    let bundle = state
        .updates
        .installed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
        .ok_or_else(|| fail("no update was installed"))?;
    // A shell that outlives BiWrite opens the new version once it is gone.
    let script = format!("sleep 1; /usr/bin/open -n {}", shell_quote(&bundle));
    std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    app.exit(0);
    Ok(())
}

fn safe_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Download to `file`, reporting progress, then check size and SHA-256.
async fn download(app: &AppHandle, tag: &str, asset: &GhAsset, file: &Path) -> CommandResult<()> {
    let mut response = client()?
        .get(&asset.browser_download_url)
        .timeout(Duration::from_secs(1800))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| fail(format!("download: {e}")))?;
    let part = file.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| CommandError::io(&part, e))?;
    let mut hash = Sha256::new();
    let mut received = 0u64;
    let mut reported = Instant::now();
    let result = async {
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| fail(format!("download: {e}")))?
        {
            received += chunk.len() as u64;
            if received > MAX_INSTALLER_BYTES {
                return Err(fail("the installer is unexpectedly large"));
            }
            hash.update(&chunk);
            out.write_all(&chunk)
                .map_err(|e| CommandError::io(&part, e))?;
            if reported.elapsed() > Duration::from_millis(120) {
                reported = Instant::now();
                let _ = app.emit(
                    EVENT_UPDATE,
                    Progress {
                        tag: tag.to_owned(),
                        received,
                        total: asset.size,
                    },
                );
            }
        }
        out.sync_all().map_err(|e| CommandError::io(&part, e))?;
        if received != asset.size {
            return Err(fail(format!(
                "the download is incomplete ({received} of {} bytes)",
                asset.size
            )));
        }
        let got = format!("{:x}", hash.finalize_reset());
        if let Some(want) = sha256_of(asset.digest.as_deref())
            && want != got
        {
            return Err(fail(
                "the download does not match the checksum GitHub reports",
            ));
        }
        std::fs::rename(&part, file).map_err(|e| CommandError::io(file, e))
    }
    .await;
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    let _ = app.emit(
        EVENT_UPDATE,
        Progress {
            tag: tag.to_owned(),
            received,
            total: asset.size,
        },
    );
    result
}

#[cfg(target_os = "macos")]
async fn install(
    _app: &AppHandle,
    state: &AppState,
    file: &Path,
    version: String,
) -> CommandResult<InstalledView> {
    let file = file.to_path_buf();
    let installed = tokio::task::spawn_blocking(move || replace_bundle(&file))
        .await
        .map_err(|e| CommandError::Task(e.to_string()))?;
    match installed {
        Ok(bundle) => {
            log::info!("installed BiWrite {version} into {}", bundle.display());
            *state
                .updates
                .installed
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(bundle);
            Ok(InstalledView {
                version,
                relaunch: true,
                quitting: false,
                file: None,
            })
        }
        Err(e) => Err(e),
    }
}

/// Mount the disk image, copy its app next to the running one, and swap
/// them. The running process keeps its files; the next launch is the new
/// version.
#[cfg(target_os = "macos")]
fn replace_bundle(dmg: &Path) -> CommandResult<PathBuf> {
    use std::process::Command;
    let exe = std::env::current_exe().map_err(|e| fail(e.to_string()))?;
    let bundle = exe
        .ancestors()
        .nth(3)
        .filter(|p| p.extension().is_some_and(|x| x == "app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| fail("only an installed BiWrite.app can update itself"))?;
    let parent = bundle
        .parent()
        .unwrap_or(Path::new("/Applications"))
        .to_path_buf();
    let mount = dmg.with_extension("mount");
    let _ = std::fs::create_dir_all(&mount);
    let attach = Command::new("/usr/bin/hdiutil")
        .args([
            "attach",
            "-nobrowse",
            "-readonly",
            "-noautoopen",
            "-mountpoint",
        ])
        .arg(&mount)
        .arg(dmg)
        .output()
        .map_err(|e| fail(format!("hdiutil: {e}")))?;
    if !attach.status.success() {
        return Err(fail(format!(
            "the disk image could not be opened: {}",
            String::from_utf8_lossy(&attach.stderr).trim()
        )));
    }
    let result = (|| {
        let app = std::fs::read_dir(&mount)
            .map_err(|e| CommandError::io(&mount, e))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .ok_or_else(|| fail("the disk image holds no app"))?;
        let staged = parent.join(".BiWrite.app.new");
        let old = parent.join(".BiWrite.app.old");
        let _ = std::fs::remove_dir_all(&staged);
        let _ = std::fs::remove_dir_all(&old);
        let copy = Command::new("/usr/bin/ditto")
            .arg(&app)
            .arg(&staged)
            .output()
            .map_err(|e| fail(format!("ditto: {e}")))?;
        if !copy.status.success() {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(fail(format!(
                "BiWrite cannot write to {}: {}",
                parent.display(),
                String::from_utf8_lossy(&copy.stderr).trim()
            )));
        }
        std::fs::rename(&bundle, &old).map_err(|e| CommandError::io(&bundle, e))?;
        if let Err(e) = std::fs::rename(&staged, &bundle) {
            let _ = std::fs::rename(&old, &bundle);
            return Err(CommandError::io(&bundle, e));
        }
        let _ = std::fs::remove_dir_all(&old);
        // Downloaded by BiWrite itself, the copy carries no quarantine flag;
        // clear it anyway so the first launch is not blocked.
        let _ = Command::new("/usr/bin/xattr")
            .args(["-dr", "com.apple.quarantine"])
            .arg(&bundle)
            .output();
        Ok(bundle.clone())
    })();
    let _ = Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet", "-force"])
        .arg(&mount)
        .output();
    let _ = std::fs::remove_dir(&mount);
    result
}

#[cfg(windows)]
async fn install(
    app: &AppHandle,
    state: &AppState,
    file: &Path,
    version: String,
) -> CommandResult<InstalledView> {
    if state.is_dirty() {
        return Err(fail(
            "Save or discard the changes first: BiWrite quits to install.",
        ));
    }
    std::process::Command::new(file)
        .spawn()
        .map_err(|e| CommandError::io(file, e))?;
    // Quit after the answer reaches the window, so the installer can
    // replace the program files.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(600)).await;
        handle.exit(0);
    });
    Ok(InstalledView {
        version,
        relaunch: false,
        quitting: true,
        file: Some(file.display().to_string()),
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn install(
    _app: &AppHandle,
    _state: &AppState,
    file: &Path,
    version: String,
) -> CommandResult<InstalledView> {
    crate::files::reveal_file(file)?;
    Ok(InstalledView {
        version,
        relaunch: false,
        quitting: false,
        file: Some(file.display().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_like_semver() {
        assert_eq!(compare_versions("0.2.0", "0.1.1"), Ordering::Greater);
        assert_eq!(compare_versions("v0.2.0", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.0-beta.1", "0.2.0"), Ordering::Less);
        assert_eq!(
            compare_versions("0.2.0-beta.2", "0.2.0-beta.10"),
            Ordering::Less
        );
        assert_eq!(
            compare_versions("0.2.0-beta", "0.2.0-alpha"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("1.0", "0.9.9"), Ordering::Greater);
        assert!(parse_version("latest").is_none());
    }

    fn asset(name: &str) -> GhAsset {
        GhAsset {
            name: name.into(),
            size: 1,
            browser_download_url: format!("https://example.invalid/{name}"),
            digest: Some("sha256:ABCDEF".into()),
        }
    }

    #[test]
    fn the_installer_for_this_platform() {
        let assets = vec![
            asset("BiWrite_0.2.0_x64-setup.exe"),
            asset("BiWrite_0.2.0_universal.dmg"),
            asset("notes.txt"),
        ];
        let picked = pick_asset(&assets).map(|a| a.name.as_str());
        if cfg!(target_os = "macos") {
            assert_eq!(picked, Some("BiWrite_0.2.0_universal.dmg"));
        } else if cfg!(windows) {
            assert_eq!(picked, Some("BiWrite_0.2.0_x64-setup.exe"));
        }
        assert_eq!(sha256_of(Some("sha256:ABCDEF")).as_deref(), Some("abcdef"));
        assert_eq!(sha256_of(Some("md5:00")), None);
    }

    #[test]
    fn releases_from_the_github_api() {
        let json = r#"[
            {"tag_name":"v0.2.0-beta.1","name":"BiWrite 0.2.0 beta","prerelease":true,"draft":false,
             "published_at":"2026-10-10T01:02:03Z","body":"Branch build","assets":[
               {"name":"BiWrite_0.2.0-beta.1_universal.dmg","size":10,"browser_download_url":"https://x/y.dmg","digest":"sha256:00"}]},
            {"tag_name":"v0.1.1","name":"","prerelease":false,"draft":false,"published_at":null,"body":null,"assets":[]}
        ]"#;
        let releases: Vec<GhRelease> = serde_json::from_str(json).unwrap();
        let first = view(&releases[0], pick_asset(&releases[0].assets));
        assert_eq!(first.version, "0.2.0-beta.1");
        assert_eq!(first.date, "2026-10-10");
        assert!(first.prerelease);
        let second = view(&releases[1], None);
        assert_eq!(second.name, "v0.1.1");
        assert!(second.asset.is_none());
    }

    #[test]
    fn names_and_quotes_are_safe() {
        assert_eq!(safe_name("BiWrite 0.2/../x.dmg"), "BiWrite_0.2_.._x.dmg");
        assert_eq!(shell_quote(Path::new("/A/it's.app")), r"'/A/it'\''s.app'");
    }
}
