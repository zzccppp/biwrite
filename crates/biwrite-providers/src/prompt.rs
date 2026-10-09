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
    if let Some(rev) = &req.revision {
        block(&mut user, "previous_source", &rev.old_source);
        block(&mut user, "previous_translation", &rev.old_translation);
    }
    block(&mut user, "source", &req.source);

    let language = target_language(req.direction);
    user.push_str(&format!("Translate the text in <source> into {language}."));
    if req.context_before.is_some() || req.context_after.is_some() {
        user.push_str(" The context blocks are the neighbouring paragraphs, for reference only: do not translate them.");
    }
    if req.revision.is_some() {
        user.push_str(" <previous_translation> is the translation of <previous_source>; the author has since edited that text into <source>. Revise the previous translation minimally so it matches <source>: keep unchanged parts word for word and change only what the edit requires.");
    }
    if !req.glossary.is_empty() {
        user.push_str(" Use the glossary renderings.");
    }
    user.push_str(" Reply with the translation only, without tags, labels or commentary.");
    Messages { system, user }
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
