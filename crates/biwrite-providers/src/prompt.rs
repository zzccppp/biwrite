//! Prompt construction shared by all providers.
//!
//! The system prompt (persona and rules) comes from an editable file. The
//! user message structure (tagged blocks and the closing instruction) is
//! built here, so editing the prompt file can't break how inputs are marked.

use std::path::PathBuf;

use biwrite_core::Direction;
use biwrite_engine::TranslationRequest;

pub const DEFAULT_PROMPT_EN_ZH: &str = "You translate academic English into Chinese for the author, who is reading their own draft. Translate faithfully. Follow the glossary. Keep placeholders ⟦n⟧ exactly as given. Output only the Chinese translation.";

pub const DEFAULT_PROMPT_ZH_EN: &str = "You translate the author's Chinese draft of an academic paper into English; the English is what gets published. Write precise, natural academic English. Translate faithfully, adding and omitting nothing. Follow the glossary. Keep placeholders ⟦n⟧ and any LaTeX or Markdown markup exactly as given. Output only the English translation.";

pub fn default_prompt(direction: Direction) -> &'static str {
    match direction {
        Direction::EnZh => DEFAULT_PROMPT_EN_ZH,
        Direction::ZhEn => DEFAULT_PROMPT_ZH_EN,
    }
}

/// File name of the editable prompt for `direction`.
pub fn prompt_file_name(direction: Direction) -> &'static str {
    match direction {
        Direction::EnZh => "en-zh.txt",
        Direction::ZhEn => "zh-en.txt",
    }
}

/// Where system prompts come from.
pub trait PromptSource: Send + Sync {
    fn system_prompt(&self, direction: Direction) -> String;
}

/// The built-in defaults.
pub struct DefaultPrompts;

impl PromptSource for DefaultPrompts {
    fn system_prompt(&self, direction: Direction) -> String {
        default_prompt(direction).to_owned()
    }
}

/// Prompt files in a directory, re-read on every request so edits apply
/// immediately; falls back to the default if a file is missing or empty.
pub struct PromptFiles {
    pub dir: PathBuf,
}

impl PromptSource for PromptFiles {
    fn system_prompt(&self, direction: Direction) -> String {
        std::fs::read_to_string(self.dir.join(prompt_file_name(direction)))
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default_prompt(direction).to_owned())
    }
}

/// System and user message for one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Messages {
    pub system: String,
    pub user: String,
}

fn target_language(direction: Direction) -> &'static str {
    match direction {
        Direction::EnZh => "Chinese (Simplified)",
        Direction::ZhEn => "English",
    }
}

fn block(out: &mut String, tag: &str, body: &str) {
    out.push_str(&format!("<{tag}>\n{}\n</{tag}>\n\n", body.trim()));
}

