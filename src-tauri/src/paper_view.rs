//! The whole paper of a project of several files for the translation pane:
//! the files the main file includes, to any depth and in order, around the
//! open document, each paragraph with its translation as known. That is
//! the paragraph a paired file in the other language has for it, else the
//! translation in the cache. Nothing is requested.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use biwrite_core::pair::{align, units};
use biwrite_core::utf16::byte_to_utf16;
use biwrite_core::{Direction, Mode};
use biwrite_latex as latex;
use serde::Serialize;
use tauri::State;

use crate::error::CommandResult;
use crate::latex_commands::{canonical, project, relative};
use crate::state::AppState;

/// Most bytes of the files read for the view.
const BUDGET: usize = 4 << 20;
/// Deepest chain of includes followed.
const DEPTH: usize = 6;

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperView {
    /// The paper before the open document, and after it.
    pub before: Vec<PaperPart>,
    pub after: Vec<PaperPart>,
    /// What the open document includes, by the name its include gives
    /// (without `./` and `.tex`), with what that includes in turn.
    pub inside: Vec<Inside>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inside {
    pub name: String,
    pub parts: Vec<PaperPart>,
}

/// Paragraphs of one file in a row.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperPart {
    /// Relative to the project folder.
    pub file: String,
    pub blocks: Vec<PaperBlock>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperBlock {
    /// UTF-16 range of the paragraph in its file as saved.
    pub from: usize,
    pub to: usize,
    pub heading: bool,
    pub source: String,
    /// In the language the pane shows; `None` when not known.
    pub translation: Option<String>,
}

enum Item {
    Part(PaperPart),
    Open,
    Inside(Inside),
}

struct Walk<'a> {
    state: &'a AppState,
    dir: PathBuf,
    open: PathBuf,
    editor: &'a str,
    /// The language the pane shows is Chinese.
    zh: bool,
    seen: HashSet<PathBuf>,
    budget: usize,
}

/// The include commands of `text` outside comments: where each ends and
/// the name it gives.
fn includes(text: &str) -> Vec<(usize, usize, String)> {
    const COMMANDS: [&str; 3] = ["\\input{", "\\include{", "\\subfile{"];
    let mut out = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let end = latex::project::uncommented(line).len().min(line.len());
        let mut at = 0;
        while let Some((i, len)) = COMMANDS
            .iter()
            .filter_map(|c| line[at..end].find(c).map(|i| (at + i, c.len())))
            .min_by_key(|(i, _)| *i)
        {
            let Some(close) = line[i + len..end].find('}') else {
                break;
            };
            let name = line[i + len..i + len + close].trim().to_owned();
            out.push((start + i, start + i + len + close + 1, name));
            at = i + len + close + 1;
        }
        start += line.len();
    }
    out
}

/// The name as the view gives it: without `./` and `.tex`.
fn plain_name(name: &str) -> String {
    name.trim()
        .trim_start_matches("./")
        .trim_end_matches(".tex")
        .to_owned()
}

