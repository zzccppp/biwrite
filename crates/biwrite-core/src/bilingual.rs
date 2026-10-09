//! Bilingual Markdown export: every paragraph in English followed by its
//! Chinese, everything else verbatim.

use crate::compose::{Insert, fit};
use crate::document::{DocSegment, DocumentModel};
use crate::lang::Direction;

const PENDING_ZH: &str = "*（尚未翻译）*";
const PENDING_EN: &str = "*(not translated yet)*";

/// A bilingual export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bilingual {
    pub text: String,
    /// Paragraphs exported with a "not translated yet" note.
    pub missing: usize,
}

/// Build the export for `doc`, written in the source language of
/// `direction`. `translation` returns a segment's current translation (or
/// `None` if it has none). English always comes first; a translation is put
/// in the segment's markup (`# 引言`, `\section{引言}`), so headings stay
/// headings, and machine output is shaped as for a swap, so the export is
/// the same from either side.
pub fn bilingual_markdown<F>(
    doc: &DocumentModel,
    direction: Direction,
    mut translation: F,
) -> Bilingual
where
    F: FnMut(&DocSegment) -> Option<Insert>,
{
    let text = doc.text();
    let mut blocks: Vec<String> = Vec::new();
    let mut missing = 0;
    for seg in doc.segments() {
        let s = &seg.segment;
        let source = s.source(text);
        if !s.kind.is_translatable() || s.content(text).trim().is_empty() {
            blocks.push(source.to_owned());
            continue;
        }
        let shaped = translation(seg).map(|ins| match ins.exact {
            true => ins.text,
            false => fit(ins.text.trim(), s.kind, doc.mode(), s.content(text)),
        });
        let other = match shaped.filter(|t| !t.trim().is_empty()) {
            Some(t) => format!(
                "{}{}{}",
                &text[s.range.start..s.content.start],
                t,
                &text[s.content.end..s.range.end]
            ),
            None => {
                missing += 1;
                match direction {
                    Direction::EnZh => PENDING_ZH.to_owned(),
                    Direction::ZhEn => PENDING_EN.to_owned(),
                }
            }
        };
        match direction {
            Direction::EnZh => blocks.extend([source.to_owned(), other]),
            Direction::ZhEn => blocks.extend([other, source.to_owned()]),
        }
    }
    let mut out = blocks.join("\n\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Bilingual { text: out, missing }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::Mode;

    fn doc(text: &str, mode: Mode) -> DocumentModel {
        let mut d = DocumentModel::new(1);
        d.apply(text.to_owned(), mode);
        d
    }

    #[test]
    fn english_then_chinese_with_skipped_blocks_verbatim() {
        let d = doc(
            "# Intro\n\nFirst para\nwraps.\n\n```\ncode\n```\n\nSecond.",
            Mode::Markdown,
        );
        let out = bilingual_markdown(&d, Direction::EnZh, |s| {
            let content = s.segment.content(d.text());
            (content != "Second.").then(|| Insert::machine(format!("译：{content}")))
        });
        assert_eq!(out.missing, 1);
        assert_eq!(
            out.text,
            "# Intro\n\n# 译：Intro\n\nFirst para\nwraps.\n\n译：First para\nwraps.\n\n```\ncode\n```\n\nSecond.\n\n*（尚未翻译）*\n"
        );
    }

    #[test]
    fn chinese_drafts_still_put_english_first() {
        let d = doc("\\section{引言}\n第一段。", Mode::Latex);
        let out = bilingual_markdown(&d, Direction::ZhEn, |s| match s.segment.content(d.text()) {
            "引言" => Some(Insert::machine("Introduction\n\n")),
            _ => None,
        });
        assert_eq!(
            out.text,
            "\\section{Introduction}\n\n\\section{引言}\n\n*(not translated yet)*\n\n第一段。\n"
        );
    }

    #[test]
    fn sample_paper_keeps_every_source_line() {
        let paper = include_str!("../../../samples/paper.tex");
        let d = doc(paper, Mode::Latex);
        let out = bilingual_markdown(&d, Direction::EnZh, |_| Some(Insert::exact("中文"))).text;
        for line in paper.lines().filter(|l| !l.trim().is_empty()) {
            assert!(out.contains(line), "missing {line:?}");
        }
        assert_eq!(out.matches("中文").count(), 22);
        assert!(
            bilingual_markdown(&doc("", Mode::Plain), Direction::EnZh, |_| None)
                .text
                .is_empty()
        );
    }
}
