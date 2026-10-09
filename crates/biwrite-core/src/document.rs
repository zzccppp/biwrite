//! Document model with stable segment IDs.
//!
//! On every edit the document is re-segmented and the new segment list is
//! aligned against the old one, so that unchanged segments keep their IDs and
//! the right pane neither flickers nor reorders:
//!
//! 1. LCS diff over segment keys (kind + content hash). Equal runs keep IDs.
//! 2. Moved segments: an inserted segment whose key matches a deleted one
//!    takes over that ID.
//! 3. Edited segments: inside each replaced hunk, old and new segments are
//!    paired in order by text similarity, and a pair keeps the old ID.
//! 4. Anything left gets a fresh ID.

use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use similar::{Algorithm, DiffOp, capture_diff_slices, capture_diff_slices_deadline, diff_ratio};

use crate::hash::ContentHash;
use crate::lang::tokens;
use crate::mode::Mode;
use crate::segment::{Segment, SegmentKind, segment};

/// Minimum similarity for an edited segment to keep the ID of the segment it
/// replaced (keeps the block in place while its translation is refreshed).
pub const ID_REUSE_SIMILARITY: f32 = 0.3;

/// Pairing costs one token diff per (old, new) pair and runs under the engine
/// lock. Hunks with more pairs than this, or more tokens to compare in total
/// than [`MAX_PAIRING_TOKENS`] (e.g. pasting over a whole section; Chinese has
/// one token per character), are not paired: their segments get fresh IDs.
const MAX_PAIRING_CELLS: usize = 256;
const MAX_PAIRING_TOKENS: usize = 50_000;

/// Upper bound on a single similarity diff; past it the diff is approximate.
const SIMILARITY_DEADLINE: Duration = Duration::from_millis(20);

/// Stable identifier of a segment within an editing session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SegmentId(pub u64);

/// A segment of the current document together with its identity.
#[derive(Clone, Debug)]
pub struct DocSegment {
    pub id: SegmentId,
    pub segment: Segment,
    /// Hash of the normalized translatable content (raw source for skipped
    /// segments).
    pub hash: ContentHash,
}

impl DocSegment {
    pub fn kind(&self) -> SegmentKind {
        self.segment.kind
    }

    /// Key used by the diff: segments are equal only if both kind and content
    /// match.
    fn key(&self) -> (SegmentKind, ContentHash) {
        (self.segment.kind, self.hash)
    }
}

/// What changed in the last [`DocumentModel::apply`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// IDs that no longer exist.
    pub removed: Vec<SegmentId>,
    /// IDs that are new in this revision.
    pub added: Vec<SegmentId>,
    /// IDs that survived but whose content hash or kind changed.
    pub changed: Vec<SegmentId>,
}

/// The current text, mode and aligned segment list.
#[derive(Debug, Default)]
pub struct DocumentModel {
    text: String,
    mode: Mode,
    segments: Vec<DocSegment>,
    next_id: u64,
}

impl DocumentModel {
    /// Create an empty document. `first_id` lets callers keep IDs unique
    /// across documents opened in the same session.
    pub fn new(first_id: u64) -> Self {
        Self {
            next_id: first_id,
            ..Self::default()
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn segments(&self) -> &[DocSegment] {
        &self.segments
    }

    /// The next ID that would be allocated.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    pub fn index_of(&self, id: SegmentId) -> Option<usize> {
        self.segments.iter().position(|s| s.id == id)
    }

    pub fn get(&self, id: SegmentId) -> Option<&DocSegment> {
        self.segments.iter().find(|s| s.id == id)
    }

    /// Replace the text (and optionally the mode), re-segment and align.
    pub fn apply(&mut self, text: String, mode: Mode) -> ApplyReport {
        let new: Vec<(Segment, ContentHash)> = segment(&text, mode)
            .into_iter()
            .map(|s| {
                let hash = hash_segment(&s, &text);
                (s, hash)
            })
            .collect();
        let ids = align(&self.segments, &self.text, &new, &text);

        let mut report = ApplyReport::default();
        let old_by_id: HashMap<SegmentId, (SegmentKind, ContentHash)> =
            self.segments.iter().map(|s| (s.id, s.key())).collect();
        let mut segments = Vec::with_capacity(new.len());
        for ((seg, hash), id) in new.into_iter().zip(ids) {
            let id = id.unwrap_or_else(|| {
                let id = SegmentId(self.next_id);
                self.next_id += 1;
                id
            });
            match old_by_id.get(&id) {
                None => report.added.push(id),
                Some(key) if *key != (seg.kind, hash) => report.changed.push(id),
                Some(_) => {}
            }
            segments.push(DocSegment {
                id,
                segment: seg,
                hash,
            });
        }
        let kept: std::collections::HashSet<SegmentId> = segments.iter().map(|s| s.id).collect();
        report.removed = self
            .segments
            .iter()
            .map(|s| s.id)
            .filter(|id| !kept.contains(id))
            .collect();

        self.text = text;
        self.mode = mode;
        self.segments = segments;
        report
    }
}

fn hash_segment(seg: &Segment, text: &str) -> ContentHash {
    if seg.kind.is_translatable() {
        ContentHash::of(seg.content(text))
    } else {
        ContentHash::of_raw(seg.source(text).as_bytes())
    }
}

/// Token-level similarity ratio in `[0, 1]` (`2·matches / total tokens`).
/// Tokens are words, or single characters for Chinese (see
/// [`crate::lang::tokens`]); whitespace is ignored so unrelated paragraphs do
/// not look similar just because both contain spaces.
pub fn similarity(a: &str, b: &str) -> f32 {
    let a = tokens(a);
    let b = tokens(b);
    let deadline = Instant::now() + SIMILARITY_DEADLINE;
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(deadline));
    diff_ratio(&ops, a.len(), b.len())
}

