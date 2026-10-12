//! The author's own materials, in the folders of the skill's `materials/`:
//! `domain/` (papers), `general/` (grant proposals and reports),
//! `writing-samples/` (the voice to write in), `figures/` (figures to
//! follow), `icons/` (icons for PowerPoint figures) and `ppt/` (talks). The
//! folder is the skill's own `materials/` when the skill is the author's
//! folder, and otherwise one in the app's data folder, which updating or
//! changing the skill keeps.
//!
//! BiWrite reads the files as they are. The openings of `writing-samples/`
//! are samples of the voice for polishing, edits and new paragraphs. The
//! SVG files of `icons/`, the SVG pictures of its PowerPoint files and the
//! libraries in it (a `manifest.json` as the skill's) are icons, named
//! after the file or after a picture's alternative text. Any paper or
//! figure of the folder can be a reference of the writing assistant.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use base64::Engine as _;
use biwrite_core::assist::{Skill, SkillIcon};
use serde::Serialize;
use tauri::State;

use crate::assist_commands::{AttachmentView, ReferenceView};
use crate::error::{CommandError, CommandResult};
use crate::skills::SkillInfo;
use crate::state::AppState;

/// The folders, as the skill names them.
pub const CATEGORIES: [&str; 6] = [
    "domain",
    "general",
    "writing-samples",
    "figures",
    "icons",
    "ppt",
];
/// The folders whose files are texts to imitate.
const TEXTS: [&str; 3] = ["domain", "general", "writing-samples"];
const SAMPLES: &str = "writing-samples";
const FIGURES: &str = "figures";
const ICONS: &str = "icons";
/// The materials' icons are named `materials/<name>`.
pub const LIBRARY: &str = "materials";
/// Texts read here, and figures (a PDF is drawn by the webview, and a
/// PowerPoint file turned into a PDF by LibreOffice first). A PDF among the
/// texts is read by the webview too.
const TEXT_KINDS: [&str; 5] = ["tex", "md", "markdown", "txt", "docx"];
const IMAGE_KINDS: [&str; 8] = ["png", "jpg", "jpeg", "webp", "gif", "svg", "pdf", "pptx"];
/// How deep a folder is read: a paper's folder and one more within it.
const MAX_DEPTH: usize = 3;
/// Most files of a folder listed.
const MAX_LISTED: usize = 400;
const MAX_TEXT_BYTES: u64 = 8_000_000;
/// The most text a paper of several files is put together from.
const MAX_INLINED: usize = 4_000_000;
const MAX_SVG_BYTES: u64 = 300_000;
const MAX_PPTX_BYTES: u64 = 200_000_000;
const MAX_IMAGE_BYTES: u64 = 8_000_000;
const MAX_PDF_BYTES: u64 = 40_000_000;
const MAX_ICONS: usize = 300;
/// Of each sample, the characters sent as the author's voice, and the most
/// samples sent.
const VOICE_CHARS: usize = 2_000;
const VOICE_SAMPLES: usize = 3;

/// The README of a new materials folder in the app's data folder.
const README: &str = "# 我的素材 · My materials

与 research-builder 的 `materials/` 分类相同，把文件放进对应的文件夹即可。
The same folders as the `materials/` of research-builder. Put each file into its folder.

| 文件夹 Folder | 放什么 What goes there | BiWrite 怎样用 How BiWrite uses it |
|---|---|---|
| `domain/` | 你的论文，LaTeX 源（每篇一个文件夹）、Word、PDF、`.md` 或 `.txt` · your papers | 写作助手「+ 素材」中的参照样例 · a reference of the writing assistant |
| `general/` | 基金本子、项目申请书、技术报告 · proposals and reports | 同上 · the same |
| `writing-samples/` | 最能代表你文风的中英文段落或全文 · your voice | 润色、修改与仿写按它的文风写，也可作参照样例 · the voice of polishing, edits and new paragraphs |
| `figures/` | 你满意的图，PNG、JPEG、SVG、PDF 或 PPTX（PPTX 需要 LibreOffice） · figures you like | 图表任务中临摹或仿风格的参照图 · a figure to trace or take the look of |
| `icons/` | SVG 图标，或含 SVG 图标的 PPTX · icons | PPT 图的图标，含义取 SVG 的文件名或 PPTX 中图片的替代文字 · icons of PowerPoint figures, named by file name or alternative text |
| `ppt/` | 学术报告与海报 · talks and posters | 不使用 · not used |
";

