//! Public engine API: document updates, reconciliation against the cache,
//! and user commands (retranslate, pause, concurrency).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use biwrite_core::glossary::Glossary;
use biwrite_core::{ComposeError, ContentHash, Direction, GlossaryEntry, Mode, SegmentId};
use tokio::runtime::Handle;

use crate::cache::TranslationCache;
use crate::events::{EventSink, SegmentStatus, SessionUsage, Snapshot};
use crate::state::{EngineSettings, Priority, SegMeta, State};
use crate::translator::Translator;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EngineError {
    #[error("segment {} does not exist", .0.0)]
    UnknownSegment(SegmentId),
    #[error("segment {} is not translatable", .0.0)]
    NotTranslatable(SegmentId),
    #[error(
        "{missing} paragraph(s) are not translated yet; wait for them to finish \
         (or retry the failed ones) and try again"
    )]
    NotReady { missing: usize },
    #[error(
        "the translation of paragraph {} would change the document structure; \
         retranslate or edit it and try again",
        .index + 1
    )]
    UnsafeTranslation { index: usize },
}

impl From<ComposeError> for EngineError {
    fn from(e: ComposeError) -> Self {
        match e {
            ComposeError::NotReady { missing } => Self::NotReady { missing },
            ComposeError::Structure { index } => Self::UnsafeTranslation { index },
        }
    }
}

pub(crate) struct Inner {
    pub state: Mutex<State>,
    pub cache: Arc<dyn TranslationCache>,
    pub sink: Arc<dyn EventSink>,
    pub runtime: Handle,
}

