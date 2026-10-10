//! Engine state guarded by a single mutex. Never held across `.await`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use biwrite_core::compose::fit;
use biwrite_core::glossary::{self, Glossary};
use biwrite_core::lang::moved_into;
use biwrite_core::utf16::byte_to_utf16;
use biwrite_core::{ContentHash, Direction, DocumentModel, Protector, SegmentId};
use tokio::task::AbortHandle;

use crate::cache::CacheKey;
use crate::events::{Fill, SegmentLayout, SegmentState, SegmentStatus, SessionUsage, Snapshot};
use crate::translator::Translator;

/// User-tunable engine behaviour.
#[derive(Clone, Debug)]
pub struct EngineSettings {
    /// Maximum concurrent provider requests.
    pub concurrency: usize,
    /// Translate changed segments automatically (toolbar "pause" turns it off).
    pub auto_translate: bool,
    pub doc_note: Option<String>,
    /// Use revise mode when old and new source are more similar than this.
    pub revise_threshold: f32,
    /// Retries for rate-limit/server/network errors.
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// Minimum interval between streamed partial updates per segment.
    pub stream_throttle: Duration,
    /// Segments translated together in one request (1: one per request).
    /// Only fresh translations are batched, never revisions of an edit.
    pub batch_size: usize,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            concurrency: 4,
            auto_translate: true,
            doc_note: None,
            revise_threshold: 0.6,
            max_retries: 5,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            stream_throttle: Duration::from_millis(50),
            batch_size: 1,
        }
    }
}

/// Largest batch the settings allow.
pub const MAX_BATCH: usize = 8;

/// Translation bookkeeping for one segment.
#[derive(Debug)]
pub(crate) struct SegMeta {
    pub version: u64,
    /// Bumped on every dispatch and cancellation; results from older
    /// generations are discarded.
    pub generation: u64,
    pub status: SegmentStatus,
    /// Last good translation (possibly of an older source text).
    pub translation: Option<String>,
    /// Hash of the source that `translation` belongs to.
    pub translated_hash: Option<ContentHash>,
    /// Source text that `translation` belongs to (for revise mode).
    pub translated_source: Option<String>,
    /// Streamed output of the in-flight request.
    pub partial: Option<String>,
    /// Last failure, tied to the source hash it happened on.
    pub error: Option<SegError>,
    /// Explicit retranslation requested: bypass cache and the pause switch.
    pub forced: bool,
    /// From a language swap: while the segment's hash is still this one, its
    /// translation is exactly this original text.
    pub seed: Option<(ContentHash, String)>,
    /// `translation` is an exact original (seed), to be spliced verbatim.
    pub exact: bool,
    /// `translation` came from a placeholder translator (the mock).
    pub placeholder: bool,
    /// Fingerprint of the glossary entries `translation` was made with.
    pub glossary_fp: u64,
    /// A batch had no usable answer for this segment: send it on its own.
    pub single: bool,
    /// Still written in the language of the translations (left so by an
    /// early swap): while the segment's hash is this one, it is translated
    /// into the edited language and replaced in the editor, and its own
    /// translation stays the exact original.
    pub fill: Option<ContentHash>,
}

impl SegMeta {
    pub fn new() -> Self {
        Self {
            version: 0,
            generation: 0,
            status: SegmentStatus::Stale,
            translation: None,
            translated_hash: None,
            translated_source: None,
            partial: None,
            error: None,
            forced: false,
            seed: None,
            exact: false,
            placeholder: false,
            glossary_fp: 0,
            single: false,
            fill: None,
        }
    }

    pub fn view(&self, id: SegmentId) -> SegmentState {
        let (text, partial) = match (&self.status, &self.partial) {
            (SegmentStatus::Translating, Some(p)) if !p.is_empty() => (Some(p.clone()), true),
            _ => (self.translation.clone(), false),
        };
        SegmentState {
            id,
            version: self.version,
            status: self.status,
            text,
            partial,
            error: match self.status {
                SegmentStatus::Error => self.error.as_ref().map(|e| e.message.clone()),
                _ => None,
            },
        }
    }
}

/// A failed translation attempt.
#[derive(Debug)]
pub(crate) struct SegError {
    pub hash: ContentHash,
    pub message: String,
    /// Transient (network, rate limit, 5xx): retried on the next reconcile
    /// instead of sticking until the user clicks retry.
    pub retryable: bool,
}

/// Where a job enters the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Priority {
    /// Document order behind existing work (initial load, retranslate all).
    Normal,
    /// Ahead of the backlog (the user just edited or asked for it).
    Urgent,
}

pub(crate) struct Running {
    pub generation: u64,
    pub hash: ContentHash,
    /// Fingerprint of the glossary entries sent with the request.
    pub glossary_fp: u64,
    pub abort: AbortHandle,
    /// Set when the request carries other segments too.
    pub batch: Option<Arc<BatchLink>>,
}

/// Shared by the segments of one batch request: the request is aborted
/// only when every one of them has been cancelled.
#[derive(Debug)]
pub(crate) struct BatchLink {
    pub live: AtomicUsize,
}

