//! Markdown segmentation: ATX/setext headings, fenced code, `$$` display
//! math, HTML comments, front matter, thematic breaks. Everything else is
//! paragraph text.

use super::{Builder, Line, Segment, SkipReason, lines};

pub(super) fn segment(text: &str) -> Vec<Segment> {
    let all: Vec<Line<'_>> = lines(text).collect();
    let mut b = Builder::default();
    let mut i = front_matter(&all, &mut b);

    while let Some(line) = all.get(i) {
        if let Some(fence) = Fence::open(line.text) {
            let close = all[i + 1..]
                .iter()
                .position(|l| fence.closes(l.text))
                .map_or(all.len() - 1, |p| i + 1 + p);
            b.skipped(SkipReason::Code, line.start..all[close].end);
            i = close + 1;
            continue;
        }
        if let Some((reason, close)) = block_delimited(&all, i) {
            b.skipped(reason, line.start..all[close].end);
            i = close + 1;
            continue;
        }
        if line.is_blank() {
            b.flush();
        } else if let Some((level, content)) = atx_heading(line) {
            b.heading(level, line.start..line.end, content);
        } else if let Some(level) = setext_underline(line.text).filter(|_| b.has_paragraph()) {
            b.paragraph_to_heading(level, line.end);
        } else if is_thematic_break(line.text) {
            b.skipped(SkipReason::Rule, line.start..line.end);
        } else {
            b.line(line);
        }
        i += 1;
    }
    b.finish()
}

