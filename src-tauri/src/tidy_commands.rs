//! Smart revert: once an inserted figure is taken out, which of the lines it
//! added before `\begin{document}` can go too. The model reads the document
//! and the lines and decides line by line. Before the lines it picks go,
//! the document is built without them (in `.biwrite`, nothing saved): when
//! that brings an error the document did not have, every line stays. When
//! the model cannot be reached, every line stays, as a plain revert leaves
//! them.

use std::collections::HashMap;

use biwrite_core::Mode;
use biwrite_core::repair::TexError;
use biwrite_latex::tidy::{place_of, statements};
use biwrite_providers::{ChatModel, ChatRequest};
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::{CommandError, CommandResult};
use crate::latex_commands::check_build;
use crate::state::AppState;

/// Where the copies are built, apart from the checks of Accept.
const TIDY_FOLDER: &str = ".biwrite/tidy";
const BEFORE_FOLDER: &str = ".biwrite/tidy-before";
/// Most characters of the document the model reads.
const MAX_DOCUMENT_CHARS: usize = 120_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TidyRequest {
    /// The editor's text, the inserted figure taken out.
    pub text: String,
    /// The lines it added before `\begin{document}`.
    pub lines: String,
}

/// Which added lines go and which stay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tidied {
    /// Statements to take out (each a line or a block of lines).
    pub remove: Vec<String>,
    /// Statements that stay: the document still needs them, or they are
    /// not in the preamble as they were added (edited since).
    pub kept: Vec<String>,
}

/// `after`'s errors beyond those `before` had, counted by message (the
/// lines move when lines are taken out).
fn new_errors(after: &[TexError], before: &[TexError]) -> usize {
    let mut had: HashMap<&str, usize> = HashMap::new();
    for e in before {
        *had.entry(e.message.as_str()).or_insert(0) += 1;
    }
    after
        .iter()
        .filter(|e| match had.get_mut(e.message.as_str()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .count()
}

/// `text` without `statements` (each found once in its preamble).
fn without(text: &str, statements: &[String]) -> String {
    let mut ranges: Vec<_> = statements
        .iter()
        .filter_map(|s| place_of(text, s))
        .collect();
    ranges.sort_by_key(|r| std::cmp::Reverse(r.start));
    let mut out = text.to_owned();
    for r in ranges {
        out.replace_range(r, "");
    }
    out
}

/// The messages for the model: the document and the numbered lines.
fn prompt(text: &str, lines: &[String]) -> (String, String) {
    let system = "You check a LaTeX document. A figure that was inserted into it has been taken out again, and the lines it had added before \\begin{document} may no longer be needed. For each of those lines, decide whether anything left in the document needs what the line loads or defines: a package's commands and environments, a TikZ library's syntax, a colour's name, a command, a style. A line needed only by other lines of the list is not needed when they all go. Keep a line when anything else needs it or when you are not sure.".to_owned();
    let mut document: String = text.chars().take(MAX_DOCUMENT_CHARS).collect();
    if document.len() < text.len() {
        document.push_str("\n[…]");
    }
    let numbered: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}. {}", i + 1, l.replace('\n', " ")))
        .collect();
    let user = format!(
        "<document>\n{document}\n</document>\n\n<lines>\n{}\n</lines>\n\nReply with one line for each numbered line, `n keep` or `n remove`, and nothing else.",
        numbered.join("\n")
    );
    (system, user)
}

/// The model's verdict: the numbers (from 1) of the lines to remove, or
/// `None` when the reply names no line at all.
fn parse(reply: &str, count: usize) -> Option<Vec<usize>> {
    let mut named = false;
    let mut remove = Vec::new();
    for line in reply.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
        let Ok(n) = digits.parse::<usize>() else {
            continue;
        };
        if n == 0 || n > count {
            continue;
        }
        let word = line[digits.len()..]
            .trim_start_matches(['.', ':', ')', ' '])
            .to_ascii_lowercase();
        if word.starts_with("remove") {
            named = true;
            if !remove.contains(&n) {
                remove.push(n);
            }
        } else if word.starts_with("keep") {
            named = true;
        }
    }
    named.then_some(remove)
}

/// Which of `lines` can go from `text` (see the module). An error means the
/// model could not decide, and the caller keeps every line.
pub(crate) async fn tidy(
    state: &AppState,
    model: &dyn ChatModel,
    text: &str,
    lines: &str,
) -> CommandResult<Tidied> {
    let mut result = Tidied::default();
    // In the order they were added; one edited since stays.
    let mut asked = Vec::new();
    for statement in statements(lines) {
        if place_of(text, &statement).is_some() {
            asked.push(statement);
        } else {
            result.kept.push(statement);
        }
    }
    if asked.is_empty() {
        return Ok(result);
    }
    // The whole paper of a project of several files: its sections may use
    // what the root's preamble loads.
    let paper = crate::latex_commands::whole_paper(state, text, 0).map(|(paper, _)| paper);
    let (system, user) = prompt(paper.as_deref().unwrap_or(text), &asked);
    let chat = ChatRequest {
        purpose: "revert".to_owned(),
        system,
        user,
        images: Vec::new(),
    };
    let out = crate::repair_commands::ask(None, 0, model, &chat)
        .await
        .map_err(|e| CommandError::Settings(format!("the model could not check the lines: {e}")))?;
    let numbers = parse(&out.text, asked.len()).ok_or_else(|| {
        CommandError::Settings("the model's reply named none of the lines".into())
    })?;
    let take: Vec<String> = numbers.iter().map(|n| asked[n - 1].clone()).collect();
    // Never a document that no longer compiles: built without them first.
    let breaks = if take.is_empty() {
        false
    } else {
        match check_build(state, &without(text, &take), TIDY_FOLDER).await {
            Ok(checked) if checked.errors.is_empty() => false,
            Ok(checked) => match check_build(state, text, BEFORE_FOLDER).await {
                Ok(before) => new_errors(&checked.errors, &before.errors) > 0,
                Err(e) => {
                    log::info!("smart revert: no build of the document as it is ({e})");
                    false
                }
            },
            // Sections the model may have judged wrongly: every line stays.
            Err(e) if paper.is_some() => {
                log::info!("smart revert: no build of the paper, every line stays ({e})");
                true
            }
            Err(e) => {
                log::info!("smart revert: no build, the model decides ({e})");
                false
            }
        }
    };
    if breaks {
        log::info!(
            "smart revert: without the lines the model picked the document does not compile"
        );
        result.kept.extend(asked);
        return Ok(result);
    }
    result
        .kept
        .extend(asked.into_iter().filter(|s| !take.contains(s)));
    result.remove = take;
    Ok(result)
}

/// Which lines an inserted figure added before `\begin{document}` can go
/// with it (see the module).
#[tauri::command]
pub async fn assist_tidy(
    state: State<'_, AppState>,
    request: TidyRequest,
) -> CommandResult<Tidied> {
    if state.engine.mode() != Mode::Latex {
        return Err(CommandError::Settings(
            "only a LaTeX document can be compiled".into(),
        ));
    }
    let model = state.assistant_model()?;
    tidy(&state, model.as_ref(), &request.text, &request.lines).await
}

#[cfg(test)]
#[path = "tidy_tests.rs"]
mod tests;
