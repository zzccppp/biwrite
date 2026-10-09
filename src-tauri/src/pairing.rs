//! A document paired with its hand-made mirror in the other language: an
//! English `paper.tex` with a Chinese `paper_zh.tex`, `sections_en/x.tex`
//! with `sections_zh/x.tex`. The mirror's paragraphs are the translations,
//! so nothing is translated until a paragraph changes; saving writes the
//! changed paragraphs back into the mirror in place and leaves the rest of
//! it byte for byte.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use biwrite_core::pair::{Edit, Origin, align, patch, units};
use biwrite_core::{Direction, Mode, SegmentId, TextFile};
use serde::Serialize;
use tauri::{AppHandle, WebviewWindow};

use crate::error::{CommandError, CommandResult};
use crate::files;
use crate::state::{AppState, SessionView, display_name};

/// Least share of paragraphs that must pair up for two files to count as
/// translations of each other.
const MIN_COVERAGE: f64 = 0.5;

/// The mirror of the open document.
pub struct PairState {
    pub path: PathBuf,
    /// The file as on disk (encoding, line endings).
    pub file: TextFile,
    /// The mirror's current text (differs from `file` after a swap).
    pub text: String,
    /// Segment of the open document → unit of `text`.
    pub links: HashMap<SegmentId, usize>,
    /// Segments without a partner when paired: their translations stay out
    /// of the mirror.
    pub unpaired: HashSet<SegmentId>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairView {
    pub path: String,
    pub name: String,
    /// Paragraphs paired, of the open document's.
    pub paired: usize,
    pub units: usize,
    /// The mirror has changes not written yet.
    pub dirty: bool,
}

/// What a save did with the mirror.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorSaved {
    pub name: String,
    /// The mirror was written.
    pub written: bool,
    /// Paragraphs whose translation is still on its way (the mirror waits).
    pub pending: usize,
    /// Paragraphs changed in the mirror.
    pub changed: usize,
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::Settings(message.into())
}

/// The text is mostly Chinese.
pub fn is_chinese(text: &str) -> bool {
    cjk_share(text) > 0.3
}

fn cjk_share(text: &str) -> f64 {
    let (mut cjk, mut letters) = (0usize, 0usize);
    for c in text.chars() {
        if matches!(c as u32, 0x4E00..=0x9FFF | 0x3400..=0x4DBF) {
            cjk += 1;
        } else if c.is_ascii_alphabetic() {
            letters += 1;
        }
    }
    cjk as f64 / (cjk + letters / 5).max(1) as f64
}

/// Folder names of a language and their counterparts.
const FOLDERS: &[(&str, &str)] = &[
    ("en", "zh"),
    ("english", "chinese"),
    ("sections_en", "sections_zh"),
    ("sections-en", "sections-zh"),
    ("en_sections", "zh_sections"),
    ("sec_en", "sec_zh"),
];
/// File name suffixes of the Chinese version.
const SUFFIXES: &[&str] = &["_zh", ".zh", "-zh", "_cn", "-cn", "_chinese"];

/// The existing file that mirrors `path` by name: `x.tex` ↔ `x_zh.tex`,
/// `sections_en/x.tex` ↔ `sections_zh/x.tex`, `en/x.tex` ↔ `zh/x.tex`.
pub fn counterpart(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut candidates: Vec<PathBuf> = Vec::new();
    // x.tex → x_zh.tex; x_zh.tex → x.tex.
    match SUFFIXES.iter().find(|s| stem.to_lowercase().ends_with(*s)) {
        Some(s) => candidates.push(dir.join(format!("{}{ext}", &stem[..stem.len() - s.len()]))),
        None => candidates.extend(SUFFIXES.iter().map(|s| dir.join(format!("{stem}{s}{ext}")))),
    }
    // A language folder anywhere in the path.
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (i, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
        let lower = part.to_lowercase();
        for (en, zh) in FOLDERS {
            let other = if lower == *en {
                Some(zh)
            } else if lower == *zh {
                Some(en)
            } else {
                None
            };
            if let Some(other) = other {
                let mut swapped = parts.clone();
                swapped[i] = (*other).to_owned();
                candidates.push(swapped.iter().collect());
            }
        }
    }
    candidates.into_iter().find(|p| p != path && p.is_file())
}

