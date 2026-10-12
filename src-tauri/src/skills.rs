//! The writing skill behind the assistant: the research-builder skill
//! (https://github.com/qzkinhit/research-builder) by default.
//!
//! Three copies can be in use, first match wins:
//! 1. a folder the author chose (for example their own skill with private
//!    materials), if it still holds the skill's files;
//! 2. the latest version downloaded from GitHub into the app data folder;
//! 3. the version bundled with the app.
//!
//! Only the files the assistant reads are loaded (see
//! `biwrite_core::assist::Action::skill_files`), each at most 1 MB.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use biwrite_core::assist::{Skill, SkillIcon};
use serde::{Deserialize, Serialize};

/// The files loaded from a skill, relative to its root.
pub const SKILL_FILES: &[&str] = &[
    "SKILL.md",
    "README.md",
    "writing-playbook.md",
    "writing-deai.md",
    "knowledge/paper-anatomy.md",
    "knowledge/storytelling.md",
    "knowledge/figure-archetypes.md",
    "knowledge/rebuttal.md",
    "knowledge/grant-proposal.md",
    "knowledge/benchmark-papers.md",
    "figure-style/README.md",
    "figure-style/figstyle.py",
    "figure-style/tables/README.md",
    "figure-style/tables/table_macros.tex",
    "paper-figure-pptx/SKILL.md",
    "paper-figure-pptx/references/layout-and-type.md",
    "paper-figure-pptx/references/pptx-native.md",
    "paper-figure-pptx/references/paper-styles.md",
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
];

/// Where a skill keeps its icon libraries: a folder each, listed by its
/// `manifest.json`.
pub const ICON_DIR: &str = "paper-figure-pptx/assets/icon-library";
/// Most icons loaded, and the largest icon file.
const MAX_ICONS: usize = 300;
const MAX_ICON_BYTES: u64 = 400_000;

/// Files without which the assistant cannot follow the skill.
const REQUIRED: &[&str] = &["writing-deai.md", "writing-playbook.md"];

const REPO: &str = "qzkinhit/research-builder";
const MAX_FILE_BYTES: u64 = 1_000_000;
const MAX_ARCHIVE_BYTES: usize = 60_000_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Manifest {
    name: String,
    source: String,
    version: String,
    date: String,
}

/// The skill in use, as the settings show it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub source: String,
    pub version: String,
    pub date: String,
    /// `folder`, `downloaded` or `builtin`.
    pub origin: &'static str,
    pub folder: Option<String>,
    pub files: usize,
    pub missing: Vec<String>,
}

pub struct SkillStore {
    builtin: Option<PathBuf>,
    downloaded: Option<PathBuf>,
    cache: Mutex<Option<(PathBuf, Arc<Skill>, SkillInfo)>>,
}

/// Whether `dir` holds a usable skill.
pub fn is_skill_dir(dir: &Path) -> bool {
    REQUIRED.iter().all(|f| dir.join(f).is_file())
}

