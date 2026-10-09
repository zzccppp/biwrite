//! The writing assistant's pure logic: which skill documents a task uses,
//! the prompt for a target text, reading the model's answer, a word-level
//! diff of a revision, and finding the paragraph or sentence around a
//! position (a click in the editor or in the PDF preview).
//!
//! The target is sent with its protected spans (math, citations,
//! references) masked as `⟦n⟧`. A revision may drop or repeat one of them
//! when the instruction asks for it, so restoring is lenient and reports
//! what changed instead of failing.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffOp, capture_diff_slices};

use crate::glossary::GlossaryEntry;
use crate::lang::{Direction, is_cjk};
use crate::mode::Mode;
use crate::protect::{Protector, SpanKind, placeholder_at, spans};
use crate::segment::segment;

/// What the assistant is asked to do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Improve the target by the writing rules.
    #[default]
    Polish,
    /// Change the target as the instruction says.
    Edit,
    /// Answer a question about the target or the document.
    Ask,
    /// Write a figure or table (LaTeX or Markdown) to insert at the target.
    Figure,
    /// The author rewrote the paragraph's translation (the instruction):
    /// bring the paragraph in line with it.
    Mirror,
    /// Write new paragraphs as the instruction says, in the manner of the
    /// reference samples, to insert after the target.
    Write,
}

impl Action {
    /// Purpose shown in the request log.
    pub fn purpose(self) -> &'static str {
        match self {
            Self::Polish => "polish",
            Self::Edit => "edit",
            Self::Ask => "ask",
            Self::Figure => "figure",
            Self::Mirror => "mirror",
            Self::Write => "write",
        }
    }

    /// The answer is new text inserted after the target, not a revision of
    /// it.
    pub fn inserts(self) -> bool {
        matches!(self, Self::Figure | Self::Write)
    }

    /// The skill files the task's system prompt is built from, in order.
    pub fn skill_files(self) -> &'static [&'static str] {
        match self {
            Self::Polish | Self::Edit | Self::Mirror => &["writing-deai.md", "writing-playbook.md"],
            Self::Ask => &[
                "writing-playbook.md",
                "knowledge/paper-anatomy.md",
                "knowledge/storytelling.md",
                "writing-deai.md",
            ],
            Self::Figure => &[
                "figure-style/README.md",
                "knowledge/figure-archetypes.md",
                "figure-style/tables/README.md",
                "figure-style/tables/table_macros.tex",
                "writing-deai.md",
            ],
            Self::Write => &[
                "writing-deai.md",
                "writing-playbook.md",
                "knowledge/paper-anatomy.md",
                "knowledge/storytelling.md",
            ],
        }
    }
}

/// How much of the document goes with the target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The target only.
    #[default]
    Target,
    /// The target and the paragraphs around it.
    Neighbors,
    /// The whole document.
    Document,
}

/// A skill: its identity and the text of its files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    /// Where it comes from (a repository URL or a folder).
    pub source: String,
    /// Version shown to the user (commit, date).
    pub version: String,
    /// `(path in the skill, content)`.
    pub files: Vec<(String, String)>,
}

impl Skill {
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, c)| c.as_str())
    }
}

/// One assistant request.
#[derive(Clone, Debug, Default)]
pub struct Request<'a> {
    pub action: Action,
    pub mode: Mode,
    /// Which language the author is editing.
    pub direction: Direction,
    /// The text to revise, or the place a figure goes after (may be empty).
    pub target: &'a str,
    /// The edit instruction or the question (empty for polishing).
    pub instruction: &'a str,
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub document: Option<&'a str>,
    pub doc_note: Option<&'a str>,
    pub glossary: &'a [GlossaryEntry],
    /// Text samples whose style to follow.
    pub references: &'a [String],
    /// Number of attached reference images.
    pub images: usize,
    /// Earlier questions and answers in this conversation.
    pub history: &'a [(String, String)],
    /// The packages the document loads (for a figure), when known.
    pub packages: Option<&'a [String]>,
}

/// The messages for a request, and the protector that restores the answer.
#[derive(Debug)]
pub struct Prompt {
    pub system: String,
    pub user: String,
    pub protector: Protector,
}

fn language(direction: Direction, source: bool) -> &'static str {
    match (direction, source) {
        (Direction::EnZh, true) | (Direction::ZhEn, false) => "English",
        _ => "Chinese (Simplified)",
    }
}

fn markup(mode: Mode) -> &'static str {
    match mode {
        Mode::Latex => "LaTeX source",
        Mode::Markdown => "Markdown",
        Mode::Plain => "plain text",
    }
}

