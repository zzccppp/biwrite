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

use biwrite_core::assist::Skill;
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
    "LICENSE",
    "THIRD_PARTY_NOTICES.md",
];

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
        if !SKILL_FILES.contains(&rel_str.as_str()) || entry.size() > MAX_FILE_BYTES {
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
}