impl Walk<'_> {
    /// The project file `name` names, once.
    fn find(&mut self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains('\\') || name.contains('#') {
            return None;
        }
        let plain = self.dir.join(name);
        let path = if plain.extension().is_some() && plain.is_file() {
            plain
        } else {
            self.dir.join(format!("{name}.tex"))
        };
        let path = canonical(&path);
        (path.starts_with(&self.dir) && path.is_file() && self.seen.insert(path.clone()))
            .then_some(path)
    }

    fn read(&mut self, path: &Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        self.budget = self.budget.checked_sub(bytes.len())?;
        Some(
            biwrite_core::TextFile::decode(bytes)
                .ok()?
                .text()
                .to_owned(),
        )
    }

    /// The paragraphs of `text` (of `file`) with their translations.
    fn blocks(&self, file: &Path, text: &str) -> Vec<(usize, PaperBlock)> {
        let list = units(text, Mode::Latex);
        let chinese = crate::pairing::is_chinese(text);
        let translations: HashMap<usize, String> = if chinese == self.zh {
            // Already in the pane's language.
            HashMap::new()
        } else {
            let toward = if self.zh {
                Direction::EnZh
            } else {
                Direction::ZhEn
            };
            // Its own file in that language, paragraph for paragraph.
            let paired = crate::pairing::mirror_path(self.state, file)
                .and_then(|m| std::fs::read(m).ok())
                .and_then(|b| biwrite_core::TextFile::decode(b).ok())
                .map(|f| f.text().to_owned())
                .filter(|t| crate::pairing::is_chinese(t) == self.zh);
            match paired {
                Some(other) => {
                    let theirs = units(&other, Mode::Latex);
                    align(text, &other, Mode::Latex)
                        .pairs
                        .into_iter()
                        .filter_map(|(i, j)| {
                            let unit = theirs.get(j)?;
                            Some((list.get(i)?.segment, other[unit.content.clone()].to_owned()))
                        })
                        .collect()
                }
                None => self
                    .state
                    .engine
                    .cached_translations(text, Mode::Latex, toward),
            }
        };
        list.iter()
            .map(|u| {
                let source = text[u.content.clone()].to_owned();
                let translation = if chinese == self.zh {
                    Some(source.clone())
                } else {
                    translations.get(&u.segment).cloned()
                };
                let r = byte_to_utf16(text, [u.content.start, u.content.end]);
                (
                    u.range.start,
                    PaperBlock {
                        from: r[0],
                        to: r[1],
                        heading: matches!(u.kind, biwrite_core::SegmentKind::Heading { .. }),
                        source,
                        translation,
                    },
                )
            })
            .collect()
    }

    /// `file` (its text `text`) as items, with the files it includes in
    /// their place, to `depth` levels.
    fn walk(&mut self, file: &Path, text: &str, depth: usize) -> Vec<Item> {
        let mut items = Vec::new();
        let found: Vec<(usize, usize, String)> = includes(text);
        if *file == self.open {
            items.push(Item::Open);
            for (_, _, name) in found {
                let Some(sub) = (depth > 0).then(|| self.find(&name)).flatten() else {
                    continue;
                };
                let Some(sub_text) = self.read(&sub) else {
                    continue;
                };
                let inner = self.walk(&sub, &sub_text, depth - 1);
                items.push(Item::Inside(Inside {
                    name: plain_name(&name),
                    parts: parts_of(inner),
                }));
            }
            return items;
        }
        let rel = relative(file, &self.dir).unwrap_or_default();
        let mut blocks = self.blocks(file, text).into_iter().peekable();
        let mut run: Vec<PaperBlock> = Vec::new();
        for (at, _, name) in found {
            while let Some((_, block)) = blocks.next_if(|(start, _)| *start < at) {
                run.push(block);
            }
            let Some(sub) = (depth > 0).then(|| self.find(&name)).flatten() else {
                continue;
            };
            let sub_text = if sub == self.open {
                self.editor.to_owned()
            } else {
                let Some(t) = self.read(&sub) else {
                    continue;
                };
                t
            };
            if !run.is_empty() {
                items.push(Item::Part(PaperPart {
                    file: rel.clone(),
                    blocks: std::mem::take(&mut run),
                }));
            }
            items.extend(self.walk(&sub, &sub_text, depth - 1));
        }
        run.extend(blocks.map(|(_, b)| b));
        if !run.is_empty() {
            items.push(Item::Part(PaperPart {
                file: rel,
                blocks: run,
            }));
        }
        items
    }
}

/// The parts among `items` (what an include of the open document holds).
fn parts_of(items: Vec<Item>) -> Vec<PaperPart> {
    items
        .into_iter()
        .filter_map(|i| match i {
            Item::Part(p) => Some(p),
            _ => None,
        })
        .collect()
}

/// The whole paper around the open document `text` (see the module), or
/// `None` outside a LaTeX project of several files.
pub(crate) fn paper_view(state: &AppState, text: &str) -> Option<PaperView> {
    if state.engine.mode() != Mode::Latex {
        return None;
    }
    let p = project(state).ok()?;
    let zh = state.engine.direction() == Direction::EnZh;
    let mut walk = Walk {
        state,
        dir: canonical(&p.dir),
        open: canonical(&p.path),
        editor: text,
        zh,
        seen: HashSet::new(),
        budget: BUDGET,
    };
    let root = canonical(&p.root);
    walk.seen.insert(root.clone());
    let root_text = if root == walk.open {
        text.to_owned()
    } else {
        p.root_text.clone()
    };
    let items = walk.walk(&root, &root_text, DEPTH);
    let mut view = PaperView::default();
    let mut passed = false;
    for item in items {
        match item {
            Item::Open => passed = true,
            Item::Inside(i) => view.inside.push(i),
            Item::Part(part) if passed => view.after.push(part),
            Item::Part(part) => view.before.push(part),
        }
    }
    let alone = view.before.is_empty() && view.after.is_empty() && view.inside.is_empty();
    (!alone && passed).then_some(view)
}

/// The whole paper of the open document's project for the translation
/// pane (see the module).
#[tauri::command]
pub async fn latex_paper(
    state: State<'_, AppState>,
    text: String,
) -> CommandResult<Option<PaperView>> {
    Ok(paper_view(&state, &text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_are_found_outside_comments() {
        let text = "a\n\\input{sections/intro}\n% \\input{x}\n\\include{./b.tex} \\subfile{c}\n";
        let names: Vec<String> = includes(text).into_iter().map(|(_, _, n)| n).collect();
        assert_eq!(names, ["sections/intro", "./b.tex", "c"]);
        assert_eq!(plain_name("./b.tex"), "b");
    }
}