/// Emit a front-matter block if the document starts with `---`. Returns the
/// index of the first line after it.
fn front_matter(all: &[Line<'_>], b: &mut Builder) -> usize {
    let Some(first) = all.first() else { return 0 };
    if first.text.trim_end() != "---" {
        return 0;
    }
    let close = all[1..]
        .iter()
        .position(|l| matches!(l.text.trim_end(), "---" | "..."));
    match close {
        Some(p) => {
            b.skipped(SkipReason::FrontMatter, first.start..all[p + 1].end);
            p + 2
        }
        None => 0,
    }
}

/// `$$ .. $$` math or `<!-- .. -->` comment starting at line `i`: the reason
/// and the index of the closing line (last line if unclosed).
fn block_delimited(all: &[Line<'_>], i: usize) -> Option<(SkipReason, usize)> {
    let t = block_indent(all[i].text)?;
    let (reason, opener, closer) = if t.starts_with("$$") {
        (SkipReason::Math, "$$", "$$")
    } else if t.starts_with("<!--") {
        (SkipReason::Comment, "<!--", "-->")
    } else {
        return None;
    };
    if t[opener.len()..].contains(closer) {
        return Some((reason, i));
    }
    let close = all[i + 1..]
        .iter()
        .position(|l| l.text.contains(closer))
        .map_or(all.len() - 1, |p| i + 1 + p);
    Some((reason, close))
}

/// Strip up to three leading spaces (CommonMark block indentation).
fn block_indent(text: &str) -> Option<&str> {
    let spaces = text.len() - text.trim_start_matches(' ').len();
    (spaces <= 3).then(|| &text[spaces..])
}

struct Fence {
    marker: char,
    len: usize,
}

impl Fence {
    fn open(text: &str) -> Option<Self> {
        let rest = block_indent(text)?;
        let marker = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
        let len = rest.len() - rest.trim_start_matches(marker).len();
        // A backtick fence's info string may not contain backticks.
        let info = &rest[len..];
        (len >= 3 && !(marker == '`' && info.contains('`'))).then_some(Self { marker, len })
    }

    fn closes(&self, text: &str) -> bool {
        let Some(rest) = block_indent(text) else {
            return false;
        };
        let run = rest.len() - rest.trim_start_matches(self.marker).len();
        run >= self.len && rest[run..].trim().is_empty()
    }
}

/// `## Title ##` → (2, byte range of "Title").
fn atx_heading(line: &Line<'_>) -> Option<(u8, std::ops::Range<usize>)> {
    let rest = block_indent(line.text)?;
    let indent = line.text.len() - rest.len();
    let hashes = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = &rest[hashes..];
    if !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return None;
    }
    let body_start = indent + hashes + (after.len() - after.trim_start().len());
    let mut body = line.text[body_start..].trim_end();
    // Optional closing sequence: spaces followed by #s only.
    let without_closing = body.trim_end_matches('#');
    if without_closing.is_empty() || without_closing.ends_with([' ', '\t']) {
        body = without_closing.trim_end();
    }
    let level = u8::try_from(hashes).ok()?;
    Some((
        level,
        line.abs(body_start)..line.abs(body_start + body.len()),
    ))
}

fn setext_underline(text: &str) -> Option<u8> {
    let rest = block_indent(text)?.trim_end();
    if !rest.is_empty() && rest.chars().all(|c| c == '=') {
        Some(1)
    } else if !rest.is_empty() && rest.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

fn is_thematic_break(text: &str) -> bool {
    let Some(rest) = block_indent(text) else {
        return false;
    };
    let marks: Vec<char> = rest.chars().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3 && matches!(marks[0], '-' | '*' | '_') && marks.iter().all(|c| *c == marks[0])
}

#[cfg(test)]
mod tests {
    use crate::mode::Mode;
    use crate::segment::{SegmentKind, SkipReason, segment};

    fn kinds_and_content(text: &str) -> Vec<(SegmentKind, &str)> {
        segment(text, Mode::Markdown)
            .iter()
            .map(|s| (s.kind, s.content(text)))
            .collect()
    }

    #[test]
    fn atx_headings_are_own_segments() {
        let text = "# Title\nIntro text\ncontinues.\n## Methods ##\n";
        let got = kinds_and_content(text);
        assert_eq!(
            got,
            vec![
                (SegmentKind::Heading { level: 1 }, "Title"),
                (SegmentKind::Paragraph, "Intro text\ncontinues."),
                (SegmentKind::Heading { level: 2 }, "Methods"),
            ]
        );
    }

    #[test]
    fn hash_without_space_is_text() {
        let got = kinds_and_content("#hashtag here\n");
        assert_eq!(got, vec![(SegmentKind::Paragraph, "#hashtag here")]);
    }

    #[test]
    fn fenced_code_is_skipped_across_blank_lines() {
        let text = "Before.\n\n```rust\nfn main() {}\n\nlet x = 1;\n```\nAfter.\n";
        let segs = segment(text, Mode::Markdown);
        assert_eq!(segs.len(), 3);
        assert_eq!(
            segs[1].kind,
            SegmentKind::Skipped {
                reason: SkipReason::Code
            }
        );
        assert_eq!(
            segs[1].source(text),
            "```rust\nfn main() {}\n\nlet x = 1;\n```"
        );
        assert_eq!(segs[2].source(text), "After.");
    }

    #[test]
    fn unclosed_fence_runs_to_end() {
        let text = "~~~\ncode\n\nmore";
        let segs = segment(text, Mode::Markdown);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].source(text), text);
    }

    #[test]
    fn front_matter_and_rules() {
        let text = "---\ntitle: x\n---\n\nText.\n\n***\n\nEnd.";
        let got = kinds_and_content(text);
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[0].0,
            SegmentKind::Skipped {
                reason: SkipReason::FrontMatter
            }
        );
        assert_eq!(
            got[2].0,
            SegmentKind::Skipped {
                reason: SkipReason::Rule
            }
        );
    }

    #[test]
    fn display_math_and_html_comments() {
        let text = "Intro.\n\n$$\na^2 + b^2\n\n= c^2\n$$\n<!-- note\nhidden -->\nOutro.";
        let got = kinds_and_content(text);
        assert_eq!(got.len(), 4);
        assert_eq!(
            got[1].0,
            SegmentKind::Skipped {
                reason: SkipReason::Math
            }
        );
        assert_eq!(
            got[2].0,
            SegmentKind::Skipped {
                reason: SkipReason::Comment
            }
        );
        assert_eq!(got[3], (SegmentKind::Paragraph, "Outro."));
        crate::segment::test_util::check_invariants(text, Mode::Markdown);
    }

    #[test]
    fn sample_notes_segmentation() {
        let text = include_str!("../../../../samples/notes.md");
        let got: Vec<SegmentKind> = segment(text, Mode::Markdown)
            .iter()
            .map(|s| s.kind)
            .collect();
        let p = SegmentKind::Paragraph;
        assert_eq!(
            got,
            [
                SegmentKind::Skipped {
                    reason: SkipReason::FrontMatter
                },
                SegmentKind::Heading { level: 1 },
                p,
                SegmentKind::Heading { level: 2 },
                p,
                p,
                SegmentKind::Skipped {
                    reason: SkipReason::Code
                },
                SegmentKind::Skipped {
                    reason: SkipReason::Rule
                },
                SegmentKind::Heading { level: 2 },
                p,
            ]
        );
    }

    #[test]
    fn setext_heading() {
        let text = "Big Title\n=========\n\nBody.";
        let segs = segment(text, Mode::Markdown);
        assert_eq!(segs[0].kind, SegmentKind::Heading { level: 1 });
        assert_eq!(segs[0].content(text), "Big Title");
        assert_eq!(segs[0].source(text), "Big Title\n=========");
    }
}
