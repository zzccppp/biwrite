//! Figures that are files: a PDF drawn with TikZ or pgfplots, or a
//! PowerPoint diagram and its PDF. Where the document keeps its figures
//! and how it names them, making the files, and keeping them until the
//! document is saved: nothing is written into the project before that.
//! Builds of unsaved text find the waiting PDFs in the project's
//! `.biwrite` folder; a save writes the ones its text includes next to the
//! document's other figures, never over a file that is there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use biwrite_core::assist::{FigureAnswer, FigureFormat};
use serde::Serialize;

use crate::figure_scene::{Scene, file_stem};

/// Longest LibreOffice conversion.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(120);
/// Where builds of unsaved text find the waiting figures, in the project.
pub(crate) const WAITING_DIR: &str = ".biwrite/figures";

/// How the document includes its figures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Place {
    /// What comes before the file name in `\includegraphics`: `figures/`,
    /// `\figdir `, or nothing (`\graphicspath` finds it).
    pub(crate) prefix: String,
    /// The folder that is, relative to the folder TeX runs in.
    pub(crate) dir: String,
    /// What the file names begin with (`fig_`), when they share it.
    pub(crate) stem_prefix: String,
    /// The width of a figure one column wide (`\columnwidth`).
    pub(crate) width: String,
}

/// The uncommented part of each line of `text`.
fn code(text: &str) -> String {
    text.lines()
        .map(|l| {
            let b = l.as_bytes();
            let mut cut = l.len();
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'\\' => i += 2,
                    b'%' => {
                        cut = i;
                        break;
                    }
                    _ => i += 1,
                }
            }
            &l[..cut.min(l.len())]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The value `\newcommand`, `\renewcommand` or `\def` gives `\name` in
/// `text`.
fn macro_value(text: &str, name: &str) -> Option<String> {
    for form in [
        format!("\\newcommand{{\\{name}}}{{"),
        format!("\\newcommand*{{\\{name}}}{{"),
        format!("\\renewcommand{{\\{name}}}{{"),
        format!("\\newcommand\\{name}{{"),
        format!("\\def\\{name}{{"),
    ] {
        if let Some(at) = text.find(&form) {
            let rest = &text[at + form.len()..];
            return rest.find('}').map(|end| rest[..end].trim().to_owned());
        }
    }
    None
}

/// `\includegraphics` arguments of `text`: the options and the file.
fn graphics(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("\\includegraphics") {
        let mut rest = text[at + "\\includegraphics".len()..].trim_start();
        let mut options = String::new();
        if let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { continue };
            options = r[..end].to_owned();
            rest = r[end + 1..].trim_start();
        }
        let Some(r) = rest.strip_prefix('{') else {
            continue;
        };
        let Some(end) = r.find('}') else { continue };
        out.push((options, r[..end].trim().to_owned()));
    }
    out
}

/// The prefix of `file` (the argument of `\includegraphics`) and the folder
/// it stands for, when it can be told.
fn split(preamble: &str, file: &str) -> Option<(String, String)> {
    if let Some(rest) = file.strip_prefix('\\') {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphabetic() || *c == '@')
            .collect();
        let after = &rest[name.len()..];
        let tail = after.trim_start_matches([' ', '{', '}']);
        let value = macro_value(preamble, &name)?;
        let prefix = file[..file.len() - tail.len()].to_owned();
        let sub = tail.rfind('/').map_or("", |i| &tail[..=i]);
        return Some((format!("{prefix}{sub}"), format!("{value}{sub}")));
    }
    let dir = file.rfind('/').map_or("", |i| &file[..=i]);
    Some((dir.to_owned(), dir.to_owned()))
}

/// The most common of `items`, the first met on a tie.
fn most_common<T: Clone + PartialEq>(items: &[T]) -> Option<T> {
    let mut best: Option<(&T, usize)> = None;
    for item in items {
        let n = items.iter().filter(|i| *i == item).count();
        if best.is_none_or(|(_, m)| n > m) {
            best = Some((item, n));
        }
    }
    best.map(|(t, _)| t.clone())
}

