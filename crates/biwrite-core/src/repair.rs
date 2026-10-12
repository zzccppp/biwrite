//! Repairing LaTeX that does not compile. The model gets the errors TeX
//! reported, each with the lines TeX printed after it, and the source with
//! line numbers. For the whole document it answers with replacements of
//! exact lines. For text the assistant just wrote, it answers with that
//! text corrected and the lines it needs before `\begin{document}`.

use std::ops::Range;

use serde::Serialize;
use similar::{Algorithm, DiffOp, capture_diff_slices};

use crate::assist::{DiffPart, block, diff, lines, strip_fence, tag};

/// An error TeX reported.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TexError {
    /// 1-based line of the repaired text where TeX found it, when it is in
    /// that text.
    pub line: Option<usize>,
    /// The file it was found in, as the log names it.
    pub file: Option<String>,
    pub message: String,
    /// What TeX printed after the message, up to the line it stopped at.
    pub context: String,
}

/// Longest document sent whole, in characters. A longer one goes as its
/// preamble and the lines around the errors.
const WHOLE_DOCUMENT_CHARS: usize = 80_000;
/// Lines shown before and after an error of a longer document.
const AROUND: usize = 30;
/// Most errors sent.
const MAX_ERRORS: usize = 12;

const SYSTEM: &str = "You are the LaTeX assistant of BiWrite, an editor for academic papers. You fix LaTeX source that does not compile, from the errors TeX reported. Change as little as possible. Keep the author's text, numbers, citations, labels and the look of every figure and table, and never delete content to make an error go away.";

