//! Finding protected spans: constructs a translator must copy exactly.

use std::ops::Range;

use super::placeholder_at;
use crate::mode::Mode;

/// What a protected span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpanKind {
    /// `$…$`, `\(…\)`, `$$…$$`, `\[…\]`.
    Math,
    /// `\cite`, `\ref`, `\label`, `\url`, `\href{url}`, … with their arguments.
    Command,
    /// LaTeX `%` comment, up to (not including) the end of the line.
    Comment,
    /// Markdown inline code, LaTeX `\verb`.
    Code,
    /// Markdown autolink `<https://…>` or link destination `(url)`.
    Link,
    /// Placeholder-shaped text that was already in the source.
    Literal,
}

/// A protected byte range of the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: SpanKind,
}

/// Commands protected together with their arguments, and how many required
/// `{…}` arguments belong to the span. `\href` keeps only its URL protected;
/// the link text after it is translated.
fn required_args(name: &str) -> Option<usize> {
    match name {
        "cite" | "citep" | "citet" | "citealp" | "citealt" | "citeauthor" | "citeyear"
        | "citeyearpar" | "citenum" | "Cite" | "Citep" | "Citet" | "Citealp" | "Citealt"
        | "Citeauthor" | "parencite" | "Parencite" | "textcite" | "Textcite" | "autocite"
        | "Autocite" | "footcite" | "smartcite" | "supercite" | "nocite" | "ref" | "eqref"
        | "autoref" | "Autoref" | "cref" | "Cref" | "cpageref" | "Cpageref" | "pageref"
        | "nameref" | "vref" | "Vref" | "label" | "url" | "href" => Some(1),
        "crefrange" | "Crefrange" | "cpagerefrange" => Some(2),
        _ => None,
    }
}

/// Protected spans of `text`, ordered and disjoint. Plain text protects
/// nothing except placeholder-shaped text already present.
pub fn spans(text: &str, mode: Mode) -> Vec<Span> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let step = match mode {
            Mode::Latex => latex_at(text, i),
            Mode::Markdown => markdown_at(text, i),
            Mode::Plain => Step::Skip(i + 1),
        };
        let step = match step {
            Step::Skip(_) if text.as_bytes()[i] == 0xE2 => literal_at(text, i).unwrap_or(step),
            _ => step,
        };
        match step {
            Step::Span(end, kind) => {
                out.push(Span {
                    range: i..end,
                    kind,
                });
                i = end;
            }
            Step::Skip(next) => i = next,
        }
    }
    out
}

/// Outcome of looking at position `i`.
#[derive(Clone, Copy)]
enum Step {
    /// A protected span `i..end`.
    Span(usize, SpanKind),
    /// Nothing protected starts here; continue at this byte. Scanning is
    /// bytewise: spans only start at ASCII delimiters or a placeholder.
    Skip(usize),
}

fn literal_at(text: &str, i: usize) -> Option<Step> {
    let (len, _) = placeholder_at(&text[i..])?;
    Some(Step::Span(i + len, SpanKind::Literal))
}

fn latex_at(text: &str, i: usize) -> Step {
    let b = text.as_bytes();
    match b[i] {
        b'\\' => latex_backslash(text, i),
        b'$' if b.get(i + 1) == Some(&b'$') => match find_seq(b, i + 2, b"$$") {
            Some(end) => Step::Span(end, SpanKind::Math),
            None => Step::Skip(i + 2),
        },
        b'$' => match find_seq(b, i + 1, b"$") {
            Some(end) => Step::Span(end, SpanKind::Math),
            None => Step::Skip(i + 1),
        },
        b'%' => Step::Span(line_end(b, i), SpanKind::Comment),
        _ => Step::Skip(i + 1),
    }
}

fn latex_backslash(text: &str, i: usize) -> Step {
    let b = text.as_bytes();
    match b.get(i + 1) {
        None => Step::Skip(i + 1),
        Some(b'(') => math_until(b, i, b"\\)"),
        Some(b'[') => math_until(b, i, b"\\]"),
        Some(c) if c.is_ascii_alphabetic() => {
            let name_end = i
                + 1
                + b[i + 1..]
                    .iter()
                    .take_while(|c| c.is_ascii_alphabetic())
                    .count();
            let name = &text[i + 1..name_end];
            let span = if name == "verb" {
                verb_end(b, name_end).map(|end| Step::Span(end, SpanKind::Code))
            } else {
                required_args(name)
                    .and_then(|n| command_end(b, name_end, n))
                    .map(|end| Step::Span(end, SpanKind::Command))
            };
            span.unwrap_or(Step::Skip(name_end))
        }
        // An escaped character (`\$`, `\%`, `\\`, `\{`) is text.
        Some(c) if c.is_ascii() => Step::Skip(i + 2),
        Some(_) => Step::Skip(i + 1),
    }
}

fn math_until(b: &[u8], i: usize, close: &[u8]) -> Step {
    match find_seq(b, i + 2, close) {
        Some(end) => Step::Span(end, SpanKind::Math),
        None => Step::Skip(i + 2),
    }
}

/// End of `[*][opt][opt]{arg}…` after a command name, or `None` if the
/// required arguments are missing.
fn command_end(b: &[u8], mut pos: usize, required: usize) -> Option<usize> {
    if b.get(pos) == Some(&b'*') {
        pos += 1;
    }
    for _ in 0..2 {
        let at = skip_spaces(b, pos);
        if b.get(at) != Some(&b'[') {
            break;
        }
        pos = group_end(b, at, b'[', b']')?;
    }
    for _ in 0..required {
        let at = skip_spaces(b, pos);
        if b.get(at) != Some(&b'{') {
            return None;
        }
        pos = group_end(b, at, b'{', b'}')?;
    }
    Some(pos)
}