/// How the document with main file `root_text` (its folder `root_dir`)
/// and open file `doc_text` includes figures, so a new one follows suit.
pub(crate) fn place(root_text: &str, doc_text: &str, root_dir: Option<&Path>) -> Place {
    let root = code(root_text);
    let doc = code(doc_text);
    let preamble = root.split("\\begin{document}").next().unwrap_or("");
    let mut found = graphics(&root);
    found.extend(graphics(&doc));
    let splits: Vec<(String, String, String, String)> = found
        .iter()
        .filter_map(|(options, file)| {
            let (prefix, dir) = split(preamble, file)?;
            let name = file[prefix.len().min(file.len())..].to_owned();
            Some((prefix, dir, name, options.clone()))
        })
        .collect();
    let width = most_common(
        &splits
            .iter()
            .filter_map(|(.., o)| {
                o.split(',')
                    .find_map(|kv| kv.trim().strip_prefix("width="))
                    .filter(|w| *w == "\\columnwidth" || *w == "\\linewidth")
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|| "\\linewidth".to_owned());
    if let Some((prefix, dir)) = most_common(
        &splits
            .iter()
            .map(|(p, d, ..)| (p.clone(), d.clone()))
            .collect::<Vec<_>>(),
    ) {
        let names: Vec<&str> = splits
            .iter()
            .filter(|(p, ..)| *p == prefix)
            .map(|(_, _, n, _)| n.as_str())
            .collect();
        let stem_prefix = ["fig_", "fig-", "figure_", "figure-", "Fig_", "Fig-"]
            .into_iter()
            .find(|f| {
                names.len() >= 2
                    && names.iter().filter(|n| n.starts_with(f)).count() * 2 > names.len()
            })
            .unwrap_or("")
            .to_owned();
        return Place {
            prefix,
            dir,
            stem_prefix,
            width,
        };
    }
    // No figure yet: the graphics path, else a figures folder.
    if let Some(at) = preamble.find("\\graphicspath{") {
        let rest = &preamble[at + "\\graphicspath{".len()..];
        if let Some(first) = rest.strip_prefix('{').and_then(|r| r.split('}').next()) {
            let dir = if first.ends_with('/') {
                first.to_owned()
            } else {
                format!("{first}/")
            };
            return Place {
                prefix: String::new(),
                dir,
                stem_prefix: String::new(),
                width,
            };
        }
    }
    let dir = root_dir
        .and_then(|d| {
            ["figures", "figs", "fig", "images", "img"]
                .into_iter()
                .find(|f| d.join(f).is_dir())
        })
        .unwrap_or("figures");
    Place {
        prefix: format!("{dir}/"),
        dir: format!("{dir}/"),
        stem_prefix: String::new(),
        width,
    }
}

/// A figure waiting for the save of the document that includes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiting {
    /// What the document names it by in `\includegraphics`.
    pub(crate) reference: String,
    /// The files to write, relative to the folder TeX runs in.
    pub(crate) files: Vec<(String, Vec<u8>)>,
}

impl Waiting {
    /// The PDF among the files, for builds of unsaved text.
    fn pdf(&self) -> Option<(&str, &[u8])> {
        self.files
            .iter()
            .find(|(p, _)| p.ends_with(".pdf"))
            .map(|(p, b)| (p.as_str(), b.as_slice()))
    }
}

/// What a figure job made, for its preview and Save As.
#[derive(Clone, Debug, Default)]
pub(crate) struct Made {
    pub(crate) stem: String,
    pub(crate) pdf: Option<Vec<u8>>,
    pub(crate) pptx: Option<Vec<u8>>,
}

/// Figures waiting for a save, and what figure jobs made.
#[derive(Default)]
pub struct FigureStore {
    waiting: Mutex<Vec<Waiting>>,
    made: Mutex<HashMap<u64, Made>>,
}

impl FigureStore {
    fn waiting(&self) -> MutexGuard<'_, Vec<Waiting>> {
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn made(&self) -> MutexGuard<'_, HashMap<u64, Made>> {
        self.made.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Another document is open: the figures of the last one go.
    pub(crate) fn clear(&self) {
        self.waiting().clear();
        self.made().clear();
    }

    /// Keep `figure` until a save includes it (in place of one with the
    /// same reference).
    pub(crate) fn wait(&self, figure: Waiting) {
        let mut w = self.waiting();
        w.retain(|f| f.reference != figure.reference);
        w.push(figure);
    }

    /// A file name is taken: waiting, or on disk in `dir`.
    pub(crate) fn taken(&self, dir: Option<&Path>, relative: &str) -> bool {
        self.waiting()
            .iter()
            .any(|f| f.files.iter().any(|(p, _)| p == relative))
            || dir.is_some_and(|d| biwrite_latex::clean(&d.join(relative)).exists())
    }

    /// `text` as a build of unsaved text reads it: each waiting figure it
    /// includes is copied to the project's `.biwrite` folder (`project`)
    /// and named there instead. Lines stay as they are.
    pub(crate) fn staged(&self, project: &Path, text: &str) -> String {
        let mut out = text.to_owned();
        for figure in self.waiting().iter() {
            let Some((path, bytes)) = figure.pdf() else {
                continue;
            };
            if !out.contains(&figure.reference) {
                continue;
            }
            let name = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let copy = project.join(WAITING_DIR).join(&name);
            let written = std::fs::create_dir_all(project.join(WAITING_DIR))
                .and_then(|()| std::fs::write(&copy, bytes));
            match written {
                Ok(()) => out = out.replace(&figure.reference, &format!("{WAITING_DIR}/{name}")),
                Err(e) => log::warn!("could not stage {name}: {e}"),
            }
        }
        out
    }

    /// Write the waiting figures `text` includes, relative to `root_dir`,
    /// never over a file that is there. The files written, and those left
    /// because a file of that name appeared meanwhile.
    pub(crate) fn save(&self, root_dir: &Path, text: &str) -> (Vec<String>, Vec<String>) {
        let mut written = Vec::new();
        let mut kept = Vec::new();
        let mut waiting = self.waiting();
        let included = code(text);
        waiting.retain(|figure| {
            if !included.contains(&figure.reference) {
                return true;
            }
            for (relative, bytes) in &figure.files {
                let target = biwrite_latex::clean(&root_dir.join(relative));
                let made = target
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| {
                        use std::io::Write as _;
                        let mut f = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)?;
                        f.write_all(bytes)
                    });
                match made {
                    Ok(()) => written.push(relative.clone()),
                    Err(e) => {
                        log::warn!("{} not written: {e}", target.display());
                        kept.push(relative.clone());
                    }
                }
            }
            false
        });
        (written, kept)
    }
}