/// Build the messages for `req` with the given system prompt.
pub fn build(req: &TranslationRequest, system: String) -> Messages {
    let mut user = String::new();
    if let Some(note) = req.doc_note.as_deref().filter(|n| !n.trim().is_empty()) {
        block(&mut user, "document_note", note);
    }
    if !req.glossary.is_empty() {
        let lines: Vec<String> = req
            .glossary
            .iter()
            .map(|g| match &g.translation {
                Some(t) => format!("{} → {}", g.term, t),
                None => format!("{} → (keep in English)", g.term),
            })
            .collect();
        block(&mut user, "glossary", &lines.join("\n"));
    }
    if let Some(before) = req.context_before.as_deref() {
        block(&mut user, "context_before", before);
    }
    if let Some(after) = req.context_after.as_deref() {
        block(&mut user, "context_after", after);
    }
    // The same source with a previous translation means the glossary
    // changed: there's no edit to describe.
    let glossary_only = req
        .revision
        .as_ref()
        .is_some_and(|rev| rev.old_source == req.source);
    if let Some(rev) = &req.revision {
        if !glossary_only {
            block(&mut user, "previous_source", &rev.old_source);
        }
        block(&mut user, "previous_translation", &rev.old_translation);
    }
    block(&mut user, "source", &req.source);

    let language = target_language(req.direction);
    user.push_str(&format!("Translate the text in <source> into {language}."));
    if req.context_before.is_some() || req.context_after.is_some() {
        user.push_str(" The context blocks are the neighbouring paragraphs, for reference only: do not translate them.");
    }
    if glossary_only {
        user.push_str(" <previous_translation> is an earlier translation of <source>, made before the glossary changed. Revise it minimally so it follows the glossary: keep everything else word for word.");
    } else if req.revision.is_some() {
        user.push_str(" <previous_translation> is the translation of <previous_source>; the author has since edited that text into <source>. Revise the previous translation minimally so it matches <source>: keep unchanged parts word for word and change only what the edit requires. If the edit only changes whitespace or punctuation, return the previous translation with just that change.");
    }
    if !req.glossary.is_empty() {
        user.push_str(" Use the glossary renderings.");
    }
    if req.source.contains('⟦') {
        user.push_str(" Each ⟦n⟧ stands for protected text such as math or a citation: copy every one into the translation unchanged, exactly as often as it appears in <source>.");
    }
    user.push_str(" Reply with the translation only, without tags, labels or commentary.");
    Messages { system, user }
}

/// Build the messages for several segments in one request. The segments
/// are numbered `<source n="1">`, `<source n="2">`, ... and the answer is
/// expected as `<translation n="k">` blocks (see [`parse_batch`]).
/// Revisions are never batched, so only the fresh-translation instructions
/// apply. Context is the text before the first and after the last segment.
pub fn build_batch(reqs: &[TranslationRequest], system: String) -> Messages {
    let Some(first) = reqs.first() else {
        return Messages {
            system,
            user: String::new(),
        };
    };
    let mut user = String::new();
    if let Some(note) = first.doc_note.as_deref().filter(|n| !n.trim().is_empty()) {
        block(&mut user, "document_note", note);
    }
    let mut lines: Vec<String> = Vec::new();
    for g in reqs.iter().flat_map(|r| &r.glossary) {
        let line = match &g.translation {
            Some(t) => format!("{} → {}", g.term, t),
            None => format!("{} → (keep in English)", g.term),
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    if !lines.is_empty() {
        block(&mut user, "glossary", &lines.join("\n"));
    }
    if let Some(before) = first.context_before.as_deref() {
        block(&mut user, "context_before", before);
    }
    if let Some(after) = reqs.last().and_then(|r| r.context_after.as_deref()) {
        block(&mut user, "context_after", after);
    }
    for (i, r) in reqs.iter().enumerate() {
        user.push_str(&format!(
            "<source n=\"{}\">\n{}\n</source>\n\n",
            i + 1,
            r.source.trim()
        ));
    }
    let language = target_language(first.direction);
    user.push_str(&format!(
        "Translate each <source> block into {language}. The blocks are consecutive paragraphs or headings of one document: translate every block on its own, without merging, splitting or reordering them."
    ));
    if first.context_before.is_some() || reqs.last().is_some_and(|r| r.context_after.is_some()) {
        user.push_str(" The context blocks are the neighbouring text, for reference only: do not translate them.");
    }
    if !lines.is_empty() {
        user.push_str(" Use the glossary renderings.");
    }
    if reqs.iter().any(|r| r.source.contains('⟦')) {
        user.push_str(" Each ⟦n⟧ stands for protected text such as math or a citation: copy every one into the translation of the same block unchanged, exactly as often as it appears in that block.");
    }
    user.push_str(&format!(
        " Reply with exactly {} blocks, <translation n=\"1\">…</translation> to <translation n=\"{}\">…</translation>, in order, each holding only the translation of the source with the same number, and nothing outside them.",
        reqs.len(),
        reqs.len()
    ));
    Messages { system, user }
}

/// One `<translation …>` block found in a batch answer: its number (1-based
/// as written), its text, and whether its closing tag has arrived.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchBlock {
    pub n: usize,
    pub text: String,
    pub complete: bool,
}

/// All `<translation n="k">…</translation>` blocks in `text`, in order. The
/// last one may still be open while streaming. The number may be written
/// `n="2"`, `n=2` or `id="2"`.
pub fn batch_blocks(text: &str) -> Vec<BatchBlock> {
    const OPEN: &str = "<translation";
    const CLOSE: &str = "</translation>";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after_name = &rest[start + OPEN.len()..];
        let Some(gt) = after_name.find('>') else {
            break; // tag still streaming
        };
        let attrs = &after_name[..gt];
        let n: Option<usize> = attrs
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok();
        let body = &after_name[gt + 1..];
        let (content, complete, next) = match body.find(CLOSE) {
            Some(end) => (&body[..end], true, &body[end + CLOSE.len()..]),
            None => (body, false, ""),
        };
        if let Some(n) = n {
            out.push(BatchBlock {
                n,
                text: content.trim().to_owned(),
                complete,
            });
        }
        rest = next;
    }
    out
}

