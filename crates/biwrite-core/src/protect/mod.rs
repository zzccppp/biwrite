//! Placeholder protection.
//!
//! Text a translator must reproduce exactly (inline math, citations,
//! cross-references, labels, URLs, inline comments, inline code) is replaced
//! by numbered placeholders `⟦n⟧` before a request and put back afterwards.
//! [`Protector::restore`] checks that every placeholder came back exactly as
//! often as it was sent, so a model can't silently drop or alter a `\cite`.
//!
//! Identical spans share a number (`$x$` twice → `⟦0⟧` twice). Placeholder-
//! shaped text already in the source is protected like any other span, so it
//! can't be confused with ours.

mod scan;

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

pub use scan::{Span, SpanKind, spans};

use crate::mode::Mode;

pub const OPEN: char = '⟦';
pub const CLOSE: char = '⟧';

/// Parse a placeholder at the start of `s`: `⟦n⟧`, tolerating spaces inside
/// the brackets. Returns its byte length and number.
pub fn placeholder_at(s: &str) -> Option<(usize, usize)> {
    let rest = s.strip_prefix(OPEN)?.trim_start_matches(' ');
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 6 {
        return None;
    }
    let n = rest[..digits].parse().ok()?;
    let after = rest[digits..].trim_start_matches(' ').strip_prefix(CLOSE)?;
    Some((s.len() - after.len(), n))
}

fn placeholder(n: usize) -> String {
    format!("{OPEN}{n}{CLOSE}")
}

#[derive(Clone, Debug)]
struct Protected {
    text: String,
    kind: SpanKind,
    /// Occurrences in the masked source: what the translation must contain.
    expected: usize,
}

/// Masks one request's texts and restores the translation.
#[derive(Clone, Debug)]
pub struct Protector {
    mode: Mode,
    spans: Vec<Protected>,
    by_text: HashMap<String, usize>,
}

