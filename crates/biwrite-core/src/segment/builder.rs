//! Line iteration and the segment builder shared by all segmenters.

use std::ops::Range;

use super::{Segment, SegmentKind, SkipReason};

/// A source line without its terminator, with absolute byte offsets.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Line<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
}

impl Line<'_> {
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Absolute byte offset of a position inside this line.
    pub fn abs(&self, offset: usize) -> usize {
        self.start + offset
    }

    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    /// Absolute offset of the first non-whitespace character.
    pub fn text_start(&self) -> usize {
        self.start + (self.text.len() - self.text.trim_start().len())
    }
}

/// Iterate lines with absolute byte offsets. Terminators (`\n` or `\r\n`) are
/// excluded from the line.
pub(crate) fn lines(text: &str) -> impl Iterator<Item = Line<'_>> {
    let mut pos = 0;
    text.split_inclusive('\n').map(move |raw| {
        let start = pos;
        pos += raw.len();
        let body = raw.strip_suffix('\n').unwrap_or(raw);
        let body = body.strip_suffix('\r').unwrap_or(body);
        Line {
            start,
            end: start + body.len(),
            text: body,
        }
    })
}

/// A segment still being extended line by line.
enum Open {
    Paragraph {
        start: usize,
        end: usize,
        /// Where the translatable text starts (`None`: not seen yet, e.g. a
        /// bare `\item` line).
        content_start: Option<usize>,
    },
    Skip {
        reason: SkipReason,
        start: usize,
        end: usize,
    },
}

/// Accumulates lines and emits segments in order.
#[derive(Default)]
pub(crate) struct Builder {
    out: Vec<Segment>,
    open: Option<Open>,
}

impl Builder {
    /// Append a prose line to the current paragraph (starting one if needed).
    pub fn line(&mut self, line: &Line<'_>) {
        match &mut self.open {
            Some(Open::Paragraph {
                end, content_start, ..
            }) => {
                *end = line.end;
                content_start.get_or_insert(line.text_start());
            }
            _ => {
                self.flush();
                self.open = Some(Open::Paragraph {
                    start: line.start,
                    end: line.end,
                    content_start: Some(line.start),
                });
            }
        }
    }

    /// Start a new paragraph whose translatable text begins at
    /// `content_start` (after an `\item` marker), or on a later line if `None`.
    pub fn item(&mut self, line: &Line<'_>, content_start: Option<usize>) {
        self.flush();
        self.open = Some(Open::Paragraph {
            start: line.start,
            end: line.end,
            content_start,
        });
    }

    /// Add a non-translatable line; consecutive lines with the same reason
    /// form one segment.
    pub fn skip_line(&mut self, reason: SkipReason, range: Range<usize>) {
        if let Some(Open::Skip { reason: r, end, .. }) = &mut self.open
            && *r == reason
        {
            *end = range.end;
            return;
        }
        self.flush();
        self.open = Some(Open::Skip {
            reason,
            start: range.start,
            end: range.end,
        });
    }

    pub fn has_paragraph(&self) -> bool {
        matches!(self.open, Some(Open::Paragraph { .. }))
    }

    /// Close whatever is open.
    pub fn flush(&mut self) {
        match self.open.take() {
            Some(Open::Paragraph {
                start,
                end,
                content_start,
            }) => {
                let content_start = content_start.unwrap_or(end).min(end);
                self.out.push(Segment {
                    kind: SegmentKind::Paragraph,
                    range: start..end,
                    content: content_start..end,
                });
            }
            Some(Open::Skip { reason, start, end }) => self.push_skipped(reason, start..end),
            None => {}
        }
    }

    /// Turn the open paragraph into a heading whose range extends to `end`
    /// (Markdown setext headings).
    pub fn paragraph_to_heading(&mut self, level: u8, end: usize) {
        if let Some(Open::Paragraph {
            start, end: last, ..
        }) = self.open
        {
            self.open = None;
            self.out.push(Segment {
                kind: SegmentKind::Heading { level },
                range: start..end,
                content: start..last,
            });
        }
    }

    pub fn heading(&mut self, level: u8, range: Range<usize>, content: Range<usize>) {
        self.flush();
        self.out.push(Segment {
            kind: SegmentKind::Heading { level },
            range,
            content,
        });
    }

    pub fn caption(&mut self, range: Range<usize>, content: Range<usize>) {
        self.flush();
        self.out.push(Segment {
            kind: SegmentKind::Caption,
            range,
            content,
        });
    }

    /// Emit a complete skipped block (closing anything open first).
    pub fn skipped(&mut self, reason: SkipReason, range: Range<usize>) {
        self.flush();
        self.push_skipped(reason, range);
    }

    fn push_skipped(&mut self, reason: SkipReason, range: Range<usize>) {
        self.out.push(Segment {
            kind: SegmentKind::Skipped { reason },
            content: range.start..range.start,
            range,
        });
    }

    pub fn finish(mut self) -> Vec<Segment> {
        self.flush();
        self.out
    }
}
