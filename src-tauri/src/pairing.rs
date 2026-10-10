//! A document paired with its hand-made mirror in the other language: an
//! English `paper.tex` with a Chinese `paper_zh.tex`, `sections_en/x.tex`
//! with `sections_zh/x.tex`. The mirror's paragraphs are the translations,
//! so nothing is translated until a paragraph changes; saving writes the
//! changed paragraphs back into the mirror in place and leaves the rest of
//! it byte for byte.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use biwrite_core::lang::{chinese_of_two, written_in};
use biwrite_core::pair::{Edit, Origin, align, patch_checked, units};
use biwrite_core::{Direction, Mode, SegmentId, SegmentKind, TextFile};
use serde::Serialize;
use tauri::{AppHandle, WebviewWindow};

use crate::commands::home_text;
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
    /// The mirror lags behind the saved document: it waits for translations,
    /// or its write failed or was refused. Unsaved until it is written.
    pub behind: bool,
    /// The file is on disk. Not after a Save As whose mirror couldn't be
    /// written: it is still to be created there (and nothing else may be).
    pub exists: bool,
}

impl PairState {
    /// Something of the mirror is not on disk.
    pub fn unsaved(&self) -> bool {
        self.behind || self.text != self.file.text()
    }
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
    /// New headings, captions and list items, which have no place of their
    /// own in the mirror: left out, to be added there by hand.
    pub left_out: usize,
    /// Why a mirror that was ready was not written.
    pub problem: Option<MirrorProblem>,
    /// The document was edited since it was saved: the mirror, which
    /// follows the saved document, is written with the next save.
    pub deferred: bool,
    /// The mirror still lags behind the saved document after this.
    pub behind: bool,
}

impl MirrorSaved {
    fn new(name: String) -> Self {
        Self {
            name,
            written: false,
            pending: 0,
            changed: 0,
            left_out: 0,
            problem: None,
            deferred: false,
            behind: false,
        }
    }
}

/// Why the mirror was not written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MirrorProblem {
    /// It changed on disk since it was read: not overwritten.
    ChangedOnDisk,
    /// The changes would not fit its structure (a translation that adds a
    /// heading, say): written, the paragraphs would land in wrong places.
    Structure,
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::Settings(message.into())
}

pub use biwrite_core::lang::is_chinese;

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
    candidates
        .into_iter()
        .find(|p| p != path && p.is_file() && !same_file(p, path))
}

/// `a` and `b` are the same file (through a link, or spelled differently).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `path` with its language folder swapped (`sections_en/x.tex` →
/// `sections_zh/x.tex`), if it is in one.
fn in_other_folder(path: &Path) -> Option<PathBuf> {
    let parts: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (i, part) in parts
        .iter()
        .enumerate()
        .take(parts.len().saturating_sub(1))
        .rev()
    {
        let lower = part.to_lowercase();
        let other = FOLDERS.iter().find_map(|(en, zh)| {
            (lower == *en)
                .then_some(*zh)
                .or((lower == *zh).then_some(*en))
        });
        if let Some(other) = other {
            let mut swapped = parts.clone();
            swapped[i] = other.to_owned();
            let swapped: PathBuf = swapped.iter().collect();
            // Only a folder that is there (`English/` needn't have a `chinese/`).
            return swapped
                .parent()
                .is_some_and(Path::is_dir)
                .then_some(swapped);
        }
    }
    None
}

/// Pair `text` (the open document, at `path`) with the mirror at
/// `mirror_path`, and load the engine with the mirror's paragraphs as
/// translations. The two must plainly read as two languages, the one with
/// more Chinese being Chinese: a copy in the same language (a translation
/// just started with `cp paper.tex paper_zh.tex`) is never written into as
/// the translation.
pub fn load_paired(
    state: &AppState,
    path: &Path,
    text: &str,
    mode: Mode,
    mirror_path: PathBuf,
    mirror: TextFile,
) -> CommandResult<(biwrite_engine::Snapshot, PairState)> {
    let other = mirror.text().to_owned();
    let direction = match chinese_of_two(text, &other, mode) {
        Some(true) => Direction::ZhEn,
        Some(false) => Direction::EnZh,
        None => {
            return Err(fail(format!(
                "{} and {} are not one in English and one in Chinese",
                display_name(Some(path)),
                display_name(Some(&mirror_path)),
            )));
        }
    };
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
            behind: false,
            exists: true,
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
    // A Chinese file is the Chinese side: translated into English.
    let alone = |state: &AppState, text: &str| {
        let home = written_in(text, mode).unwrap_or_default();
        state
            .engine
            .load_known(text.to_owned(), mode, home, Vec::new())
    };
    let snapshot = match mirror {
        Some((mirror_path, mirror)) => {
            match load_paired(state, &path, file.text(), mode, mirror_path, mirror) {
                Ok((snapshot, p)) => {
                    pair = Some(p);
                    snapshot
                }
                Err(e) => {
                    log::info!("not paired: {e}");
                    alone(state, file.text())
                }
            }
        }
        None => alone(state, file.text()),
    };
    log::info!(
        "opened {} ({:?}, {})",
        display_name(Some(&path)),
        snapshot.mode,
        snapshot.direction.as_str()
    );
    *state.file() = crate::state::FileState {
        path: Some(path),
        file,
        dirty: false,
        pair,
        home: snapshot.direction,
    };
    snapshot
}