fn read_manifest(dir: &Path) -> Manifest {
    std::fs::read_to_string(dir.join("skill.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// A small file of `dir` (at most `limit` bytes), by its path relative to it.
fn read_small(dir: &Path, rel: &str, limit: u64) -> Option<Vec<u8>> {
    let path = dir.join(rel);
    // Paths come from manifests: never out of the library.
    if rel.contains("..") || Path::new(rel).is_absolute() {
        return None;
    }
    std::fs::metadata(&path)
        .is_ok_and(|m| m.is_file() && m.len() <= limit)
        .then(|| std::fs::read(&path).ok())
        .flatten()
}

/// The icons of the icon libraries under `dir`'s [`ICON_DIR`], as their
/// manifests list them: the skill's starter library (`id`, `meaning`,
/// `svg`, `png`) and libraries taken from the author's slides (`path`,
/// `metadata.name_zh`, `png_fallbacks`). Icons of another library than the
/// starter one are named `library/id`; icons without a meaning are left
/// out, since nothing tells what they show.
pub(crate) fn load_icons(dir: &Path) -> Vec<SkillIcon> {
    let root = dir.join(ICON_DIR);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut libraries: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("manifest.json").is_file())
        .collect();
    // The starter library first, then the others by name.
    libraries.sort_by_key(|p| (p.file_name().is_none_or(|n| n != "starter"), p.clone()));
    let mut out: Vec<SkillIcon> = Vec::new();
    for lib in libraries {
        let library = lib
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(json) = read_small(&lib, "manifest.json", 4_000_000)
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        else {
            continue;
        };
        for icon in json["icons"].as_array().into_iter().flatten() {
            if out.len() >= MAX_ICONS {
                return out;
            }
            let text = |v: &serde_json::Value| v.as_str().unwrap_or("").trim().to_owned();
            let tags = |v: &serde_json::Value| {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let raw_id = text(&icon["id"]);
            let id = raw_id.trim_end_matches(".svg").to_owned();
            let (meaning, svg_rel, png_rel) = if icon["svg"].is_string() {
                let tags = tags(&icon["tags"]);
                let meaning = text(&icon["meaning"]);
                (
                    if tags.is_empty() {
                        meaning
                    } else {
                        format!("{meaning} ({tags})")
                    },
                    text(&icon["svg"]),
                    text(&icon["png"]),
                )
            } else {
                let name = text(&icon["metadata"]["name_zh"]);
                let tags = tags(&icon["metadata"]["purpose_tags"]);
                (
                    if name.is_empty() || tags.is_empty() {
                        name
                    } else {
                        format!("{name} ({tags})")
                    },
                    text(&icon["path"]),
                    text(&icon["png_fallbacks"][0]["path"]),
                )
            };
            if id.is_empty() || meaning.is_empty() || svg_rel.is_empty() {
                continue;
            }
            let Some(svg) = read_small(&lib, &svg_rel, MAX_ICON_BYTES)
                .and_then(|b| String::from_utf8(b).ok())
                .filter(|t| t.contains("<svg"))
            else {
                continue;
            };
            let id = if library == "starter" {
                id
            } else {
                format!("{library}/{id}")
            };
            if out.iter().any(|i| i.id == id) {
                continue;
            }
            out.push(SkillIcon {
                id,
                meaning,
                library: library.clone(),
                svg,
                png: (!png_rel.is_empty())
                    .then(|| read_small(&lib, &png_rel, MAX_ICON_BYTES))
                    .flatten(),
            });
        }
    }
    out
}

/// Load the skill's files from `dir`.
fn load(dir: &Path, origin: &'static str) -> (Skill, SkillInfo) {
    let manifest = read_manifest(dir);
    let mut files = Vec::new();
    for path in SKILL_FILES {
        let full = dir.join(path);
        let small =
            std::fs::metadata(&full).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES);
        if small && let Ok(text) = std::fs::read_to_string(&full) {
            files.push(((*path).to_owned(), text));
        }
    }
    let missing: Vec<String> = REQUIRED
        .iter()
        .filter(|r| !files.iter().any(|(p, _)| p == *r))
        .map(|r| (*r).to_owned())
        .collect();
    let name = if manifest.name.is_empty() {
        "research-builder".to_owned()
    } else {
        manifest.name
    };
    let source = if origin == "folder" {
        dir.display().to_string()
    } else if manifest.source.is_empty() {
        format!("https://github.com/{REPO}")
    } else {
        manifest.source
    };
    let version = if manifest.version.is_empty() {
        "local".to_owned()
    } else {
        manifest.version
    };
    let info = SkillInfo {
        name: name.clone(),
        source: source.clone(),
        version: version.clone(),
        date: manifest.date,
        origin,
        folder: (origin == "folder").then(|| dir.display().to_string()),
        files: files.len(),
        missing,
    };
    (
        Skill {
            name,
            source,
            version,
            files,
            icons: load_icons(dir),
        },
        info,
    )
}

impl SkillStore {
    /// `builtin`: the bundled copy. `downloaded`: where downloads go.
    pub fn new(builtin: Option<PathBuf>, downloaded: Option<PathBuf>) -> Self {
        Self {
            builtin,
            downloaded,
            cache: Mutex::new(None),
        }
    }

    fn pick(&self, folder: Option<&Path>) -> Option<(PathBuf, &'static str)> {
        if let Some(f) = folder.filter(|f| is_skill_dir(f)) {
            return Some((f.to_path_buf(), "folder"));
        }
        if let Some(d) = self.downloaded.as_ref().filter(|d| is_skill_dir(d)) {
            return Some((d.clone(), "downloaded"));
        }
        self.builtin
            .as_ref()
            .filter(|b| is_skill_dir(b))
            .map(|b| (b.clone(), "builtin"))
    }

    /// The skill in use (cached until the choice changes).
    pub fn current(&self, folder: Option<&Path>) -> Option<(Arc<Skill>, SkillInfo)> {
        let (dir, origin) = self.pick(folder)?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((cached, skill, info)) = cache.as_ref()
            && *cached == dir
        {
            return Some((Arc::clone(skill), info.clone()));
        }
        let (skill, info) = load(&dir, origin);
        let skill = Arc::new(skill);
        *cache = Some((dir, Arc::clone(&skill), info.clone()));
        Some((skill, info))
    }

    /// Where downloads go (shown by "Show folder").
    pub fn downloaded_dir(&self) -> Option<PathBuf> {
        self.downloaded.clone().filter(|d| d.is_dir())
    }

    /// Forget the cached copy (after an update or a new folder).
    pub fn invalidate(&self) {
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Download the latest version from GitHub and keep it next to the app
    /// data. Only the skill's files are extracted.
    pub async fn update(&self, client: &reqwest::Client) -> Result<(), String> {
        let target = self
            .downloaded
            .clone()
            .ok_or("the app data folder is not available")?;
        let body = client
            .get(format!("https://api.github.com/repos/{REPO}/commits/main"))
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("GitHub: {e}"))?
            .bytes()
            .await
            .map_err(|e| format!("GitHub: {e}"))?;
        let commit: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("GitHub: {e}"))?;
        let sha = commit["sha"].as_str().unwrap_or("").to_owned();
        let date = commit
            .pointer("/commit/committer/date")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .get(..10)
            .unwrap_or("")
            .to_owned();
        let archive = client
            .get(format!(
                "https://codeload.github.com/{REPO}/zip/refs/heads/main"
            ))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| format!("download: {e}"))?
            .bytes()
            .await
            .map_err(|e| format!("download: {e}"))?;
        if archive.len() > MAX_ARCHIVE_BYTES {
            return Err("the download is unexpectedly large".into());
        }
        let manifest = Manifest {
            name: "research-builder".into(),
            source: format!("https://github.com/{REPO}"),
            version: sha.get(..7).unwrap_or(&sha).to_owned(),
            date,
        };
        tokio::task::spawn_blocking(move || extract(&archive, &target, &manifest))
            .await
            .map_err(|e| e.to_string())??;
        self.invalidate();
        Ok(())
    }
}