/// The translations of a batch of `count` segments, by position. A missing,
/// unterminated or duplicated block leaves its position empty (the first
/// complete block of a number wins).
pub fn parse_batch(text: &str, count: usize) -> Vec<Option<String>> {
    let mut out = vec![None; count];
    for b in batch_blocks(text).into_iter().filter(|b| b.complete) {
        if let Some(slot) = b.n.checked_sub(1).and_then(|i| out.get_mut(i))
            && slot.is_none()
            && !b.text.is_empty()
        {
            *slot = Some(b.text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use biwrite_engine::{GlossaryEntry, Revision};

    use super::*;

    #[test]
    fn minimal_request() {
        let req = TranslationRequest {
            source: "Hello.".into(),
            ..Default::default()
        };
        let m = build(&req, "SYS".into());
        assert_eq!(m.system, "SYS");
        assert!(m.user.starts_with("<source>\nHello.\n</source>"));
        assert!(m.user.contains("into Chinese (Simplified)."));
        assert!(!m.user.contains("context"));
    }

    #[test]
    fn full_request_includes_every_block_in_order() {
        let req = TranslationRequest {
            direction: Direction::ZhEn,
            source: "新句子。".into(),
            context_before: Some("前文。".into()),
            context_after: Some("后文。".into()),
            glossary: vec![
                GlossaryEntry {
                    term: "上下文学习".into(),
                    translation: Some("in-context learning".into()),
                },
                GlossaryEntry {
                    term: "GNN".into(),
                    translation: None,
                },
            ],
            doc_note: Some("ML paper".into()),
            revision: Some(Revision {
                old_source: "旧句子。".into(),
                old_translation: "Old sentence.".into(),
            }),
        };
        let m = build(&req, default_prompt(Direction::ZhEn).into());
        let order = [
            "<document_note>",
            "<glossary>",
            "<context_before>",
            "<context_after>",
            "<previous_source>",
            "<previous_translation>",
            "<source>",
        ];
        let positions: Vec<usize> = order.iter().map(|t| m.user.find(t).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
        assert!(m.user.contains("GNN → (keep in English)"));
        assert!(m.user.contains("into English."));
        assert!(m.user.contains("Revise the previous translation minimally"));
        assert!(m.user.contains("do not translate them"));
    }

    #[test]
    fn glossary_only_revision_omits_the_identical_source() {
        let req = TranslationRequest {
            source: "GNNs are strong.".into(),
            glossary: vec![GlossaryEntry::new("GNN", None)],
            revision: Some(Revision {
                old_source: "GNNs are strong.".into(),
                old_translation: "图神经网络很强。".into(),
            }),
            ..Default::default()
        };
        let m = build(&req, "SYS".into());
        assert!(!m.user.contains("<previous_source>"));
        assert!(m.user.contains("<previous_translation>\n图神经网络很强。"));
        assert!(m.user.contains("made before the glossary changed"));
        assert!(!m.user.contains("edited that text"));
    }

    #[test]
    fn placeholders_get_an_instruction() {
        let plain = TranslationRequest {
            source: "No math.".into(),
            ..Default::default()
        };
        assert!(!build(&plain, "S".into()).user.contains("⟦n⟧"));
        let masked = TranslationRequest {
            source: "Let ⟦0⟧ be given ⟦1⟧.".into(),
            ..Default::default()
        };
        assert!(
            build(&masked, "S".into())
                .user
                .contains("copy every one into the translation unchanged")
        );
    }

    fn batch_requests() -> Vec<TranslationRequest> {
        vec![
            TranslationRequest {
                source: "First ⟦0⟧ paragraph.".into(),
                context_before: Some("Before.".into()),
                glossary: vec![GlossaryEntry::new("GNN", Some("图神经网络"))],
                doc_note: Some("ML paper".into()),
                ..Default::default()
            },
            TranslationRequest {
                source: "Second paragraph.".into(),
                glossary: vec![GlossaryEntry::new("GNN", Some("图神经网络"))],
                context_after: Some("After.".into()),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn batch_prompt_numbers_the_sources() {
        let m = build_batch(&batch_requests(), "SYS".into());
        let order = [
            "<document_note>",
            "<glossary>",
            "<context_before>\nBefore.",
            "<context_after>\nAfter.",
            "<source n=\"1\">\nFirst ⟦0⟧ paragraph.",
            "<source n=\"2\">\nSecond paragraph.",
        ];
        let positions: Vec<usize> = order.iter().map(|t| m.user.find(t).unwrap()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{}", m.user);
        assert_eq!(
            m.user.matches("GNN → 图神经网络").count(),
            1,
            "glossary deduplicated"
        );
        assert!(m.user.contains("exactly 2 blocks"));
        assert!(m.user.contains("same block unchanged"));
    }

    #[test]
    fn batch_answers_are_split_by_number() {
        let answer = "<translation n=\"2\">第二段。</translation>\n<translation n=1>第一 ⟦0⟧ 段。</translation>";
        assert_eq!(
            parse_batch(answer, 3),
            vec![Some("第一 ⟦0⟧ 段。".into()), Some("第二段。".into()), None]
        );
        // Unterminated, duplicated, out of range or empty blocks are ignored.
        let messy = "<translation n=\"1\">一</translation><translation n=\"1\">又一</translation><translation n=\"9\">九</translation><translation n=\"2\"></translation><translation n=\"3\">三";
        assert_eq!(parse_batch(messy, 3), vec![Some("一".into()), None, None]);
    }

    #[test]
    fn streamed_batch_answers_show_the_open_block() {
        let blocks =
            batch_blocks("<translation n=\"1\">一</translation>\n<translation n=\"2\">二部");
        assert_eq!(
            blocks,
            vec![
                BatchBlock {
                    n: 1,
                    text: "一".into(),
                    complete: true
                },
                BatchBlock {
                    n: 2,
                    text: "二部".into(),
                    complete: false
                },
            ]
        );
        assert!(batch_blocks("<translation n=\"1\"").is_empty());
    }

    #[test]
    fn prompt_files_fall_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("biwrite-prompts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files = PromptFiles { dir: dir.clone() };
        assert_eq!(files.system_prompt(Direction::EnZh), DEFAULT_PROMPT_EN_ZH);
        std::fs::write(dir.join("en-zh.txt"), "  Custom prompt. \n").unwrap();
        assert_eq!(files.system_prompt(Direction::EnZh), "Custom prompt.");
        std::fs::write(dir.join("zh-en.txt"), "   ").unwrap();
        assert_eq!(files.system_prompt(Direction::ZhEn), DEFAULT_PROMPT_ZH_EN);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