pub fn view(pair: &PairState, units_total: usize) -> PairView {
    PairView {
        path: pair.path.display().to_string(),
        name: display_name(Some(&pair.path)),
        paired: pair.links.len(),
        units: units_total,
        dirty: pair.unsaved(),
    }
}

/// The edits that bring the mirror in line with the open document's
/// current translations.
struct Plan {
    edits: Vec<Edit>,
    /// Insert tags → the segment inserted.
    tags: HashMap<u64, SegmentId>,
    /// The open document's segments now.
    present: HashSet<SegmentId>,
    left_out: usize,
}

/// Where a new paragraph goes in the mirror: next to one of its units.
enum Spot {
    After(usize),
    Before(usize),
}

/// Why there is no plan yet.
enum Wait {
    /// Paragraphs whose translation is on its way.
    Pending(usize),
    /// The document is no longer the saved one.
    Moved,
}

/// The [`Plan`] for the document as it is, or only if its text is
/// `saved` (the mirror follows the saved document). A new paragraph goes
/// into the mirror next to a paired paragraph or heading it sits next to in
/// the document (nothing but blank lines between, other new paragraphs
/// aside): right after or before that one's counterpart, so it lands in
/// the same place, inside a list or around the whole body alike. New
/// headings, captions, list items and paragraphs with no such neighbour
/// are left out (`left_out`), to be added by hand.
fn plan(state: &AppState, pair: &PairState, saved: Option<&str>) -> Result<Plan, Wait> {
    let mode = state.engine.mode();
    let (text, current) = state.engine.text_and_translations();
    if saved.is_some_and(|saved| saved != text) {
        return Err(Wait::Moved);
    }
    let pending = current
        .iter()
        .filter(|(id, t)| t.is_none() && !pair.unpaired.contains(id))
        .count();
    if pending > 0 {
        return Err(Wait::Pending(pending));
    }
    let ua = units(&text, mode);
    let ub = units(&pair.text, mode);
    // A mirror unit a new paragraph can go next to.
    let beside = |j: usize| {
        ub.get(j).is_some_and(|u| {
            u.is_plain_paragraph(&pair.text) || matches!(u.kind, SegmentKind::Heading { .. })
        })
    };
    let linked = |k: usize| {
        current
            .get(k)
            .and_then(|(id, _)| pair.links.get(id))
            .copied()
    };
    let new_paragraph = |k: usize| {
        current
            .get(k)
            .is_some_and(|(id, _)| !pair.links.contains_key(id) && !pair.unpaired.contains(id))
            && ua.get(k).is_some_and(|u| u.is_plain_paragraph(&text))
    };
    // Units k and k + 1 of the document with only blank lines between.
    let adjacent = |k: usize| match (ua.get(k), ua.get(k + 1)) {
        (Some(a), Some(b)) => text[a.range.end..b.range.start].trim().is_empty(),
        _ => false,
    };
    // Where new paragraph `k` goes: after the paired unit before its run of
    // new paragraphs, else before the one after it.
    let place = |k: usize| -> Option<Spot> {
        let mut j = k;
        while j > 0 && adjacent(j - 1) {
            match linked(j - 1) {
                Some(unit) if beside(unit) => return Some(Spot::After(unit)),
                None if new_paragraph(j - 1) => j -= 1,
                _ => break,
            }
        }
        let mut j = k;
        while adjacent(j) {
            match linked(j + 1) {
                Some(unit) if beside(unit) => return Some(Spot::Before(unit)),
                None if new_paragraph(j + 1) => j += 1,
                _ => break,
            }
        }
        None
    };
    let present: HashSet<SegmentId> = current.iter().map(|(id, _)| *id).collect();
    let mut edits = Vec::new();
    let mut tags = HashMap::new();
    let mut left_out = 0;
    for (k, (id, translation)) in current.iter().enumerate() {
        let Some(translation) = translation.clone() else {
            continue;
        };
        if let Some(&unit) = pair.links.get(id) {
            let now = ub.get(unit).map(|u| &pair.text[u.content.clone()]);
            if now != Some(translation.as_str()) {
                edits.push(Edit::Replace {
                    unit,
                    text: translation,
                });
            }
            continue;
        }
        if pair.unpaired.contains(id) {
            continue;
        }
        let tag = id.0;
        match place(k).filter(|_| new_paragraph(k)) {
            Some(Spot::After(unit)) => edits.push(Edit::InsertAfter {
                unit: Some(unit),
                text: translation,
                tag,
            }),
            Some(Spot::Before(unit)) => edits.push(Edit::InsertBefore {
                unit,
                text: translation,
                tag,
            }),
            None => {
                left_out += 1;
                continue;
            }
        }
        tags.insert(tag, *id);
    }
    for (id, unit) in &pair.links {
        if !present.contains(id) {
            edits.push(Edit::Delete { unit: *unit });
        }
    }
    Ok(Plan {
        edits,
        tags,
        present,
        left_out,
    })
}

