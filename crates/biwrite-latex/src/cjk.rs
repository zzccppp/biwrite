//! Chinese, Japanese and Korean text in a document whose preamble loads
//! nothing to typeset it. pdfLaTeX stops at each such character ("Unicode
//! character … not set up for use with LaTeX"), and XeLaTeX and LuaLaTeX
//! leave it out of the PDF. Such a build gets the support from outside the
//! document, as TeX code run before the root file is read (latexmk's
//! `-usepretex`), so no file changes and SyncTeX lines stay the source's.

use std::collections::HashSet;
use std::path::Path;

use crate::project::{Engine, inputs, preamble, uncommented};

/// Loads that already typeset CJK text.
pub(crate) const SUPPORT: &[&str] = &[
    "{ctex}",
    "{xeCJK}",
    "{CJK}",
    "{CJKutf8}",
    "{luatexja}",
    "{luatexja-fontspec}",
    "{kotex}",
    "{zxjatype}",
    "{ctexart}",
    "{ctexrep}",
    "{ctexbook}",
    "{ctexbeamer}",
    ",ctex}",
    "{ctex,",
    ",xeCJK}",
    "{xeCJK,",
    ",CJKutf8}",
    "{CJKutf8,",
];

/// Files read when looking for CJK text, the root and what it includes.
const MAX_FILES: usize = 200;

/// A character typeset with a CJK font (ideographs, kana, Hangul, CJK
/// punctuation and full-width forms).
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FA1F)
}

/// Whether the preamble `text` loads CJK support.
pub fn supported(text: &str) -> bool {
    let pre = uncommented(preamble(text));
    SUPPORT.iter().any(|p| pre.contains(p))
}

/// Whether the document rooted at `root` (relative to `dir`, where TeX
/// runs) has CJK text outside comments, in the root or a file it includes,
/// while its preamble, with the files the preamble includes, loads no CJK
/// support. Files that cannot be found or read are passed over.
pub fn needs_support(dir: &Path, root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(root)) else {
        return false;
    };
    let mut seen = HashSet::new();
    seen.insert(dir.join(root));
    let mut preamble_text = preamble(&text).to_owned();
    for name in inputs(&uncommented(preamble(&text))) {
        if let Some(more) = read_input(dir, &name, &mut seen) {
            preamble_text.push('\n');
            preamble_text.push_str(&more);
        }
    }
    if supported(&format!("{preamble_text}\n\\begin{{document}}")) {
        return false;
    }
    if uncommented(&preamble_text).chars().any(is_cjk) {
        return true;
    }
    let mut queue = vec![text];
    while let Some(text) = queue.pop() {
        let code = uncommented(&text);
        if code.chars().any(is_cjk) {
            return true;
        }
        for name in inputs(&code) {
            if seen.len() >= MAX_FILES {
                break;
            }
            if let Some(more) = read_input(dir, &name, &mut seen) {
                queue.push(more);
            }
        }
    }
    false
}

