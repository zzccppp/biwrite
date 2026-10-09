//! The Chinese mirror as a compilable document: the composed translation
//! with Chinese font support added, compiled with XeLaTeX into a folder of
//! its own. Every change keeps line numbers, so SyncTeX lines of the
//! mirror PDF are lines of the composed text.

use crate::project::{preamble, uncommented};

/// Where the mirror build lives, relative to the project folder.
pub const MIRROR_DIR: &str = ".biwrite/zh";

/// Loads that already typeset Chinese.
const CJK_SUPPORT: &[&str] = &[
    "{ctex}",
    "{xeCJK}",
    "{CJK}",
    "{CJKutf8}",
    "{luatexja}",
    "{ctexart}",
    "{ctexrep}",
    "{ctexbook}",
    "{ctexbeamer}",
    ",ctex}",
    "{ctex,",
    ",xeCJK}",
    "{xeCJK,",
];

/// pdfTeX registers that XeTeX lacks, defined as plain counters when
/// missing so `\pdfoutput=1` and the like do not stop the build.
const PDFTEX_GUARD: &str = "\\ifdefined\\pdfoutput\\else\\newcount\\pdfoutput\\fi\
\\ifdefined\\pdfminorversion\\else\\newcount\\pdfminorversion\\fi";

/// The mirror text with Chinese support: `ctex` (plain scheme, so headings
/// and dates stay as the class sets them) loaded right before
/// `\begin{document}`, on the same line.
pub fn with_chinese(text: &str) -> String {
    let pre = uncommented(preamble(text));
    let mut out = String::with_capacity(text.len() + 160);
    out.push_str(PDFTEX_GUARD);
    if CJK_SUPPORT.iter().any(|p| pre.contains(p)) {
        out.push_str(text);
        return out;
    }
    match begin_document(text) {
        Some(at) => {
            out.push_str(&text[..at]);
            out.push_str("\\usepackage[UTF8,scheme=plain]{ctex}");
            out.push_str(&text[at..]);
        }
        None => out.push_str(text),
    }
    out
}

/// Byte offset of the first `\begin{document}` outside a comment.
fn begin_document(text: &str) -> Option<usize> {
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let code = uncommented(line);
        if let Some(i) = code.find("\\begin{document}") {
            return Some(start + i);
        }
        start += line.len();
    }
    None
}

/// The root text with the `\input`, `\include` or `\subfile` of `from`
/// (a path relative to the root's folder, without `.tex`) pointed at `to`.
/// `None` when the root does not include `from`.
pub fn redirect_include(root: &str, from: &str, to: &str) -> Option<String> {
    const COMMANDS: [&str; 3] = ["\\input{", "\\include{", "\\subfile{"];
    let mut out = String::with_capacity(root.len() + to.len());
    let mut found = false;
    for line in root.split_inclusive('\n') {
        let code = uncommented(line).len().min(line.len());
        let mut copied = 0;
        let mut i = 0;
        while i < code {
            let rest = &line[i..code];
            let Some(cmd) = COMMANDS.iter().find(|c| rest.starts_with(**c)) else {
                i += rest.chars().next().map_or(1, char::len_utf8);
                continue;
            };
            let arg = i + cmd.len();
            let Some(len) = line[arg..code].find('}') else {
                break;
            };
            let name = line[arg..arg + len].trim();
            if name.trim_end_matches(".tex").trim_start_matches("./") == from {
                out.push_str(&line[copied..arg]);
                out.push_str(to);
                copied = arg + len;
                found = true;
            }
            i = arg + len;
        }
        out.push_str(&line[copied..]);
    }
    found.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\\pdfoutput=1\n\\documentclass{article}\n% \\begin{document} in a comment\n\\usepackage{amsmath}\n\\begin{document}\n中文段落。\n\\end{document}\n";

    #[test]
    fn chinese_support_keeps_line_numbers() {
        let zh = with_chinese(DOC);
        assert_eq!(zh.lines().count(), DOC.lines().count());
        let line = zh.lines().nth(4).unwrap();
        assert_eq!(
            line,
            "\\usepackage[UTF8,scheme=plain]{ctex}\\begin{document}"
        );
        assert!(zh.starts_with("\\ifdefined\\pdfoutput"));
        assert!(zh.lines().nth(2).unwrap().starts_with('%'));
    }

    #[test]
    fn documents_with_chinese_support_are_left_alone() {
        let doc = "\\documentclass{ctexart}\n\\begin{document}\nx\n\\end{document}\n";
        assert_eq!(with_chinese(doc), format!("{PDFTEX_GUARD}{doc}"));
        let doc = "\\documentclass{article}\n\\usepackage[UTF8]{ctex}\n\\begin{document}\n\\end{document}";
        assert!(!with_chinese(doc).contains("scheme=plain"));
    }

    #[test]
    fn includes_are_redirected() {
        let root = "\\begin{document}\n\\input{sections/intro}\n% \\input{sections/intro}\n\\include{sections/method.tex}\n\\end{document}\n";
        let out = redirect_include(root, "sections/intro", ".biwrite/zh/sections/intro").unwrap();
        assert!(out.contains("\\input{.biwrite/zh/sections/intro}\n% \\input{sections/intro}"));
        assert_eq!(out.lines().count(), root.lines().count());
        let out = redirect_include(root, "sections/method", "x/method").unwrap();
        assert!(out.contains("\\include{x/method}"));
        assert!(redirect_include(root, "sections/other", "x").is_none());
    }
}