/// Assign old IDs to new segments. `None` means "allocate a fresh ID".
fn align(
    old: &[DocSegment],
    old_text: &str,
    new: &[(Segment, ContentHash)],
    new_text: &str,
) -> Vec<Option<SegmentId>> {
    let old_keys: Vec<_> = old.iter().map(DocSegment::key).collect();
    let new_keys: Vec<_> = new.iter().map(|(s, h)| (s.kind, *h)).collect();
    let mut ids: Vec<Option<SegmentId>> = vec![None; new.len()];
    let mut old_used = vec![false; old.len()];
    let mut hunks: Vec<(Range<usize>, Range<usize>)> = Vec::new();

    // 1. LCS alignment.
    for op in capture_diff_slices(Algorithm::Lcs, &old_keys, &new_keys) {
        match op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    ids[new_index + k] = Some(old[old_index + k].id);
                    old_used[old_index + k] = true;
                }
            }
            DiffOp::Replace { .. } => hunks.push((op.old_range(), op.new_range())),
            DiffOp::Delete { .. } | DiffOp::Insert { .. } => {}
        }
    }

    // 2. Moves: identical key elsewhere in the document.
    let mut unused_by_key: HashMap<(SegmentKind, ContentHash), Vec<usize>> = HashMap::new();
    for (i, key) in old_keys.iter().enumerate().rev() {
        if !old_used[i] {
            unused_by_key.entry(*key).or_default().push(i);
        }
    }
    for (j, key) in new_keys.iter().enumerate() {
        if ids[j].is_some() {
            continue;
        }
        if let Some(i) = unused_by_key.get_mut(key).and_then(Vec::pop) {
            ids[j] = Some(old[i].id);
            old_used[i] = true;
        }
    }

    // 3. Edits: pair remaining segments inside each replaced hunk.
    for (old_range, new_range) in hunks {
        let olds: Vec<usize> = old_range.filter(|&i| !old_used[i]).collect();
        let news: Vec<usize> = new_range.filter(|&j| ids[j].is_none()).collect();
        if !pairing_affordable(
            &olds,
            &news,
            |i| old[i].segment.source(old_text),
            |j| new[j].0.source(new_text),
        ) {
            continue;
        }
        for (i, j) in pair_by_similarity(&olds, &news, |i, j| {
            pair_score(&old[i].segment, old_text, &new[j].0, new_text)
        }) {
            ids[j] = Some(old[i].id);
            old_used[i] = true;
        }
    }
    ids
}

/// Whether pairing this hunk stays within the work budget.
fn pairing_affordable<'a>(
    olds: &[usize],
    news: &[usize],
    old_src: impl Fn(usize) -> &'a str,
    new_src: impl Fn(usize) -> &'a str,
) -> bool {
    if olds.len() * news.len() > MAX_PAIRING_CELLS {
        return false;
    }
    let old_tokens: usize = olds.iter().map(|&i| tokens(old_src(i)).len()).sum();
    let new_tokens: usize = news.iter().map(|&j| tokens(new_src(j)).len()).sum();
    // Every old segment is compared with every new one.
    old_tokens * news.len() + new_tokens * olds.len() <= MAX_PAIRING_TOKENS
}

/// Similarity used for pairing; segments of incompatible kinds never pair.
fn pair_score(old: &Segment, old_text: &str, new: &Segment, new_text: &str) -> f32 {
    if old.kind.is_translatable() != new.kind.is_translatable() {
        return 0.0;
    }
    similarity(old.source(old_text), new.source(new_text))
}

/// Order-preserving pairing of `olds` with `news` that maximizes the total
/// similarity, only accepting pairs with score >= [`ID_REUSE_SIMILARITY`].
fn pair_by_similarity(
    olds: &[usize],
    news: &[usize],
    score: impl Fn(usize, usize) -> f32,
) -> Vec<(usize, usize)> {
    let (n, m) = (olds.len(), news.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    let s: Vec<Vec<f32>> = olds
        .iter()
        .map(|&i| news.iter().map(|&j| score(i, j)).collect())
        .collect();
    // best[a][b] = best total for olds[a..] x news[b..]
    let mut best = vec![vec![0.0f32; m + 1]; n + 1];
    for a in (0..n).rev() {
        for b in (0..m).rev() {
            let mut v = best[a + 1][b].max(best[a][b + 1]);
            if s[a][b] >= ID_REUSE_SIMILARITY {
                v = v.max(s[a][b] + best[a + 1][b + 1]);
            }
            best[a][b] = v;
        }
    }
    let (mut a, mut b, mut out) = (0, 0, Vec::new());
    while a < n && b < m {
        if s[a][b] >= ID_REUSE_SIMILARITY && best[a][b] == s[a][b] + best[a + 1][b + 1] {
            out.push((olds[a], news[b]));
            a += 1;
            b += 1;
        } else if best[a][b] == best[a + 1][b] {
            a += 1;
        } else {
            b += 1;
        }
    }
    out
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