/// The file `name` names in `\input{name}` or the like, read once.
fn read_input(dir: &Path, name: &str, seen: &mut HashSet<std::path::PathBuf>) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('\\') || name.contains('#') {
        return None;
    }
    let plain = dir.join(name);
    let path = if plain.extension().is_some() && plain.is_file() {
        plain
    } else {
        dir.join(format!("{name}.tex"))
    };
    if !seen.insert(path.clone()) {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The Chinese font of pdfLaTeX builds, Arphic Song from the TeX
/// distribution's `cjk` package, with the bold made by the package.
const PDF_FAMILY: &str = "gbsn";

/// The TeX code that adds CJK support to a build with `engine`, run before
/// the root file is read. `\AddToHook` needs LaTeX 2020-10 or later. An
/// older LaTeX skips the code and builds as before.
///
/// With pdfLaTeX, `CJKutf8` is loaded at the end of the preamble and the
/// whole body is set in one `CJK*` environment, so CJK characters take the
/// CJK font wherever they appear while Latin text keeps the fonts, encoding
/// and engine the document chose. The font has one shape and two weights.
/// Every other series and shape of it is substituted without a warning,
/// after its font file is loaded inside the environment the way NFSS loads
/// it.
///
/// With XeLaTeX and LuaLaTeX, `ctex` with its plain scheme (headings and
/// dates as the class sets them) is loaded right before `\begin{document}`,
/// where it can still set up its own end of preamble. XeLaTeX takes the
/// system's Chinese fonts, as the Chinese mirror does. LuaLaTeX takes the
/// Fandol fonts of the TeX distribution, since it cannot load some of the
/// system's (macOS's Heiti).
pub fn pretex(engine: Engine) -> String {
    let code = match engine {
        Engine::Pdflatex => {
            let mut shapes = String::new();
            for series in ["m", "b", "bx", "sb", "l"] {
                for shape in ["n", "it", "sl", "sc", "ui"] {
                    if matches!((series, shape), ("m", "n") | ("bx", "n")) {
                        continue;
                    }
                    let to = if matches!(series, "b" | "bx" | "sb") {
                        "bx"
                    } else {
                        "m"
                    };
                    shapes.push_str(&format!(
                        "\\DeclareFontShape{{C70}}{{{PDF_FAMILY}}}{{{series}}}{{{shape}}}\
                         {{<->ssub*{PDF_FAMILY}/{to}/n}}{{}}"
                    ));
                }
            }
            format!(
                "\\AddToHook{{env/document/before}}{{\\RequirePackage{{CJKutf8}}}}\
                 \\AddToHook{{begindocument/end}}{{\\begin{{CJK*}}{{UTF8}}{{{PDF_FAMILY}}}\
                 \\begingroup\\fontencoding{{C70}}\\fontfamily{{{PDF_FAMILY}}}\
                 \\csname try@load@fontshape\\endcsname\\endgroup{shapes}\
                 \\AtEndDocument{{\\clearpage\\end{{CJK*}}}}}}"
            )
        }
        Engine::Xelatex => {
            "\\AddToHook{env/document/before}{\\RequirePackage[UTF8,scheme=plain]{ctex}}".to_owned()
        }
        Engine::Lualatex => "\\AddToHook{env/document/before}\
             {\\RequirePackage[UTF8,scheme=plain,fontset=fandol]{ctex}}"
            .to_owned(),
    };
    format!("\\ifdefined\\AddToHook{code}\\fi")
}

/// The file `engine` needs for [`pretex`].
pub fn package_file(engine: Engine) -> &'static str {
    match engine {
        Engine::Pdflatex => "CJKutf8.sty",
        Engine::Xelatex | Engine::Lualatex => "ctex.sty",
    }
}