/// Pair `text` (the open document, at `path`) with the mirror at
/// `mirror_path`, and load the engine with the mirror's paragraphs as
/// translations. The language with more Chinese characters is Chinese.
pub fn load_paired(
    state: &AppState,
    path: &Path,
    text: &str,
    mode: Mode,
    mirror_path: PathBuf,
    mirror: TextFile,
) -> CommandResult<(biwrite_engine::Snapshot, PairState)> {
    let other = mirror.text().to_owned();
    let pairing = align(text, &other, mode);
    if pairing.pairs.is_empty() || pairing.coverage() < MIN_COVERAGE {
        return Err(fail(format!(
            "{} does not look like a translation of {}: only {} of {} paragraphs pair up",
            display_name(Some(&mirror_path)),
            display_name(Some(path)),
            pairing.pairs.len(),
            pairing.a_units.max(pairing.b_units)
        )));
    }
    let direction = if cjk_share(text) > cjk_share(&other) {
        Direction::ZhEn
    } else {
        Direction::EnZh
    };
    let (ua, ub) = (units(text, mode), units(&other, mode));
    let known: Vec<(usize, String)> = pairing
        .pairs
        .iter()
        .map(|(i, j)| (ua[*i].segment, other[ub[*j].content.clone()].to_owned()))
        .collect();
    let snapshot = state
        .engine
        .load_known(text.to_owned(), mode, direction, known);
    let ids: Vec<SegmentId> = snapshot.layout.iter().map(|s| s.id).collect();
    let unit_ids: Vec<SegmentId> = ua
        .iter()
        .filter_map(|u| ids.get(u.segment).copied())
        .collect();
    let links: HashMap<SegmentId, usize> = pairing
        .pairs
        .iter()
        .filter_map(|(i, j)| unit_ids.get(*i).map(|id| (*id, *j)))
        .collect();
    let unpaired = unit_ids
        .iter()
        .filter(|id| !links.contains_key(id))
        .copied()
        .collect();
    log::info!(
        "paired {} with {}: {} of {} paragraphs",
        display_name(Some(path)),
        display_name(Some(&mirror_path)),
        links.len(),
        unit_ids.len()
    );
    Ok((
        snapshot,
        PairState {
            path: mirror_path,
            file: mirror,
            text: other,
            links,
            unpaired,
        },
    ))
}

/// Make `file` (at `path`) the open document, paired with `mirror` (its
/// counterpart by name, read by the caller) when the two pair up.
pub fn open(
    state: &AppState,
    path: PathBuf,
    file: TextFile,
    mirror: Option<(PathBuf, TextFile)>,
) -> biwrite_engine::Snapshot {
    // The note must be in place before the first requests start.
    state.engine.set_doc_note(Some(state.note_for(Some(&path))));
    let mode = Mode::from_path(&path);
    let mut pair = None;
    let snapshot = match mirror {
        Some((mirror_path, mirror)) => {
            match load_paired(state, &path, file.text(), mode, mirror_path, mirror) {
                Ok((snapshot, p)) => {
                    pair = Some(p);
                    snapshot
                }
                Err(e) => {
                    log::info!("not paired: {e}");
                    state.engine.load(file.text().to_owned(), mode)
                }
            }
        }
        None => state.engine.load(file.text().to_owned(), mode),
    };
    log::info!("opened {} ({:?})", display_name(Some(&path)), snapshot.mode);
    *state.file() = crate::state::FileState {
        path: Some(path),
        file,
        dirty: false,
        pair,
    };
    snapshot
}

pub fn view(pair: &PairState, units_total: usize) -> PairView {
    PairView {
        path: pair.path.display().to_string(),
        name: display_name(Some(&pair.path)),
        paired: pair.links.len(),
        units: units_total,
        dirty: pair.text != pair.file.text(),
    }
}