/// The materials folder of the app, and the icons last read.
pub struct Materials {
    own: PathBuf,
    icons: Mutex<Option<IconCache>>,
}

/// Icons read from a folder, with the files they came from and the
/// pictures left out for want of a name.
struct IconCache {
    files: Vec<(PathBuf, u64, Option<SystemTime>)>,
    icons: Arc<Vec<SkillIcon>>,
    unnamed: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderCount {
    pub name: String,
    pub files: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterialsView {
    pub folder: String,
    /// The folder is the `materials/` of the author's skill folder.
    pub in_skill: bool,
    /// What each folder holds that BiWrite uses (icons for `icons/`).
    pub counts: Vec<FolderCount>,
    /// SVG pictures of the PowerPoint files in `icons/` without a name
    /// (alternative text), which are not used.
    pub unnamed_icons: usize,
}

/// A file the assistant can take as a reference.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MaterialEntry {
    pub folder: String,
    /// From the materials folder, with `/`.
    pub path: String,
    /// `text`, `pdftext` (a paper as a PDF), `image` (PNG, JPEG, WebP, GIF
    /// or SVG), `pdf` or `pptx` (a figure).
    pub kind: String,
}

/// The lowercase extension of `path`.
fn kind(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// A name of letters, digits, `-` and `_` from `name`.
fn safe_name(name: &str) -> String {
    let out: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    out.trim_matches('-').chars().take(60).collect()
}

/// XML text with its entities replaced.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of attribute `name` in the tag text `tag`.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    let end = tag[at..].find('"')?;
    Some(unescape(&tag[at..at + end]))
}

/// The text of a Word file: its paragraphs, a blank line apart.
fn docx_text(bytes: &[u8]) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut file = zip.by_name("word/document.xml").ok()?;
    if file.size() > MAX_TEXT_BYTES * 8 {
        return None;
    }
    let mut xml = String::new();
    file.read_to_string(&mut xml).ok()?;
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;
    let mut rest = xml.as_str();
    while let Some(lt) = rest.find('<') {
        if in_text {
            para.push_str(&unescape(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else {
            break;
        };
        let tag = &rest[lt + 1..lt + gt];
        rest = &rest[lt + gt + 1..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match (name, closing) {
            ("w:t", false) => in_text = !tag.ends_with('/'),
            ("w:t", true) => in_text = false,
            ("w:tab", false) => para.push('\t'),
            ("w:br" | "w:cr", false) => para.push('\n'),
            ("w:p", true) => {
                let p = para.trim();
                if !p.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(p);
                }
                para.clear();
            }
            _ => {}
        }
    }
    Some(out)
}

/// A path of the package for `target`, relative to `base` (a folder of it).
fn resolve(base: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_owned();
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in target.split('/') {
        match part {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

/// An SVG picture of a PowerPoint file: its name there, its SVG and the
/// PNG shown in its place by programs without SVG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Extracted {
    pub(crate) name: String,
    pub(crate) svg: String,
    pub(crate) png: Vec<u8>,
}

/// The SVG pictures of the PowerPoint file `pptx`, each once, in slide
/// order: the pictures whose `a:blip` holds an `asvg:svgBlip`.
pub(crate) fn extract_icons(pptx: &[u8]) -> Result<Vec<Extracted>, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(pptx)).map_err(|e| e.to_string())?;
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let mut f = zip.by_name(name).ok()?;
        if f.size() > MAX_SVG_BYTES * 40 {
            return None;
        }
        let mut out = Vec::new();
        f.read_to_end(&mut out).ok()?;
        Some(out)
    };
    let mut out: Vec<Extracted> = Vec::new();
    for n in 1..=500 {
        let Some(slide) = read(&format!("ppt/slides/slide{n}.xml")) else {
            if n > 1 {
                break;
            }
            continue;
        };
        let slide = String::from_utf8_lossy(&slide).into_owned();
        let rels = read(&format!("ppt/slides/_rels/slide{n}.xml.rels"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let targets: std::collections::HashMap<String, String> = rels
            .split("<Relationship ")
            .skip(1)
            .filter_map(|r| {
                let r = format!(" {r}");
                Some((attribute(&r, "Id")?, attribute(&r, "Target")?))
            })
            .collect();
        for pic in slide.split("<p:pic>").skip(1) {
            let pic = pic.split("</p:pic>").next().unwrap_or("");
            let tag = |start: &str| -> &str {
                pic.find(start).map_or("", |at| {
                    &pic[at..pic[at..].find('>').map_or(pic.len(), |e| at + e)]
                })
            };
            let (svg_tag, blip_tag, name_tag) =
                (tag("<asvg:svgBlip"), tag("<a:blip "), tag("<p:cNvPr "));
            let (Some(svg_id), Some(png_id)) = (
                attribute(svg_tag, "r:embed"),
                attribute(blip_tag, "r:embed"),
            ) else {
                continue;
            };
            let (Some(svg_target), Some(png_target)) = (targets.get(&svg_id), targets.get(&png_id))
            else {
                continue;
            };
            let (Some(svg), Some(png)) = (
                read(&resolve("ppt/slides", svg_target)),
                read(&resolve("ppt/slides", png_target)),
            ) else {
                continue;
            };
            let Ok(svg) = String::from_utf8(svg) else {
                continue;
            };
            if svg.len() as u64 > MAX_SVG_BYTES || out.iter().any(|e| e.svg == svg) {
                continue;
            }
            let name = attribute(name_tag, "descr")
                .filter(|d| !d.trim().is_empty())
                .or_else(|| attribute(name_tag, "name"))
                .unwrap_or_default();
            out.push(Extracted { name, svg, png });
        }
    }
    Ok(out)
}

/// A slide picture's name that says nothing of what it shows ("图片 3",
/// "Picture 7").
fn generic(name: &str) -> bool {
    let name = name.trim();
    name.is_empty()
        || [
            "图形", "图片", "图标", "Graphic", "Picture", "Icon", "Image",
        ]
        .iter()
        .any(|g| {
            name.strip_prefix(g)
                .is_some_and(|rest| rest.trim().chars().all(|c| c.is_ascii_digit()))
        })
}

/// The files of `kinds` in `dir` and its folders (to `MAX_DEPTH`), sorted,
/// without hidden files and READMEs. With `skip_libraries`, folders below
/// `dir` that hold a `manifest.json` (icon libraries) are left out.
fn files(dir: &Path, kinds: &[&str], skip_libraries: bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 1)];
    while let Some((folder, depth)) = folders.pop() {
        if skip_libraries && depth > 1 && folder.join("manifest.json").is_file() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.starts_with('.') || name.starts_with("readme") {
                continue;
            }
            if path.is_dir() {
                if depth < MAX_DEPTH {
                    folders.push((path, depth + 1));
                }
            } else if kinds.contains(&kind(&path).as_str()) && out.len() < MAX_LISTED * 4 {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Where a comment starts in `line` (an unescaped `%`), or its length.
fn comment_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'%' => return i,
            _ => i += 1,
        }
    }
    line.len()
}

/// `text` with the files its `\input`, `\include` and `\subfile` name (from
/// the folder `dir` of the main file, inside `root`) in their place, to
/// `depth` levels and `budget` bytes.
fn inline_inputs(root: &Path, dir: &Path, text: &str, depth: usize, budget: &mut usize) -> String {
    const COMMANDS: [&str; 3] = ["\\input{", "\\include{", "\\subfile{"];
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let end = comment_start(line).min(line.len());
        let mut at = 0;
        while let Some((i, len)) = COMMANDS
            .iter()
            .filter_map(|c| line[at..end].find(c).map(|i| (at + i, c.len())))
            .min_by_key(|(i, _)| *i)
        {
            let Some(close) = line[i + len..end].find('}') else {
                break;
            };
            let arg = line[i + len..i + len + close].trim();
            out.push_str(&line[at..i]);
            let file = (depth > 0 && !arg.is_empty())
                .then(|| {
                    [dir.join(arg), dir.join(format!("{arg}.tex"))]
                        .into_iter()
                        .find(|p| p.is_file())
                })
                .flatten()
                .and_then(|p| inside(root, &p))
                .filter(|p| std::fs::metadata(p).is_ok_and(|m| (m.len() as usize) <= *budget))
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| biwrite_core::TextFile::decode(b).ok());
            match file {
                Some(sub) => {
                    *budget = budget.saturating_sub(sub.text().len());
                    out.push_str(&inline_inputs(root, dir, sub.text(), depth - 1, budget));
                    out.push('\n');
                }
                None => out.push_str(&line[i..i + len + close + 1]),
            }
            at = i + len + close + 1;
        }
        out.push_str(&line[at..]);
    }
    out
}

