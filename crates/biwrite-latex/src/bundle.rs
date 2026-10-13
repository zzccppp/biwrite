//! A paper with the files it needs to build, for saving or exporting it
//! into a folder of its own. The files of the root's folder come along
//! (its template with class, styles and bibliography style, its figures,
//! its other parts), and so do the files outside that folder that its last
//! build read or its root names (`../figures/…`, `../refs.bib`), at the
//! same places relative to each other, so the copy builds as the original
//! does.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::project::{inputs, uncommented};
use crate::templates::{TemplateError, project_files};

/// How many folders above a root's folder files are still taken along.
pub const MAX_UP: usize = 3;

/// What [`bundle`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    /// The folder the copy is laid out from, the roots' folder or the
    /// folder above it that holds every file taken along.
    pub base: PathBuf,
    /// The files to copy, relative to `base`.
    pub files: Vec<PathBuf>,
    /// Files the build read further up than [`MAX_UP`] folders, not copied.
    pub left_out: Vec<PathBuf>,
}

/// The files the documents rooted at `roots` (a paper, and the root of its
/// paired file) need. These are each root's folder as [`project_files`]
/// lists it, and the files outside it named by the last build's records
/// (`.fls`, `.fdb_latexmk`) or by the root itself (`\bibliography`,
/// `\addbibresource`, `\input`, `\include`, `\includegraphics`).
pub fn bundle(roots: &[&Path]) -> Result<Bundle, TemplateError> {
    let mut wanted: BTreeSet<PathBuf> = BTreeSet::new();
    let mut left_out: BTreeSet<PathBuf> = BTreeSet::new();
    let mut folders: Vec<PathBuf> = Vec::new();
    for root in roots {
        let root = normalize(root);
        let Some(dir) = root.parent().map(Path::to_path_buf) else {
            continue;
        };
        for rel in project_files(&dir, Some(&root))? {
            wanted.insert(dir.join(rel));
        }
        wanted.insert(root.clone());
        let limit = dir.ancestors().nth(MAX_UP).unwrap_or(&dir).to_path_buf();
        let stem = root
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = fs::read_to_string(&root).unwrap_or_default();
        let mut named = recorded(&dir, &stem);
        named.extend(referenced(&text));
        // What the parts name too (a figure of a section kept elsewhere).
        for part in crate::project::tex_files(&dir)
            .iter()
            .filter(|f| **f != root)
        {
            named.extend(referenced(&fs::read_to_string(part).unwrap_or_default()));
        }
        for name in named {
            let Some(path) = candidates(&dir, &name).into_iter().find(|p| p.is_file()) else {
                continue;
            };
            if path.starts_with(&dir) || generated(&path) {
                continue;
            }
            if path.starts_with(&limit) {
                wanted.insert(path);
            } else {
                left_out.insert(path);
            }
        }
        // A paper whose bibliography comes only as its `.bbl` (as arXiv
        // sources ship it) needs that file.
        let bbl = dir.join(format!("{stem}.bbl"));
        if bbl.is_file()
            && !wanted
                .iter()
                .any(|p| p.extension().is_some_and(|e| e == "bib"))
        {
            wanted.insert(bbl);
        }
        folders.push(dir);
    }
    let base = wanted
        .iter()
        .filter_map(|p| p.parent())
        .chain(folders.iter().map(PathBuf::as_path))
        .fold(None::<PathBuf>, |acc, dir| {
            Some(match acc {
                None => dir.to_path_buf(),
                Some(acc) => common(&acc, dir),
            })
        })
        .unwrap_or_default();
    let files = wanted
        .iter()
        .filter_map(|p| p.strip_prefix(&base).ok().map(Path::to_path_buf))
        .collect();
    Ok(Bundle {
        base,
        files,
        left_out: left_out.into_iter().collect(),
    })
}

/// Copy the files of `bundle` into `dest`, a new or empty folder, leaving
/// out the paths in `skip` (relative to the base, files written from the
/// editor instead). Nothing in `dest` is replaced. After a failure the
/// files copied so far are removed again. Returns how many were copied.
pub fn copy_bundle(bundle: &Bundle, dest: &Path, skip: &[PathBuf]) -> Result<usize, TemplateError> {
    if fs::read_dir(dest).is_ok_and(|mut d| d.next().is_some()) {
        return Err(TemplateError::NotEmpty(dest.display().to_string()));
    }
    let created = !dest.exists();
    let mut copied: Vec<PathBuf> = Vec::new();
    let result = (|| {
        fs::create_dir_all(dest).map_err(io(dest))?;
        for rel in bundle.files.iter().filter(|f| !skip.contains(f)) {
            let to = dest.join(rel);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(io(parent))?;
            }
            // Never over a file. The folder was empty, so a file there now
            // was put there meanwhile.
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&to)
                .map_err(io(&to))?;
            copied.push(to.clone());
            let mut from = fs::File::open(bundle.base.join(rel)).map_err(io(&to))?;
            std::io::copy(&mut from, &mut out).map_err(io(&to))?;
        }
        Ok(copied.len())
    })();
    if result.is_err() {
        if created {
            let _ = fs::remove_dir_all(dest);
        } else {
            for file in &copied {
                let _ = fs::remove_file(file);
            }
        }
    }
    result
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> TemplateError + '_ {
    move |source| TemplateError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Relative paths the last build of `stem` in `dir` read, from the