/// The edits that bring the mirror in line with the open document's
/// current translations, or the number of paragraphs still waiting for
/// one.
fn edits(
    state: &AppState,
    pair: &PairState,
) -> Result<(Vec<Edit>, HashMap<u64, SegmentId>), usize> {
    let current = state.engine.translations();
    let ub = units(&pair.text, state.engine.mode());
    let present: HashSet<SegmentId> = current.iter().map(|(id, _)| *id).collect();
    let pending = current
        .iter()
        .filter(|(id, t)| t.is_none() && !pair.unpaired.contains(id))
        .count();
    if pending > 0 {
        return Err(pending);
    }
    let mut out = Vec::new();
    let mut tags = HashMap::new();
    let mut last: Option<usize> = None;
    for (id, translation) in current {
        let Some(translation) = translation else {
            continue;
        };
        match pair.links.get(&id) {
            Some(&unit) => {
                let now = ub.get(unit).map(|u| &pair.text[u.content.clone()]);
                if now != Some(translation.as_str()) {
                    out.push(Edit::Replace {
                        unit,
                        text: translation,
                    });
                }
                last = Some(last.map_or(unit, |l| l.max(unit)));
            }
            None if pair.unpaired.contains(&id) => {}
            None => {
                let tag = id.0;
                tags.insert(tag, id);
                out.push(Edit::InsertAfter {
                    unit: last,
                    text: translation,
                    tag,
                });
            }
        }
    }
    for (id, unit) in &pair.links {
        if !present.contains(id) {
            out.push(Edit::Delete { unit: *unit });
        }
    }
    Ok((out, tags))
}

/// The mirror's text with the current translations, and its new links.
/// `Err(pending)` while translations are on their way.
pub fn patched(
    state: &AppState,
    pair: &PairState,
) -> Result<(String, HashMap<SegmentId, usize>, usize), usize> {
    let (edits, tags) = edits(state, pair)?;
    let changed = edits.len();
    if edits.is_empty() {
        return Ok((pair.text.clone(), pair.links.clone(), 0));
    }
    let mode = state.engine.mode();
    let (text, origin) = patch(&pair.text, mode, &edits);
    let by_unit: HashMap<usize, SegmentId> = pair.links.iter().map(|(id, u)| (*u, *id)).collect();
    let present: HashSet<SegmentId> = state
        .engine
        .translations()
        .iter()
        .map(|(id, _)| *id)
        .collect();
    let mut links = HashMap::new();
    for (k, o) in origin.iter().enumerate() {
        let id = match o {
            Origin::Kept(old) => by_unit.get(old).copied(),
            Origin::Inserted(tag) => tags.get(tag).copied(),
        };
        if let Some(id) = id.filter(|id| present.contains(id)) {
            links.insert(id, k);
        }
    }
    if units(&text, mode).len() != origin.len() {
        log::warn!("the patched mirror segments differently than expected; links may drift");
    }
    Ok((text, links, changed))
}

/// Write the mirror if its translations are ready.
pub async fn save_mirror(state: &AppState) -> CommandResult<Option<MirrorSaved>> {
    let (path, name, text, links, changed, bytes) = {
        let fs = state.file();
        let Some(pair) = fs.pair.as_ref() else {
            return Ok(None);
        };
        let name = display_name(Some(&pair.path));
        match patched(state, pair) {
            Ok((text, _, _)) if text == pair.file.text() => {
                return Ok(Some(MirrorSaved {
                    name,
                    written: false,
                    pending: 0,
                    changed: 0,
                }));
            }
            Ok((text, links, changed)) => {
                let bytes = pair.file.encode(&text);
                (pair.path.clone(), name, text, links, changed, bytes)
            }
            Err(pending) => {
                return Ok(Some(MirrorSaved {
                    name,
                    written: false,
                    pending,
                    changed: 0,
                }));
            }
        }
    };
    files::write_file_atomic(path.clone(), bytes.clone()).await?;
    {
        let mut fs = state.file();
        if let Some(pair) = fs.pair.as_mut().filter(|p| p.path == path) {
            pair.file = pair.file.saved(text.clone(), bytes);
            pair.text = text;
            pair.links = links;
        }
    }
    log::info!("saved the mirror {name} ({changed} paragraphs changed)");
    Ok(Some(MirrorSaved {
        name,
        written: true,
        pending: 0,
        changed,
    }))
}