/// `path` when it is a file inside `root` (both as the file system has
/// them).
fn inside(root: &Path, path: &Path) -> Option<PathBuf> {
    let root = std::fs::canonicalize(root).ok()?;
    let path = std::fs::canonicalize(path).ok()?;
    (path.starts_with(&root) && path.is_file()).then_some(path)
}

/// The text of a text material: a paper's main `.tex` file with the files
/// it includes put in, the paragraphs of a Word file, else the file as it
/// is.
fn read_text(root: &Path, path: &Path) -> Option<String> {
    // A Word file holds its pictures too: only its text part is read.
    if kind(path) == "docx" {
        if std::fs::metadata(path).ok()?.len() > MAX_PPTX_BYTES {
            return None;
        }
        return docx_text(&std::fs::read(path).ok()?);
    }
    if std::fs::metadata(path).ok()?.len() > MAX_TEXT_BYTES {
        return None;
    }
    let file = biwrite_core::TextFile::decode(std::fs::read(path).ok()?).ok()?;
    let text = file.text();
    if kind(path) == "tex" && biwrite_latex::project::is_main(text) {
        let mut budget = MAX_INLINED;
        return Some(inline_inputs(root, path.parent()?, text, 4, &mut budget));
    }
    Some(text.to_owned())
}

/// Whether a text file of `folder` is a paper of its own: anything but a
/// `.tex` file inside a paper's folder that is not its main file.
fn is_paper(folder: &Path, path: &Path) -> bool {
    if kind(path) != "tex" || path.parent() == Some(folder) {
        return true;
    }
    std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_TEXT_BYTES)
        && std::fs::read(path)
            .ok()
            .and_then(|b| biwrite_core::TextFile::decode(b).ok())
            .is_some_and(|t| biwrite_latex::project::is_main(t.text()))
}