/// recorder's `INPUT` lines and latexmk's sources (the bibliography
/// included). Absolute paths are the TeX distribution's or fixed anyway.
fn recorded(dir: &Path, stem: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(fls) = fs::read_to_string(dir.join(format!("{stem}.fls"))) {
        out.extend(
            fls.lines()
                .filter_map(|l| l.strip_prefix("INPUT "))
                .map(str::trim)
                .map(str::to_owned),
        );
    }
    if let Ok(fdb) = fs::read_to_string(dir.join(format!("{stem}.fdb_latexmk"))) {
        out.extend(fdb.lines().filter_map(|l| {
            let rest = l.strip_prefix("  \"")?;
            Some(rest[..rest.find('"')?].to_owned())
        }));
    }
    out.retain(|p| !p.is_empty() && !Path::new(p).is_absolute());
    out
}

/// Paths the root names in commands that read files.
fn referenced(text: &str) -> Vec<String> {
    let code = uncommented(text);
    let mut out = inputs(&code);
    for cmd in ["\\bibliography{", "\\addbibresource{", "\\addbibresource["] {
        for (i, _) in code.match_indices(cmd) {
            let mut arg = &code[i + cmd.len()..];
            if cmd.ends_with('[') {
                let Some(end) = arg.find("]{") else { continue };
                arg = &arg[end + 2..];
            }
            let Some(end) = arg.find('}') else { continue };
            for name in arg[..end].split(',') {
                let name = name.trim();
                out.push(if name.ends_with(".bib") {
                    name.to_owned()
                } else {
                    format!("{name}.bib")
                });
            }
        }
    }
    for (i, _) in code.match_indices("\\includegraphics") {
        let rest = &code[i + "\\includegraphics".len()..];
        let rest = rest.trim_start_matches('*');
        let rest = match rest.strip_prefix('[') {
            Some(r) => r.find(']').map_or(r, |e| &r[e + 1..]),
            None => rest,
        };
        let Some(rest) = rest.trim_start().strip_prefix('{') else {
            continue;
        };
        if let Some(end) = rest.find('}') {
            out.push(rest[..end].trim().to_owned());
        }
    }
    out.retain(|n| !n.is_empty() && !n.contains('\\') && !n.contains('#'));
    out
}

/// Where TeX looks for `name` from `dir`, as given and then with the usual
/// extensions.
fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    let plain = normalize(&dir.join(name));
    let mut out = vec![plain.clone()];
    for ext in ["tex", "pdf", "png", "jpg", "jpeg", "eps"] {
        out.push(PathBuf::from(format!("{}.{ext}", plain.display())));
    }
    out
}

