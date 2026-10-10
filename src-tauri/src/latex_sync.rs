//! Lines of the text TeX compiled and places in the editor's current text.
//!
//! A PDF shows the text as it was when the build started. The editor may
//! have changed since, and may show the other language. In the same
//! language, lines map through a line diff. Across languages, paragraphs
//! map by position (the k-th paragraph of one side is the k-th of the
//! other, as composing guarantees), then through the diff.

use std::ops::Range;
use std::time::Duration;

use biwrite_core::segment::segment;
use biwrite_core::{Mode, Segment};
use similar::{Algorithm, DiffTag, TextDiff};

/// What a build compiled for the open document.
#[derive(Clone, Debug)]
pub struct Basis {
    /// The open document's text as TeX read it (line for line).
    pub compiled: String,
    /// The editor's text when the build started.
    pub editor: String,
    /// `compiled` is in the editor's language.
    pub same_language: bool,
    pub mode: Mode,
}

/// A place in the editor's current text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A 1-based line (same language: the words under the click narrow it
    /// down to a sentence).
    Line(usize),
    /// The byte range of a paragraph's content (other language).
    Paragraph(Range<usize>),
}

/// Byte offset of the start of 0-based `line` (the end of the text past
/// the last line).
pub fn line_start(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 1)
        .map_or(text.len(), |(i, _)| i + 1)
}

/// 0-based line of byte offset `at`.
pub fn line_of(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
}

/// 1-based `line` of `old` to the corresponding 1-based line of `new`. A
/// line inside a changed block maps to the same distance into the new
/// block (clamped), a deleted line to where it was.
pub fn map_line(old: &str, new: &str, line: usize) -> usize {
    if old == new {
        return line.max(1);
    }
    let target = line.saturating_sub(1);
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .timeout(Duration::from_millis(250))
        .diff_lines(old, new);
    for op in diff.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        if !o.contains(&target) {
            continue;
        }
        let into = target - o.start;
        return 1 + match tag {
            DiffTag::Equal => n.start + into,
            _ => n.start + into.min(n.len().saturating_sub(1)),
        };
    }
    1 + line_of(new, new.len())
}

/// Translatable segments with text, in document order.
fn paragraphs(text: &str, mode: Mode) -> Vec<Segment> {
    segment(text, mode)
        .into_iter()
        .filter(|s| s.kind.is_translatable() && !s.content(text).trim().is_empty())
        .collect()
}

/// Index of the paragraph for the line that starts at `at` (see
/// [`index_at_line`]).
fn paragraph_index(paras: &[Segment], text: &str, at: usize) -> Option<usize> {
    let ranges: Vec<Range<usize>> = paras.iter().map(|s| s.range.clone()).collect();
    index_at_line(&ranges, text, at)
}

/// Lines that lead into what follows them: blank lines, comments,
/// `\begin{…}` with its options and arguments, `\label{…}`.
fn only_openers(lines: &str) -> bool {
    lines.lines().all(|line| {
        let line = line.trim();
        line.is_empty()
            || line.starts_with('%')
            || line.starts_with("\\begin{")
            || line.starts_with("\\label{")
    })
}

/// Which of `ranges` (in order) the line of `text` that starts at `at`
/// stands for. A line inside one, or one that starts on the line, is that
/// one. A line between them stands for the next one when only opening
/// lines lead to it, as `\begin{theorem}[Title]` does, whose title SyncTeX
/// places on that line though it is set with the theorem's first line.
/// Any other line between them, such as `\end{…}`, stands for the one
/// before it.
pub fn index_at_line(ranges: &[Range<usize>], text: &str, at: usize) -> Option<usize> {
    if ranges.is_empty() {
        return None;
    }
    let at = at.min(text.len());
    let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let before = ranges.iter().rposition(|r| r.start <= at);
    if let Some(i) = before.filter(|&i| at < ranges[i].end) {
        return Some(i);
    }
    let next = before.map_or(0, |i| i + 1);
    if let Some(r) = ranges.get(next) {
        if r.start <= line_end || text.get(at..r.start).is_some_and(only_openers) {
            return Some(next);
        }
    }
    Some(before.unwrap_or(0))
}

impl Basis {
    /// A compiled line (1-based) as a place in the editor's `current` text.
    pub fn to_editor(&self, line: usize, current: &str) -> Option<Place> {
        if self.same_language {
            return Some(Place::Line(map_line(&self.compiled, current, line)));
        }
        let compiled = paragraphs(&self.compiled, self.mode);
        let k = paragraph_index(
            &compiled,
            &self.compiled,
            line_start(&self.compiled, line.saturating_sub(1)),
        )?;
        let editor = paragraphs(&self.editor, self.mode);
        let then = editor.get(k)?;
        let then_line = line_of(&self.editor, then.content.start) + 1;
        let now_line = map_line(&self.editor, current, then_line);
        let now = paragraphs(current, self.mode);
        let i = paragraph_index(&now, current, line_start(current, now_line - 1))?;
        Some(Place::Paragraph(now[i].content.clone()))
    }

