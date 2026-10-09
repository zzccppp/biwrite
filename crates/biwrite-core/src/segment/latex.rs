//! LaTeX segmentation.
//!
//! - Preamble (up to and including `\begin{document}`) and everything from
//!   `\end{document}` on are skipped.
//! - `%` comment lines, display math (`\[..\]`, `$$..$$`, equation/align/...),
//!   tabular, verbatim/listing and algorithm environments are skipped.
//! - Figure/table floats are skipped except for their `\caption{..}`, which is
//!   its own translatable segment.
//! - Sectioning commands are headings; `\item` starts a new paragraph whose
//!   content excludes the marker; lines made only of structural commands
//!   (`\maketitle`, `\begin{abstract}`, `\label{..}`, ...) are skipped.

use std::ops::Range;

use super::{Builder, Line, Segment, SkipReason, lines};

/// Sectioning commands and the heading level they map to.
const SECTIONS: &[(&str, u8)] = &[
    ("part", 1),
    ("chapter", 1),
    ("section", 2),
    ("subsection", 3),
    ("subsubsection", 4),
    ("paragraph", 5),
    ("subparagraph", 6),
];

/// Environments skipped as a whole (matched without a trailing `*`).
const SKIP_ENVS: &[(&str, SkipReason)] = &[
    ("equation", SkipReason::Math),
    ("align", SkipReason::Math),
    ("alignat", SkipReason::Math),
    ("gather", SkipReason::Math),
    ("multline", SkipReason::Math),
    ("flalign", SkipReason::Math),
    ("eqnarray", SkipReason::Math),
    ("displaymath", SkipReason::Math),
    ("math", SkipReason::Math),
    ("subequations", SkipReason::Math),
    ("tabular", SkipReason::Table),
    ("tabularx", SkipReason::Table),
    ("tabulary", SkipReason::Table),
    ("longtable", SkipReason::Table),
    ("array", SkipReason::Table),
    ("algorithm", SkipReason::Code),
    ("algorithmic", SkipReason::Code),
    ("algorithm2e", SkipReason::Code),
    ("verbatim", SkipReason::Code),
    ("Verbatim", SkipReason::Code),
    ("lstlisting", SkipReason::Code),
    ("minted", SkipReason::Code),
    ("comment", SkipReason::Comment),
    ("tikzpicture", SkipReason::Float),
    ("thebibliography", SkipReason::Markup),
];

/// Floats: skipped except for captions.
const FLOAT_ENVS: &[&str] = &[
    "figure",
    "table",
    "wrapfigure",
    "wraptable",
    "sidewaysfigure",
    "sidewaystable",
    "subfigure",
    "subtable",
];

/// Structural commands that take arguments (consumed when classifying a line).
const MARKUP_WITH_ARGS: &[&str] = &[
    "begin",
    "end",
    "label",
    "includegraphics",
    "input",
    "include",
    "vspace",
    "hspace",
    "vskip",
    "hskip",
    "setlength",
    "addtolength",
    "setcounter",
    "renewcommand",
    "newcommand",
    "providecommand",
    "thispagestyle",
    "pagestyle",
    "bibliographystyle",
    "bibliography",
    "printbibliography",
    "addbibresource",
    "nocite",
    "graphicspath",
    "cline",
    "pagebreak",
];

/// Argument-less declarations. Followed by `{` they wrap prose
/// (`\small{All numbers are averaged…}`), so such a line is not markup.
const MARKUP_DECLARATIONS: &[&str] = &[
    "maketitle",
    "tableofcontents",
    "listoffigures",
    "listoftables",
    "appendix",
    "newpage",
    "clearpage",
    "cleardoublepage",
    "centering",
    "raggedright",
    "raggedleft",
    "smallskip",
    "medskip",
    "bigskip",
    "vfill",
    "hfill",
    "noindent",
    "indent",
    "par",
    "small",
    "footnotesize",
    "scriptsize",
    "tiny",
    "normalsize",
    "large",
    "Large",
    "LARGE",
    "huge",
    "Huge",
    "hline",
    "toprule",
    "midrule",
    "bottomrule",
    "onecolumn",
    "twocolumn",
    "frontmatter",
    "mainmatter",
    "backmatter",
    "balance",
    "FloatBarrier",
];

