//! Lines an inserted figure added before `\begin{document}`, which a revert
//! may take out again: the statements they hold, and where each stands.

use crate::project::uncommented;

/// The statements of `lines`: a line, or lines that open braces the next
/// lines close (a `\tikzset{...}` block). Blank lines are left out.
pub fn statements(lines: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut depth: i64 = 0;
    for line in lines.lines() {
        if current.is_empty() && line.trim().is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
        depth += braces(&uncommented(line));
        if depth <= 0 {
            out.push(std::mem::take(&mut current));
            depth = 0;
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// Opening minus closing braces of `code`, escaped ones left out.
fn braces(code: &str) -> i64 {
    let mut n = 0;
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '{' => n += 1,
            '}' => n -= 1,
            _ => {}
        }
    }
    n
}

/// Where `statement` stands in the preamble of `text` (before
/// `\begin{document}`), as whole lines with the line break after it: only
/// when it stands there exactly once.
pub fn place_of(text: &str, statement: &str) -> Option<std::ops::Range<usize>> {
    let end = text.find("\\begin{document}")?;
    let preamble = &text[..end];
    let mut found = preamble.match_indices(statement).filter(|(i, _)| {
        let starts_line = *i == 0 || preamble[..*i].ends_with('\n');
        let after = &preamble[i + statement.len()..];
        starts_line && (after.starts_with('\n') || after.starts_with("\r\n"))
    });
    let (at, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let mut stop = at + statement.len();
    stop += if text[stop..].starts_with("\r\n") {
        2
    } else {
        1
    };
    Some(at..stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\\documentclass{article}\n\\usepackage{graphicx}\n\\usepackage{booktabs}\n\\usetikzlibrary{arrows.meta}\n\\definecolor{figBlue}{RGB}{31,78,121}\n\\begin{document}\nText.\n\\end{document}\n";

    #[test]
    fn statements_are_lines_or_blocks_whose_braces_close() {
        let lines = "\\usepackage{tikz}\n\n\\tikzset{\n  box/.style={draw},\n}\n\\definecolor{a}{RGB}{1,2,3}\n";
        assert_eq!(
            statements(lines),
            [
                "\\usepackage{tikz}",
                "\\tikzset{\n  box/.style={draw},\n}",
                "\\definecolor{a}{RGB}{1,2,3}"
            ]
        );
        // An escaped brace does not open a block.
        assert_eq!(
            statements("\\newcommand{\\lb}{\\{}\nx\n"),
            ["\\newcommand{\\lb}{\\{}", "x"]
        );
    }

    #[test]
    fn a_statement_is_found_once_as_whole_lines_of_the_preamble() {
        let r = place_of(DOC, "\\usepackage{booktabs}").unwrap();
        assert_eq!(&DOC[r.clone()], "\\usepackage{booktabs}\n");
        // Part of a line, or after \begin{document}, does not count.
        assert!(place_of(DOC, "\\usepackage{graph").is_none());
        assert!(place_of(DOC, "Text.").is_none());
        let twice = DOC.replace("\\usepackage{graphicx}", "\\usepackage{booktabs}");
        assert!(
            place_of(&twice, "\\usepackage{booktabs}").is_none(),
            "ambiguous"
        );
    }
}
