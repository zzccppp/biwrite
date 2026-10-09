//! A LaTeX project: which file is compiled (the root), with which engine,
//! and which `.tex` files belong to it.
//!
//! The root is the file itself when it has `\documentclass` and
//! `\begin{document}`; otherwise a `% !TEX root = main.tex` comment names
//! it, or a main file in the same folder that `\input`s or `\include`s it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The TeX engine a project compiles with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Pdflatex,
    Xelatex,
    Lualatex,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pdflatex => "pdflatex",
            Self::Xelatex => "xelatex",
            Self::Lualatex => "lualatex",
        }
    }

    /// latexmk's option for this engine.
    pub fn latexmk_flag(self) -> &'static str {
        match self {
            Self::Pdflatex => "-pdf",
            Self::Xelatex => "-xelatex",
            Self::Lualatex => "-lualatex",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "pdflatex" | "pdftex" | "latex" => Some(Self::Pdflatex),
            "xelatex" | "xetex" => Some(Self::Xelatex),
            "lualatex" | "luatex" => Some(Self::Lualatex),
            _ => None,
        }
    }
}

/// `% !TEX …` comments in the first lines of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Magic {
    pub root: Option<String>,
    pub program: Option<Engine>,
}

/// Read `% !TEX root = …` and `% !TEX program = …` (also `TS-program`)
/// from the first 30 lines.
pub fn magic(text: &str) -> Magic {
    let mut out = Magic::default();
    for line in text.lines().take(30) {
        let Some(rest) = line.trim_start().strip_prefix('%') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(rest) = rest
            .strip_prefix("!TEX")
            .or_else(|| rest.strip_prefix("!tex"))
        else {
            continue;
        };
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "root" && !value.is_empty() {
            out.root = Some(value.to_owned());
        } else if key == "program" || key == "ts-program" {
            out.program = Engine::parse(value);
        }
    }
    out
}

/// `text` without `%` comments (escaped `\%` kept), for scanning commands.
pub(crate) fn uncommented(text: &str) -> String {
    text.lines()
        .map(|line| {
            let bytes = line.as_bytes();
            let mut cut = line.len();
            let mut i = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i += 2,
                    b'%' => {
                        cut = i;
                        break;
                    }
                    _ => i += 1,
                }
            }
            &line[..cut.min(line.len())]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The file can be compiled on its own.
pub fn is_main(text: &str) -> bool {
    let t = uncommented(text);
    t.contains("\\documentclass") && t.contains("\\begin{document}")
}

/// The preamble (up to `\begin{document}`).
pub fn preamble(text: &str) -> &str {
    match text.find("\\begin{document}") {
        Some(i) => &text[..i],
        None => text,
    }
}

/// The engine: the magic comment, else the packages the preamble loads.
pub fn engine_for(text: &str) -> Engine {
    if let Some(e) = magic(text).program {
        return e;
    }
    let pre = uncommented(preamble(text));
    let uses = |pkg: &str| {
        pre.contains(&format!("{{{pkg}}}"))
            || pre.contains(&format!(",{pkg}}}"))
            || pre.contains(&format!("{{{pkg},"))
    };
    let ctex_class = ["{ctexart}", "{ctexrep}", "{ctexbook}", "{ctexbeamer}"]
        .iter()
        .any(|c| pre.contains(c));
    if uses("luatexja") || uses("luacode") {
        Engine::Lualatex
    } else if ctex_class
        || [
            "fontspec",
            "xeCJK",
            "ctex",
            "unicode-math",
            "polyglossia",
            "xltxtra",
        ]
        .iter()
        .any(|p| uses(p))
    {
        Engine::Xelatex
    } else {
        Engine::Pdflatex
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectError {
    #[error(
        "{file} cannot be compiled on its own and no main file was found: add `% !TEX root = main.tex` at its top"
    )]
    NoRoot { file: String },
    #[error("the root file {0} named by `% !TEX root` does not exist")]
    MissingRoot(String),
}

/// Directories never searched for project files.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "out",
    "dist",
    "history",
    "__pycache__",
];

/// `.tex` files under `dir` (depth ≤ 4, hidden and build folders skipped,
/// at most 300), sorted.
pub fn tex_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(dir, 0, &mut out);
    out.sort();
    out
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 || out.len() >= 300 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect(&path, depth + 1, out);
            }
        } else if name.ends_with(".tex") && out.len() < 300 {
            out.push(path);
        }
    }
}

/// The main file of a folder: a compilable `.tex` directly in it, by
/// preference `main.tex`, `paper.tex`, `<folder>.tex`, then the largest.
pub fn find_main(dir: &Path) -> Option<PathBuf> {
    let folder = dir
        .file_name()
        .map(|n| format!("{}.tex", n.to_string_lossy()))
        .unwrap_or_default();
    let mut mains: Vec<(PathBuf, u64)> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "tex") && p.is_file())
        .filter_map(|p| {
            let text = fs::read_to_string(&p).ok()?;
            is_main(&text).then(|| {
                let size = text.len() as u64;
                (p, size)
            })
        })
        .collect();
    let rank = |p: &Path| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match name.as_str() {
            "main.tex" => 0,
            "paper.tex" => 1,
            n if n == folder => 2,
            _ => 3,
        }
    };
    mains.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(b.1.cmp(&a.1)));
    mains.into_iter().next().map(|(p, _)| p)
}