pub(crate) struct State {
    pub doc: DocumentModel,
    pub direction: Direction,
    /// Fallback for swap seeds by content (when a segment lost its ID, e.g.
    /// cut and pasted back): hash → original, `None` if several different
    /// originals share the hash. Per-segment seeds live in [`SegMeta::seed`].
    pub seeds: HashMap<ContentHash, Option<String>>,
    pub meta: HashMap<SegmentId, SegMeta>,
    /// Bumped whenever the document is replaced (a file loaded, a swap), so
    /// text sent for an earlier one can be refused.
    pub document: u64,
    pub revision: u64,
    pub version_seq: u64,
    pub queue: VecDeque<SegmentId>,
    pub running: HashMap<SegmentId, Running>,
    pub translator: Arc<dyn Translator>,
    pub settings: EngineSettings,
    pub glossary: Glossary,
    pub usage: SessionUsage,
    /// Segments whose state changed during the current operation.
    pub touched: Vec<SegmentId>,
    /// Non-fatal messages to surface after the lock is released.
    pub notices: Vec<String>,
    /// Paragraphs for the editor to replace, sent after the lock is released.
    pub fills: Vec<Fill>,
}

impl State {
    pub fn new(translator: Arc<dyn Translator>, settings: EngineSettings) -> Self {
        Self {
            doc: DocumentModel::new(1),
            direction: Direction::default(),
            seeds: HashMap::new(),
            meta: HashMap::new(),
            document: 0,
            revision: 0,
            version_seq: 0,
            queue: VecDeque::new(),
            running: HashMap::new(),
            translator,
            settings,
            glossary: Glossary::default(),
            usage: SessionUsage::default(),
            touched: Vec::new(),
            notices: Vec::new(),
            fills: Vec::new(),
        }
    }

    pub fn cache_key(&self, hash: ContentHash, glossary_fp: u64) -> CacheKey {
        self.cache_key_toward(self.direction, hash, glossary_fp)
    }

    /// Cache key of a translation in `direction` (the other way round for
    /// a paragraph still in the language of the translations).
    pub fn cache_key_toward(
        &self,
        direction: Direction,
        hash: ContentHash,
        glossary_fp: u64,
    ) -> CacheKey {
        CacheKey {
            hash,
            direction,
            provider: self.translator.provider().to_owned(),
            model: self.translator.model().to_owned(),
            glossary: glossary_fp,
        }
    }

    /// Fingerprint of the entries a segment with this text gets.
    pub fn glossary_fp(&self, content: &str) -> u64 {
        self.glossary_fp_toward(self.direction, content)
    }

    /// [`Self::glossary_fp`] for a translation in `direction`. Entries are
    /// matched on the text with protected spans masked, so terms inside
    /// citation keys or math don't count.
    pub fn glossary_fp_toward(&self, direction: Direction, content: &str) -> u64 {
        if self.glossary.is_empty() {
            return 0;
        }
        let masked = Protector::new(self.doc.mode()).mask(content);
        glossary::fingerprint(&self.glossary.relevant(&masked, direction))
    }

    /// Mutate a segment's meta, bump its version and mark it for emission.
    pub fn update(&mut self, id: SegmentId, f: impl FnOnce(&mut SegMeta)) {
        if let Some(meta) = self.meta.get_mut(&id) {
            f(meta);
            self.version_seq += 1;
            meta.version = self.version_seq;
            self.touched.push(id);
        }
    }

    /// Replace a paragraph still in the language of the translations with
    /// `translation` (into the edited language): the editor is sent the new
    /// text (a [`Fill`]), and once it arrives the paragraph's translation is
    /// its present text, exactly.
    ///
    /// Refused when the paragraph turns out to be in the edited language
    /// already (taken for the other one by mistake, like Chinese dense with
    /// English names): the "translation" didn't change the language, or the
    /// paragraph has an exact original that isn't its own text. It is then
    /// translated the usual way round instead, and its original kept.
    pub fn fill(&mut self, id: SegmentId, translation: &str) {
        let Some(seg) = self.doc.get(id) else {
            return;
        };
        let text = self.doc.text();
        let (range, content) = (seg.segment.range.clone(), seg.segment.content.clone());
        let present = text[content.clone()].to_owned();
        let shaped = fit(translation.trim(), seg.kind(), self.doc.mode(), &present);
        let current = seg.hash;
        let has_original = self.meta.get(&id).is_some_and(|m| {
            m.exact
                && m.translated_hash == Some(current)
                && m.translation.as_deref() != Some(present.as_str())
        });
        if has_original || !moved_into(self.direction, &present, &shaped, self.doc.mode()) {
            self.refuse_fill(id, current);
            return;
        }
        let fill = (!shaped.trim().is_empty() && shaped != present).then(|| Fill {
            id,
            old: text[range.clone()].to_owned(),
            new: format!(
                "{}{shaped}{}",
                &text[range.start..content.start],
                &text[content.end..range.end]
            ),
        });
        let hash = ContentHash::of(&shaped);
        if fill.is_some() {
            // Also by content, in case the paragraph's ID changes on the way.
            self.seeds
                .entry(hash)
                .and_modify(|known| {
                    if known.as_deref() != Some(present.as_str()) {
                        *known = None;
                    }
                })
                .or_insert_with(|| Some(present.clone()));
        }
        self.update(id, |m| {
            m.fill = None;
            m.forced = false;
            m.partial = None;
            m.error = None;
            if m.translated_hash == Some(current) {
                m.status = SegmentStatus::Translated;
            }
            if fill.is_some() {
                m.seed = Some((hash, present));
            }
        });
        self.fills.extend(fill);
    }