/// The LibreOffice program, when it is installed.
pub(crate) fn soffice() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/LibreOffice.app/Contents/MacOS/soffice".into());
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home).join("Applications/LibreOffice.app/Contents/MacOS/soffice"),
            );
        }
    }
    if cfg!(target_os = "windows") {
        for base in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = std::env::var_os(base) {
                candidates.push(PathBuf::from(dir).join("LibreOffice/program/soffice.exe"));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["soffice", "libreoffice", "soffice.exe"] {
                candidates.push(dir.join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// `dir` as a `file://` URL.
fn file_url(dir: &Path) -> String {
    let path = dir.display().to_string().replace('\\', "/");
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        out.push('/');
    }
    for c in path.chars() {
        match c {
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            '#' => out.push_str("%23"),
            _ => out.push(c),
        }
    }
    out
}

/// The PDF LibreOffice makes of the PowerPoint file `pptx`, in `work` (a
/// folder of its own). A profile of BiWrite's own keeps it apart from a
/// LibreOffice the author has open.
pub(crate) async fn convert(
    soffice: &Path,
    work: &Path,
    stem: &str,
    pptx: &[u8],
) -> Result<Vec<u8>, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    let input = work.join(format!("{stem}.pptx"));
    std::fs::write(&input, pptx).map_err(|e| e.to_string())?;
    let profile = std::env::temp_dir().join("biwrite-libreoffice");
    let mut cmd = tokio::process::Command::new(soffice);
    cmd.arg(format!("-env:UserInstallation={}", file_url(&profile)))
        .args([
            "--headless",
            "--norestore",
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(work)
        .arg(&input)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(CONVERT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "LibreOffice did not finish in time".to_owned())?
        .map_err(|e| format!("LibreOffice did not start: {e}"))?;
    std::fs::read(work.join(format!("{stem}.pdf"))).map_err(|_| {
        format!(
            "LibreOffice made no PDF: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })
}

/// A figure as the card shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FigureView {
    pub format: FigureFormat,
    /// Where it goes, relative to the folder TeX runs in.
    pub file: String,
    /// There is a PDF (to show and to save).
    pub pdf: bool,
    /// There is a PowerPoint file.
    pub pptx: bool,
    /// A drawing of the scene, when there is no PDF to show.
    pub svg: Option<String>,
    /// No LibreOffice: the PDF is to be exported from PowerPoint.
    pub export: bool,
}

/// The figure environment that includes the file named `reference`.
pub(crate) fn environment(
    place: &Place,
    reference: &str,
    caption: &str,
    label: &str,
    wide: bool,
    export_from: Option<&str>,
) -> String {
    let (env, width) = if wide {
        ("figure*", "\\textwidth")
    } else {
        ("figure", place.width.as_str())
    };
    let graphic = format!("\\includegraphics[width={width}]{{{reference}}}");
    let body = match export_from {
        // Until the PDF is exported from the PowerPoint file, a box names it.
        Some(pptx) => format!(
            "\\IfFileExists{{{reference}}}{{{graphic}}}{{\\fbox{{\\parbox{{0.9{width}}}{{\\centering\\texttt{{{}}}}}}}}}",
            pptx.replace('_', "\\_")
        ),
        None => graphic,
    };
    format!(
        "\\begin{{{env}}}[t]\n\\centering\n{body}\n\\caption{{{caption}}}\n\\label{{fig:{label}}}\n\\end{{{env}}}"
    )
}

/// The file stem for an answer: its name, else from the caption, with the
/// document's prefix, free in `dir` (`-2`, `-3`… when taken).
pub(crate) fn free_stem(
    store: &FigureStore,
    place: &Place,
    root_dir: Option<&Path>,
    answer: &FigureAnswer,
    extension: &str,
) -> String {
    let mut base = file_stem(&answer.name);
    if base.is_empty() {
        base = file_stem(
            &answer
                .caption
                .split_whitespace()
                .take(4)
                .collect::<Vec<_>>()
                .join("-"),
        );
    }
    if base.is_empty() {
        base = "figure".into();
    }
    let base = format!(
        "{}{}",
        place.stem_prefix,
        base.trim_start_matches(&place.stem_prefix)
    );
    let mut n = 1;
    loop {
        let stem = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let relative = format!("{}{stem}.{extension}", place.dir);
        if !store.taken(root_dir, &relative) {
            return stem;
        }
        n += 1;
    }
}

/// A figure wider than one column of a two-column paper, in centimetres.
const COLUMN: f64 = 10.0;

/// The width of the first page of `pdf`, in centimetres.
pub(crate) fn pdf_width(pdf: &[u8]) -> Option<f64> {
    let text = String::from_utf8_lossy(&pdf[..pdf.len().min(200_000)]);
    let at = text.find("/MediaBox")?;
    let rest = text[at + "/MediaBox".len()..]
        .trim_start()
        .strip_prefix('[')?;
    let numbers: Vec<f64> = rest
        .split(']')
        .next()?
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    (numbers.len() == 4).then(|| (numbers[2] - numbers[0]) / 72.0 * 2.54)
}

/// A figure of `width` centimetres spans both columns.
pub(crate) fn wide(width: f64) -> bool {
    width > COLUMN
}

/// The scene of `answer` laid out, with the skill's `icons`; or why it
/// cannot be.
pub(crate) fn laid(
    answer: &FigureAnswer,
    icons: &[biwrite_core::assist::SkillIcon],
) -> Result<crate::figure_scene::Laid, String> {
    Scene::parse(&answer.body)?.laid_with(icons)
}

#[cfg(test)]
#[path = "figures_tests.rs"]
mod tests;