/// `/a/./b/../c.tex` → `/a/c.tex`, without touching the file system.
pub fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The file to compile for `file` (whose current text is `text`).
pub fn root_for(file: &Path, text: &str) -> Result<PathBuf, ProjectError> {
    let dir = file.parent().unwrap_or(Path::new("."));
    if let Some(root) = magic(text).root {
        let path = clean(&dir.join(&root));
        return if path.is_file() {
            Ok(path)
        } else {
            Err(ProjectError::MissingRoot(root))
        };
    }
    if is_main(text) {
        return Ok(file.to_path_buf());
    }
    // A main file nearby that pulls this one in.
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut search = vec![dir.to_path_buf()];
    if let Some(parent) = dir.parent() {
        search.push(parent.to_path_buf());
    }
    for d in search {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.filter_map(Result::ok) {
            let p = e.path();
            if p == file || p.extension().is_none_or(|x| x != "tex") {
                continue;
            }
            let Ok(main) = fs::read_to_string(&p) else {
                continue;
            };
            if is_main(&main) && includes(&uncommented(&main), &stem) {
                return Ok(p);
            }
        }
    }
    Err(ProjectError::NoRoot {
        file: file.display().to_string(),
    })
}

/// `\input{…stem}` or `\include{…stem}` (with or without `.tex`, any folder).
fn includes(text: &str, stem: &str) -> bool {
    ["\\input{", "\\include{", "\\subfile{"].iter().any(|cmd| {
        text.match_indices(cmd).any(|(i, _)| {
            let arg = &text[i + cmd.len()..];
            let Some(end) = arg.find('}') else {
                return false;
            };
            let name = arg[..end].trim().trim_end_matches(".tex");
            name == stem || name.ends_with(&format!("/{stem}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_cleaned() {
        assert_eq!(
            clean(Path::new("/a/./b/../c.tex")),
            PathBuf::from("/a/c.tex")
        );
    }

    #[test]
    fn magic_comments() {
        let m = magic("% !TEX root = ../main.tex\n%!TEX program=xelatex\n\\section{A}");
        assert_eq!(m.root.as_deref(), Some("../main.tex"));
        assert_eq!(m.program, Some(Engine::Xelatex));
        assert_eq!(
            magic("% !TEX TS-program = lualatex").program,
            Some(Engine::Lualatex)
        );
        assert_eq!(magic("\\section{No magic}"), Magic::default());
    }

    #[test]
    fn engines_follow_the_packages() {
        let doc = |pre: &str| {
            format!("\\documentclass{{article}}\n{pre}\n\\begin{{document}}x\\end{{document}}")
        };
        assert_eq!(engine_for(&doc("\\usepackage{amsmath}")), Engine::Pdflatex);
        assert_eq!(
            engine_for(&doc("\\usepackage[UTF8]{ctex}")),
            Engine::Xelatex
        );
        assert_eq!(
            engine_for(&doc("\\usepackage{amsmath,fontspec}")),
            Engine::Xelatex
        );
        assert_eq!(
            engine_for(&doc("% \\usepackage{fontspec}")),
            Engine::Pdflatex
        );
        assert_eq!(engine_for(&doc("\\usepackage{luatexja}")), Engine::Lualatex);
        assert_eq!(
            engine_for("% !TEX program = lualatex\n\\documentclass{article}"),
            Engine::Lualatex
        );
    }

    #[test]
    fn main_files_need_both_markers_outside_comments() {
        assert!(is_main(
            "\\documentclass{article}\n\\begin{document}\nx\n\\end{document}"
        ));
        assert!(!is_main("\\section{Intro}\nText."));
        assert!(!is_main("% \\documentclass{article}\n\\begin{document}"));
        assert!(is_main("50\\% \\documentclass{a}\\begin{document}"));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-proj-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sections")).unwrap();
        dir
    }

    #[test]
    fn roots_and_projects() {
        let dir = temp("roots");
        let main = "\\documentclass{article}\n\\begin{document}\n\\input{sections/intro}\n\\end{document}\n";
        fs::write(dir.join("paper.tex"), main).unwrap();
        fs::write(
            dir.join("notes.tex"),
            "\\documentclass{article}\\begin{document}x\\end{document}",
        )
        .unwrap();
        let intro = dir.join("sections/intro.tex");
        fs::write(&intro, "\\section{Intro}\nText.").unwrap();
        // A section finds the main file that inputs it.
        assert_eq!(
            root_for(&intro, "\\section{Intro}").unwrap(),
            dir.join("paper.tex")
        );
        // A magic comment wins.
        assert_eq!(
            root_for(&intro, "% !TEX root = ../notes.tex\n\\section{Intro}").unwrap(),
            dir.join("notes.tex")
        );
        assert!(matches!(
            root_for(&intro, "% !TEX root = ../gone.tex\n"),
            Err(ProjectError::MissingRoot(_))
        ));
        let lone = dir.join("sections/lone.tex");
        assert!(matches!(
            root_for(&lone, "Text."),
            Err(ProjectError::NoRoot { .. })
        ));
        // The folder's main file: paper.tex ranks above notes.tex.
        assert_eq!(find_main(&dir).unwrap(), dir.join("paper.tex"));
        let files = tex_files(&dir);
        assert_eq!(files.len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }
}