/// Build output met in the records, never copied.
fn generated(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.ends_with("-eps-converted-to.pdf")
        || [
            ".aux",
            ".bbl",
            ".blg",
            ".out",
            ".toc",
            ".log",
            ".fls",
            ".fdb_latexmk",
            ".synctex.gz",
            ".xdv",
            ".dvi",
            ".bcf",
            ".run.xml",
            ".lof",
            ".lot",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// `path` with `.` and `..` resolved by the names alone.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The deepest folder holding both `a` and `b`.
fn common(a: &Path, b: &Path) -> PathBuf {
    a.components()
        .zip(b.components())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.as_os_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "biwrite-bundle-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        for (name, text) in files {
            let path = dir.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    const MAIN: &str = "\\documentclass{ieeecolor}\n\\newcommand{\\figdir}{../figures/}\n\\begin{document}\n\\includegraphics{\\figdir fig_a.pdf}\n\\includegraphics[width=1in]{../shared/logo}\n\\bibliographystyle{IEEEtran}\n\\bibliography{../refs}\n\\end{document}\n";

    /// A paper in `en/` with figures in `../figures` (through a macro, so
    /// only the build's record names them), a bibliography in `..`, and a
    /// sibling `zh/` it has nothing to do with.
    fn paper() -> PathBuf {
        project(&[
            ("en/paper.tex", MAIN),
            ("en/ieeecolor.cls", "class"),
            ("en/IEEEtran.bst", "bst"),
            ("en/paper.aux", "aux"),
            ("en/paper.pdf", "pdf"),
            ("en/TII.eps", "eps"),
            (
                "en/paper.fls",
                "PWD /x\nINPUT /usr/local/texlive/2023/texmf-dist/tex/latex/base/article.cls\nINPUT ../figures/fig_a.pdf\nINPUT ./ieeecolor.cls\nINPUT ./TII-eps-converted-to.pdf\nINPUT paper.aux\n",
            ),
            (
                "en/paper.fdb_latexmk",
                "[\"bibtex paper\"] 1 \"paper.aux\" \"paper.bbl\" \"paper\" 1 0\n  \"../refs.bib\" 1 2 abc \"\"\n  \"./IEEEtran.bst\" 1 2 abc \"\"\n  \"/usr/local/texlive/2023/texmf-dist/bibtex/bst/base/plain.bst\" 1 2 abc \"\"\n",
            ),
            ("en/TII-eps-converted-to.pdf", "converted"),
            ("figures/fig_a.pdf", "fig a"),
            ("figures/unused.pdf", "unused"),
            ("shared/logo.png", "logo"),
            ("refs.bib", "@article{x}"),
            ("zh/paper_zh.tex", "中文"),
        ])
    }

    #[test]
    fn a_paper_takes_its_folder_and_the_files_it_reads_outside() {
        let dir = paper();
        let b = bundle(&[&dir.join("en/paper.tex")]).unwrap();
        assert_eq!(b.base, normalize(&dir));
        let files: Vec<String> = b.files.iter().map(|f| f.display().to_string()).collect();
        assert_eq!(
            files,
            [
                "en/IEEEtran.bst",
                "en/TII-eps-converted-to.pdf",
                "en/TII.eps",
                "en/ieeecolor.cls",
                "en/paper.tex",
                "figures/fig_a.pdf",
                "refs.bib",
                "shared/logo.png",
            ]
        );
        assert!(b.left_out.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_paper_with_everything_in_its_folder_stays_flat() {
        let dir = project(&[
            (
                "main.tex",
                "\\documentclass{article}\n\\begin{document}\n\\input{sections/intro}\n\\bibliography{refs}\n\\end{document}\n",
            ),
            ("sections/intro.tex", "Intro."),
            ("refs.bib", "@article{x}"),
            ("main.pdf", "pdf"),
            (".biwrite/zh/main.tex", "mirror build"),
        ]);
        let b = bundle(&[&dir.join("main.tex")]).unwrap();
        assert_eq!(b.base, normalize(&dir));
        let files: Vec<String> = b.files.iter().map(|f| f.display().to_string()).collect();
        assert_eq!(files, ["main.tex", "refs.bib", "sections/intro.tex"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_pair_takes_both_folders() {
        let dir = paper();
        let b = bundle(&[&dir.join("en/paper.tex"), &dir.join("zh/paper_zh.tex")]).unwrap();
        assert!(b.files.contains(&PathBuf::from("zh/paper_zh.tex")));
        assert!(b.files.contains(&PathBuf::from("en/paper.tex")));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_far_above_are_left_out() {
        let dir = project(&[
            (
                "a/b/c/d/main.tex",
                "\\documentclass{article}\n\\begin{document}\n\\bibliography{../../../../refs}\n\\end{document}\n",
            ),
            ("refs.bib", "@article{x}"),
        ]);
        let b = bundle(&[&dir.join("a/b/c/d/main.tex")]).unwrap();
        assert_eq!(b.base, normalize(&dir.join("a/b/c/d")));
        assert_eq!(b.files, [PathBuf::from("main.tex")]);
        assert_eq!(b.left_out, [normalize(&dir.join("refs.bib"))]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copies_go_into_an_empty_folder_and_replace_nothing() {
        let dir = paper();
        let b = bundle(&[&dir.join("en/paper.tex")]).unwrap();
        let dest = dir.join("copy");
        let n = copy_bundle(&b, &dest, &[PathBuf::from("en/paper.tex")]).unwrap();
        assert_eq!(n, b.files.len() - 1);
        assert!(!dest.join("en/paper.tex").exists(), "left to the editor");
        assert_eq!(
            fs::read_to_string(dest.join("refs.bib")).unwrap(),
            "@article{x}"
        );
        assert_eq!(
            fs::read_to_string(dest.join("figures/fig_a.pdf")).unwrap(),
            "fig a"
        );
        // A folder with files is refused, and nothing in it changes.
        let Err(TemplateError::NotEmpty(_)) = copy_bundle(&b, &dest, &[]) else {
            panic!("copied into a folder with files");
        };
        assert_eq!(
            fs::read_to_string(dest.join("refs.bib")).unwrap(),
            "@article{x}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_copy_takes_its_files_away_again() {
        let dir = paper();
        let mut b = bundle(&[&dir.join("en/paper.tex")]).unwrap();
        b.files.push(PathBuf::from("missing.tex"));
        let dest = dir.join("copy");
        assert!(copy_bundle(&b, &dest, &[]).is_err());
        assert!(!dest.exists(), "the new folder is gone again");
        fs::remove_dir_all(dir).unwrap();
    }
}