/// Whether a TeX log reports a CJK character no font could typeset, as
/// pdfLaTeX's "Unicode character 中 (U+4E2D)" or XeTeX's and LuaTeX's
/// "Missing character: There is no 中 (U+4E2D) in font …".
pub fn missing_in_log(log: &str) -> bool {
    log.lines()
        .filter(|l| l.contains("Unicode character") || l.contains("Missing character"))
        .any(|l| {
            l.match_indices("(U+").any(|(i, _)| {
                let hex: String = l[i + 3..]
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .collect();
                u32::from_str_radix(&hex, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .is_some_and(is_cjk)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "biwrite-cjk-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    const ENGLISH: &str = "\\documentclass{article}\n\\begin{document}\nText.\n\\end{document}\n";

    #[test]
    fn chinese_text_without_support_needs_it() {
        let dir = project(&[(
            "main.tex",
            "\\documentclass{article}\n\\begin{document}\nEnglish, then 中文。\n\\end{document}\n",
        )]);
        assert!(needs_support(&dir, Path::new("main.tex")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn english_documents_and_chinese_comments_need_nothing() {
        let dir = project(&[
            ("main.tex", ENGLISH),
            (
                "notes.tex",
                "\\documentclass{article}\n% 中文注释\n\\begin{document}\nText. % 注释\n50\\% 完成\n\\end{document}\n",
            ),
        ]);
        assert!(!needs_support(&dir, Path::new("main.tex")));
        // `50\%` starts no comment, so the Chinese after it counts.
        assert!(needs_support(&dir, Path::new("notes.tex")));
        let dir2 = project(&[(
            "main.tex",
            "\\documentclass{article}\n% 中文注释\n\\begin{document}\nText. % 注释\n\\end{document}\n",
        )]);
        assert!(!needs_support(&dir2, Path::new("main.tex")));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(dir2).unwrap();
    }

    #[test]
    fn documents_with_support_are_left_alone() {
        for preamble in [
            "\\usepackage[UTF8]{ctex}",
            "\\usepackage{fontspec,xeCJK}",
            "\\usepackage{CJKutf8}",
            "\\input{setup}",
        ] {
            let text = format!(
                "\\documentclass{{article}}\n{preamble}\n\\begin{{document}}\n中文\n\\end{{document}}\n"
            );
            let dir = project(&[("main.tex", &text), ("setup.tex", "\\usepackage{xeCJK}\n")]);
            assert!(!needs_support(&dir, Path::new("main.tex")), "{preamble}");
            std::fs::remove_dir_all(dir).unwrap();
        }
        let dir = project(&[(
            "main.tex",
            "\\documentclass[UTF8]{ctexart}\n\\begin{document}\n中文\n\\end{document}\n",
        )]);
        assert!(!needs_support(&dir, Path::new("main.tex")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chinese_in_an_included_file_counts() {
        let dir = project(&[
            (
                "main.tex",
                "\\documentclass{article}\n\\begin{document}\n\\input{sections/intro}\n\\include{sections/more.tex}\n\\end{document}\n",
            ),
            ("sections/intro.tex", "English only.\n"),
            ("sections/more.tex", "Then 中文。\n"),
        ]);
        assert!(needs_support(&dir, Path::new("main.tex")));
        std::fs::write(dir.join("sections/more.tex"), "English. % 中文\n").unwrap();
        assert!(!needs_support(&dir, Path::new("main.tex")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn includes_that_loop_end() {
        let dir = project(&[
            (
                "main.tex",
                "\\documentclass{article}\n\\begin{document}\n\\input{a}\n\\end{document}\n",
            ),
            ("a.tex", "\\input{b}\n"),
            ("b.tex", "\\input{a}\n\\input{main}\n"),
        ]);
        assert!(!needs_support(&dir, Path::new("main.tex")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn logs_name_missing_cjk_characters() {
        assert!(missing_in_log(
            "./main.tex:5: LaTeX Error: Unicode character 中 (U+4E2D)\n               not set up for use with LaTeX."
        ));
        assert!(missing_in_log(
            "Missing character: There is no 文 (U+6587) in font [lmroman10-regular]:mapping=tex-text;!"
        ));
        assert!(missing_in_log(
            "Missing character: There is no ， (U+FF0C) in font cmr10!"
        ));
        assert!(!missing_in_log(
            "./main.tex:5: LaTeX Error: Unicode character ≈ (U+2248)\n not set up for use with LaTeX."
        ));
        assert!(!missing_in_log("Output written on main.pdf (1 page)."));
    }

    #[test]
    fn pretex_keeps_to_one_line_and_skips_old_latex() {
        for engine in [Engine::Pdflatex, Engine::Xelatex, Engine::Lualatex] {
            let code = pretex(engine);
            assert!(code.starts_with("\\ifdefined\\AddToHook"));
            assert!(code.ends_with("\\fi"));
            assert!(!code.contains('\n'));
            // latexmk puts the code in double quotes for the shell.
            assert!(!code.contains(['"', '$', '`', '%']));
        }
        assert!(pretex(Engine::Pdflatex).contains("{CJK*}{UTF8}{gbsn}"));
        assert!(pretex(Engine::Xelatex).contains("scheme=plain]{ctex}"));
        assert!(pretex(Engine::Lualatex).contains("fontset=fandol]{ctex}"));
    }
}
