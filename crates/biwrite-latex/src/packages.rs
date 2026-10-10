//! Packages: what a document loads, and what a figure or table written for
//! it needs. A figure from the writing assistant compiles only when the
//! preamble loads the packages its commands come from.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::project::{preamble, uncommented};

/// Packages a class loads by itself (only those a figure or table uses).
const CLASS_PACKAGES: &[(&str, &[&str])] = &[
    (
        "acmart",
        &[
            "amsmath", "amssymb", "booktabs", "graphicx", "hyperref", "natbib", "xcolor",
        ],
    ),
    (
        "beamer",
        &["amsmath", "amssymb", "graphicx", "hyperref", "xcolor"],
    ),
    ("ctexart", &["ctex"]),
    ("ctexrep", &["ctex"]),
    ("ctexbook", &["ctex"]),
];

/// Commands and environments of a snippet, and the package each needs.
const NEEDS: &[(&str, &str)] = &[
    ("\\begin{axis}", "pgfplots"),
    ("\\begin{semilogyaxis}", "pgfplots"),
    ("\\begin{semilogxaxis}", "pgfplots"),
    ("\\begin{loglogaxis}", "pgfplots"),
    ("\\addplot", "pgfplots"),
    ("\\pgfplotsset", "pgfplots"),
    ("\\begin{tikzpicture}", "tikz"),
    ("\\tikz", "tikz"),
    ("\\usetikzlibrary", "tikz"),
    ("\\toprule", "booktabs"),
    ("\\midrule", "booktabs"),
    ("\\bottomrule", "booktabs"),
    ("\\cmidrule", "booktabs"),
    ("\\includegraphics", "graphicx"),
    ("\\resizebox", "graphicx"),
    ("\\scalebox", "graphicx"),
    ("\\multirow", "multirow"),
    ("\\makecell", "makecell"),
    // Colours such as gray!20 are xcolor's, loaded before colortbl.
    ("\\cellcolor", "xcolor"),
    ("\\rowcolor", "xcolor"),
    ("\\cellcolor", "colortbl"),
    ("\\rowcolor", "colortbl"),
    ("\\textcolor", "xcolor"),
    ("\\definecolor", "xcolor"),
    ("\\begin{subfigure}", "subcaption"),
    ("\\subcaption", "subcaption"),
    ("\\subfloat", "subfig"),
    ("\\begin{tabularx}", "tabularx"),
    ("\\begin{threeparttable}", "threeparttable"),
    ("\\begin{wrapfigure}", "wrapfig"),
    ("\\begin{wraptable}", "wrapfig"),
    ("\\begin{adjustbox}", "adjustbox"),
    ("\\SI{", "siunitx"),
    ("\\num{", "siunitx"),
    ("\\checkmark", "amssymb"),
    ("\\mathbb", "amssymb"),
];

/// Packages that load others, as far as a figure is concerned.
const BRINGS: &[(&str, &[&str])] = &[
    ("pgfplots", &["tikz", "graphicx", "xcolor"]),
    ("tikz", &["graphicx", "xcolor"]),
    ("colortbl", &[]),
    ("xcolor", &[]),
    ("ctex", &[]),
    ("amssymb", &["amsfonts"]),
];

