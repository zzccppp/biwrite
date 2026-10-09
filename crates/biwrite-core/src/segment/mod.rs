//! Splitting a source document into translation segments.
//!
//! The unit is a paragraph (lines separated by blank lines). Headings and
//! captions are their own segments. Blocks that must not be translated (code,
//! math, LaTeX preamble, comments, structural markup) become skipped segments
//! so the right pane can show them collapsed while staying aligned.
//!
//! Invariants (tested): segments are ordered and disjoint, every non-blank
//! source line lies inside some segment, and `content` lies within `range`.

mod builder;
mod latex;
mod markdown;

use std::ops::Range;

use serde::Serialize;

use crate::mode::Mode;

pub(crate) use builder::{Builder, Line, lines};

/// Why a segment is not sent for translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Markdown YAML/TOML front matter.
    FrontMatter,
    /// Code: fenced blocks, verbatim/listing/algorithm environments.
    Code,
    /// Markdown thematic break (`---`, `***`).
    Rule,
    /// LaTeX preamble (everything up to `\begin{document}`).
    Preamble,
    /// Comment lines (`%` in LaTeX, `<!-- -->` in Markdown).
    Comment,
    /// Display math.
    Math,
    /// Tabular material.
    Table,
    /// Non-caption parts of figures/tables.
    Float,
    /// Structural commands without prose (`\maketitle`, `\begin{abstract}`,
    /// `\bibliography{..}`, `\end{document}` and after, ...).
    Markup,
}

/// What a segment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SegmentKind {
    Paragraph,
    Heading {
        level: u8,
    },
    /// Figure/table caption (LaTeX `\caption{..}`).
    Caption,
    Skipped {
        reason: SkipReason,
    },
}

impl SegmentKind {
    /// Whether the segment's content is sent to the translator.
    pub fn is_translatable(&self) -> bool {
        !matches!(self, Self::Skipped { .. })
    }
}

/// One segment of the source document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    /// Byte range of the whole segment in the source.
    pub range: Range<usize>,
    /// Byte range of the text to translate: the paragraph (after any `\item`),
    /// a heading's or caption's title, and empty for skipped segments.
    pub content: Range<usize>,
}

impl Segment {
    /// The full source text of the segment.
    pub fn source<'a>(&self, text: &'a str) -> &'a str {
        &text[self.range.clone()]
    }

    /// The translatable text of the segment.
    pub fn content<'a>(&self, text: &'a str) -> &'a str {
        &text[self.content.clone()]
    }
}

/// Split `text` into segments according to `mode`.
pub fn segment(text: &str, mode: Mode) -> Vec<Segment> {
    match mode {
        Mode::Plain => plain(text),
        Mode::Markdown => markdown::segment(text),
        Mode::Latex => latex::segment(text),
    }
}

fn plain(text: &str) -> Vec<Segment> {
    let mut b = Builder::default();
    for line in lines(text) {
        if line.is_blank() {
            b.flush();
        } else {
            b.line(&line);
        }
    }
    b.finish()
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    /// Assert the documented invariants for `text`.
    pub fn check_invariants(text: &str, mode: Mode) {
        let segs = segment(text, mode);
        for pair in segs.windows(2) {
            assert!(
                pair[0].range.end <= pair[1].range.start,
                "overlap: {pair:?}"
            );
        }
        for s in &segs {
            assert!(
                s.range.start <= s.content.start && s.content.end <= s.range.end,
                "{s:?}"
            );
            assert!(text.is_char_boundary(s.range.start) && text.is_char_boundary(s.range.end));
            assert!(text.is_char_boundary(s.content.start) && text.is_char_boundary(s.content.end));
        }
        for line in lines(text).filter(|l| !l.is_blank()) {
            assert!(
                segs.iter()
                    .any(|s| s.range.start <= line.start && line.end <= s.range.end),
                "line not covered by any segment: {:?}",
                line.text
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources(text: &str, mode: Mode) -> Vec<&str> {
        segment(text, mode).iter().map(|s| s.source(text)).collect()
    }

    #[test]
    fn plain_splits_on_blank_lines() {
        let text = "First line\nstill first.\n\n\nSecond.\n   \nThird\n";
        assert_eq!(
            sources(text, Mode::Plain),
            vec!["First line\nstill first.", "Second.", "Third"]
        );
    }

    #[test]
    fn ranges_are_byte_offsets() {
        let text = "Ünïcödé — ok.\n\n数学 text";
        let segs = segment(text, Mode::Plain);
        assert_eq!(segs.len(), 2);
        assert_eq!(&text[segs[1].range.clone()], "数学 text");
        assert_eq!(segs[0].range, 0.."Ünïcödé — ok.".len());
    }

    #[test]
    fn empty_and_blank_documents() {
        assert!(segment("", Mode::Plain).is_empty());
        assert!(segment("\n\n  \n", Mode::Markdown).is_empty());
        assert!(segment("\n\n  \n", Mode::Latex).is_empty());
    }

    #[test]
    fn crlf_lines_exclude_terminators() {
        let text = "a\r\nb\r\n\r\nc";
        assert_eq!(sources(text, Mode::Plain), vec!["a\r\nb", "c"]);
    }

    #[test]
    fn invariants_hold_for_samples() {
        for mode in [Mode::Plain, Mode::Markdown, Mode::Latex] {
            test_util::check_invariants(include_str!("../../../../samples/paper.tex"), mode);
            test_util::check_invariants(include_str!("../../../../samples/notes.md"), mode);
        }
    }
}