impl Materials {
    /// `own`: the materials folder in the app's data folder.
    pub fn new(own: PathBuf) -> Self {
        Self {
            own,
            icons: Mutex::new(None),
        }
    }

    /// The folder in use: the skill folder's `materials/` (`skill`), else
    /// the app's own.
    pub fn root(&self, skill: Option<&Path>) -> PathBuf {
        match skill {
            Some(dir) => dir.join("materials"),
            None => self.own.clone(),
        }
    }

    /// Make the app's own folder with its subfolders and README.
    pub fn ensure_own(&self) -> std::io::Result<()> {
        for name in CATEGORIES {
            std::fs::create_dir_all(self.own.join(name))?;
        }
        let readme = self.own.join("README.md");
        if !readme.exists() {
            std::fs::write(readme, README)?;
        }
        Ok(())
    }

    /// The papers and texts of folder `name` of `root`.
    fn texts(root: &Path, name: &str) -> Vec<PathBuf> {
        let folder = root.join(name);
        files(&folder, &TEXT_KINDS, false)
            .into_iter()
            .filter(|p| is_paper(&folder, p))
            .take(MAX_LISTED)
            .collect()
    }

    /// The PDFs of folder `name` of `root` (papers the webview reads).
    fn pdfs(root: &Path, name: &str) -> Vec<PathBuf> {
        files(&root.join(name), &["pdf"], false)
            .into_iter()
            .take(MAX_LISTED)
            .collect()
    }

    /// The figures of `root`.
    fn figures(root: &Path) -> Vec<PathBuf> {
        files(&root.join(FIGURES), &IMAGE_KINDS, false)
            .into_iter()
            .take(MAX_LISTED)
            .collect()
    }