impl Protector {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            spans: Vec::new(),
            by_text: HashMap::new(),
        }
    }

    /// Number of distinct protected spans.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Mask the text to translate. Its placeholders are what
    /// [`restore`](Self::restore) expects back.
    pub fn mask(&mut self, text: &str) -> String {
        self.mask_with(text, true)
    }

    /// Mask auxiliary text (revise mode's previous source) with the same
    /// numbering, without expecting its spans in the translation.
    pub fn mask_context(&mut self, text: &str) -> String {
        self.mask_with(text, false)
    }

    fn mask_with(&mut self, text: &str, expect: bool) -> String {
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for span in spans(text, self.mode) {
            let body = &text[span.range.clone()];
            let n = match self.by_text.get(body) {
                Some(&n) => n,
                None => {
                    self.spans.push(Protected {
                        text: body.to_owned(),
                        kind: span.kind,
                        expected: 0,
                    });
                    self.by_text.insert(body.to_owned(), self.spans.len() - 1);
                    self.spans.len() - 1
                }
            };
            if expect {
                self.spans[n].expected += 1;
            }
            out.push_str(&text[pos..span.range.start]);
            out.push_str(&placeholder(n));
            pos = span.range.end;
        }
        out.push_str(&text[pos..]);
        out
    }

    /// Mask the protected spans of `text` that are already known (by text),
    /// leaving others as they are. Used for revise mode's previous
    /// translation, which contains the restored spans rather than
    /// placeholders. Spans are found by the same scanner as [`mask`](Self::mask),
    /// so escapes like `\%` are never mistaken for a comment.
    pub fn mask_known(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        for span in spans(text, self.mode) {
            if let Some(&n) = self.by_text.get(&text[span.range.clone()]) {
                out.push_str(&text[pos..span.range.start]);
                out.push_str(&placeholder(n));
                pos = span.range.end;
            }
        }
        out.push_str(&text[pos..]);
        out
    }

    /// Put the protected texts back and check that each placeholder appears
    /// exactly as often as in the source and that none is unknown.
    ///
    /// In LaTeX, a `%` the model wrote itself always means a percent sign
    /// (the source's comments come back only through placeholders), so it is
    /// escaped as `\%` rather than left to comment out the rest of the line.
    pub fn restore(&self, translation: &str) -> Result<String, PlaceholderError> {
        if self.spans.is_empty() {
            return Ok(self.model_text(translation).into_owned());
        }
        let (out, counts, unknown) = self.substitute(translation);
        let mismatched: Vec<Mismatch> = self
            .spans
            .iter()
            .zip(counts)
            .enumerate()
            .filter(|(_, (span, found))| span.expected != *found)
            .map(|(index, (span, found))| Mismatch {
                index,
                text: span.text.clone(),
                expected: span.expected,
                found,
            })
            .collect();
        if mismatched.is_empty() && unknown.is_empty() {
            Ok(out)
        } else {
            Err(PlaceholderError {
                mismatched,
                unknown,
            })
        }
    }

    /// Put the protected texts back without failing on count mismatches:
    /// the writing assistant may legitimately drop or repeat a citation when
    /// asked to. Returns the text and what differs from the source.
    /// Placeholders that were never sent cannot be restored, so they are an
    /// error.
    pub fn restore_lenient(&self, text: &str) -> Result<(String, RestoreReport), PlaceholderError> {
        let (out, counts, unknown) = self.substitute(text);
        if !unknown.is_empty() {
            return Err(PlaceholderError {
                mismatched: Vec::new(),
                unknown,
            });
        }
        let mut report = RestoreReport::default();
        for (span, found) in self.spans.iter().zip(counts) {
            if found < span.expected {
                report.missing.push(span.text.clone());
            } else if found > span.expected {
                report.added.push(span.text.clone());
            }
        }
        Ok((out, report))
    }

    /// Restore streamed output for display: complete placeholders are
    /// replaced, a trailing incomplete one is hidden, nothing is validated.
    pub fn restore_partial(&self, partial: &str) -> String {
        if self.spans.is_empty() {
            return self.model_text(partial).into_owned();
        }
        let partial = match partial.rfind(OPEN) {
            Some(p) if is_incomplete(&partial[p + OPEN.len_utf8()..]) => &partial[..p],
            _ => partial,
        };
        self.substitute(partial).0
    }

    /// Replace known placeholders; returns the text, per-span counts and
    /// unknown placeholder numbers.
    fn substitute(&self, text: &str) -> (String, Vec<usize>, Vec<usize>) {
        let mut out = String::with_capacity(text.len() * 2);
        let mut counts = vec![0; self.spans.len()];
        let mut unknown = Vec::new();
        let mut rest = text;
        while let Some(pos) = rest.find(OPEN) {
            out.push_str(&self.model_text(&rest[..pos]));
            let tail = &rest[pos..];
            let Some((len, n)) = placeholder_at(tail) else {
                out.push(OPEN);
                rest = &tail[OPEN.len_utf8()..];
                continue;
            };
            rest = &tail[len..];
            let Some(span) = self.spans.get(n) else {
                unknown.push(n);
                out.push_str(&tail[..len]);
                continue;
            };
            counts[n] += 1;
            out.push_str(&span.text);
            if span.kind == SpanKind::Comment {
                // A comment runs to the end of the line: anything the model
                // put after it moves to the next line instead of vanishing.
                let after = rest.trim_start_matches([' ', '\t']);
                if !after.is_empty() && !after.starts_with(['\n', '\r']) {
                    out.push('\n');
                    rest = after;
                }
            }
        }
        out.push_str(&self.model_text(rest));
        (out, counts, unknown)
    }

    /// Text the model wrote (outside placeholders), made safe for the mode.
    fn model_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.mode == Mode::Latex && text.contains('%') {
            Cow::Owned(escape_percent(text))
        } else {
            Cow::Borrowed(text)
        }
    }
}

/// `%` → `\%` unless already escaped.
fn escape_percent(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '%' => out.push_str("\\%"),
            _ => out.push(c),
        }
    }
    out
}

/// Only spaces and digits after `⟦`: a placeholder still being streamed.
fn is_incomplete(after_open: &str) -> bool {
    after_open.chars().all(|c| c == ' ' || c.is_ascii_digit())
}

/// Protected texts that a lenient restore found fewer or more times than
/// they were sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    pub missing: Vec<String>,
    pub added: Vec<String>,
}

impl RestoreReport {
    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.added.is_empty()
    }
}

/// A placeholder that came back a different number of times than it was sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub index: usize,
    /// The protected text.
    pub text: String,
    pub expected: usize,
    pub found: usize,
}

/// The translation lost, duplicated or invented placeholders.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct PlaceholderError {
    pub mismatched: Vec<Mismatch>,
    /// Placeholder numbers that were never sent.
    pub unknown: Vec<usize>,
}

impl fmt::Display for PlaceholderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the model changed protected text: ")?;
        let mut parts: Vec<String> = self
            .mismatched
            .iter()
            .map(|m| {
                let what = format!("{} `{}`", placeholder(m.index), shorten(&m.text));
                match (m.expected, m.found) {
                    (_, 0) => format!("missing {what}"),
                    (0, _) => format!("unexpected {what}"),
                    (e, f) => format!("{what} appears {f}× instead of {e}×"),
                }
            })
            .collect();
        parts.extend(
            self.unknown
                .iter()
                .map(|n| format!("unknown {}", placeholder(*n))),
        );
        f.write_str(&parts.join("; "))
    }
}

fn shorten(text: &str) -> String {
    const MAX: usize = 40;
    if text.chars().count() <= MAX {
        text.to_owned()
    } else {
        let head: String = text.chars().take(MAX - 1).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests;