/// The mirror with the open document's current translations.
pub enum Patched {
    Ready {
        text: String,
        /// Segment of the open document → unit of `text`.
        links: HashMap<SegmentId, usize>,
        changed: usize,
        left_out: usize,
    },
    /// Paragraphs whose translation is on its way.
    Pending(usize),
    /// The document is no longer the saved text asked for.
    Moved,
    /// The changes would not fit the mirror's structure.
    Unsafe,
}

/// The mirror patched with the document's translations; with `saved`, only
/// while the document's text is that.
pub fn patched(state: &AppState, pair: &PairState, saved: Option<&str>) -> Patched {
    let plan = match plan(state, pair, saved) {
        Ok(plan) => plan,
        Err(Wait::Pending(pending)) => return Patched::Pending(pending),
        Err(Wait::Moved) => return Patched::Moved,
    };
    let changed = plan.edits.len();
    let left_out = plan.left_out;
    if plan.edits.is_empty() {
        return Patched::Ready {
            text: pair.text.clone(),
            links: pair.links.clone(),
            changed,
            left_out,
        };
    }
    let Some((text, origin)) = patch_checked(&pair.text, state.engine.mode(), &plan.edits) else {
        log::warn!("the paired file would change structure; not patched");
        return Patched::Unsafe;
    };
    let by_unit: HashMap<usize, SegmentId> = pair.links.iter().map(|(id, u)| (*u, *id)).collect();
    let mut links = HashMap::new();
    for (k, o) in origin.iter().enumerate() {
        let id = match o {
            Origin::Kept(old) => by_unit.get(old).copied(),
            Origin::Inserted(tag) => plan.tags.get(tag).copied(),
        };
        if let Some(id) = id.filter(|id| plan.present.contains(id)) {
            links.insert(id, k);
        }
    }
    Patched::Ready {
        text,
        links,
        changed,
        left_out,
    }
}

/// Set whether the mirror at `path` lags behind (if it is still the pair's).
fn set_behind(state: &AppState, path: &Path, behind: bool) {
    if let Some(pair) = state.file().pair.as_mut().filter(|p| p.path == path) {
        pair.behind = behind;
    }
}