/// Extract the skill's files from a GitHub archive (one top folder) into
/// `target`, replacing what was there only once everything is written.
fn extract(archive: &[u8], target: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive)).map_err(|e| e.to_string())?;
    let staging = target.with_extension("new");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        // Drop the archive's top folder ("research-builder-main/").
        let rel: PathBuf = name.components().skip(1).collect();
        let Some(rel_str) = rel.to_str().map(|s| s.replace('\\', "/")) else {
            continue;
        };
        // The text files the assistant reads, and the icon libraries.
        let icon = rel_str.starts_with(&format!("{ICON_DIR}/"))
            && (rel_str.ends_with(".svg")
                || rel_str.ends_with(".png")
                || rel_str.ends_with("/manifest.json"))
            && !rel_str.contains("/../");
        if !(SKILL_FILES.contains(&rel_str.as_str()) || icon) || entry.size() > MAX_FILE_BYTES {
            continue;
        }
        let out = staging.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        std::fs::write(&out, bytes).map_err(|e| e.to_string())?;
    }
    if !is_skill_dir(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("the downloaded skill is missing its writing rules".into());
    }
    let json = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    std::fs::write(staging.join("skill.json"), json).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(target);
    std::fs::rename(&staging, target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-skill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, tag: &str) {
        std::fs::write(dir.join("writing-deai.md"), format!("deai {tag}")).unwrap();
        std::fs::write(dir.join("writing-playbook.md"), format!("playbook {tag}")).unwrap();
    }

    #[test]
    fn a_chosen_folder_wins_then_the_download_then_the_bundle() {
        let builtin = temp("builtin");
        write_skill(&builtin, "bundled");
        std::fs::write(
            builtin.join("skill.json"),
            r#"{"name":"research-builder","source":"https://github.com/qzkinhit/research-builder","version":"d227e92","date":"2026-10-09"}"#,
        )
        .unwrap();
        let downloaded = temp("downloaded");
        let store = SkillStore::new(
            Some(builtin.clone()),
            Some(downloaded.join("research-builder")),
        );
        let (skill, info) = store.current(None).unwrap();
        assert_eq!(info.origin, "builtin");
        assert_eq!(info.version, "d227e92");
        assert_eq!(skill.file("writing-deai.md"), Some("deai bundled"));

        let mine = temp("mine");
        write_skill(&mine, "mine");
        let (skill, info) = store.current(Some(&mine)).unwrap();
        assert_eq!(info.origin, "folder");
        assert_eq!(skill.file("writing-deai.md"), Some("deai mine"));
        // A folder that lost its files no longer counts.
        std::fs::remove_file(mine.join("writing-deai.md")).unwrap();
        assert_eq!(store.current(Some(&mine)).unwrap().1.origin, "builtin");
        for d in [builtin, downloaded, mine] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }

    #[test]
    fn archives_yield_only_the_skill_files() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
            for (name, body) in [
                ("research-builder-main/writing-deai.md", "deai new"),
                ("research-builder-main/writing-playbook.md", "playbook new"),
                ("research-builder-main/knowledge/storytelling.md", "story"),
                ("research-builder-main/tools/polish_check.py", "not loaded"),
                ("research-builder-main/../escape.md", "never written"),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let root = temp("extract");
        let target = root.join("research-builder");
        let manifest = Manifest {
            name: "research-builder".into(),
            source: "https://github.com/qzkinhit/research-builder".into(),
            version: "abc1234".into(),
            date: "2026-10-10".into(),
        };
        extract(buf.get_ref(), &target, &manifest).unwrap();
        assert!(is_skill_dir(&target));
        assert!(target.join("knowledge/storytelling.md").is_file());
        assert!(!target.join("tools").exists());
        assert!(!root.join("escape.md").exists());
        let (_, info) = load(&target, "downloaded");
        assert_eq!(info.version, "abc1234");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn icons_come_from_every_library_with_a_manifest() {
        let dir = temp("icons");
        write_skill(&dir, "x");
        let starter = dir.join(ICON_DIR).join("starter");
        std::fs::create_dir_all(starter.join("svg")).unwrap();
        std::fs::create_dir_all(starter.join("fallback")).unwrap();
        std::fs::write(
            starter.join("svg/database.svg"),
            "<svg viewBox=\"0 0 64 64\"/>",
        )
        .unwrap();
        std::fs::write(starter.join("fallback/database.png"), b"\x89PNG").unwrap();
        std::fs::write(
            starter.join("manifest.json"),
            r#"{"library":"starter","icons":[{"id":"database","meaning":"数据集","tags":["data","pool"],"svg":"svg/database.svg","png":"fallback/database.png"},{"id":"gone","meaning":"x","svg":"svg/gone.svg"}]}"#,
        )
        .unwrap();
        let mine = dir.join(ICON_DIR).join("user-pptx");
        std::fs::create_dir_all(mine.join("svg")).unwrap();
        std::fs::write(mine.join("svg/image53.svg"), "<svg/>").unwrap();
        std::fs::write(mine.join("svg/image60.svg"), "<svg/>").unwrap();
        std::fs::write(
            mine.join("manifest.json"),
            r#"{"icons":[{"id":"image53.svg","path":"svg/image53.svg","metadata":{"name_zh":"机械臂","purpose_tags":["工业设备"]},"png_fallbacks":[{"path":"fallback/none.png"}]},{"id":"image60.svg","path":"svg/image60.svg"},{"id":"x.svg","path":"../../escape.svg","metadata":{"name_zh":"逃逸"}}]}"#,
        )
        .unwrap();
        let icons = load_icons(&dir);
        let ids: Vec<&str> = icons.iter().map(|i| i.id.as_str()).collect();
        // The starter library first; another library's ids carry its name;
        // icons without a file or a meaning, or outside the library, go.
        assert_eq!(ids, ["database", "user-pptx/image53"]);
        assert_eq!(icons[0].meaning, "数据集 (data, pool)");
        assert_eq!(icons[0].png.as_deref(), Some(&b"\x89PNG"[..]));
        assert_eq!(icons[1].meaning, "机械臂 (工业设备)");
        assert_eq!(icons[1].png, None);
        let (skill, _) = load(&dir, "folder");
        assert_eq!(skill.icons.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The skill on GitHub as the update downloads it: `BIWRITE_SKILL_ZIP`
    /// names a copy of the archive.
    #[test]
    #[ignore = "needs BIWRITE_SKILL_ZIP"]
    fn the_published_skill_updates_with_its_figure_rules_and_icons() {
        let archive = std::fs::read(std::env::var("BIWRITE_SKILL_ZIP").unwrap()).unwrap();
        assert!(archive.len() < MAX_ARCHIVE_BYTES);
        let root = temp("published");
        let target = root.join("research-builder");
        let manifest = Manifest {
            name: "research-builder".into(),
            source: "https://github.com/qzkinhit/research-builder".into(),
            version: "main".into(),
            date: String::new(),
        };
        extract(&archive, &target, &manifest).unwrap();
        let (skill, info) = load(&target, "downloaded");
        assert!(info.missing.is_empty(), "{:?}", info.missing);
        for file in [
            "paper-figure-pptx/SKILL.md",
            "paper-figure-pptx/references/pptx-native.md",
            "knowledge/figure-archetypes.md",
            "writing-deai.md",
        ] {
            assert!(skill.file(file).is_some(), "{file}");
        }
        assert!(skill.icons.len() >= 16, "{}", skill.icons.len());
        assert!(skill.icons.iter().all(|i| i.png.is_some()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