/// The lines of `text` (without their line breaks).
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Lines `range` (0-based) of `lines`, numbered from 1.
fn numbered(lines: &[&str], range: Range<usize>) -> String {
    let width = lines.len().to_string().len().max(3);
    range
        .map(|i| format!("{:>width$}| {}", i + 1, lines[i]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 0-based line of the first `\begin{document}` outside a comment.
fn begin_document_line(lines: &[&str]) -> Option<usize> {
    lines.iter().position(|l| {
        let code = l.find('%').map_or(*l, |i| &l[..i]);
        code.contains("\\begin{document}")
    })
}

/// `text` with line numbers: whole when it is short, else its preamble and
/// the lines around `at` (1-based), `…` marking what is left out.
fn excerpt(text: &str, at: &[usize]) -> String {
    let lines = split_lines(text);
    if text.chars().count() <= WHOLE_DOCUMENT_CHARS {
        return numbered(&lines, 0..lines.len());
    }
    let n = lines.len();
    let mut spans: Vec<Range<usize>> = Vec::new();
    if let Some(begin) = begin_document_line(&lines) {
        spans.push(0..begin + 1);
    }
    for &line in at {
        let i = line.saturating_sub(1).min(n.saturating_sub(1));
        spans.push(i.saturating_sub(AROUND)..(i + AROUND + 1).min(n));
    }
    spans.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in spans {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
        .into_iter()
        .map(|r| numbered(&lines, r))
        .collect::<Vec<_>>()
        .join("\n…\n")
}

fn errors_block(errors: &[TexError]) -> String {
    errors
        .iter()
        .take(MAX_ERRORS)
        .map(|e| {
            let place = match (e.line, &e.file) {
                (Some(line), _) => format!("line {line}: "),
                (None, Some(file)) => format!("in {file}: "),
                (None, None) => String::new(),
            };
            let mut out = format!("{place}{}", e.message);
            if !e.context.trim().is_empty() {
                out.push('\n');
                out.push_str(e.context.trim_end());
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// What a message means when its cause is not on the line TeX names.
fn hints(errors: &[TexError]) -> String {
    let says = |s: &str| {
        errors
            .iter()
            .any(|e| e.message.contains(s) || e.context.contains(s))
    };
    let mut out = Vec::new();
    if says("Undefined color model") {
        out.push(" \"Undefined color model\" in TikZ comes from a color the class or a style file defined before xcolor was loaded. Use colors defined after xcolor is loaded, or define new ones with \\definecolor.");
    }
    if says("I do not know the key") || says("Unknown arrow tip kind") {
        out.push(" An unknown TikZ key or arrow tip usually means its library is not loaded (positioning for right=of, arrows.meta for tips such as Stealth, fit, calc, backgrounds, shapes.geometric); load it with \\usetikzlibrary in the preamble.");
    }
    out.concat()
}

/// A block of numbered lines: only the line breaks around it are trimmed,
/// so the numbers stay aligned.
fn numbered_block(out: &mut String, tag: &str, body: &str) {
    out.push_str(&format!(
        "<{tag}>\n{}\n</{tag}>\n\n",
        body.trim_matches('\n')
    ));
}

fn packages_block(user: &mut String, packages: Option<&[String]>) {
    if let Some(p) = packages.filter(|p| !p.is_empty()) {
        block(user, "loaded_packages", &p.join(", "));
    }
}

/// A request to make the whole document compile.
#[derive(Clone, Debug, Default)]
pub struct FixRequest<'a> {
    pub text: &'a str,
    pub errors: &'a [TexError],
    /// What the author asks for besides (may be empty).
    pub instruction: &'a str,
    /// The packages the document loads, when known.
    pub packages: Option<&'a [String]>,
}

const EDIT_FORMAT: &str = "\n\nReply with one <edit> block per change:\n<edit>\n<find>\nthe lines to change, copied exactly from <document> without their numbers, as many as it takes to occur only once\n</find>\n<replace>\nthe lines to put in their place\n</replace>\n</edit>\nTo add lines, put the line before them in <find> and repeat it at the start of <replace>. After the edits:\n<changes_zh>\nat most three short lines in Chinese on what was wrong and what changed\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>\nAn error that cannot be fixed in this file gets no edit; say why in the change notes.";

/// The system and user messages for `req`.
pub fn fix_prompt(req: &FixRequest<'_>) -> (String, String) {
    let at: Vec<usize> = req.errors.iter().filter_map(|e| e.line).collect();
    let mut user = String::new();
    packages_block(&mut user, req.packages);
    numbered_block(&mut user, "document", &excerpt(req.text, &at));
    block(&mut user, "errors", &errors_block(req.errors));
    let instructed = !req.instruction.trim().is_empty();
    if instructed {
        block(&mut user, "instruction", req.instruction);
    }
    user.push_str("Fix the errors in <errors> so that the document compiles. The numbers before | in <document> are line numbers for reference, not part of the text. Make only the changes the errors need, such as a missing \\usepackage or \\usetikzlibrary in the preamble, a color or command that is not defined, a misspelled command or an unbalanced brace.");
    if instructed {
        user.push_str(" Follow <instruction> as well.");
    }
    user.push_str(&hints(req.errors));
    user.push_str(EDIT_FORMAT);
    (SYSTEM.to_owned(), user)
}

/// A request to make text the assistant wrote compile where it was put.
#[derive(Clone, Debug, Default)]
pub struct AnswerFixRequest<'a> {
    /// The document with the text put in.
    pub text: &'a str,
    /// Where the text is in `text` (bytes).
    pub answer: Range<usize>,
    /// The lines added before `\begin{document}` for it, in `text` (bytes),
    /// when the preamble is in this file; `None` when it is in another one.
    pub preamble: Option<Range<usize>>,
    pub errors: &'a [TexError],
    pub packages: Option<&'a [String]>,
}

/// 1-based line of byte `at` of `text`.
pub fn line_at(text: &str, at: usize) -> usize {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count()
        + 1
}

/// The 1-based lines that the bytes `range` of `text` cover.
pub fn lines_of(text: &str, range: &Range<usize>) -> Range<usize> {
    let first = line_at(text, range.start);
    let end = range.end.max(range.start);
    // A range ending with its line break ends on the line before.
    let last = if end > range.start && text.as_bytes().get(end - 1) == Some(&b'\n') {
        line_at(text, end - 1)
    } else {
        line_at(text, end)
    };
    first..last.max(first) + 1
}

/// The system and user messages for `req`.
pub fn answer_fix_prompt(req: &AnswerFixRequest<'_>) -> (String, String) {
    let lines = split_lines(req.text);
    let answer = lines_of(req.text, &req.answer);
    let mut user = String::new();
    packages_block(&mut user, req.packages);
    if let Some(begin) = begin_document_line(&lines) {
        numbered_block(&mut user, "preamble", &numbered(&lines, 0..begin + 1));
    }
    let shown = (answer.start - 1).min(lines.len())..(answer.end - 1).min(lines.len());
    numbered_block(&mut user, "inserted", &numbered(&lines, shown));
    let fitting = req.errors.is_empty();
    if !fitting {
        block(&mut user, "errors", &errors_block(req.errors));
    }
    let added = req
        .preamble
        .as_ref()
        .filter(|r| r.end > r.start)
        .map(|r| lines_of(req.text, r));
    user.push_str("The text in <inserted> was just put into the document at the lines shown");
    if let Some(added) = &added {
        user.push_str(&format!(
            ", and lines {} to {} of <preamble> were added for it",
            added.start,
            added.end - 1
        ));
    }
    user.push_str(". The numbers before | are line numbers for reference, not part of the text.");
    if fitting {
        // Before it is applied: fitted to the document, not repaired.
        user.push_str(" Before it is applied, fit it to this document so that it compiles here: use the packages, colours and commands the document has, define only what it lacks, keep its \\label unlike the document's other labels, and give TikZ libraries and other preamble lines it needs");
        if req.preamble.is_some() {
            user.push_str(" in the lines added before \\begin{document}. Those lines may load packages (\\usepackage) and TikZ libraries (\\usetikzlibrary) and define colors and commands.");
        } else {
            user.push_str(
                " inside it, since the preamble is in another file and cannot be changed.",
            );
        }
        user.push_str(" Change nothing else, and when it fits as it is, return it unchanged.");
    } else {
        user.push_str(" The document now fails to compile with the errors in <errors>, which it did not have before. Fix them");
        if req.preamble.is_some() {
            user.push_str(" in <inserted> and in the lines added before \\begin{document}. Those lines may load packages (\\usepackage) and TikZ libraries (\\usetikzlibrary) and define colors and commands.");
        } else {
            user.push_str(" in <inserted>. The preamble is in another file and cannot be changed, so make <inserted> work with what is loaded and define the colors and commands it needs inside it.");
        }
    }
    user.push_str(
        " Keep what <inserted> shows and says: its content, data, labels, caption and \\label.",
    );
    user.push_str(&hints(req.errors));
    user.push_str("\n\nReply in exactly this form:\n<revision>\nthe corrected text, all of it, without line numbers, to put in place of <inserted>\n</revision>\n");
    if req.preamble.is_some() {
        user.push_str("<preamble>\nall the lines to add before \\begin{document} for it, or nothing\n</preamble>\n");
    }
    if fitting {
        user.push_str("<changes_zh>\nat most three short lines in Chinese on what changed, or nothing\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>");
    } else {
        user.push_str("<changes_zh>\nat most three short lines in Chinese on what was wrong and what changed\n</changes_zh>\n<changes_en>\nthe same in English\n</changes_en>");
    }
    (SYSTEM.to_owned(), user)
}

/// The model's corrected answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AnswerFix {
    pub revision: Option<String>,
    /// The lines to add before `\begin{document}` (empty for none), when
    /// the reply gives them.
    pub preamble: Option<String>,
    pub changes_zh: Vec<String>,
    pub changes_en: Vec<String>,
}

/// Read the reply to an [`answer_fix_prompt`].
pub fn parse_answer_fix(reply: &str) -> AnswerFix {
    let revision = tag(reply, "revision")
        .map(unnumbered)
        .filter(|r| !r.trim().is_empty());
    let preamble = tag(reply, "preamble").map(|p| {
        let p = unnumbered(p);
        if p.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n", p.trim_end())
        }
    });
    AnswerFix {
        revision,
        preamble,
        changes_zh: lines(tag(reply, "changes_zh")),
        changes_en: lines(tag(reply, "changes_en")),
    }
}

/// A change of exact lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replacement {
    pub find: String,
    pub replace: String,
}

/// `text` without the line numbers of [`numbered`], when every line that
/// is not blank has one.
fn unnumbered(text: &str) -> String {
    let number = |l: &str| -> Option<usize> {
        let t = l.trim_start();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && t[digits..].starts_with('|')).then(|| l.len() - t.len() + digits + 1)
    };
    let all = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .all(|l| number(l).is_some());
    if !all || text.trim().is_empty() {
        return text.to_owned();
    }
    text.split('\n')
        .map(|l| match number(l) {
            Some(cut) => {
                let rest = &l[cut..];
                rest.strip_prefix(' ').unwrap_or(rest)
            }
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The content of `<name>…</name>` in `body`, without the line breaks right
/// inside the tags and without a code fence, indentation kept.
fn inner(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close).map_or(body.len(), |e| start + e);
    let mut content = &body[start..end];
    content = content
        .strip_prefix("\r\n")
        .or_else(|| content.strip_prefix('\n'))
        .unwrap_or(content);
    content = content.strip_suffix('\n').unwrap_or(content);
    content = content.strip_suffix('\r').unwrap_or(content);
    let fenced = content.trim_start().starts_with("```");
    Some(if fenced {
        strip_fence(content.trim()).to_owned()
    } else {
        content.to_owned()
    })
}

/// Read the replacements of a reply to a [`fix_prompt`].
pub fn parse_replacements(reply: &str) -> Vec<Replacement> {
    let mut out = Vec::new();
    let mut rest = reply;
    while let Some(start) = rest.find("<edit>") {
        let after = &rest[start + "<edit>".len()..];
        let end = after.find("</edit>").unwrap_or(after.len());
        let body = &after[..end];
        if let (Some(find), Some(replace)) = (inner(body, "find"), inner(body, "replace")) {
            out.push(Replacement {
                find: unnumbered(&find),
                replace: unnumbered(&replace),
            });
        }
        rest = &after[(end + "</edit>".len()).min(after.len())..];
    }
    out
}

/// Byte ranges of whole lines of `text` that read as `find` line for line,
/// leading and trailing spaces aside.
fn loose_matches(text: &str, find: &str) -> Vec<Range<usize>> {
    let wanted: Vec<&str> = find.lines().map(str::trim).collect();
    if wanted.is_empty() || wanted.iter().all(|l| l.is_empty()) {
        return Vec::new();
    }
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let lines: Vec<(usize, &str)> = starts
        .iter()
        .map(|&s| {
            let e = text[s..].find('\n').map_or(text.len(), |i| s + i);
            (s, &text[s..e])
        })
        .collect();
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if i + wanted.len() > lines.len() {
            break;
        }
        if wanted
            .iter()
            .enumerate()
            .all(|(k, w)| lines[i + k].1.trim() == *w)
        {
            let (s, _) = lines[i];
            let (ls, last) = lines[i + wanted.len() - 1];
            out.push(s..ls + last.len());
        }
    }
    out
}

/// `text` with the replacements made, each where its lines are, exactly or
/// line for line with spaces at the ends aside. Of several places, the one
/// nearest a line of `near` (1-based) is taken, and without `near` none.
/// Also the indices of the replacements whose lines were not found once.
pub fn apply_replacements(
    text: &str,
    replacements: &[Replacement],
    near: &[usize],
) -> (String, Vec<usize>) {
    let mut out = text.to_owned();
    let mut missed = Vec::new();
    for (k, r) in replacements.iter().enumerate() {
        if r.find.trim().is_empty() {
            missed.push(k);
            continue;
        }
        let mut places: Vec<Range<usize>> = out
            .match_indices(r.find.as_str())
            .map(|(i, m)| i..i + m.len())
            .collect();
        if places.is_empty() {
            places = loose_matches(&out, &r.find);
        }
        let chosen = match places.len() {
            0 => None,
            1 => places.pop(),
            _ if near.is_empty() => None,
            _ => places.into_iter().min_by_key(|p| {
                let line = line_at(&out, p.start);
                near.iter()
                    .map(|n| n.abs_diff(line))
                    .min()
                    .unwrap_or(usize::MAX)
            }),
        };
        match chosen {
            Some(range) => out.replace_range(range, &r.replace),
            None => missed.push(k),
        }
    }
    (out, missed)
}

/// One change between two versions of a text, in whole lines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hunk {
    /// 1-based line of the old text where the change starts.
    pub line: usize,
    pub old: String,
    pub new: String,
    /// Up to two lines before and after the change in the old text, to
    /// find its place again.
    pub before: String,
    pub after: String,
    /// Word-level diff from `old` to `new`.
    pub diff: Vec<DiffPart>,
}

/// Unchanged lines that still part two changes into separate hunks.
const HUNK_GAP: usize = 2;

/// The changes from `old` to `new`, in whole lines, in order.
pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    // Changed spans: (old lines, new lines).
    let mut spans: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        let (o, n) = match op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Delete {
                old_index,
                old_len,
                new_index,
            } => (old_index..old_index + old_len, new_index..new_index),
            DiffOp::Insert {
                old_index,
                new_index,
                new_len,
            } => (old_index..old_index, new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => (
                old_index..old_index + old_len,
                new_index..new_index + new_len,
            ),
        };
        match spans.last_mut() {
            Some((lo, ln)) if o.start <= lo.end + HUNK_GAP && n.start <= ln.end + HUNK_GAP => {
                lo.end = o.end.max(lo.end);
                ln.end = n.end.max(ln.end);
            }
            _ => spans.push((o, n)),
        }
    }
    spans
        .into_iter()
        .map(|(o, n)| {
            let old_part = a[o.clone()].concat();
            let new_part = b[n].concat();
            Hunk {
                line: o.start + 1,
                before: a[o.start.saturating_sub(2)..o.start].concat(),
                after: a[o.end..(o.end + 2).min(a.len())].concat(),
                diff: diff(&old_part, &new_part),
                old: old_part,
                new: new_part,
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "repair_tests.rs"]
mod tests;
