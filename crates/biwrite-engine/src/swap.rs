//! Swapping the panes (edit the other language) and composing the
//! other-language document.
//!
//! Swapping builds the target-language document from the current
//! translations ([`biwrite_core::compose`]) and loads it as the new source.
//! The original texts become *seeds*: every segment the user leaves alone
//! translates back to its exact original wording, so swapping back (or
//! saving the English while editing Chinese) changes only edited paragraphs.
//! A paragraph not translated yet when swapping early keeps its text and is
//! *filled* in afterwards: its translation the other way round replaces it
//! in the editor (see [`crate::Fill`]).

use std::collections::{HashMap, HashSet};

use biwrite_core::{
    Bilingual, ComposeError, Composed, ContentHash, DocSegment, DocumentModel, Insert, SegmentId,
    bilingual_markdown, compose,
};

use crate::engine::{Engine, EngineError};
use crate::events::Snapshot;
use crate::state::{SegMeta, State};

/// Result of [`Engine::compose_mirror`].
#[derive(Debug, PartialEq, Eq)]
pub struct Mirror {
    pub text: String,
    /// Paragraphs that kept their source text.
    pub untranslated: usize,
}

/// Result of [`Engine::swap`].
#[derive(Debug)]
pub struct Swapped {
    /// The new editor text (in the newly edited language).
    pub text: String,
    pub snapshot: Snapshot,
}

/// Current translation of a segment, if it is up to date. Exact originals
/// (seeds) are spliced verbatim; machine output is shaped to fit.
fn current(meta: &HashMap<SegmentId, SegMeta>, seg: &DocSegment) -> Option<Insert> {
    let m = meta
        .get(&seg.id)
        .filter(|m| m.translated_hash == Some(seg.hash))?;
    let text = m.translation.clone()?;
    Some(Insert {
        text,
        exact: m.exact,
    })
}

fn compose_current(st: &State) -> Result<Composed, ComposeError> {
    compose(&st.doc, |seg| current(&st.meta, seg))
}

/// Like [`compose_current`], with each paragraph lacking a translation (or
/// whose translation would change the structure) kept as it is. Also says,
/// for each translated slot in order, whether it was kept.
fn compose_keeping(st: &State) -> Result<(Composed, Vec<bool>), ComposeError> {
    let index: HashMap<SegmentId, usize> = st
        .doc
        .segments()
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id, i))
        .collect();
    let mut keep: HashSet<usize> = HashSet::new();
    loop {
        let mut kept = Vec::new();
        let composed = compose(&st.doc, |seg| {
            let pinned = index.get(&seg.id).is_some_and(|i| keep.contains(i));
            let insert = current(&st.meta, seg).filter(|_| !pinned);
            kept.push(insert.is_none());
            insert.or_else(|| Some(Insert::exact(seg.segment.content(st.doc.text()))))
        });
        match composed {
            Err(ComposeError::Structure { index }) => {
                let last = index.min(st.doc.segments().len().saturating_sub(1));
                match (0..=last).rev().find(|i| !keep.contains(i)) {
                    Some(i) => {
                        keep.insert(i);
                    }
                    None => return Err(ComposeError::Structure { index }),
                }
            }
            other => return other.map(|c| (c, kept)),
        }
    }
}

/// Attach each original to the segment it was spliced into. `compose`
/// verified that the k-th translated segment of the new document holds
/// exactly the k-th inserted text, so this is positional, not by hash:
/// paragraphs whose translations happen to coincide keep distinct originals.
/// The k-th segment is to be filled in if `kept[k]`.
fn install_seeds(st: &mut State, pairs: Vec<(String, String)>, kept: &[bool]) {
    let text = st.doc.text();
    let targets: Vec<(SegmentId, ContentHash)> = st
        .doc
        .segments()
        .iter()
        .filter(|s| s.kind().is_translatable() && !s.segment.content(text).trim().is_empty())
        .map(|s| (s.id, s.hash))
        .collect();
    let mut by_hash: HashMap<ContentHash, Option<String>> = HashMap::new();
    for (k, ((id, hash), (_inserted, original))) in targets.into_iter().zip(pairs).enumerate() {
        by_hash
            .entry(hash)
            .and_modify(|known| {
                if known.as_deref() != Some(original.as_str()) {
                    *known = None;
                }
            })
            .or_insert_with(|| Some(original.clone()));
        let mut meta = SegMeta::new();
        meta.seed = Some((hash, original));
        if kept.get(k).copied().unwrap_or(false) {
            meta.fill = Some(hash);
        }
        st.meta.insert(id, meta);
        st.touched.push(id);
    }
    st.seeds = by_hash;
}

