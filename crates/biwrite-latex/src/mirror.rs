//! The Chinese mirror as a compilable document: the composed translation
//! with Chinese font support added, compiled with XeLaTeX into a folder of
//! its own. Every change keeps line numbers, so SyncTeX lines of the
//! mirror PDF are lines of the composed text.

use crate::project::uncommented;

/// Where the mirror build lives, relative to the project folder.
pub const MIRROR_DIR: &str = ".biwrite/zh";

/// Where the English mirror of a Chinese document is built.
pub const EN_MIRROR_DIR: &str = ".biwrite/en";

/// Where a document is built to see whether it compiles, apart from the
/// PDFs shown.
pub const CHECK_DIR: &str = ".biwrite/check";

/// pdfTeX registers that XeTeX lacks, defined as plain counters when
/// missing so `\pdfoutput=1` and the like do not stop the build.
const PDFTEX_GUARD: &str = "\\ifdefined\\pdfoutput\\else\\newcount\\pdfoutput\\fi\
\\ifdefined\\pdfminorversion\\else\\newcount\\pdfminorversion\\fi";

/// The mirror text with Chinese support: `ctex` (plain scheme, so headings
/// and dates stay as the class sets them) loaded right before
/// `\begin{document}`, on the same line.
pub fn with_chinese(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 160);
    out.push_str(PDFTEX_GUARD);
    if crate::cjk::supported(text) {
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
    // An `\includeonly` list names it too, or the copy is left out.
    found.then(|| includeonly_to(&out, from, to))
}

/// `root` with `from` in its `\includeonly{…}` list read as `to`.
fn includeonly_to(root: &str, from: &str, to: &str) -> String {
    const COMMAND: &str = "\\includeonly{";
    let mut out = String::with_capacity(root.len());
    for line in root.split_inclusive('\n') {
        let code = uncommented(line).len().min(line.len());
        let list = line[..code].find(COMMAND).and_then(|i| {
            let arg = i + COMMAND.len();
            line[arg..code].find('}').map(|len| (arg, arg + len))
        });
        let Some((start, end)) = list else {
            out.push_str(line);
            continue;
        };
        let names: Vec<String> = line[start..end]
            .split(',')
            .map(|name| {
                if name
                    .trim()
                    .trim_end_matches(".tex")
                    .trim_start_matches("./")
                    == from
                {
                    to.to_owned()
                } else {
                    name.to_owned()
                }
            })
            .collect();
        out.push_str(&line[..start]);
        out.push_str(&names.join(","));
        out.push_str(&line[end..]);
    }
    out
}

/// The files a `\bibliography{a,b}` of `text` names (comments ignored), as
/// written.
pub fn bibliographies(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let code = uncommented(line);
        for (i, _) in code.match_indices("\\bibliography{") {
            let rest = &code[i + "\\bibliography{".len()..];
            let Some(end) = rest.find('}') else { continue };
            out.extend(
                rest[..end]
                    .split(',')
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_includeonly_list_names_the_copy_too() {
        let root = "\\includeonly{sections/intro,sections/method}\n\\begin{document}\n\\include{sections/method}\n% \\includeonly{sections/method}\n\\end{document}\n";
        let out = redirect_include(root, "sections/method", ".biwrite/zh/sections/method").unwrap();
        assert!(
            out.starts_with("\\includeonly{sections/intro,.biwrite/zh/sections/method}\n"),
            "{out}"
        );
        assert!(
            out.contains("\\include{.biwrite/zh/sections/method}"),
            "{out}"
        );
        assert!(
            out.contains("% \\includeonly{sections/method}"),
            "a comment stays: {out}"
        );
    }

    #[test]
    fn bibliographies_are_read_as_written() {
        let text = "\\bibliographystyle{IEEEtran}\n% \\bibliography{old}\n\\bibliography{../refs, bib/more,IEEEabrv}\n";
        assert_eq!(bibliographies(text), ["../refs", "bib/more", "IEEEabrv"]);
        assert!(bibliographies("\\addbibresource{refs.bib}").is_empty());
    }

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