    /// The paragraph `id` (with hash `current`) is in the edited language
    /// after all: no fill. It keeps an up-to-date translation; without one it
    /// is translated the usual way round.
    fn refuse_fill(&mut self, id: SegmentId, current: ContentHash) {
        let mut queue = false;
        let auto = self.settings.auto_translate;
        self.update(id, |m| {
            m.fill = None;
            m.partial = None;
            m.error = None;
            if m.translated_hash == Some(current) {
                m.status = SegmentStatus::Translated;
                m.forced = false;
            } else {
                queue = m.forced || auto;
                m.status = SegmentStatus::Stale;
            }
        });
        if queue {
            self.enqueue_all(&[id], Priority::Normal);
        }
    }

    /// Set a status (and clear transient fields) only if it differs.
    pub fn set_status(&mut self, id: SegmentId, status: SegmentStatus) {
        if self.meta.get(&id).is_some_and(|m| m.status != status) {
            self.update(id, |m| {
                m.status = status;
                m.partial = None;
            });
        }
    }

    /// Drop any queued or in-flight job for `id`. A batch request goes on
    /// for its other segments, and the result for `id` is discarded.
    pub fn cancel(&mut self, id: SegmentId) {
        self.queue.retain(|q| *q != id);
        if let Some(running) = self.running.remove(&id) {
            let last = running
                .batch
                .as_ref()
                .is_none_or(|link| link.live.fetch_sub(1, Ordering::SeqCst) == 1);
            if last {
                running.abort.abort();
            }
            if let Some(meta) = self.meta.get_mut(&id) {
                meta.generation += 1;
            }
        }
    }

    /// Provider requests in flight (a batch counts once).
    pub fn requests_in_flight(&self) -> usize {
        let mut batches = HashSet::new();
        self.running
            .values()
            .filter(|r| match &r.batch {
                None => true,
                Some(link) => batches.insert(Arc::as_ptr(link)),
            })
            .count()
    }

    pub fn cancel_all(&mut self) {
        self.queue.clear();
        for (_, running) in self.running.drain() {
            running.abort.abort();
        }
    }

    /// Queue segments (in the given order). Normal work joins the back of the
    /// queue, keeping the position of anything already queued; urgent work is
    /// moved ahead of the backlog.
    pub fn enqueue_all(&mut self, ids: &[SegmentId], priority: Priority) {
        let ids: Vec<SegmentId> = ids
            .iter()
            .copied()
            .filter(|id| !self.running.contains_key(id))
            .collect();
        match priority {
            Priority::Normal => {
                for &id in &ids {
                    if !self.queue.contains(&id) {
                        self.queue.push_back(id);
                    }
                }
            }
            Priority::Urgent => {
                self.queue.retain(|q| !ids.contains(q));
                for &id in ids.iter().rev() {
                    self.queue.push_front(id);
                }
            }
        }
        for id in ids {
            self.set_status(id, SegmentStatus::Queued);
        }
    }

    /// States of the segments touched since the last call (deduplicated).
    pub fn take_touched(&mut self) -> Vec<SegmentState> {
        let mut seen = HashSet::new();
        let touched = std::mem::take(&mut self.touched);
        touched
            .into_iter()
            .filter(|id| seen.insert(*id))
            .filter_map(|id| self.meta.get(&id).map(|m| m.view(id)))
            .collect()
    }

    pub fn all_states(&self) -> Vec<SegmentState> {
        self.doc
            .segments()
            .iter()
            .filter_map(|s| self.meta.get(&s.id).map(|m| m.view(s.id)))
            .collect()
    }

    pub fn layout(&self) -> Vec<SegmentLayout> {
        let segments = self.doc.segments();
        let bounds = segments
            .iter()
            .flat_map(|s| [s.segment.range.start, s.segment.range.end]);
        let utf16 = byte_to_utf16(self.doc.text(), bounds);
        segments
            .iter()
            .zip(utf16.chunks_exact(2))
            .map(|(s, r)| SegmentLayout {
                id: s.id,
                kind: s.kind(),
                from: r[0],
                to: r[1],
            })
            .collect()
    }

    /// Layout plus either all states or only those touched in this operation.
    pub fn snapshot(&mut self, full: bool) -> Snapshot {
        let states = if full {
            self.touched.clear();
            self.all_states()
        } else {
            self.take_touched()
        };
        Snapshot {
            document: self.document,
            revision: self.revision,
            mode: self.doc.mode(),
            direction: self.direction,
            full,
            layout: self.layout(),
            states,
            usage: self.usage,
        }
    }
}