/// Where the mirror goes when the document is saved as `new_source`: the
/// same change of name (`paper` → `paper-2` makes `paper_zh` → `paper_zh-2`),
/// in the mirror's folder, never an existing file.
pub fn mirror_path_for(old_source: &Path, new_source: &Path, old_mirror: &Path) -> PathBuf {
    let stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (os, ns, ms) = (stem(old_source), stem(new_source), stem(old_mirror));
    let new_stem = match (ns.strip_prefix(&os), ms.strip_prefix(&os)) {
        (Some(suffix), _) => format!("{ms}{suffix}"),
        (None, Some(tag)) => format!("{ns}{tag}"),
        (None, None) => format!("{ns}_{}", if is_chinese(&ms) { "zh" } else { "mirror" }),
    };
    let ext = old_mirror
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = old_mirror.parent().unwrap_or(Path::new("."));
    files::fresh_path(&dir.join(format!("{new_stem}{ext}")))
}

/// Write the mirror to `dest` (a Save As of the document): the pair moves
/// there, and the old mirror stays as it was.
pub async fn save_mirror_as(state: &AppState, dest: PathBuf) -> CommandResult<Option<MirrorSaved>> {
    let (text, links, changed, pending, bytes) = {
        let fs = state.file();
        let Some(pair) = fs.pair.as_ref() else {
            return Ok(None);
        };
        let (text, links, changed, pending) = match patched(state, pair) {
            Ok((t, l, c)) => (t, Some(l), c, 0),
            // Paragraphs still on their way follow once translated.
            Err(p) => (pair.text.clone(), None, 0, p),
        };
        let bytes = pair.file.encode(&text);
        (text, links, changed, pending, bytes)
    };
    files::write_file_atomic(dest.clone(), bytes.clone()).await?;
    {
        let mut fs = state.file();
        if let Some(pair) = fs.pair.as_mut() {
            pair.path = dest.clone();
            pair.file = pair.file.saved(text.clone(), bytes);
            pair.text = text;
            if let Some(links) = links {
                pair.links = links;
            }
        }
    }
    log::info!("saved the mirror as {}", display_name(Some(&dest)));
    Ok(Some(MirrorSaved {
        name: display_name(Some(&dest)),
        written: true,
        pending,
        changed,
    }))
}

/// Pair the open document with a mirror the user picks.
#[tauri::command]
pub async fn import_mirror(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    text: String,
) -> CommandResult<Option<SessionView>> {
    let Some(path) = state.file().path.clone() else {
        return Err(fail(
            "Save the document first, then import its translation.",
        ));
    };
    let label = match state.engine.direction() {
        Direction::EnZh => "Chinese version",
        Direction::ZhEn => "English version",
    };
    let Some(mirror_path) =
        files::pick_open_kind(&app, &window, label, &["tex", "md", "markdown", "txt"]).await
    else {
        return Ok(None);
    };
    if mirror_path == path {
        return Err(fail(
            "Choose the other language's file, not the document itself.",
        ));
    }
    let mirror = files::read_text_file(mirror_path.clone()).await?;
    let mode = state.engine.mode();
    let (snapshot, pair) = load_paired(&state, &path, &text, mode, mirror_path, mirror)?;
    let dirty = {
        let mut fs = state.file();
        fs.pair = Some(pair);
        fs.dirty
    };
    let mut view = state.session_view(snapshot);
    // The editor keeps its (possibly unsaved) text.
    view.dirty = dirty;
    Ok(Some(view))
}

/// Stop writing to the mirror. Its paragraphs stay as the translations.
#[tauri::command]
pub async fn close_mirror(state: tauri::State<'_, AppState>) -> CommandResult<()> {
    if let Some(pair) = state.file().pair.take() {
        log::info!("unpaired {}", display_name(Some(&pair.path)));
    }
    Ok(())
}

/// Write the mirror now (after its translations arrived).
#[tauri::command]
pub async fn write_mirror(state: tauri::State<'_, AppState>) -> CommandResult<Option<MirrorSaved>> {
    save_mirror(&state).await
}