/// `\verb|…|`: any non-letter delimiter, closed on the same line.
fn verb_end(b: &[u8], mut pos: usize) -> Option<usize> {
    if b.get(pos) == Some(&b'*') {
        pos += 1;
    }
    let delim = *b.get(pos)?;
    if delim.is_ascii_alphabetic() || delim.is_ascii_whitespace() || !delim.is_ascii() {
        return None;
    }
    let close = b[pos + 1..]
        .iter()
        .take_while(|&&c| c != b'\n')
        .position(|&c| c == delim)?;
    Some(pos + 1 + close + 1)
}

fn markdown_at(text: &str, i: usize) -> Step {
    let b = text.as_bytes();
    match b[i] {
        b'\\' => match b.get(i + 1) {
            Some(c) if c.is_ascii_punctuation() => Step::Skip(i + 2),
            _ => Step::Skip(i + 1),
        },
        b'`' => {
            let run = b[i..].iter().take_while(|&&c| c == b'`').count();
            match code_span_end(b, i + run, run) {
                Some(end) => Step::Span(end, SpanKind::Code),
                None => Step::Skip(i + run),
            }
        }
        b'$' if b.get(i + 1) == Some(&b'$') => match find_seq(b, i + 2, b"$$") {
            Some(end) => Step::Span(end, SpanKind::Math),
            None => Step::Skip(i + 2),
        },
        b'$' => match pandoc_math_end(b, i) {
            Some(end) => Step::Span(end, SpanKind::Math),
            None => Step::Skip(i + 1),
        },
        b'<' => match autolink_end(b, i) {
            Some(end) => Step::Span(end, SpanKind::Link),
            None => Step::Skip(i + 1),
        },
        b'(' if i > 0 && b[i - 1] == b']' => match group_end(b, i, b'(', b')') {
            Some(end) if !b[i..end].contains(&b'\n') => Step::Span(end, SpanKind::Link),
            _ => Step::Skip(i + 1),
        },
        _ => Step::Skip(i + 1),
    }
}

/// A backtick run of exactly `run` characters closes the code span.
fn code_span_end(b: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if blank_line_at(b, k) {
            return None;
        }
        if b[k] == b'`' {
            let n = b[k..].iter().take_while(|&&c| c == b'`').count();
            if n == run {
                return Some(k + n);
            }
            k += n;
        } else {
            k += 1;
        }
    }
    None
}

/// Pandoc's rule for `$…$`: the opening `$` is followed by a non-space, the
/// closing one preceded by a non-space and not followed by a digit (so
/// "$5 and $10" is not math).
fn pandoc_math_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1).is_none_or(|c| c.is_ascii_whitespace()) {
        return None;
    }
    let mut k = i + 1;
    while k < b.len() {
        match b[k] {
            b'\\' => k += 2,
            b'$' if !b[k - 1].is_ascii_whitespace()
                && !b.get(k + 1).is_some_and(u8::is_ascii_digit) =>
            {
                return Some(k + 1);
            }
            _ if blank_line_at(b, k) => return None,
            _ => k += 1,
        }
    }
    None
}

/// CommonMark URI autolink: `<scheme:…>` with a 2–32 character scheme and
/// no spaces or angle brackets.
fn autolink_end(b: &[u8], i: usize) -> Option<usize> {
    let rest = &b[i + 1..];
    let scheme = rest
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'))
        .count();
    if !(2..=32).contains(&scheme)
        || !rest[0].is_ascii_alphabetic()
        || rest.get(scheme) != Some(&b':')
    {
        return None;
    }
    let body = &rest[scheme + 1..];
    let close = body
        .iter()
        .position(|&c| c == b'>' || c == b'<' || c.is_ascii_whitespace() || c.is_ascii_control())?;
    (body[close] == b'>').then_some(i + 1 + scheme + 1 + close + 1)
}

/// End (exclusive) of the first unescaped `close` at or after `from`. Math
/// never spans a blank line, so an unmatched delimiter stays text.
fn find_seq(b: &[u8], from: usize, close: &[u8]) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if b[k..].starts_with(close) {
            return Some(k + close.len());
        }
        if b[k] == b'\\' {
            k += 2;
            continue;
        }
        if blank_line_at(b, k) {
            return None;
        }
        k += 1;
    }
    None
}

/// End (exclusive) of the group opened at `open`, honoring nesting and
/// backslash escapes. Braces inside `[…]` are balanced too.
fn group_end(b: &[u8], open: usize, opener: u8, closer: u8) -> Option<usize> {
    let (mut depth, mut braces) = (0usize, 0usize);
    let mut k = open;
    while k < b.len() {
        match b[k] {
            b'\\' => k += 1,
            b'{' if opener != b'{' => braces += 1,
            b'}' if opener != b'{' => braces = braces.saturating_sub(1),
            c if c == opener && braces == 0 => depth += 1,
            c if c == closer && braces == 0 => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(k + 1);
                }
            }
            _ if blank_line_at(b, k) => return None,
            _ => {}
        }
        k += 1;
    }
    None
}

fn line_end(b: &[u8], from: usize) -> usize {
    b[from..]
        .iter()
        .position(|&c| c == b'\n')
        .map_or(b.len(), |p| from + p)
}

fn skip_spaces(b: &[u8], from: usize) -> usize {
    from + b[from..]
        .iter()
        .take_while(|&&c| c == b' ' || c == b'\t')
        .count()
}

/// A newline followed by a whitespace-only line.
fn blank_line_at(b: &[u8], k: usize) -> bool {
    b[k] == b'\n'
        && b[k + 1..]
            .iter()
            .find(|&&c| c != b' ' && c != b'\t' && c != b'\r')
            .is_none_or(|&c| c == b'\n')
}
