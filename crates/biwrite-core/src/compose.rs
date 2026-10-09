//! Building the other-language document: used when the panes are swapped
//! and when saving the English file while editing Chinese.
//!
//! Every translatable segment's *content* is replaced by its translation;
//! everything else (gaps, skipped blocks, `\section{`…`}` markup, `\item`
//! markers) is copied byte for byte. Composing twice with the inverse pairs
//! therefore restores the original text exactly.

use std::collections::HashMap;

use crate::document::{DocSegment, DocumentModel};
use crate::mode::Mode;
use crate::protect::{SpanKind, spans};
use crate::segment::{SegmentKind, segment};

/// A translation to splice into a segment's content slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Insert {
    pub text: String,
    /// The text came from this very slot (a swap seed): insert it verbatim.
    /// Otherwise it is machine output and is shaped to fit (see [`fit`]).
    pub exact: bool,
}

impl Insert {
    pub fn exact(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exact: true,
        }
    }

    pub fn machine(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            exact: false,
        }
    }
}

/// A composed document.
#[derive(Debug, PartialEq, Eq)]
pub struct Composed {
    pub text: String,
    /// `(inserted translation, original content)` per translated segment,
    /// in document order.
    pub pairs: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ComposeError {
    /// Some translatable segments have no usable translation yet.
    #[error("{missing} paragraph(s) are not translated yet")]
    NotReady { missing: usize },
    /// A translation would change the document's structure (e.g. unbalanced
    /// braces in a LaTeX heading), so the result could not be swapped back.
    #[error("the translation of paragraph {index} would change the document structure")]
    Structure { index: usize },
}

/// Replace each translatable segment's content with `translation(segment)`.
/// Segments with empty content are kept as they are.
pub fn compose<F>(doc: &DocumentModel, mut translation: F) -> Result<Composed, ComposeError>
where
    F: FnMut(&DocSegment) -> Option<Insert>,
{
    let text = doc.text();
    let mut out = String::with_capacity(text.len() * 2);
    let mut pairs = Vec::new();
    let mut missing = 0;
    let mut pos = 0;
    for seg in doc.segments() {
        let s = &seg.segment;
        let content = s.content(text);
        if !s.kind.is_translatable() || content.trim().is_empty() {
            continue;
        }
        let fitted = translation(seg)
            .map(|ins| {
                if ins.exact {
                    ins.text
                } else {
                    fit(&ins.text, s.kind, doc.mode(), content)
                }
            })
            .filter(|t| !t.trim().is_empty());
        let Some(t) = fitted else {
            missing += 1;
            continue;
        };
        out.push_str(&text[pos..s.content.start]);
        out.push_str(&t);
        pos = s.content.end;
        pairs.push((t, content.to_owned()));
    }
    if missing > 0 {
        return Err(ComposeError::NotReady { missing });
    }
    out.push_str(&text[pos..]);
    verify(doc, &out, &pairs)?;
    Ok(Composed { text: out, pairs })
}

/// Shape machine output so it fits the slot, changing only what must change:
/// whitespace-only lines are dropped (they would split the paragraph),
/// multi-line headings are joined, and in LaTeX an unescaped `%` becomes
/// `\%` (it would comment out the rest of the line) unless it starts one of
/// the source's own comments, which placeholder protection carried over.
pub(crate) fn fit(t: &str, kind: SegmentKind, mode: Mode, source: &str) -> String {
    let mut out = if t.lines().any(|l| l.trim().is_empty()) {
        t.lines()
            .filter(|l| !l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        t.to_owned()
    };
    if matches!(kind, SegmentKind::Heading { .. }) && out.contains('\n') {
        out = out.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    }
    if mode == Mode::Latex {
        let comments: Vec<&str> = spans(source, Mode::Latex)
            .into_iter()
            .filter(|s| s.kind == SpanKind::Comment)
            .map(|s| &source[s.range])
            .collect();
        out = escape_percent(&out, &comments);
    }
    out
}

/// Escape each `%` that would start a comment the source doesn't have.
/// Escaped `\%` and `%` inside protected spans (`\url{…%20…}`, `\verb|…|`,
/// math) are left alone, and so are the source's own comments (each as
/// often as the source has it).
fn escape_percent(s: &str, comments: &[&str]) -> String {
    let mut allowed: Vec<&str> = comments.to_vec();
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'scan: loop {
        for span in spans(rest, Mode::Latex) {
            if span.kind != SpanKind::Comment {
                continue;
            }
            let comment = &rest[span.range.clone()];
            if let Some(i) = allowed.iter().position(|c| *c == comment) {
                allowed.swap_remove(i);
                continue;
            }
            out.push_str(&rest[..span.range.start]);
            out.push_str("\\%");
            // The rest of that line is text now: scan it again.
            rest = &rest[span.range.start + 1..];
            continue 'scan;
        }
        out.push_str(rest);
        return out;
    }
}

/// The composed text must segment into the same kinds, with each translated
/// segment's content equal to what was inserted.
fn verify(doc: &DocumentModel, text: &str, pairs: &[(String, String)]) -> Result<(), ComposeError> {
    let new = segment(text, doc.mode());
    let old = doc.segments();
    let mut inserted = pairs.iter().map(|(t, _)| t.as_str());
    for (index, old_seg) in old.iter().enumerate() {
        let Some(new_seg) = new.get(index) else {
            return Err(ComposeError::Structure { index });
        };
        if new_seg.kind != old_seg.kind() {
            return Err(ComposeError::Structure { index });
        }
        let translated = old_seg.kind().is_translatable()
            && !old_seg.segment.content(doc.text()).trim().is_empty();
        if translated && inserted.next() != Some(new_seg.content(text)) {
            return Err(ComposeError::Structure { index });
        }
    }
    if new.len() != old.len() {
        return Err(ComposeError::Structure { index: old.len() });
    }
    Ok(())
}

/// Index pairs by inserted text, for looking up the original of a segment
/// after composing (the swap "seeds").
pub fn pair_index(pairs: Vec<(String, String)>) -> HashMap<String, String> {
    pairs.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake translation that is obviously different and contains CJK.
    fn fake_zh(s: &str) -> String {
        format!(
            "中文：{}",
            s.split_whitespace().rev().collect::<Vec<_>>().join(" ")
        )
    }

    fn round_trip(text: &str, mode: Mode) {
        let mut en = DocumentModel::new(1);
        en.apply(text.to_owned(), mode);
        let zh = compose(&en, |s| {
            Some(Insert::machine(fake_zh(s.segment.content(en.text()))))
        })
        .unwrap();
        assert_ne!(zh.text, text);

        let mut zh_doc = DocumentModel::new(1000);
        zh_doc.apply(zh.text.clone(), mode);
        let originals = pair_index(zh.pairs);
        let back = compose(&zh_doc, |s| {
            originals
                .get(s.segment.content(zh_doc.text()))
                .cloned()
                .map(Insert::exact)
        })
        .unwrap();
        assert_eq!(
            back.text, text,
            "swapping back must restore the original exactly"
        );
    }

    #[test]
    fn samples_round_trip_exactly() {
        round_trip(include_str!("../../../samples/paper.tex"), Mode::Latex);
        round_trip(include_str!("../../../samples/notes.md"), Mode::Markdown);
        round_trip(include_str!("../../../samples/notes.md"), Mode::Plain);
        round_trip(
            "  indented para\nwith two lines  \n\n\n# H\n\\item x\n",
            Mode::Plain,
        );
    }

    #[test]
    fn missing_translations_are_reported() {
        let mut d = DocumentModel::new(1);
        d.apply("a\n\nb\n\nc".to_owned(), Mode::Plain);
        let err = compose(&d, |s| (s.id.0 != 2).then(|| Insert::machine("x"))).unwrap_err();
        assert_eq!(err, ComposeError::NotReady { missing: 1 });
        let err = compose(&d, |_| Some(Insert::machine("   "))).unwrap_err();
        assert_eq!(err, ComposeError::NotReady { missing: 3 });
    }

    #[test]
    fn blank_lines_in_translations_are_removed() {
        let mut d = DocumentModel::new(1);
        d.apply("One para.\n\nTwo.".to_owned(), Mode::Plain);
        let out = compose(&d, |s| {
            Some(Insert::machine(format!(
                "{}\n\n  extra\n",
                s.segment.content(d.text())
            )))
        })
        .unwrap();
        assert_eq!(out.text, "One para.\n  extra\n\nTwo.\n  extra");
    }

    #[test]
    fn structure_breaking_translation_is_refused() {
        let mut d = DocumentModel::new(1);
        d.apply("\\section{Intro}\nText.".to_owned(), Mode::Latex);
        // An unbalanced brace turns the heading into a paragraph.
        let err = compose(&d, |s| {
            Some(Insert::machine(if s.kind() == SegmentKind::Paragraph {
                "正文"
            } else {
                "引}言"
            }))
        })
        .unwrap_err();
        assert!(matches!(err, ComposeError::Structure { .. }));
    }

    #[test]
    fn headings_become_single_line() {
        let mut d = DocumentModel::new(1);
        d.apply("# Title\n\nBody.".to_owned(), Mode::Markdown);
        let out = compose(&d, |s| {
            Some(Insert::machine(format!(
                "{}\n第二行",
                s.segment.content(d.text())
            )))
        })
        .unwrap();
        assert_eq!(out.text, "# Title 第二行\n\nBody.\n第二行");
    }

    #[test]
    fn exact_originals_are_spliced_verbatim() {
        // Shapes `fit` would normalize must survive a round trip untouched.
        let text = "\\section{ Introduction }\n\\begin{figure}\n\\caption{\n    Accuracy on Cora.\n\n  more\n  }\n\\end{figure}\nBody.";
        let mut d = DocumentModel::new(1);
        d.apply(text.to_owned(), Mode::Latex);
        let out = compose(&d, |s| Some(Insert::exact(s.segment.content(d.text())))).unwrap();
        assert_eq!(out.text, text);
    }

    #[test]
    fn percent_is_escaped_in_latex_only() {
        let mut d = DocumentModel::new(1);
        d.apply("\\section{准确率}\n正文。".to_owned(), Mode::Latex);
        let out = compose(&d, |s| {
            Some(Insert::machine(if s.kind() == SegmentKind::Paragraph {
                "95% of \\% nodes"
            } else {
                "Accuracy of 95%"
            }))
        })
        .unwrap();
        assert_eq!(out.text, "\\section{Accuracy of 95\\%}\n95\\% of \\% nodes");
        let mut md = DocumentModel::new(1);
        md.apply("一段。".to_owned(), Mode::Markdown);
        let out = compose(&md, |_| Some(Insert::machine("95% done"))).unwrap();
        assert_eq!(out.text, "95% done");
    }

    #[test]
    fn percent_inside_protected_text_is_kept() {
        let mut d = DocumentModel::new(1);
        d.apply("一段。".to_owned(), Mode::Latex);
        let out = compose(&d, |_| {
            Some(Insert::machine(
                "See \\url{https://x.org/a%20b} and \\verb|50%| at $p=5%$ now, 5% up.",
            ))
        })
        .unwrap();
        assert_eq!(
            out.text,
            "See \\url{https://x.org/a%20b} and \\verb|50%| at $p=5%$ now, 5\\% up."
        );
    }

    #[test]
    fn a_bare_source_comment_exempts_only_itself() {
        let mut d = DocumentModel::new(1);
        d.apply("第一行。%\n第二行。".to_owned(), Mode::Latex);
        let out = compose(&d, |_| Some(Insert::machine("Line one.%\nUp 5%"))).unwrap();
        assert_eq!(out.text, "Line one.%\nUp 5\\%");
    }

    #[test]
    fn source_comments_survive_composing() {
        let mut d = DocumentModel::new(1);
        d.apply(
            "一段 % keep me
第二行 5%。"
                .to_owned(),
            Mode::Latex,
        );
        let out = compose(&d, |_| {
            Some(Insert::machine(
                "One % keep me
line 5%. % not mine",
            ))
        })
        .unwrap();
        assert_eq!(out.text, "One % keep me\nline 5\\%. \\% not mine");
    }
}