    /// Samples of the author's voice: the opening of each of the first
    /// texts of `writing-samples/`, cut at a paragraph.
    pub fn voice(root: &Path) -> Vec<String> {
        Self::texts(root, SAMPLES)
            .into_iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                let text = read_text(root, &p)?;
                let (prose, _) = crate::assist_commands::reference_text(&name, &text);
                let mut sample = String::new();
                for para in prose.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
                    if !sample.is_empty()
                        && sample.chars().count() + para.chars().count() > VOICE_CHARS
                    {
                        break;
                    }
                    if !sample.is_empty() {
                        sample.push_str("\n\n");
                    }
                    sample.push_str(para);
                }
                let sample: String = sample.chars().take(VOICE_CHARS * 2).collect();
                (!sample.trim().is_empty()).then_some(sample)
            })
            .take(VOICE_SAMPLES)
            .collect()
    }

    /// The icons of `icons/` in `root`: its libraries, its SVG files (named
    /// after the file) and the SVG pictures of its PowerPoint files (named
    /// after their alternative text, and left out without one). Read again
    /// only when its files change.
    pub fn icons(&self, root: &Path) -> Arc<Vec<SkillIcon>> {
        self.icons_and_unnamed(root).0
    }

    /// The icons of `icons/` in `root`, and how many pictures it left out
    /// for want of a name.
    fn icons_and_unnamed(&self, root: &Path) -> (Arc<Vec<SkillIcon>>, usize) {
        let dir = root.join(ICONS);
        let stamp: Vec<(PathBuf, u64, Option<SystemTime>)> =
            files(&dir, &["svg", "pptx", "json", "png"], false)
                .into_iter()
                .map(|p| {
                    let meta = std::fs::metadata(&p).ok();
                    let len = meta.as_ref().map_or(0, std::fs::Metadata::len);
                    (p, len, meta.and_then(|m| m.modified().ok()))
                })
                .collect();
        let mut cache = self.icons.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(c) = cache.as_ref().filter(|c| c.files == stamp) {
            return (c.icons.clone(), c.unnamed);
        }
        let (icons, unnamed) = read_icons(&dir);
        let icons = Arc::new(icons);
        *cache = Some(IconCache {
            files: stamp,
            icons: icons.clone(),
            unnamed,
        });
        (icons, unnamed)
    }

    /// What `root` holds that BiWrite uses.
    pub fn view(&self, root: &Path, in_skill: bool) -> MaterialsView {
        let (icons, unnamed_icons) = self.icons_and_unnamed(root);
        let counts = CATEGORIES
            .iter()
            .filter(|n| **n != "ppt")
            .map(|name| FolderCount {
                name: (*name).to_owned(),
                files: match *name {
                    FIGURES => Self::figures(root).len(),
                    ICONS => icons.len(),
                    _ => Self::texts(root, name).len() + Self::pdfs(root, name).len(),
                },
            })
            .collect();
        MaterialsView {
            folder: root.display().to_string(),
            in_skill,
            counts,
            unnamed_icons,
        }
    }

    /// The files of `root` the assistant can take as references: texts
    /// first, then figures.
    pub fn list(root: &Path) -> Vec<MaterialEntry> {
        let entry = |folder: &str, p: &Path, kind_name: &str| {
            let rel = p.strip_prefix(root).ok()?;
            Some(MaterialEntry {
                folder: folder.to_owned(),
                path: rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/"),
                kind: kind_name.to_owned(),
            })
        };
        let mut out = Vec::new();
        for name in TEXTS {
            let mut texts: Vec<(PathBuf, &str)> = Self::texts(root, name)
                .into_iter()
                .map(|p| (p, "text"))
                .chain(Self::pdfs(root, name).into_iter().map(|p| (p, "pdftext")))
                .collect();
            texts.sort();
            out.extend(texts.iter().filter_map(|(p, k)| entry(name, p, k)));
        }
        out.extend(Self::figures(root).iter().filter_map(|p| {
            let k = match kind(p).as_str() {
                "pdf" => "pdf",
                "pptx" => "pptx",
                _ => "image",
            };
            entry(FIGURES, p, k)
        }));
        out
    }

    /// The file `rel` (as `list` gives it) of `root`, if it is one there.
    pub fn file(root: &Path, rel: &str) -> Option<PathBuf> {
        let rel = Path::new(rel);
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return None;
        }
        inside(root, &root.join(rel))
    }

    /// The text `rel` of `root` as a reference to imitate.
    pub fn reference(root: &Path, rel: &str) -> Result<ReferenceView, String> {
        let path = Self::file(root, rel).ok_or("no such file in the materials")?;
        if !TEXT_KINDS.contains(&kind(&path).as_str()) {
            return Err("not a text".into());
        }
        let text = read_text(root, &path).ok_or("the file cannot be read as text")?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (text, truncated) = crate::assist_commands::reference_text(&name, &text);
        if text.trim().is_empty() {
            return Err(format!("{name} has no text to imitate"));
        }
        Ok(ReferenceView {
            chars: text.chars().count(),
            name,
            text,
            truncated,
        })
    }

    /// The figure `rel` of `root` as an image: its name, media type and
    /// bytes (an SVG drawn as a PNG). A PDF is drawn by the webview.
    pub fn image(root: &Path, rel: &str) -> Result<(String, &'static str, Vec<u8>), String> {
        let path = Self::file(root, rel).ok_or("no such file in the materials")?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if std::fs::metadata(&path).map_err(|e| e.to_string())?.len() > MAX_IMAGE_BYTES {
            return Err(format!("{name} is larger than 8 MB"));
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let media = match kind(&path).as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            "gif" => "image/gif",
            "svg" => {
                let svg = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                let png = crate::figure_scene::plain_svg(&svg)
                    .and_then(crate::figure_scene::svg_png)
                    .ok_or_else(|| format!("{name} is no plain SVG drawing"))?;
                return Ok((name, "image/png", png));
            }
            _ => return Err(format!("{name} is no image")),
        };
        Ok((name, media, bytes))
    }
}