/// Write the mirror if its translations are ready and it is as it was read
/// (changes made to it elsewhere are never overwritten).
pub async fn save_mirror(state: &AppState) -> CommandResult<Option<MirrorSaved>> {
    let (path, mut saved, text, links, bytes, expected) = {
        let mut guard = state.file();
        let fs = &mut *guard;
        let Some(pair) = fs.pair.as_mut() else {
            return Ok(None);
        };
        let mut saved = MirrorSaved::new(display_name(Some(&pair.path)));
        match patched(state, pair, Some(fs.file.text())) {
            Patched::Ready {
                text,
                links,
                changed,
                left_out,
            } => {
                saved.left_out = left_out;
                if text == pair.file.text() && pair.exists {
                    pair.text = text;
                    pair.links = links;
                    pair.behind = false;
                    return Ok(Some(saved));
                }
                saved.changed = changed;
                let bytes = pair.file.encode(&text);
                // As read, or (still to be created) not there at all.
                let expected = pair.exists.then(|| pair.file.text().to_owned());
                (pair.path.clone(), saved, text, links, bytes, expected)
            }
            Patched::Pending(pending) => {
                pair.behind = true;
                saved.pending = pending;
                saved.behind = true;
                return Ok(Some(saved));
            }
            Patched::Moved => {
                pair.behind = true;
                saved.deferred = true;
                saved.behind = true;
                return Ok(Some(saved));
            }
            Patched::Unsafe => {
                pair.behind = true;
                saved.problem = Some(MirrorProblem::Structure);
                saved.behind = true;
                return Ok(Some(saved));
            }
        }
    };
    let on_disk = files::read_text_file(path.clone()).await;
    let as_expected = match (&expected, on_disk) {
        (Some(expected), Ok(disk)) => disk.text() == expected,
        (None, Err(_)) => !path.exists(),
        // Ours already, from a write that reported a failure after landing.
        (None, Ok(disk)) => disk.text() == text,
        _ => false,
    };
    if !as_expected {
        log::warn!("{} changed on disk; not overwritten", saved.name);
        set_behind(state, &path, true);
        saved.problem = Some(MirrorProblem::ChangedOnDisk);
        saved.behind = true;
        return Ok(Some(saved));
    }
    if let Err(e) = files::write_file_atomic(path.clone(), bytes.clone()).await {
        set_behind(state, &path, true);
        return Err(e);
    }
    {
        let mut fs = state.file();
        if let Some(pair) = fs.pair.as_mut().filter(|p| p.path == path) {
            pair.file = pair.file.saved(text.clone(), bytes);
            pair.text = text;
            pair.links = links;
            pair.behind = false;
            pair.exists = true;
        }
    }
    log::info!(
        "saved the mirror {} ({} paragraphs changed)",
        saved.name,
        saved.changed
    );
    saved.written = true;
    Ok(Some(saved))
}