/// Caption commands inside floats.
const CAPTIONS: &[&str] = &["\\caption", "\\subcaption"];

pub(super) fn segment(text: &str) -> Vec<Segment> {
    let all: Vec<Line<'_>> = lines(text).collect();
    let mut b = Builder::default();
    let mut i = 0;

    if let Some(begin) = all
        .iter()
        .position(|l| starts_cmd(l.text, "\\begin{document}"))
    {
        b.skipped(SkipReason::Preamble, all[0].start..all[begin].end);
        i = begin + 1;
    }

    while i < all.len() {
        let line = &all[i];
        let t = line.text.trim_start();
        if line.is_blank() {
            b.flush();
        } else if starts_cmd(t, "\\end{document}") {
            let last = all.iter().rposition(|l| !l.is_blank()).unwrap_or(i);
            b.skipped(SkipReason::Markup, line.start..all[last].end);
            break;
        } else if t.starts_with('%') {
            b.skip_line(SkipReason::Comment, line.range());
        } else if let Some((level, content)) = heading(line) {
            b.heading(level, line.range(), content);
        } else if let Some(env) = begin_env(t) {
            let end = env_end(&all, i, env);
            match env_kind(env) {
                EnvKind::Skip(reason) => b.skipped(reason, line.start..all[end].end),
                EnvKind::Float => float(text, &all[i..=end], &mut b),
                EnvKind::Other if is_markup_line(t) => {
                    b.skip_line(SkipReason::Markup, line.range())
                }
                EnvKind::Other => b.line(line),
            }
            if !matches!(env_kind(env), EnvKind::Other) {
                i = end + 1;
                continue;
            }
        } else if let Some(end) = display_math(&all, i) {
            b.skipped(SkipReason::Math, line.start..all[end].end);
            i = end + 1;
            continue;
        } else if let Some(content_start) = item(line) {
            b.item(line, content_start);
        } else if is_markup_line(t) {
            b.skip_line(SkipReason::Markup, line.range());
        } else {
            b.line(line);
        }
        i += 1;
    }
    b.finish()
}

/// `text` starts with the command `cmd` (not a longer command name).
fn starts_cmd(text: &str, cmd: &str) -> bool {
    text.trim_start().strip_prefix(cmd).is_some_and(|rest| {
        !(cmd.ends_with(|c: char| c.is_ascii_alphabetic())
            && rest.starts_with(|c: char| c.is_ascii_alphabetic()))
    })
}

enum EnvKind {
    Skip(SkipReason),
    Float,
    Other,
}

fn env_kind(env: &str) -> EnvKind {
    let base = env.trim_end_matches('*');
    if let Some((_, reason)) = SKIP_ENVS.iter().find(|(name, _)| *name == base) {
        EnvKind::Skip(*reason)
    } else if FLOAT_ENVS.contains(&base) {
        EnvKind::Float
    } else {
        EnvKind::Other
    }
}

/// Name of the environment begun at the start of `t` (`\begin{name}`).
fn begin_env(t: &str) -> Option<&str> {
    let rest = t.strip_prefix("\\begin{")?;
    rest.find('}').map(|end| &rest[..end])
}

/// Index of the line that closes the environment `env` begun on line `i`
/// (nesting-aware); the last line if it is never closed.
fn env_end(all: &[Line<'_>], i: usize, env: &str) -> usize {
    let (open, close) = (format!("\\begin{{{env}}}"), format!("\\end{{{env}}}"));
    let mut depth = 0isize;
    for (j, line) in all.iter().enumerate().skip(i) {
        let code = strip_comment(line.text);
        depth += code.matches(&open).count() as isize;
        depth -= code.matches(&close).count() as isize;
        if depth <= 0 {
            return j;
        }
    }
    all.len() - 1
}

/// The line without its trailing `%` comment (an escaped `\%` is kept).
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'%' => return &line[..i],
            _ => {}
        }
        i += 1;
    }
    line
}