/// Names in `\usepackage[...]{a,b}` and `\RequirePackage{...}` of `text`
/// (comments ignored).
fn named_packages(text: &str) -> Vec<String> {
    let code = uncommented(text);
    let mut out = Vec::new();
    for cmd in ["\\usepackage", "\\RequirePackage"] {
        for (i, _) in code.match_indices(cmd) {
            let mut rest = code[i + cmd.len()..].trim_start();
            if rest.starts_with('[') {
                let Some(end) = rest.find(']') else { continue };
                rest = rest[end + 1..].trim_start();
            }
            let Some(rest) = rest.strip_prefix('{') else {
                continue;
            };
            let Some(end) = rest.find('}') else { continue };
            out.extend(
                rest[..end]
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    out
}

/// The document class of `text`.
fn class(text: &str) -> Option<String> {
    let code = uncommented(text);
    let at = code.find("\\documentclass")?;
    let mut rest = code[at + "\\documentclass".len()..].trim_start();
    if rest.starts_with('[') {
        rest = rest[rest.find(']')? + 1..].trim_start();
    }
    let rest = rest.strip_prefix('{')?;
    Some(rest[..rest.find('}')?].trim().to_owned())
}

/// The packages the document with main file text `root_text` (in folder
/// `dir`) loads: its preamble, preamble files it `\input`s, the project's
/// own `.sty` files it loads (one level), and what its class brings.
pub fn loaded(dir: &Path, root_text: &str) -> BTreeSet<String> {
    let pre = preamble(root_text);
    let code = uncommented(pre);
    let mut texts = vec![pre.to_owned()];
    for cmd in ["\\input{", "\\include{"] {
        for (i, _) in code.match_indices(cmd) {
            let arg = &code[i + cmd.len()..];
            if let Some(end) = arg.find('}') {
                let name = arg[..end].trim();
                let path = dir.join(if name.ends_with(".tex") {
                    name.to_owned()
                } else {
                    format!("{name}.tex")
                });
                if let Ok(t) = fs::read_to_string(path) {
                    texts.push(t);
                }
            }
        }
    }
    let mut out: BTreeSet<String> = BTreeSet::new();
    for t in &texts {
        for p in named_packages(t) {
            if let Ok(sty) = fs::read_to_string(dir.join(format!("{p}.sty"))) {
                out.extend(named_packages(&sty));
            }
            out.insert(p);
        }
    }
    if let Some(class) = class(root_text) {
        if let Some((_, pkgs)) = CLASS_PACKAGES.iter().find(|(c, _)| *c == class) {
            out.extend(pkgs.iter().map(|p| (*p).to_owned()));
        }
    }
    let brought: Vec<String> = out
        .iter()
        .filter_map(|p| BRINGS.iter().find(|(b, _)| b == p))
        .flat_map(|(_, more)| more.iter().map(|m| (*m).to_owned()))
        .collect();
    out.extend(brought);
    out
}

/// The packages `snippet` needs, in a sensible loading order, without
/// duplicates.
pub fn required(snippet: &str) -> Vec<&'static str> {
    let code = uncommented(snippet);
    let mut out: Vec<&'static str> = Vec::new();
    for (pattern, package) in NEEDS {
        if code.contains(pattern) && !out.contains(package) {
            out.push(package);
        }
    }
    // pgfplots loads tikz itself.
    if out.contains(&"pgfplots") {
        out.retain(|p| *p != "tikz");
    }
    out
}

/// Of the packages `snippet` needs, those the document does not load.
pub fn missing(snippet: &str, loaded: &BTreeSet<String>) -> Vec<String> {
    required(snippet)
        .into_iter()
        .filter(|p| {
            // pgfplots loads tikz itself.
            let loaded_already =
                loaded.contains(*p) || (*p == "tikz" && loaded.contains("pgfplots"));
            !loaded_already
        })
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_come_from_the_preamble_its_files_and_the_class() {
        let dir = std::env::temp_dir().join(format!("biwrite-pkgs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("mystyle.sty"), "\\RequirePackage{natbib}\n").unwrap();
        fs::write(dir.join("preamble.tex"), "\\usepackage{siunitx}\n").unwrap();
        let root = "\\documentclass[sigconf]{acmart}\n\
                    \\usepackage[utf8]{inputenc}\n\
                    \\usepackage{mystyle, tikz}\n\
                    % \\usepackage{multirow}\n\
                    \\input{preamble}\n\
                    \\begin{document}\n\\usepackage{late}\n\\end{document}\n";
        let got = loaded(&dir, root);
        for p in [
            "inputenc", "mystyle", "natbib", "tikz", "siunitx", "booktabs", "graphicx", "xcolor",
        ] {
            assert!(got.contains(p), "{p} in {got:?}");
        }
        assert!(!got.contains("multirow"), "commented out");
        assert!(!got.contains("late"), "after \\begin{{document}}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_snippet_names_what_it_needs() {
        let table = "\\begin{table}\\centering\\begin{tabular}{lr}\\toprule A & \\multirow{2}{*}{x}\\\\\
                     \\midrule\\cellcolor{gray}B & 1\\\\\\bottomrule\\end{tabular}\\end{table}";
        assert_eq!(
            required(table),
            ["booktabs", "multirow", "xcolor", "colortbl"]
        );
        let plot = "\\begin{tikzpicture}\\begin{axis}\\addplot coordinates {(0,1)};\\end{axis}\\end{tikzpicture}";
        assert_eq!(required(plot), ["pgfplots"]);
        assert!(required("% \\includegraphics{x}\nplain").is_empty());

        let have: BTreeSet<String> = ["booktabs".to_owned(), "xcolor".to_owned()].into();
        assert_eq!(missing(table, &have), ["multirow", "colortbl"]);
        let have: BTreeSet<String> = ["pgfplots".to_owned(), "tikz".to_owned()].into();
        assert!(missing(plot, &have).is_empty());
    }
}