impl Inner {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Release the lock and deliver queued notices (deduplicated, so a broken
    /// cache doesn't produce one notice per segment).
    pub fn release(&self, mut st: MutexGuard<'_, State>) {
        let mut notices = std::mem::take(&mut st.notices);
        notices.dedup();
        drop(st);
        for notice in notices {
            self.sink.notice(&notice);
        }
    }

    /// Bring every segment's translation state in line with the current text.
    /// Segments in `urgent` (just edited) jump the queue; the rest keep their
    /// place or join the back.
    pub fn reconcile(&self, st: &mut State, urgent: &HashSet<SegmentId>) {
        let ids: Vec<SegmentId> = st.doc.segments().iter().map(|s| s.id).collect();
        let (mut now, mut later) = (Vec::new(), Vec::new());
        for id in ids {
            if self.reconcile_one(st, id) {
                if urgent.contains(&id) {
                    now.push(id);
                } else {
                    later.push(id);
                }
            }
        }
        st.enqueue_all(&later, Priority::Normal);
        st.enqueue_all(&now, Priority::Urgent);
    }

    /// Returns `true` if the segment must be queued.
    fn reconcile_one(&self, st: &mut State, id: SegmentId) -> bool {
        let Some(seg) = st.doc.get(id) else {
            return false;
        };
        let (hash, kind) = (seg.hash, seg.kind());
        let content = seg.segment.content(st.doc.text()).to_owned();
        if let std::collections::hash_map::Entry::Vacant(slot) = st.meta.entry(id) {
            slot.insert(SegMeta::new());
            st.update(id, |_| {});
        }

        if !kind.is_translatable() {
            st.cancel(id);
            st.set_status(id, SegmentStatus::Skipped);
            return false;
        }
        if content.trim().is_empty() {
            st.cancel(id);
            st.update(id, |m| {
                m.translation = Some(String::new());
                m.translated_hash = Some(hash);
                m.status = SegmentStatus::Translated;
                m.forced = false;
                m.error = None;
            });
            return false;
        }

        let Some(meta) = st.meta.get(&id) else {
            return false;
        };
        let forced = meta.forced;
        if meta.translated_hash == Some(hash) && !forced {
            st.cancel(id);
            st.set_status(id, SegmentStatus::Translated);
            return false;
        }
        // Permanent failures stick until the text changes or the user retries;
        // transient ones (network, 429, 5xx) are retried on the next pass.
        let sticky_error = meta
            .error
            .as_ref()
            .is_some_and(|e| e.hash == hash && !e.retryable);

        if !forced {
            let seed = meta
                .seed
                .as_ref()
                .filter(|(h, _)| *h == hash)
                .map(|(_, original)| original.clone())
                .or_else(|| st.seeds.get(&hash).cloned().flatten());
            if let Some(original) = seed {
                adopt(st, id, hash, content, original, true);
                return false;
            }
            let glossary_fp = st.glossary_fp(&content);
            match self.cache.get(&st.cache_key(hash, glossary_fp)) {
                Ok(Some(translation)) => {
                    st.usage.cache_hits += 1;
                    let placeholder = st.translator.is_placeholder();
                    adopt(st, id, hash, content, translation, false);
                    st.update(id, |m| {
                        m.placeholder = placeholder;
                        m.glossary_fp = glossary_fp;
                    });
                    return false;
                }
                Ok(None) => {}
                Err(e) => st.notices.push(e.to_string()),
            }
        }

        // A translation is needed.
        match st.running.get(&id) {
            Some(running) if running.hash == hash => return false,
            Some(_) => st.cancel(id),
            None => {}
        }
        if sticky_error && !forced {
            st.set_status(id, SegmentStatus::Error);
            false
        } else if forced || st.settings.auto_translate {
            true
        } else {
            st.set_status(id, SegmentStatus::Stale);
            false
        }
    }
}

/// Use a known translation (cache hit, or an `exact` swap seed) for `id`.
fn adopt(
    st: &mut State,
    id: SegmentId,
    hash: ContentHash,
    source: String,
    translation: String,
    exact: bool,
) {
    st.cancel(id);
    st.update(id, |m| {
        m.exact = exact;
        m.placeholder = false;
        m.translation = Some(translation);
        m.translated_hash = Some(hash);
        m.translated_source = Some(source);
        m.status = SegmentStatus::Translated;
        m.partial = None;
        m.error = None;
    });
}

/// The translation engine. Cheap to clone; all clones share state.
///
/// Must be used from within (or given a handle to) a tokio runtime: jobs are
/// spawned on `runtime`.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

impl Engine {
    pub fn new(
        translator: Arc<dyn Translator>,
        cache: Arc<dyn TranslationCache>,
        sink: Arc<dyn EventSink>,
        settings: EngineSettings,
        runtime: Handle,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::new(translator, settings)),
                cache,
                sink,
                runtime,
            }),
        }
    }

    /// Replace the document (new file, English). In-flight work is cancelled.
    pub fn load(&self, text: String, mode: Mode) -> Snapshot {
        let mut st = self.inner.lock();
        st.cancel_all();
        st.meta.clear();
        st.direction = Direction::EnZh;
        st.seeds.clear();
        st.doc = biwrite_core::DocumentModel::new(st.doc.next_id());
        st.doc.apply(text, mode);
        self.inner.reconcile(&mut st, &HashSet::new());
        self.inner.pump(&mut st);
        st.revision += 1;
        let snapshot = st.snapshot(true);
        self.inner.release(st);
        snapshot
    }

    /// New text from the editor (after debounce).
    pub fn update(&self, text: String) -> Snapshot {
        self.apply(text, None)
    }

    /// Change the mode (and pick up the latest text at the same time).
    pub fn set_mode(&self, mode: Mode, text: String) -> Snapshot {
        self.apply(text, Some(mode))
    }

    fn apply(&self, text: String, mode: Option<Mode>) -> Snapshot {
        let mut st = self.inner.lock();
        self.apply_locked(&mut st, text, mode);
        let snapshot = st.snapshot(false);
        self.inner.release(st);
        snapshot
    }

    /// Re-segment, align, reconcile and schedule. No-op if nothing changed.
    pub(crate) fn apply_locked(&self, st: &mut State, text: String, mode: Option<Mode>) {
        let mode = mode.unwrap_or(st.doc.mode());
        if text == st.doc.text() && mode == st.doc.mode() {
            return;
        }
        let report = st.doc.apply(text, mode);
        for id in &report.removed {
            st.cancel(*id);
            st.meta.remove(id);
        }
        // What the user just edited goes ahead of any backlog.
        let edited: HashSet<SegmentId> = report.added.into_iter().chain(report.changed).collect();
        self.inner.reconcile(st, &edited);
        self.inner.pump(st);
        st.revision += 1;
    }

    /// Layout and all segment states.
    pub fn snapshot(&self) -> Snapshot {
        let mut st = self.inner.lock();
        let snapshot = st.snapshot(true);
        self.inner.release(st);
        snapshot
    }

    pub fn text(&self) -> String {
        self.inner.lock().doc.text().to_owned()
    }

    pub fn mode(&self) -> Mode {
        self.inner.lock().doc.mode()
    }

    /// Which language is being edited.
    pub fn direction(&self) -> Direction {
        self.inner.lock().direction
    }

    /// Force a fresh translation of one segment (bypasses cache and pause).
    pub fn retranslate(&self, id: SegmentId) -> Result<(), EngineError> {
        let mut st = self.inner.lock();
        let seg = st.doc.get(id).ok_or(EngineError::UnknownSegment(id))?;
        if !seg.kind().is_translatable() {
            return Err(EngineError::NotTranslatable(id));
        }
        if seg.segment.content(st.doc.text()).trim().is_empty() {
            // Nothing to send (e.g. an empty heading); its translation is "".
            return Ok(());
        }
        self.force(&mut st, &[id], Priority::Urgent);
        self.inner.pump(&mut st);
        self.emit_touched(st);
        Ok(())
    }

    /// Force a fresh translation of every translatable segment.
    pub fn retranslate_all(&self) {
        let mut st = self.inner.lock();
        let ids: Vec<SegmentId> = st
            .doc
            .segments()
            .iter()
            .filter(|s| {
                s.kind().is_translatable() && !s.segment.content(st.doc.text()).trim().is_empty()
            })
            .map(|s| s.id)
            .collect();
        self.force(&mut st, &ids, Priority::Normal);
        self.inner.pump(&mut st);
        self.emit_touched(st);
    }

    fn force(&self, st: &mut State, ids: &[SegmentId], priority: Priority) {
        for &id in ids {
            st.cancel(id);
            st.update(id, |m| {
                m.forced = true;
                m.error = None;
            });
        }
        st.enqueue_all(ids, priority);
    }

    /// Pause or resume automatic translation of changed segments. Pausing
    /// drops queued (not in-flight) automatic work; resuming enqueues it again.
    pub fn set_auto_translate(&self, on: bool) {
        let mut st = self.inner.lock();
        st.settings.auto_translate = on;
        if on {
            self.inner.reconcile(&mut st, &HashSet::new());
            self.inner.pump(&mut st);
        } else {
            let queued: Vec<SegmentId> = st.queue.iter().copied().collect();
            for id in queued {
                if st.meta.get(&id).is_some_and(|m| !m.forced) {
                    st.queue.retain(|q| *q != id);
                    st.set_status(id, SegmentStatus::Stale);
                }
            }
        }
        self.emit_touched(st);
    }

    pub fn set_concurrency(&self, n: usize) {
        let mut st = self.inner.lock();
        st.settings.concurrency = n.clamp(1, 32);
        self.inner.pump(&mut st);
        self.emit_touched(st);
    }

    pub fn set_doc_note(&self, note: Option<String>) {
        self.inner.lock().settings.doc_note = note.filter(|n| !n.trim().is_empty());
    }

    /// Replace the glossary. Paragraphs whose relevant entries changed are
    /// translated again (from the cache if possible, respecting pause);
    /// everything else, and the user's own text from a swap, is kept.
    pub fn set_glossary(&self, entries: Vec<GlossaryEntry>) {
        // Index outside the lock: typing shouldn't wait for it.
        let glossary = Glossary::new(entries);
        let mut st = self.inner.lock();
        st.glossary = glossary;
        let text = st.doc.text();
        let current: Vec<(SegmentId, ContentHash, u64)> = st
            .doc
            .segments()
            .iter()
            .filter(|s| s.kind().is_translatable())
            .map(|s| (s.id, s.hash, st.glossary_fp(s.segment.content(text))))
            .collect();
        for (id, hash, fp) in current {
            if st.running.get(&id).is_some_and(|r| r.glossary_fp != fp) {
                st.cancel(id);
            }
            let outdated = st.meta.get(&id).is_some_and(|m| {
                m.translated_hash == Some(hash) && !m.exact && m.glossary_fp != fp
            });
            if outdated {
                st.update(id, |m| m.translated_hash = None);
            }
        }
        self.inner.reconcile(&mut st, &HashSet::new());
        self.inner.pump(&mut st);
        self.emit_touched(st);
    }

    /// Swap the translator. Queued and in-flight work is (re)done with it,
    /// failed segments are retried, and mock output is replaced when a real
    /// translator arrives. Other existing translations are kept.
    pub fn set_translator(&self, translator: Arc<dyn Translator>) {
        let mut st = self.inner.lock();
        let real = !translator.is_placeholder();
        st.translator = translator;
        // In-flight requests belong to the old provider: redo them.
        let running: Vec<SegmentId> = st.running.keys().copied().collect();
        for id in running {
            st.cancel(id);
        }
        // Failures may have come from the old provider or a missing key, and
        // mock output is only a stand-in once a real provider is chosen.
        let redo: Vec<SegmentId> = st
            .meta
            .iter()
            .filter(|(_, m)| m.error.is_some() || (real && m.placeholder))
            .map(|(id, _)| *id)
            .collect();
        for id in redo {
            st.update(id, |m| {
                m.error = None;
                // `placeholder` stays set until real output replaces it, so
                // the stand-in text is never sent as a basis for revision.
                if real && m.placeholder {
                    m.translated_hash = None;
                }
            });
        }
        self.inner.reconcile(&mut st, &HashSet::new());
        self.inner.pump(&mut st);
        self.emit_touched(st);
    }

    pub fn settings(&self) -> EngineSettings {
        self.inner.lock().settings.clone()
    }

    pub fn usage(&self) -> SessionUsage {
        self.inner.lock().usage
    }

    /// Number of queued plus in-flight jobs.
    pub fn pending(&self) -> usize {
        let st = self.inner.lock();
        st.queue.len() + st.running.len()
    }

    pub(crate) fn emit_touched(&self, mut st: MutexGuard<'_, State>) {
        let states = st.take_touched();
        self.inner.release(st);
        if !states.is_empty() {
            self.inner.sink.segment_states(&states);
        }
    }
}