/// The icons of the folder `dir` (see `Materials::icons`), and how many
/// slide pictures were left out for want of a name.
fn read_icons(dir: &Path) -> (Vec<SkillIcon>, usize) {
    let mut out: Vec<SkillIcon> = Vec::new();
    let mut unnamed = 0;
    // Libraries as the skill keeps them.
    for manifest in files(dir, &["json"], false)
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "manifest.json"))
    {
        if let Some(lib) = manifest.parent() {
            crate::skills::load_library(lib, LIBRARY, &mut out);
        }
    }
    let mut seen: HashSet<String> = out.iter().map(|i| i.svg.clone()).collect();
    let mut add =
        |out: &mut Vec<SkillIcon>, name: &str, meaning: &str, svg: &str, png: Option<Vec<u8>>| {
            if out.len() >= MAX_ICONS || meaning.trim().is_empty() {
                return;
            }
            let Some(svg) = crate::figure_scene::plain_svg(svg) else {
                return;
            };
            if !seen.insert(svg.to_owned()) {
                return;
            }
            let Some(png) = png.or_else(|| crate::figure_scene::svg_png(svg)) else {
                return;
            };
            let name = safe_name(name);
            let base = format!("{LIBRARY}/{}", if name.is_empty() { "icon" } else { &name });
            let mut id = base.clone();
            let mut n = 2;
            while out.iter().any(|i| i.id == id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            out.push(SkillIcon {
                id,
                meaning: meaning.trim().chars().take(120).collect(),
                library: LIBRARY.to_owned(),
                svg: svg.to_owned(),
                png: Some(png),
            });
        };
    for path in files(dir, &["svg", "pptx"], true) {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if kind(&path) == "svg" {
            let svg = std::fs::metadata(&path)
                .ok()
                .filter(|m| m.len() <= MAX_SVG_BYTES)
                .and_then(|_| std::fs::read_to_string(&path).ok());
            if let Some(svg) = svg {
                add(&mut out, &stem, &stem.replace(['-', '_'], " "), &svg, None);
            }
            continue;
        }
        let Some(bytes) = std::fs::metadata(&path)
            .ok()
            .filter(|m| m.len() <= MAX_PPTX_BYTES)
            .and_then(|_| std::fs::read(&path).ok())
        else {
            continue;
        };
        let Ok(found) = extract_icons(&bytes) else {
            log::warn!("could not read the icons of {}", path.display());
            continue;
        };
        for (k, icon) in found.into_iter().enumerate() {
            if generic(&icon.name) {
                unnamed += 1;
                continue;
            }
            let png = (!icon.png.is_empty()).then_some(icon.png);
            add(
                &mut out,
                &format!("{stem}-{}", k + 1),
                &icon.name,
                &icon.svg,
                png,
            );
        }
    }
    (out, unnamed)
}

/// The skill folder the author chose, if it is one.
pub(crate) fn skill_dir(state: &AppState) -> Option<PathBuf> {
    state
        .settings()
        .skill_folder
        .clone()
        .map(PathBuf::from)
        .filter(|d| crate::skills::is_skill_dir(d))
}

/// The materials folder in use.
pub(crate) fn root(state: &AppState) -> PathBuf {
    state.materials.root(skill_dir(state).as_deref())
}