/// Where the mirror goes when the document is saved as `new_source`: where
/// opening `new_source` looks for it, so the two pair up again. With a
/// language folder in the new path, the same name in the other folder;
/// otherwise next to it with the suffix the pair had (`paper` → `paper-2`
/// makes `paper_zh` → `paper-2_zh`), or `_zh`. Never the new file itself
/// or an existing file.
pub fn mirror_path_for(old_source: &Path, new_source: &Path, old_mirror: &Path) -> PathBuf {
    let stem = |p: &Path| {
        p.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let (os, ns, ms) = (stem(old_source), stem(new_source), stem(old_mirror));
    let ext = new_source
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = new_source.parent().unwrap_or(Path::new("."));
    // `longer` is `shorter` plus one of the suffixes opening looks for.
    let suffix = |longer: &str, shorter: &str| {
        let tail = longer.get(shorter.len()..)?;
        let lower = tail.to_lowercase();
        (longer.get(..shorter.len())?.eq_ignore_ascii_case(shorter)
            && SUFFIXES.contains(&lower.as_str()))
        .then(|| tail.to_owned())
    };
    let target = in_other_folder(new_source).unwrap_or_else(|| {
        let new_stem = match (suffix(&ms, &os), suffix(&os, &ms)) {
            // paper → paper_zh: the new name with the same suffix.
            (Some(tail), _) => format!("{ns}{tail}"),
            // paper_zh → paper: the new name without it (or with it, when
            // the new name has none: opening finds `x_zh` from `x` too).
            (None, Some(tail)) => {
                let cut = ns.len().saturating_sub(tail.len());
                match ns.get(cut..) {
                    Some(end) if end.eq_ignore_ascii_case(&tail) => ns[..cut].to_owned(),
                    _ => format!("{ns}{tail}"),
                }
            }
            (None, None) => format!("{ns}_zh"),
        };
        dir.join(format!("{new_stem}{ext}"))
    });
    let target = if same_file(&target, new_source) {
        dir.join(format!("{ns}_zh{ext}"))
    } else {
        target
    };
    files::fresh_path(&target)
}

/// Write the mirror to `dest` (a Save As of the document): the pair moves
/// there, and the old mirror stays as it was.
pub async fn save_mirror_as(state: &AppState, dest: PathBuf) -> CommandResult<Option<MirrorSaved>> {
    let (text, links, bytes, mut saved, behind) = {
        let fs = state.file();
        let Some(pair) = fs.pair.as_ref() else {
            return Ok(None);
        };
        let mut saved = MirrorSaved::new(display_name(Some(&dest)));
        // Paragraphs still on their way (or that don't fit) follow later.
        let (text, links, behind) = match patched(state, pair, Some(fs.file.text())) {
            Patched::Ready {
                text,
                links,
                changed,
                left_out,
            } => {
                saved.changed = changed;
                saved.left_out = left_out;
                (text, Some(links), false)
            }
            Patched::Pending(pending) => {
                saved.pending = pending;
                (pair.text.clone(), None, true)
            }
            Patched::Moved => {
                saved.deferred = true;
                (pair.text.clone(), None, true)
            }
            Patched::Unsafe => {
                saved.problem = Some(MirrorProblem::Structure);
                (pair.text.clone(), None, true)
            }
        };
        let bytes = pair.file.encode(&text);
        (text, links, bytes, saved, behind)
    };
    if let Err(e) = files::write_file_atomic(dest.clone(), bytes.clone()).await {
        // The document lives under its new name now: its mirror is the new
        // one (to be written), never the old file left behind.
        if let Some(pair) = state.file().pair.as_mut() {
            pair.path = dest.clone();
            pair.behind = true;
            pair.exists = false;
        }
        return Err(e);
    }
    {
        let mut fs = state.file();
        if let Some(pair) = fs.pair.as_mut() {
            pair.path = dest.clone();
            pair.file = pair.file.saved(text.clone(), bytes);
            pair.text = text;
            pair.behind = behind;
            pair.exists = true;
            if let Some(links) = links {
                pair.links = links;
            }
        }
    }
    log::info!("saved the mirror as {}", display_name(Some(&dest)));
    saved.written = true;
    saved.behind = behind;
    Ok(Some(saved))
}

/// Pair the open document with a mirror the user picks.
#[tauri::command]
pub async fn import_mirror(
    app: AppHandle,
    window: WebviewWindow,
    state: tauri::State<'_, AppState>,
    text: String,
    document: u64,
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
    if same_file(&mirror_path, &path) {
        return Err(fail(
            "Choose the other language's file, not the document itself.",
        ));
    }
    let mirror = files::read_text_file(mirror_path.clone()).await?;
    let _no_save_meanwhile = state.save_lock.lock().await;
    state.sync_text(document, &text)?;
    if state.file().path.as_ref() != Some(&path) {
        return Err(CommandError::Engine(biwrite_engine::EngineError::Stale));
    }
    let mode = state.engine.mode();
    // The file's own text: the editor's, or composed from the translations
    // while the other language is edited.
    let text = home_text(&state, text)?;
    let (snapshot, pair) = load_paired(&state, &path, &text, mode, mirror_path, mirror)?;
    let dirty = {
        let mut fs = state.file();
        fs.pair = Some(pair);
        fs.home = snapshot.direction;
        fs.dirty = text != fs.file.text();
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
    let mut fs = state.file();
    // After a swap, edits of the other file may live only here. (Machine
    // translations it waits for are let go.)
    if let Some(pair) = fs.pair.as_ref().filter(|p| p.text != p.file.text()) {
        return Err(fail(format!(
            "{} has unsaved changes: save first, then stop pairing.",
            display_name(Some(&pair.path))
        )));
    }
    if let Some(pair) = fs.pair.take() {
        log::info!("unpaired {}", display_name(Some(&pair.path)));
    }
    Ok(())
}

/// Write the mirror now (after its translations arrived), unless the
/// document was edited since it was saved: the mirror follows the saved
/// document, and is written with the next save instead (`deferred`).
#[tauri::command]
pub async fn write_mirror(state: tauri::State<'_, AppState>) -> CommandResult<Option<MirrorSaved>> {
    let _one_at_a_time = state.save_lock.lock().await;
    save_mirror(&state).await
}

/// Swap in a pair: the mirror (with the current translations) becomes the
/// edited document and the edited text becomes the mirror.
///
/// The engine holds `text` already ([`AppState::sync_text`]).
pub fn swap(state: &AppState, text: String) -> CommandResult<SessionView> {
    let mode = state.engine.mode();
    // The mirror becomes the edited document and is saved as such: not
    // over changes made to it elsewhere.
    let (mirror_path, kept) = {
        let fs = state.file();
        let pair = fs.pair.as_ref().ok_or_else(|| fail("no pair"))?;
        (pair.path.clone(), pair.file.text().to_owned())
    };
    let on_disk = std::fs::read(&mirror_path)
        .ok()
        .and_then(|bytes| TextFile::decode(bytes).ok());
    if on_disk.is_none_or(|disk| disk.text() != kept) {
        return Err(fail(format!(
            "{} changed on disk since it was opened. Reopen the document to pair it with \
             the new version.",
            display_name(Some(&mirror_path))
        )));
    }
    let (new_text, links) = {
        let fs = state.file();
        let pair = fs.pair.as_ref().ok_or_else(|| fail("no pair"))?;
        match patched(state, pair, None) {
            Patched::Ready { text, links, .. } => (text, links),
            Patched::Moved => return Err(biwrite_engine::EngineError::Stale.into()),
            Patched::Pending(pending) => {
                return Err(fail(format!(
                    "Swapping needs every paragraph translated: {pending} not ready yet."
                )));
            }
            Patched::Unsafe => {
                return Err(fail(format!(
                    "Swapping would put paragraphs of {} in wrong places: the changes don't fit \
                     its structure (a translation that adds a heading or a list item, or a new \
                     paragraph with no place of its own). Edit the paragraph, or update the file \
                     by hand and reopen the document.",
                    display_name(Some(&pair.path))
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
        fs.home = direction;
        // Unsaved edits of the side swapped away live on in the pair.
        fs.dirty = new_text != fs.file.text() || text != old_file.text();
        fs.pair = old_path.map(|path| PairState {
            path,
            file: old_file,
            text: text.clone(),
            links: new_links,
            unpaired,
            behind: false,
            exists: true,
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
        std::fs::create_dir_all(p("sections_en")).unwrap();
        std::fs::create_dir_all(p("sections_zh")).unwrap();
        std::fs::create_dir_all(p("other")).unwrap();
        // Where opening the new file looks for its counterpart.
        let cases = [
            ("paper.tex", "paper-2.tex", "paper_zh.tex", "paper-2_zh.tex"),
            ("paper.tex", "draft.tex", "paper_zh.tex", "draft_zh.tex"),
            ("paper_zh.tex", "draft_zh.tex", "paper.tex", "draft.tex"),
            ("paper_zh.tex", "draft.tex", "paper.tex", "draft_zh.tex"),
            (
                "paper.tex",
                "other/draft.tex",
                "paper_zh.tex",
                "other/draft_zh.tex",
            ),
            (
                "sections_en/intro.tex",
                "sections_en/intro-2.tex",
                "sections_zh/intro.tex",
                "sections_zh/intro-2.tex",
            ),
            // Saved into the other language's folder: never onto itself.
            (
                "sections_en/intro.tex",
                "sections_zh/intro-2.tex",
                "sections_zh/intro.tex",
                "sections_en/intro-2.tex",
            ),
        ];
        for (old, new, mirror, expected) in cases {
            let got = mirror_path_for(&p(old), &p(new), &p(mirror));
            assert_eq!(got, p(expected), "{old} saved as {new}");
            std::fs::write(p(new), "x").unwrap();
            std::fs::write(&got, "x").unwrap();
            assert_eq!(counterpart(&p(new)), Some(got.clone()), "{new} pairs again");
            std::fs::remove_file(p(new)).unwrap();
            std::fs::remove_file(&got).unwrap();
        }
        // A language-named folder without its counterpart: the suffix rule.
        std::fs::create_dir_all(p("English")).unwrap();
        assert_eq!(
            mirror_path_for(&p("paper.tex"), &p("English/essay.tex"), &p("paper_zh.tex")),
            p("English/essay_zh.tex")
        );
        // Only the suffixes opening looks for count, in any case.
        assert_eq!(
            mirror_path_for(&p("intro.tex"), &p("draft.tex"), &p("introduction_zh.tex")),
            p("draft_zh.tex")
        );
        assert_eq!(
            mirror_path_for(&p("paper_ZH.tex"), &p("draft_ZH.tex"), &p("paper.tex")),
            p("draft.tex")
        );
        // Never an existing file.
        std::fs::write(p("draft_zh.tex"), "x").unwrap();
        assert_eq!(
            mirror_path_for(&p("paper.tex"), &p("draft.tex"), &p("paper_zh.tex")),
            p("draft_zh-2.tex")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