/// `\[ .. \]` or `$$ .. $$` starting a line: index of the closing line.
fn display_math(all: &[Line<'_>], i: usize) -> Option<usize> {
    let t = all[i].text.trim_start();
    let (opener, closer) = if t.starts_with("\\[") {
        ("\\[", "\\]")
    } else if t.starts_with("$$") {
        ("$$", "$$")
    } else {
        return None;
    };
    if t[opener.len()..].contains(closer) {
        return Some(i);
    }
    let end = all[i + 1..]
        .iter()
        .position(|l| l.text.contains(closer))
        .map_or(all.len() - 1, |p| i + 1 + p);
    Some(end)
}

/// Lines of a float: captions become caption segments, the rest is skipped.
fn float(text: &str, lines: &[Line<'_>], b: &mut Builder) {
    let end = lines[lines.len() - 1].end;
    let mut cursor = lines[0].start;
    let mut search = cursor;
    while let Some((at, after)) = next_caption(text, search, end) {
        search = after;
        let first = line_index(lines, at);
        let before = &text[lines[first].start..at];
        if strip_comment(before).len() != before.len() {
            continue; // commented out
        }
        let Some(content) = caption_content(&text[..end], after) else {
            continue;
        };
        let last = line_index(lines, content.end);
        skip_nonblank(b, lines, cursor, lines[first].start);
        b.caption(lines[first].start..lines[last].end, content);
        cursor = lines[last].end;
        search = cursor;
    }
    skip_nonblank(b, lines, cursor, end);
}

/// Earliest caption command in `text[from..end]`: (start, end of its name).
fn next_caption(text: &str, from: usize, end: usize) -> Option<(usize, usize)> {
    CAPTIONS
        .iter()
        .filter_map(|cmd| {
            text[from..end]
                .find(cmd)
                .map(|p| (from + p, from + p + cmd.len()))
        })
        .min()
}

/// After `\caption` at `pos`: optional `*`, `[short]`, then `{content}`.
fn caption_content(text: &str, mut pos: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    if bytes.get(pos) == Some(&b'*') {
        pos += 1;
    }
    pos = skip_ws(text, pos);
    if bytes.get(pos) == Some(&b'[') {
        pos = matching(text, pos, b'[', b']')? + 1;
        pos = skip_ws(text, pos);
    }
    if bytes.get(pos) != Some(&b'{') {
        return None;
    }
    let close = matching(text, pos, b'{', b'}')?;
    Some(pos + 1..close)
}

fn line_index(lines: &[Line<'_>], pos: usize) -> usize {
    lines
        .iter()
        .position(|l| pos <= l.end)
        .unwrap_or(lines.len() - 1)
}

/// Emit the non-blank lines in `[from, to)` as one float segment.
fn skip_nonblank(b: &mut Builder, lines: &[Line<'_>], from: usize, to: usize) {
    let inside = |l: &&Line<'_>| l.start >= from && l.end <= to && !l.is_blank();
    if let (Some(first), Some(last)) = (lines.iter().find(inside), lines.iter().rfind(inside)) {
        b.skipped(SkipReason::Float, first.start..last.end);
    }
}

/// `\item[label] text` → absolute start of `text` (`None` if it begins on a
/// later line).
fn item(line: &Line<'_>) -> Option<Option<usize>> {
    let indent = line.text.len() - line.text.trim_start().len();
    let rest = line.text[indent..].strip_prefix("\\item")?;
    if rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut pos = indent + "\\item".len();
    pos = skip_ws(line.text, pos);
    if line.text.as_bytes().get(pos) == Some(&b'[') {
        pos = matching(line.text, pos, b'[', b']').map_or(line.text.len(), |p| p + 1);
        pos = skip_ws(line.text, pos);
    }
    Some((pos < line.text.len()).then(|| line.abs(pos)))
}

/// A line consisting only of structural commands (and an optional comment).
fn is_markup_line(t: &str) -> bool {
    let bytes = t.as_bytes();
    let mut i = 0;
    let mut any = false;
    loop {
        i = skip_ws(t, i);
        match bytes.get(i) {
            None | Some(b'%') => return any,
            Some(b'\\') => {}
            Some(_) => return false,
        }
        let name_end = i
            + 1
            + t[i + 1..]
                .bytes()
                .take_while(|c| c.is_ascii_alphabetic() || *c == b'@')
                .count();
        let name = &t[i + 1..name_end];
        i = name_end;
        if MARKUP_DECLARATIONS.contains(&name) {
            if bytes.get(skip_ws(t, i)) == Some(&b'{') {
                return false;
            }
            any = true;
            continue;
        }
        if !MARKUP_WITH_ARGS.contains(&name) {
            return false;
        }
        if bytes.get(i) == Some(&b'*') {
            i += 1;
        }
        loop {
            let j = skip_ws(t, i);
            let Some(&open @ (b'[' | b'{')) = bytes.get(j) else {
                break;
            };
            let close = if open == b'[' { b']' } else { b'}' };
            match matching(t, j, open, close) {
                Some(end) => i = end + 1,
                None => return false,
            }
        }
        any = true;
    }
}

/// Detect a line that is only a sectioning command, e.g.
/// `\section*[short]{Long title}  \label{sec:x}`. Returns the level and the
/// byte range of the title. Run-in headings followed by body text on the same
/// line (common with `\paragraph{...} Text`) are left as paragraph text so no
/// source text is hidden from translation.
fn heading(line: &Line<'_>) -> Option<(u8, Range<usize>)> {
    let indent = line.text.len() - line.text.trim_start().len();
    let rest = line.text[indent..].strip_prefix('\\')?;
    let (name, level) = SECTIONS.iter().find(|(name, _)| {
        rest.strip_prefix(name)
            .is_some_and(|after| after.starts_with(['*', '[', '{', ' ', '\t']))
    })?;
    let mut pos = indent + 1 + name.len();
    let bytes = line.text.as_bytes();
    if bytes.get(pos) == Some(&b'*') {
        pos += 1;
    }
    pos = skip_ws(line.text, pos);
    if bytes.get(pos) == Some(&b'[') {
        pos = matching(line.text, pos, b'[', b']')? + 1;
        pos = skip_ws(line.text, pos);
    }
    if bytes.get(pos) != Some(&b'{') {
        return None;
    }
    let close = matching(line.text, pos, b'{', b'}')?;
    if !is_trailing_noise(&line.text[close + 1..]) {
        return None;
    }
    Some((*level, line.abs(pos + 1)..line.abs(close)))
}

fn skip_ws(text: &str, pos: usize) -> usize {
    pos + (text[pos..].len() - text[pos..].trim_start().len())
}

/// Index of the delimiter matching the opener at `open`, honoring nesting and
/// backslash escapes.
fn matching(text: &str, open: usize, opener: u8, closer: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            c if c == opener => depth += 1,
            c if c == closer => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Text allowed after a heading's closing brace: whitespace, `\label{..}`
/// commands, and a trailing `%` comment.
fn is_trailing_noise(mut rest: &str) -> bool {
    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('%') {
            return true;
        }
        let Some(after) = rest.strip_prefix("\\label{") else {
            return false;
        };
        match after.find('}') {
            Some(end) => rest = &after[end + 1..],
            None => return false,
        }
    }
}

#[cfg(test)]
#[path = "latex_tests.rs"]
mod tests;