/// The skill in use, with the icons of the materials.
pub(crate) fn skill(state: &AppState) -> CommandResult<(Arc<Skill>, SkillInfo)> {
    let (skill, info) = state.skill()?;
    let icons = state.materials.icons(&root(state));
    if icons.is_empty() {
        return Ok((skill, info));
    }
    let mut with = (*skill).clone();
    with.icons.extend(icons.iter().cloned());
    Ok((Arc::new(with), info))
}

fn fail(e: String) -> CommandError {
    CommandError::Settings(e)
}

/// The materials folder and what it holds.
#[tauri::command]
pub async fn materials_get(state: State<'_, AppState>) -> CommandResult<MaterialsView> {
    let in_skill = skill_dir(&state).is_some();
    if !in_skill && let Err(e) = state.materials.ensure_own() {
        log::warn!("could not make the materials folder: {e}");
    }
    let root = root(&state);
    let materials = &state.materials;
    Ok(tokio::task::block_in_place(|| {
        materials.view(&root, in_skill)
    }))
}

/// Show the materials folder (made first when it is the app's own).
#[tauri::command]
pub async fn materials_reveal(state: State<'_, AppState>) -> CommandResult<()> {
    if skill_dir(&state).is_none() {
        state
            .materials
            .ensure_own()
            .map_err(|e| fail(e.to_string()))?;
    }
    crate::files::reveal(&root(&state))
}

/// The papers and figures the assistant can take as references.
#[tauri::command]
pub async fn materials_list(state: State<'_, AppState>) -> CommandResult<Vec<MaterialEntry>> {
    let root = root(&state);
    Ok(tokio::task::block_in_place(|| Materials::list(&root)))
}

/// A paper or text of the materials as a reference to imitate.
#[tauri::command]
pub async fn materials_text(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<ReferenceView> {
    let root = root(&state);
    tokio::task::block_in_place(|| Materials::reference(&root, &path)).map_err(fail)
}

/// A figure of the materials as a reference image.
#[tauri::command]
pub async fn materials_image(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<AttachmentView> {
    let root = root(&state);
    let (name, media, bytes) =
        tokio::task::block_in_place(|| Materials::image(&root, &path)).map_err(fail)?;
    Ok(crate::assist_commands::attach(&state, name, media, &bytes))
}

/// A PDF figure of the materials, for the webview to draw.
#[tauri::command]
pub async fn materials_pdf(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<tauri::ipc::Response> {
    let root = root(&state);
    let file = Materials::file(&root, &path)
        .filter(|p| kind(p) == "pdf")
        .ok_or_else(|| fail("no such PDF in the materials".into()))?;
    let bytes = crate::files::read_small_file(file, MAX_PDF_BYTES).await?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// A PowerPoint figure of the materials as a PDF (by LibreOffice), for the
/// webview to draw.
#[tauri::command]
pub async fn materials_pptx(
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<tauri::ipc::Response> {
    let root = root(&state);
    let file = Materials::file(&root, &path)
        .filter(|p| kind(p) == "pptx")
        .ok_or_else(|| fail("no such PowerPoint file in the materials".into()))?;
    let soffice = crate::figures::soffice()
        .ok_or_else(|| fail("reading a PowerPoint figure needs LibreOffice".into()))?;
    let bytes = crate::files::read_small_file(file, MAX_PPTX_BYTES).await?;
    let work = std::env::temp_dir().join(format!(
        "biwrite-material-{}-{}",
        std::process::id(),
        state
            .assist
            .next_job
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let pdf = crate::figures::convert(&soffice, &work, "figure", &bytes).await;
    let _ = std::fs::remove_dir_all(&work);
    Ok(tauri::ipc::Response::new(pdf.map_err(fail)?))
}

/// A reference image the webview drew (the first page of a PDF), as a PNG
/// in base64.
#[tauri::command]
pub async fn attach_image_data(
    state: State<'_, AppState>,
    name: String,
    png: String,
) -> CommandResult<AttachmentView> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(png.trim())
        .map_err(|e| fail(e.to_string()))?;
    if !bytes.starts_with(b"\x89PNG") || bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(fail("the picture must be a PNG of at most 8 MB".into()));
    }
    Ok(crate::assist_commands::attach(
        &state,
        name,
        "image/png",
        &bytes,
    ))
}

#[cfg(test)]
#[path = "materials_tests.rs"]
mod tests;
