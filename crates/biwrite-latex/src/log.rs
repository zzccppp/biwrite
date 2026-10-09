//! Reading a TeX log for the problems worth showing: errors (with the
//! file and line when `-file-line-error` gives them), LaTeX and package
//! warnings (undefined citations and references first among them), and
//! overfull boxes. Underfull boxes are left out: papers have hundreds.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    /// An overfull box: text runs into the margin.
    Box,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Issue {
    pub severity: Severity,
    /// Source file as the log names it (relative to the root's folder).
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// Most issues kept from one log.
const MAX_ISSUES: usize = 200;

/// `./sections/a.tex:12: message` (file-line-error format).
fn file_line_error(line: &str) -> Option<(String, u32, String)> {
    let tex = line.find(".tex:")?;
    let file = &line[..tex + 4];
    let rest = &line[tex + 5..];
    let colon = rest.find(':')?;
    let n: u32 = rest[..colon].parse().ok()?;
    let message = rest[colon + 1..].trim().to_owned();
    (!file.contains(' ') || file.starts_with('.') || file.starts_with('/'))
        .then(|| (file.trim_start_matches("./").to_owned(), n, message))
}

/// `… on input line 23.` at the end of a warning.
fn input_line(message: &str) -> Option<u32> {
    let at = message.rfind("on input line ")?;
    message[at + "on input line ".len()..]
        .trim_end_matches('.')
        .trim()
        .parse()
        .ok()
}

/// `… at lines 33--34` in a box warning.
fn box_lines(message: &str) -> Option<u32> {
    let at = message.find("at lines ")?;
    message[at + "at lines ".len()..]
        .split("--")
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Continuation lines of a multi-line warning: indented, or starting with
/// the package name in parentheses.
fn continues(line: &str) -> bool {
    !line.is_empty() && (line.starts_with(' ') || line.starts_with('('))
}

/// The issues in a log, errors first, without duplicates.
pub fn parse(log: &str) -> Vec<Issue> {
    let lines: Vec<&str> = log.lines().collect();
    let mut out: Vec<Issue> = Vec::new();
    let push = |issue: Issue, out: &mut Vec<Issue>| {
        if out.len() < MAX_ISSUES && !out.contains(&issue) {
            out.push(issue);
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if let Some((file, n, message)) = file_line_error(line) {
            push(
                Issue {
                    severity: Severity::Error,
                    file: Some(file),
                    line: Some(n),
                    message,
                },
                &mut out,
            );
        } else if let Some(message) = line.strip_prefix("! ") {
            // Classic format: the line number follows as `l.123 …`.
            let n = lines.iter().skip(i + 1).take(12).find_map(|l| {
                l.strip_prefix("l.")?
                    .split(|c: char| !c.is_ascii_digit())
                    .next()?
                    .parse()
                    .ok()
            });
            push(
                Issue {
                    severity: Severity::Error,
                    file: None,
                    line: n,
                    message: message.trim().to_owned(),
                },
                &mut out,
            );
        } else if line.starts_with("LaTeX Warning:")
            || line.starts_with("Package ") && line.contains(" Warning:")
            || line.starts_with("Class ") && line.contains(" Warning:")
            || line.starts_with("LaTeX Font Warning:")
        {
            let mut message = line.to_owned();
            while i + 1 < lines.len() && continues(lines[i + 1]) {
                i += 1;
                let more = lines[i].trim_start();
                // Drop the "(natbib)   " prefix of package continuation lines.
                let more = match more.strip_prefix('(') {
                    Some(rest) => rest.split_once(')').map_or(more, |(_, r)| r.trim_start()),
                    None => more,
                };
                message.push(' ');
                message.push_str(more);
            }
            let message = message
                .split_once("Warning:")
                .map_or(message.as_str(), |(_, m)| m)
                .trim()
                .to_owned();
            push(
                Issue {
                    severity: Severity::Warning,
                    file: None,
                    line: input_line(&message),
                    message,
                },
                &mut out,
            );
        } else if line.starts_with("Overfull \\hbox") || line.starts_with("Overfull \\vbox") {
            push(
                Issue {
                    severity: Severity::Box,
                    file: None,
                    line: box_lines(line),
                    message: line.trim().to_owned(),
                },
                &mut out,
            );
        }
        i += 1;
    }
    out.sort_by_key(|issue| issue.severity);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r"This is pdfTeX, Version 3.141592653-2.6-1.40.25 (TeX Live 2023)
(./main.tex
LaTeX2e <2022-11-01> patch level 1
./main.tex:12: Undefined control sequence.
l.12 \foo
         {bar}
! LaTeX Error: File `missing.sty' not found.

Type X to quit or <RETURN> to proceed,
l.5 \usepackage
               {missing}^^M
LaTeX Warning: Citation `smith2020' on page 1 undefined on input line 23.

Package natbib Warning: Citation `jones2021' on page 2 undefined on input line
(natbib)                41.

Overfull \hbox (3.97266pt too wide) in paragraph at lines 33--34
[]\OT1/cmr/m/n/10 Some text|
Underfull \hbox (badness 1009) in paragraph at lines 37--42
LaTeX Warning: There were undefined references.
LaTeX Warning: Citation `smith2020' on page 1 undefined on input line 23.
";

    #[test]
    fn errors_warnings_and_boxes() {
        let issues = parse(LOG);
        let summary: Vec<(Severity, Option<&str>, Option<u32>)> = issues
            .iter()
            .map(|i| (i.severity, i.file.as_deref(), i.line))
            .collect();
        assert_eq!(
            summary,
            [
                (Severity::Error, Some("main.tex"), Some(12)),
                (Severity::Error, None, Some(5)),
                (Severity::Warning, None, Some(23)),
                (Severity::Warning, None, Some(41)),
                (Severity::Warning, None, None),
                (Severity::Box, None, Some(33)),
            ]
        );
        assert_eq!(issues[0].message, "Undefined control sequence.");
        assert!(issues[1].message.contains("missing.sty"));
        assert_eq!(
            issues[3].message,
            "Citation `jones2021' on page 2 undefined on input line 41."
        );
    }

    #[test]
    fn file_line_errors_in_subfolders() {
        let issues = parse("./sections/intro.tex:7: Missing $ inserted.\n");
        assert_eq!(issues[0].file.as_deref(), Some("sections/intro.tex"));
        assert_eq!(issues[0].line, Some(7));
        assert!(parse("Some prose mentioning a.tex: not an error").is_empty());
    }
}