    /// A byte offset in the editor's `current` text as a compiled line
    /// (1-based).
    pub fn to_compiled(&self, current: &str, at: usize) -> Option<usize> {
        let now_line = line_of(current, at) + 1;
        if self.same_language {
            return Some(map_line(current, &self.compiled, now_line));
        }
        let then_line = map_line(current, &self.editor, now_line);
        let editor = paragraphs(&self.editor, self.mode);
        let k = paragraph_index(
            &editor,
            &self.editor,
            line_start(&self.editor, then_line - 1),
        )?;
        let compiled = paragraphs(&self.compiled, self.mode);
        compiled
            .get(k)
            .map(|s| line_of(&self.compiled, s.content.start) + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click on a theorem's title (SyncTeX names its `\\begin` line)
    /// is the theorem, a click on `\\end{theorem}` the theorem too, not the
    /// paragraph after it.
    #[test]
    fn a_line_between_paragraphs_stands_for_the_one_it_opens() {
        let text = "Intro paragraph.\n\n\\begin{theorem}[Title]\n\\label{t}\n\
                    Body of the theorem.\n\\end{theorem}\nAfter it.\n";
        let span = |s: &str| {
            let at = text.find(s).unwrap();
            at..at + s.len()
        };
        let ranges = [
            span("Intro paragraph."),
            span("Body of the theorem."),
            span("After it."),
        ];
        let at_line = |n| index_at_line(&ranges, text, line_start(text, n));
        assert_eq!(at_line(0), Some(0), "inside the first");
        assert_eq!(at_line(2), Some(1), "the begin line with the title");
        assert_eq!(at_line(3), Some(1), "the label line");
        assert_eq!(at_line(4), Some(1), "the body");
        assert_eq!(at_line(5), Some(1), "the end line");
        assert_eq!(at_line(6), Some(2), "the paragraph after");
        assert_eq!(index_at_line(&[], text, 0), None);
    }

    #[test]
    fn lines_and_offsets() {
        let text = "a\nbb\n\nccc";
        assert_eq!(line_start(text, 0), 0);
        assert_eq!(line_start(text, 1), 2);
        assert_eq!(line_start(text, 3), 6);
        assert_eq!(line_start(text, 9), text.len());
        assert_eq!(line_of(text, 0), 0);
        assert_eq!(line_of(text, 3), 1);
        assert_eq!(line_of(text, 99), 3);
    }

    #[test]
    fn lines_follow_inserted_and_deleted_text() {
        let old = "one\ntwo\nthree\nfour\n";
        let new = "zero\none\ntwo\n3\nfour\n";
        assert_eq!(map_line(old, new, 1), 2);
        assert_eq!(map_line(old, new, 2), 3);
        assert_eq!(map_line(old, new, 3), 4); // changed: same block
        assert_eq!(map_line(old, new, 4), 5);
        assert_eq!(map_line(old, old, 3), 3);
        let shorter = "one\nfour\n";
        assert_eq!(map_line(old, shorter, 3), 2); // deleted: where it was
    }

    const EN: &str = "\\section{Intro}\nFirst paragraph.\n\nSecond paragraph.\n\n\\section{Method}\nThird paragraph.\n";
    const ZH: &str = "\\section{引言}\n第一段。\n\n第二段。\n\n\\section{方法}\n第三段。\n";

    #[test]
    fn the_same_language_maps_lines() {
        let basis = Basis {
            compiled: EN.into(),
            editor: EN.into(),
            same_language: true,
            mode: Mode::Latex,
        };
        // Two lines were added at the top since the build.
        let now = format!("% note\n% more\n{EN}");
        assert_eq!(basis.to_editor(4, &now), Some(Place::Line(6)));
        let at = now.find("Second").unwrap();
        assert_eq!(basis.to_compiled(&now, at), Some(4));
    }

    #[test]
    fn the_other_language_maps_paragraphs() {
        // The English PDF while editing Chinese.
        let basis = Basis {
            compiled: EN.into(),
            editor: ZH.into(),
            same_language: false,
            mode: Mode::Latex,
        };
        let Some(Place::Paragraph(range)) = basis.to_editor(4, ZH) else {
            panic!("no paragraph");
        };
        assert_eq!(&ZH[range], "第二段。");
        // A heading is a paragraph too.
        let Some(Place::Paragraph(range)) = basis.to_editor(6, ZH) else {
            panic!("no paragraph");
        };
        assert_eq!(&ZH[range], "方法");
        // And back: the cursor in the third Chinese paragraph.
        let at = ZH.find("第三段").unwrap();
        assert_eq!(basis.to_compiled(ZH, at), Some(7));
        // After a paragraph was added in front, the mapping still lands.
        let now = ZH.replacen("第一段。", "新的一段。\n\n第一段。", 1);
        let Some(Place::Paragraph(range)) = basis.to_editor(4, &now) else {
            panic!("no paragraph");
        };
        assert_eq!(&now[range], "第二段。");
    }
}