impl Engine {
    /// Swap languages: after applying the editor's latest `text`, the
    /// translations become the new editable source and the current source
    /// becomes their (exact) translation.
    ///
    /// Fails with [`EngineError::NotReady`] if any paragraph lacks an
    /// up-to-date translation, so no half-translated document is produced.
    pub fn swap(&self, text: String) -> Result<Swapped, EngineError> {
        self.swap_with(text, false)
    }

    /// Swap now, before every paragraph is translated: paragraphs without
    /// an up-to-date translation keep their text and come back unchanged
    /// when swapping back. Meanwhile they are translated the other way
    /// round and replaced in the editor ([`crate::Fill`] events), unless
    /// translation is paused.
    pub fn swap_keeping_untranslated(&self, text: String) -> Result<Swapped, EngineError> {
        self.swap_with(text, true)
    }

    fn swap_with(&self, text: String, keep: bool) -> Result<Swapped, EngineError> {
        let mut st = self.inner.lock();
        self.apply_locked(&mut st, text, None);
        let composed = if keep {
            compose_keeping(&st)
        } else {
            compose_current(&st).map(|c| (c, Vec::new()))
        };
        let (composed, kept) = match composed {
            Ok(c) => c,
            Err(e) => {
                self.emit_touched(st);
                return Err(e.into());
            }
        };

        let mode = st.doc.mode();
        st.cancel_all();
        st.meta.clear();
        st.touched.clear();
        st.document += 1;
        st.doc = DocumentModel::new(st.doc.next_id());
        st.doc.apply(composed.text.clone(), mode);
        st.direction = st.direction.flipped();
        install_seeds(&mut st, composed.pairs, &kept);
        self.inner.reconcile(&mut st, &Default::default());
        self.inner.pump(&mut st);
        st.revision += 1;
        let snapshot = st.snapshot(true);
        self.inner.release(st);
        Ok(Swapped {
            text: composed.text,
            snapshot,
        })
    }

    /// The document in the target language (e.g. the English file while
    /// editing Chinese). Fails if any paragraph is not translated yet.
    pub fn compose_target(&self) -> Result<String, EngineError> {
        let st = self.inner.lock();
        Ok(compose_current(&st)?.text)
    }

    /// The document in the other language for previewing it (the Chinese
    /// PDF): paragraphs without an up-to-date translation, and translations
    /// that would change the document's structure, keep the source text.
    /// Returns the text and how many paragraphs kept their source.
    pub fn compose_mirror(&self) -> Mirror {
        let st = self.inner.lock();
        let index: HashMap<SegmentId, usize> = st
            .doc
            .segments()
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        let mut keep: HashSet<usize> = HashSet::new();
        // Each failed attempt pins one more paragraph to its source.
        for _ in 0..=st.doc.segments().len() {
            let mut kept = 0;
            let composed = compose(&st.doc, |seg| {
                let source = seg.segment.content(st.doc.text());
                let pinned = index.get(&seg.id).is_some_and(|i| keep.contains(i));
                match current(&st.meta, seg).filter(|_| !pinned) {
                    Some(insert) => Some(insert),
                    None => {
                        kept += 1;
                        Some(Insert::exact(source))
                    }
                }
            });
            match composed {
                Ok(c) => {
                    return Mirror {
                        text: c.text,
                        untranslated: kept,
                    };
                }
                // The paragraph named, else the closest one before it.
                Err(ComposeError::Structure { index }) => {
                    let last = index.min(st.doc.segments().len().saturating_sub(1));
                    match (0..=last).rev().find(|i| !keep.contains(i)) {
                        Some(i) => {
                            keep.insert(i);
                        }
                        None => break,
                    }
                }
                Err(ComposeError::NotReady { .. }) => break,
            }
        }
        Mirror {
            text: st.doc.text().to_owned(),
            untranslated: st.doc.segments().len(),
        }
    }

    /// Bilingual Markdown: each paragraph in English, then in Chinese.
    /// Paragraphs without an up-to-date translation get a note instead.
    pub fn bilingual_markdown(&self) -> Bilingual {
        let st = self.inner.lock();
        bilingual_markdown(&st.doc, st.direction, |seg| current(&st.meta, seg))
    }
}