fn block(out: &mut String, tag: &str, body: &str) {
    out.push_str(&format!("<{tag}>\n{}\n</{tag}>\n\n", body.trim()));
}

/// The system prompt: the assistant's role and the skill's rules.
pub fn system_prompt(action: Action, direction: Direction, skill: &Skill) -> String {
    let mut out = format!(
        "You are the writing assistant of BiWrite, an editor in which an author writes an academic paper in {} and reads it in {} beside it. Apply the rules below. They come from the {} skill ({}, {}). They govern the wording you produce. The author decides what the paper claims, so keep the content.\n\n",
        language(direction, true),
        language(direction, false),
        skill.name,
        skill.source,
        skill.version
    );
    for path in action.skill_files() {
        if let Some(text) = skill.file(path).filter(|t| !t.trim().is_empty()) {
            out.push_str(&format!(
                "<skill_file path=\"{path}\">\n{}\n</skill_file>\n\n",
                text.trim()
            ));
        }
    }
    out.trim_end().to_owned()
}

/// Build the messages for `req` with the skill's rules.
pub fn build(req: &Request<'_>, skill: &Skill) -> Prompt {
    let action = req.action;
    let mut protector = Protector::new(req.mode);
    let masked = if action.inserts() {
        req.target.trim().to_owned()
    } else {
        protector.mask(req.target.trim())
    };
    let src = language(req.direction, true);
    let other = language(req.direction, false);

    let mut user = String::new();
    if let Some(note) = req.doc_note.filter(|n| !n.trim().is_empty()) {
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
    if let Some(doc) = req.document {
        block(&mut user, "document", doc);
    } else {
        if let Some(before) = req.before {
            block(&mut user, "context_before", before);
        }
        if let Some(after) = req.after {
            block(&mut user, "context_after", after);
        }
    }
    if action == Action::Figure
        && let Some(packages) = req.packages
    {
        block(&mut user, "loaded_packages", &packages.join(", "));
    }
    for (i, sample) in req.references.iter().enumerate() {
        user.push_str(&format!(
            "<reference_text n=\"{}\">\n{}\n</reference_text>\n\n",
            i + 1,
            sample.trim()
        ));
    }
    if !req.history.is_empty() {
        let turns: Vec<String> = req
            .history
            .iter()
            .map(|(q, a)| format!("Q: {}\nA: {}", q.trim(), a.trim()))
            .collect();
        block(&mut user, "conversation", &turns.join("\n\n"));
    }
    if !masked.is_empty() {
        block(&mut user, "target", &masked);
    }
    if !req.instruction.trim().is_empty() {
        block(&mut user, "instruction", req.instruction);
    }

    let kind = markup(req.mode);
    let task = match action {
        Action::Polish => format!(
            "Polish the text in <target>, {kind} written in {src}, by the rules. Change only what the rules require. Keep claims, numbers, citations and terms, and keep every part that already follows the rules word for word."
        ),
        Action::Edit => format!(
            "Revise the text in <target>, {kind} written in {src}, as <instruction> asks, and follow the rules in what you write. Change nothing the instruction does not require."
        ),
        Action::Ask => format!(
            "Answer the question in <instruction> about the text in <target>{}. Be concrete: quote the words you would change and say how. Answer in the language of the question.",
            if req.document.is_some() { " and the document" } else { "" }
        ),
        Action::Mirror => format!(
            "The author rewrote the {other} version of the text in <target>; the new {other} version is in <instruction>. Revise <target>, {kind} written in {src}, so that it says what the new {other} version says. Keep every part the new version does not change word for word, with its terms, commands, citations and math, and follow the rules in the wording you write."
        ),
        Action::Figure => match req.mode {
            Mode::Latex if req.packages.is_some() => "Write the LaTeX for the figure or table that <instruction> asks for, following the figure and table rules. It is inserted after the text in <target>. Give it a caption and a \\label. Prefer the packages in <loaded_packages>; beyond them use only common ones (tikz, pgfplots, booktabs, graphicx, multirow, xcolor), never \\usepackage, and name any package it needs that is not loaded in the change notes.".to_owned(),
            Mode::Latex => "Write the LaTeX for the figure or table that <instruction> asks for, following the figure and table rules. It is inserted after the text in <target>. Use only common packages (tikz, pgfplots, booktabs, graphicx), give it a caption and a \\label, and make it compile on its own inside the document.".to_owned(),
            _ => format!("Write the {kind} table or figure description that <instruction> asks for, following the figure and table rules. It is inserted after the text in <target>."),
        },
        Action::Write => format!(
            "Write new {kind} in {src} as <instruction> asks: the content, how many paragraphs and how long they are. It is inserted after the text in <target>, so it continues from there (<target> may be empty). Follow the rules. Write about the author's subject as the instruction and the context give it, and never invent results, numbers or citations the author did not give: leave a clearly marked placeholder such as [result] instead."
        ),
    };
    user.push_str(&task);
    if req.document.is_some() || req.before.is_some() || req.after.is_some() {
        user.push_str(" The document and context blocks are for reference: change only <target>.");
    }
    if !req.glossary.is_empty() {
        user.push_str(" Use the glossary renderings.");
    }
    if !req.references.is_empty() && action == Action::Write {
        user.push_str(" The <reference_text> blocks are papers whose writing to imitate closely: how a paragraph opens and closes, how claims are set up and supported, sentence patterns and length, the level of terms. Take their manner, not their content: never copy their sentences, claims, numbers or citations.");
    } else if !req.references.is_empty() {
        user.push_str(" The <reference_text> blocks show the style to follow (wording, sentence length, structure). Do not copy their content.");
    }
    if req.images > 0 && action == Action::Figure {
        user.push_str(" The attached images show the figure to imitate. Draw the same kind of chart or diagram with the same layout, colour scheme, marks, line styles, fonts and legend placement, in TikZ or pgfplots, with the data and labels the instruction and the paper give. Never take numbers or labels from the image unless the instruction says so.");
    } else if req.images > 0 {
        user.push_str(" The attached images are references the author chose: follow their layout and style where the task allows.");
    }
    if masked.contains('⟦') {
        let blocks = if action == Action::Mirror {
            "<revision>"
        } else {
            "<revision> and <translation>"
        };
        user.push_str(&format!(" Each ⟦n⟧ in <target> stands for protected text such as math, a citation or a reference: keep every one in {blocks} unchanged, unless the instruction asks to remove it, and never invent a new one."));
    }
    match req.mode {
        Mode::Latex => user.push_str(
            " The text is LaTeX source: keep its commands, environments and escapes such as \\% as they are.",
        ),
        Mode::Markdown => user.push_str(" The text is Markdown: keep its markup."),
        Mode::Plain => {}
    }
    let format = match action {
        Action::Mirror => format!(
            "\n\nReply in exactly this form:\n<revision>\nthe revised text in {src}, ready to replace <target>\n</revision>\n<changes_zh>\nat most three short lines in Chinese on what changed\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>"
        ),
        Action::Polish | Action::Edit => format!(
            "\n\nReply in exactly this form:\n<revision>\nthe revised text in {src}, ready to replace <target>\n</revision>\n<translation>\na faithful {other} translation of the revised text\n</translation>\n<changes_zh>\nat most three short lines in Chinese on what changed and why\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>\nIf nothing should change, return the text unchanged in <revision> and say so in the change notes."
        ),
        Action::Ask => format!(
            "\n\nReply with the answer in <answer>…</answer>. If the answer proposes a concrete rewrite of <target>, also give it in <revision> (in {src}) with <translation> ({other}), <changes_zh> and <changes_en>."
        ),
        Action::Figure => format!(
            "\n\nReply in exactly this form:\n<revision>\nthe source to insert\n</revision>\n<translation>\nthe caption in {other}\n</translation>\n<changes_zh>\nat most three short lines in Chinese describing the figure\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>"
        ),
        Action::Write => format!(
            "\n\nReply in exactly this form:\n<revision>\nthe new text in {src}, paragraphs separated by a blank line\n</revision>\n<translation>\na faithful {other} translation, paragraph for paragraph, separated the same way\n</translation>\n<changes_zh>\nat most three short lines in Chinese on what the text says and which features of the references it follows\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>"
        ),
    };
    user.push_str(&format);
    Prompt {
        system: system_prompt(action, req.direction, skill),
        user,
        protector,
    }
}

/// The model's answer, with protected spans restored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub revision: Option<String>,
    pub translation: Option<String>,
    pub changes_zh: Vec<String>,
    pub changes_en: Vec<String>,
    pub answer: Option<String>,
    /// Protected texts (math, citations) the revision no longer contains.
    pub removed: Vec<String>,
    /// Protected texts the revision repeats more often than the target.
    pub repeated: Vec<String>,
    /// The translation keeps exactly the revision's protected texts, so it
    /// can stand as the paragraph's translation.
    pub translation_matches: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssistError {
    /// The answer has no `<revision>` block.
    NoRevision,
    /// The answer refers to protected text that was never sent.
    UnknownPlaceholder(Vec<usize>),
}

impl fmt::Display for AssistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRevision => f.write_str("the model did not return a revision"),
            Self::UnknownPlaceholder(n) => write!(
                f,
                "the model referred to protected text that does not exist ({})",
                n.iter()
                    .map(|n| format!("⟦{n}⟧"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl std::error::Error for AssistError {}

/// Content of the first `<name>…</name>` block, trimmed and without a
/// surrounding code fence.
pub fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close).map_or(text.len(), |e| start + e);
    Some(strip_fence(text[start..end].trim()))
}

fn strip_fence(t: &str) -> &str {
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let Some(nl) = rest.find('\n') else {
        return t;
    };
    match rest[nl + 1..].trim_end().strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => t,
    }
}

fn lines(text: Option<&str>) -> Vec<String> {
    text.map(|t| {
        t.lines()
            .map(|l| {
                l.trim()
                    .trim_start_matches(['-', '*', '•'])
                    .trim()
                    .to_owned()
            })
            .filter(|l| !l.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

/// How often each placeholder number occurs in `text`.
fn placeholder_counts(text: &str) -> BTreeMap<usize, usize> {
    let mut counts = BTreeMap::new();
    let mut rest = text;
    while let Some(pos) = rest.find('⟦') {
        let tail = &rest[pos..];
        match placeholder_at(tail) {
            Some((len, n)) => {
                *counts.entry(n).or_insert(0) += 1;
                rest = &tail[len..];
            }
            None => rest = &tail['⟦'.len_utf8()..],
        }
    }
    counts
}

/// Read the model's answer for `action`.
pub fn parse(action: Action, text: &str, protector: &Protector) -> Result<Answer, AssistError> {
    let revision_raw = tag(text, "revision");
    let translation_raw = tag(text, "translation");
    if action != Action::Ask && revision_raw.is_none() {
        return Err(AssistError::NoRevision);
    }
    let mut answer = Answer {
        changes_zh: lines(tag(text, "changes_zh")),
        changes_en: lines(tag(text, "changes_en")),
        ..Answer::default()
    };
    if action == Action::Ask {
        let free = tag(text, "answer").map(str::to_owned).or_else(|| {
            // No tags at all: the whole reply is the answer.
            (!text.contains("<revision>")).then(|| text.trim().to_owned())
        });
        answer.answer = free.filter(|a| !a.is_empty());
    }
    let Some(revision) = revision_raw.filter(|r| !r.is_empty()) else {
        return Ok(answer);
    };
    if action.inserts() {
        answer.revision = Some(revision.to_owned());
        answer.translation = translation_raw.map(str::to_owned);
        answer.translation_matches = false;
        return Ok(answer);
    }
    let (restored, report) = protector
        .restore_lenient(revision)
        .map_err(|e| AssistError::UnknownPlaceholder(e.unknown))?;
    answer.revision = Some(restored);
    answer.removed = report.missing;
    answer.repeated = report.added;
    if let Some(translation) = translation_raw.filter(|t| !t.is_empty()) {
        let matches = placeholder_counts(translation) == placeholder_counts(revision);
        if let Ok((t, _)) = protector.restore_lenient(translation) {
            answer.translation = Some(t);
            answer.translation_matches = matches;
        }
    }
    Ok(answer)
}

/// The text shown while an answer streams in: the open or last block's
/// content with the tags removed.
pub fn streaming_preview(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let tail = &rest[lt..];
        let is_tag = [
            "revision",
            "translation",
            "changes_zh",
            "changes_en",
            "answer",
        ]
        .iter()
        .any(|name| {
            tail.starts_with(&format!("<{name}>")) || tail.starts_with(&format!("</{name}>"))
        });
        match tail.find('>') {
            Some(gt) if is_tag => {
                out.push('\n');
                rest = &tail[gt + 1..];
            }
            None if tail.len() < 16 => return out.trim().to_owned(), // tag still arriving
            _ => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// A piece of a word-level diff.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffPart {
    /// `equal`, `insert` or `delete`.
    pub kind: &'static str,
    pub text: String,
}

/// Words, whitespace runs, single CJK characters and single punctuation
/// marks, so a diff reads naturally in both languages.
fn diff_tokens(text: &str) -> Vec<&str> {
    let class = |c: char| {
        if c.is_whitespace() {
            0
        } else if c.is_alphanumeric() && !is_cjk(c) || c == '_' || c == '\\' {
            1
        } else {
            2
        }
    };
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let k = class(c);
        let mut end = i + c.len_utf8();
        if k != 2 {
            while let Some(&(j, d)) = chars.peek() {
                if class(d) != k {
                    break;
                }
                end = j + d.len_utf8();
                chars.next();
            }
        }
        out.push(&text[i..end]);
    }
    out
}

/// Word-level diff from `old` to `new`.
pub fn diff(old: &str, new: &str) -> Vec<DiffPart> {
    let a = diff_tokens(old);
    let b = diff_tokens(new);
    let mut out: Vec<DiffPart> = Vec::new();
    let mut push = |kind: &'static str, text: String| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(&text),
            _ => out.push(DiffPart { kind, text }),
        }
    };
    for op in capture_diff_slices(Algorithm::Patience, &a, &b) {
        match op {
            DiffOp::Equal { old_index, len, .. } => {
                push("equal", a[old_index..old_index + len].concat());
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => push("delete", a[old_index..old_index + old_len].concat()),
            DiffOp::Insert {
                new_index, new_len, ..
            } => push("insert", b[new_index..new_index + new_len].concat()),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                push("delete", a[old_index..old_index + old_len].concat());
                push("insert", b[new_index..new_index + new_len].concat());
            }
        }
    }
    out
}

/// Byte range of the translatable content (paragraph, heading title,
/// caption) containing `pos`, if any.
pub fn paragraph_at(text: &str, mode: Mode, pos: usize) -> Option<Range<usize>> {
    segment(text, mode)
        .into_iter()
        .filter(|s| s.kind.is_translatable() && !s.content(text).trim().is_empty())
        .find(|s| s.range.start <= pos && pos <= s.range.end)
        .map(|s| s.content)
}

/// The content of up to `n` translatable segments before and after
/// `range`, joined by blank lines.
pub fn neighbors(
    text: &str,
    mode: Mode,
    range: &Range<usize>,
    n: usize,
) -> (Option<String>, Option<String>) {
    let segs: Vec<_> = segment(text, mode)
        .into_iter()
        .filter(|s| s.kind.is_translatable() && !s.content(text).trim().is_empty())
        .collect();
    let before: Vec<&str> = segs
        .iter()
        .filter(|s| s.content.end <= range.start)
        .rev()
        .take(n)
        .map(|s| s.content(text))
        .collect();
    let after: Vec<&str> = segs
        .iter()
        .filter(|s| s.content.start >= range.end)
        .take(n)
        .map(|s| s.content(text))
        .collect();
    let join = |v: Vec<&str>| (!v.is_empty()).then(|| v.join("\n\n"));
    let mut before = before;
    before.reverse();
    (join(before), join(after))
}

/// Abbreviations after which a period does not end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g.", "i.e.", "et al.", "etc.", "vs.", "Fig.", "Figs.", "Eq.", "Eqs.", "Sec.", "Secs.",
    "Tab.", "Ref.", "Refs.", "No.", "cf.", "approx.", "resp.", "Dr.", "Prof.",
];

/// Byte range of the sentence containing `pos` inside its paragraph. A
/// sentence ends at `.`, `!` or `?` followed by whitespace (not after a
/// common abbreviation), or at `。`, `！`, `？`. Protected spans (math,
/// citations) never split a sentence. Outside any paragraph, `None`.
pub fn sentence_at(text: &str, mode: Mode, pos: usize) -> Option<Range<usize>> {
    let para = paragraph_at(text, mode, pos)?;
    let body = &text[para.clone()];
    let shielded: Vec<Range<usize>> = spans(body, mode)
        .into_iter()
        .filter(|s| s.kind != SpanKind::Comment)
        .map(|s| s.range)
        .collect();
    let inside = |i: usize| shielded.iter().any(|r| r.start <= i && i < r.end);
    let mut ends = Vec::new();
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    for (k, &(i, c)) in chars.iter().enumerate() {
        if inside(i) {
            continue;
        }
        let end = i + c.len_utf8();
        let closes = match c {
            '。' | '！' | '？' => true,
            '.' | '!' | '?' => {
                let next_is_space = chars.get(k + 1).is_none_or(|&(_, n)| n.is_whitespace());
                let abbrev = c == '.' && ABBREVIATIONS.iter().any(|a| body[..end].ends_with(a));
                next_is_space && !abbrev
            }
            _ => false,
        };
        if closes {
            ends.push(end);
        }
    }
    let rel = pos.saturating_sub(para.start).min(body.len());
    let mut start = 0;
    for end in ends.iter().copied().chain(std::iter::once(body.len())) {
        if rel < end || end == body.len() {
            let slice = &body[start..end];
            let lead = slice.len() - slice.trim_start().len();
            let trail = slice.len() - slice.trim_end().len();
            return Some(para.start + start + lead..para.start + end - trail);
        }
        start = end;
    }
    None
}

#[cfg(test)]
#[path = "assist_tests.rs"]
mod tests;