/// Swap in a pair: the mirror (with the current translations) becomes the
/// edited document and the edited text becomes the mirror.
pub fn swap(state: &AppState, text: String) -> CommandResult<SessionView> {
    if state.engine.text() != text {
        state.engine.update(text.clone());
    }
    let mode = state.engine.mode();
    let (new_text, links) = {
        let fs = state.file();
        let pair = fs.pair.as_ref().ok_or_else(|| fail("no pair"))?;
        match patched(state, pair) {
            Ok((t, links, _)) => (t, links),
            Err(pending) => {
                return Err(fail(format!(
                    "Swapping needs every paragraph translated: {pending} not ready yet."
                )));
            }
        }
    };
    // The open document's units, and the mirror units they link to.
    let ua = units(&text, mode);
    let snapshot = state.engine.snapshot();
    let unit_ids: Vec<SegmentId> = ua
        .iter()
        .filter_map(|u| snapshot.layout.get(u.segment).map(|s| s.id))
        .collect();
    let ub = units(&new_text, mode);
    // New document: the mirror; its known translations are the old paragraphs.
    let mut known = Vec::new();
    let mut back: Vec<(usize, usize)> = Vec::new(); // (new doc unit, old doc unit)
    for (i, id) in unit_ids.iter().enumerate() {
        if let Some(&j) = links.get(id) {
            if let Some(u) = ub.get(j) {
                known.push((u.segment, text[ua[i].content.clone()].to_owned()));
                back.push((j, i));
            }
        }
    }
    let direction = state.engine.direction().flipped();
    let new_snapshot = state
        .engine
        .load_known(new_text.clone(), mode, direction, known);
    let new_ids: Vec<SegmentId> = new_snapshot.layout.iter().map(|s| s.id).collect();
    let new_unit_ids: Vec<SegmentId> = ub
        .iter()
        .filter_map(|u| new_ids.get(u.segment).copied())
        .collect();
    let new_links: HashMap<SegmentId, usize> = back
        .iter()
        .filter_map(|(j, i)| new_unit_ids.get(*j).map(|id| (*id, *i)))
        .collect();
    let unpaired = new_unit_ids
        .iter()
        .filter(|id| !new_links.contains_key(id))
        .copied()
        .collect();
    let dirty = {
        let mut fs = state.file();
        let old_pair = fs.pair.take().ok_or_else(|| fail("no pair"))?;
        let old_path = fs.path.take();
        let old_file = std::mem::take(&mut fs.file);
        fs.path = Some(old_pair.path);
        fs.file = old_pair.file;
        fs.dirty = new_text != fs.file.text();
        fs.pair = old_path.map(|path| PairState {
            path,
            file: old_file,
            text: text.clone(),
            links: new_links,
            unpaired,
        });
        fs.dirty
    };
    state.apply_doc_note();
    let mut view = state.session_view(new_snapshot);
    view.dirty = dirty;
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("biwrite-pair-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn counterparts_by_suffix_and_folder() {
        let dir = temp("names");
        std::fs::create_dir_all(dir.join("sections_en")).unwrap();
        std::fs::create_dir_all(dir.join("sections_zh")).unwrap();
        for f in [
            "paper.tex",
            "paper_zh.tex",
            "sections_en/intro.tex",
            "sections_zh/intro.tex",
            "lone.tex",
        ] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        assert_eq!(
            counterpart(&dir.join("paper.tex")),
            Some(dir.join("paper_zh.tex"))
        );
        assert_eq!(
            counterpart(&dir.join("paper_zh.tex")),
            Some(dir.join("paper.tex"))
        );
        assert_eq!(
            counterpart(&dir.join("sections_en/intro.tex")),
            Some(dir.join("sections_zh/intro.tex"))
        );
        assert_eq!(
            counterpart(&dir.join("sections_zh/intro.tex")),
            Some(dir.join("sections_en/intro.tex"))
        );
        assert_eq!(counterpart(&dir.join("lone.tex")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saved_as_mirrors_follow_the_new_name() {
        let dir = temp("saveas");
        let p = |n: &str| dir.join(n);
        assert_eq!(
            mirror_path_for(&p("paper.tex"), &p("paper-2.tex"), &p("paper_zh.tex")),
            p("paper_zh-2.tex")
        );
        assert_eq!(
            mirror_path_for(&p("paper.tex"), &p("draft.tex"), &p("paper_zh.tex")),
            p("draft_zh.tex")
        );
        std::fs::write(p("draft_zh.tex"), "x").unwrap();
        assert_eq!(
            mirror_path_for(&p("paper.tex"), &p("draft.tex"), &p("paper_zh.tex")),
            p("draft_zh-2.tex")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chinese_share() {
        assert!(cjk_share("我们研究表格数据清洗。") > 0.9);
        assert!(cjk_share("We study tabular data cleaning.") < 0.1);
        assert!(cjk_share("我们使用 TabPFN 和 CARVEPrep 方法。") > 0.5);
    }
}
