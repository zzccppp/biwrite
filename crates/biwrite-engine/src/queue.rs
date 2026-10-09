//! Job dispatch: concurrency limit, per-segment generations, streaming,
//! exponential backoff, and applying results.

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use biwrite_core::{ContentHash, Protector, SegmentId, glossary, similarity};
use tokio::time::Instant;

use crate::cache::CacheKey;
use crate::engine::Inner;
use crate::events::SegmentStatus;
use crate::random::random_between;
use crate::state::{BatchLink, MAX_BATCH, Priority, Running, SegError, State};
use crate::translator::{Revision, TokenUsage, TranslateError, TranslationRequest, Translator};

/// How far down the queue a batch looks for segments to join it.
const BATCH_LOOKAHEAD: usize = 32;

/// A dispatched translation request.
pub(crate) struct Job {
    id: SegmentId,
    generation: u64,
    hash: ContentHash,
    key: CacheKey,
    /// The segment's text as written; `request.source` is its masked form.
    source: String,
    /// Restores the placeholders in the model's output.
    protector: Protector,
    request: TranslationRequest,
    translator: Arc<dyn Translator>,
    retry: RetryPolicy,
    stream_throttle: Duration,
    /// A paragraph still in the language of the translations, translated
    /// the other way round to replace it in the editor. Its output is not
    /// streamed: the right pane keeps showing the original.
    fill: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    pub max_retries: u32,
    pub base: Duration,
    pub max: Duration,
}

/// Delay before retry number `attempt` (0-based): server hint if given,
/// otherwise `base * 2^attempt` plus up to 25% jitter, capped at `max`.
pub(crate) fn backoff_delay(
    attempt: u32,
    retry_after: Option<Duration>,
    p: RetryPolicy,
) -> Duration {
    if let Some(hint) = retry_after {
        return hint.min(p.max);
    }
    let exp = p
        .base
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(p.max);
    (exp + random_between(Duration::ZERO, exp / 4)).min(p.max)
}

impl Inner {
    /// Start queued jobs until the concurrency limit (provider requests in
    /// flight) is reached. With a batch size above one, fresh translations
    /// queued together go out in one request.
    pub(crate) fn pump(self: &Arc<Self>, st: &mut State) {
        while st.requests_in_flight() < st.settings.concurrency.max(1) {
            let Some(id) = st.queue.pop_front() else {
                break;
            };
            let size = st.settings.batch_size.clamp(1, MAX_BATCH);
            if size > 1
                && let Some(group) = batch_group(st, id)
            {
                let mut ids = vec![id];
                let mut i = 0;
                while ids.len() < size && i < st.queue.len().min(BATCH_LOOKAHEAD) {
                    let candidate = st.queue[i];
                    if batch_group(st, candidate) == Some(group) {
                        st.queue.remove(i);
                        ids.push(candidate);
                    } else {
                        i += 1;
                    }
                }
                let jobs: Vec<Job> = ids
                    .into_iter()
                    .filter_map(|id| prepare_job(st, id))
                    .collect();
                self.spawn_jobs(st, jobs);
                continue;
            }
            if let Some(job) = prepare_job(st, id) {
                self.spawn_jobs(st, vec![job]);
            }
        }
    }

    /// Run prepared jobs: one alone, several as one batch request.
    fn spawn_jobs(self: &Arc<Self>, st: &mut State, mut jobs: Vec<Job>) {
        let members: Vec<(SegmentId, u64, ContentHash, u64)> = jobs
            .iter()
            .map(|j| (j.id, j.generation, j.hash, j.key.glossary))
            .collect();
        let (handle, batch) = match jobs.len() {
            0 => return,
            1 => match jobs.pop() {
                Some(job) => (self.runtime.spawn(run_job(Arc::clone(self), job)), None),
                None => return,
            },
            n => (
                self.runtime.spawn(run_batch(Arc::clone(self), jobs)),
                Some(Arc::new(BatchLink {
                    live: AtomicUsize::new(n),
                })),
            ),
        };
        for (id, generation, hash, glossary_fp) in members {
            st.running.insert(
                id,
                Running {
                    generation,
                    hash,
                    glossary_fp,
                    abort: handle.abort_handle(),
                    batch: batch.clone(),
                },
            );
        }
    }

    /// Show streamed output (`None` clears it), unless the job has been
    /// superseded.
    fn publish_partial(&self, id: SegmentId, generation: u64, partial: Option<String>) {
        let mut st = self.lock();
        let current = st
            .meta
            .get(&id)
            .is_some_and(|m| m.generation == generation && m.status == SegmentStatus::Translating);
        if !current {
            return;
        }
        st.update(id, |m| m.partial = partial);
        let states = st.take_touched();
        drop(st);
        self.sink.segment_states(&states);
    }

    /// Count one finished provider request as soon as it returns, so its
    /// usage is kept even if the job is cancelled during a retry.
    fn count_request(&self, usage: TokenUsage) {
        let mut st = self.lock();
        st.usage.requests += 1;
        st.usage.input_tokens += usage.input_tokens;
        st.usage.output_tokens += usage.output_tokens;
        let usage = st.usage;
        drop(st);
        self.sink.usage(&usage);
    }

    fn finish_job(self: &Arc<Self>, job: Job, result: Result<String, TranslateError>) {
        let mut st = self.lock();
        self.apply_result(&mut st, job, result);
        self.pump(&mut st);
        self.emit_after(st);
    }

    /// Settle a batch: usable answers are applied like single results,
    /// segments without one go back to the queue to be sent on their own.
    fn finish_batch(self: &Arc<Self>, jobs: Vec<Job>, outcomes: Vec<Outcome>) {
        let mut st = self.lock();
        let mut alone = Vec::new();
        for (job, outcome) in jobs.into_iter().zip(outcomes) {
            match outcome {
                Outcome::Done(text) => self.apply_result(&mut st, job, Ok(text)),
                Outcome::Failed(e) => self.apply_result(&mut st, job, Err(e)),
                Outcome::Alone => {
                    if st
                        .running
                        .get(&job.id)
                        .is_some_and(|r| r.generation == job.generation)
                    {
                        st.running.remove(&job.id);
                    }
                    if is_current(&st, &job) {
                        st.update(job.id, |m| {
                            m.single = true;
                            m.partial = None;
                        });
                        alone.push(job.id);
                    }
                }
            }
        }
        if !alone.is_empty() {
            st.notices.push(format!(
                "{} paragraph(s) of a batch came back unusable; translating them one by one",
                alone.len()
            ));
            st.enqueue_all(&alone, Priority::Urgent);
        }
        self.pump(&mut st);
        self.emit_after(st);
    }

    /// Record one segment's result: cache it, and show it if the segment
    /// still has the text it was made for.
    fn apply_result(&self, st: &mut State, job: Job, result: Result<String, TranslateError>) {
        if st
            .running
            .get(&job.id)
            .is_some_and(|r| r.generation == job.generation)
        {
            st.running.remove(&job.id);
        }
        let current = is_current(st, &job);

        match result {
            Ok(text) => {
                let text = text.trim().to_owned();
                // A finished translation is valid for its source even if the
                // segment has moved on (e.g. the user will undo).
                if let Err(e) = self.cache.put(&job.key, &text) {
                    st.notices.push(e.to_string());
                }
                if current && job.fill {
                    st.fill(job.id, &text);
                } else if current {
                    let placeholder = job.translator.is_placeholder();
                    let glossary_fp = job.key.glossary;
                    st.update(job.id, |m| {
                        m.exact = false;
                        m.placeholder = placeholder;
                        m.glossary_fp = glossary_fp;
                        m.translation = Some(text);
                        m.translated_hash = Some(job.hash);
                        m.translated_source = Some(job.source);
                        m.status = SegmentStatus::Translated;
                        m.partial = None;
                        m.error = None;
                        m.forced = false;
                        m.single = false;
                    });
                }
            }
            Err(e) => {
                if current && job.fill {
                    // The paragraph keeps its text, and its translation the
                    // original; "continue" tries again.
                    let hash = job.hash;
                    st.update(job.id, |m| {
                        if m.translated_hash == Some(hash) {
                            m.status = SegmentStatus::Translated;
                        } else {
                            m.status = SegmentStatus::Stale;
                        }
                        m.partial = None;
                        m.forced = false;
                        m.fill = None;
                    });
                    st.notices.push(format!(
                        "a paragraph still in the other language could not be translated: {e}"
                    ));
                } else if current {
                    st.update(job.id, |m| {
                        m.status = SegmentStatus::Error;
                        m.error = Some(SegError {
                            hash: job.hash,
                            message: e.to_string(),
                            retryable: e.is_retryable(),
                        });
                        m.partial = None;
                        m.forced = false;
                    });
                }
            }
        }
    }

    /// Deliver the states touched by a finished job and the usage.
    fn emit_after(&self, mut st: MutexGuard<'_, State>) {
        let states = st.take_touched();
        let usage = st.usage;
        self.release(st);
        if !states.is_empty() {
            self.sink.segment_states(&states);
        }
        self.sink.usage(&usage);
    }
}

/// The segment still exists with the text the job was made for, and no
/// newer job replaced it.
fn is_current(st: &State, job: &Job) -> bool {
    st.meta
        .get(&job.id)
        .is_some_and(|m| m.generation == job.generation)
        && st.doc.get(job.id).is_some_and(|s| s.hash == job.hash)
}

/// What a batch request gave one of its segments.
enum Outcome {
    Done(String),
    Failed(TranslateError),
    /// No usable answer: send the segment on its own.
    Alone,
}

/// Previous source and translation to revise, if the segment has a real
/// translation of similar older text.
fn revise_basis(
    st: &State,
    id: SegmentId,
    source: &str,
    hash: ContentHash,
) -> Option<(String, String)> {
    let meta = st.meta.get(&id)?;
    // Revise mode needs a real previous translation of similar text. Mock
    // output is only a stand-in, and an unchanged source with a current
    // translation means an explicit retranslate: start fresh.
    match (&meta.translation, &meta.translated_source) {
        (Some(old_translation), Some(old_source))
            if meta.translated_hash != Some(hash)
                && !meta.placeholder
                && !old_translation.is_empty()
                && similarity(old_source, source) > st.settings.revise_threshold =>
        {
            Some((old_source.clone(), old_translation.clone()))
        }
        _ => None,
    }
}

/// Whether a segment may share a request with others, and with which:
/// fresh translations go together, and so do paragraphs translated the
/// other way round (`Some(true)`), since a request has one direction. Edits
/// keep their own request so revise mode stays minimal.
fn batch_group(st: &State, id: SegmentId) -> Option<bool> {
    let seg = st.doc.get(id)?;
    let meta = st.meta.get(&id)?;
    let source = seg.segment.content(st.doc.text());
    if !seg.kind().is_translatable() || meta.single || source.trim().is_empty() {
        return None;
    }
    if meta.fill == Some(seg.hash) {
        return Some(true);
    }
    revise_basis(st, id, source, seg.hash)
        .is_none()
        .then_some(false)
}

/// Build the request from the *current* document state and mark the segment
/// as translating. Returns `None` if the segment vanished or needs no request.
fn prepare_job(st: &mut State, id: SegmentId) -> Option<Job> {
    let idx = st.doc.index_of(id)?;
    let segments = st.doc.segments();
    let seg = &segments[idx];
    if !seg.kind().is_translatable() {
        return None;
    }
    let text = st.doc.text();
    let source = seg.segment.content(text).to_owned();
    let neighbor = |range: &mut dyn Iterator<Item = usize>| {
        range
            .map(|i| &segments[i])
            .find(|s| s.kind().is_translatable())
            .map(|s| s.segment.content(text).to_owned())
    };
    let context_before = neighbor(&mut (0..idx).rev());
    let context_after = neighbor(&mut (idx + 1..segments.len()));
    let hash = seg.hash;

    let fill = st.meta.get(&id)?.fill == Some(hash);
    // A paragraph still in the language of the translations is translated
    // the other way round, afresh.
    let direction = if fill {
        st.direction.flipped()
    } else {
        st.direction
    };
    let previous = if fill {
        None
    } else {
        revise_basis(st, id, &source, hash)
    };
    // Old and new text share one numbering, so unchanged math keeps its
    // placeholder in all three texts.
    let mut protector = Protector::new(st.doc.mode());
    let masked = protector.mask(&source);
    let revision = previous.map(|(old_source, old_translation)| Revision {
        old_source: protector.mask_context(&old_source),
        old_translation: protector.mask_known(&old_translation),
    });
    let glossary = st.glossary.relevant(&masked, direction);
    let key = st.cache_key_toward(direction, hash, glossary::fingerprint(&glossary));
    let request = TranslationRequest {
        direction,
        source: masked,
        context_before,
        context_after,
        glossary,
        doc_note: st.settings.doc_note.clone(),
        revision,
    };

    let mut generation = 0;
    st.update(id, |m| {
        m.generation += 1;
        generation = m.generation;
        m.status = SegmentStatus::Translating;
        m.partial = None;
    });
    Some(Job {
        id,
        generation,
        hash,
        key,
        source,
        protector,
        request,
        translator: Arc::clone(&st.translator),
        retry: RetryPolicy {
            max_retries: st
                .translator
                .max_retries()
                .unwrap_or(st.settings.max_retries),
            base: st.settings.backoff_base,
            max: st.settings.backoff_max,
        },
        stream_throttle: st.settings.stream_throttle,
        fill,
    })
}

struct StreamBuffer {
    text: String,
    last_emit: Option<Instant>,
}

async fn run_job(inner: Arc<Inner>, job: Job) {
    let buffer = Mutex::new(StreamBuffer {
        text: String::new(),
        last_emit: None,
    });
    let on_partial = |partial: &str| {
        if job.fill {
            return;
        }
        let due = {
            let mut b = buffer.lock().unwrap_or_else(PoisonError::into_inner);
            partial.clone_into(&mut b.text);
            let now = Instant::now();
            let due = b
                .last_emit
                .is_none_or(|t| now.duration_since(t) >= job.stream_throttle);
            if due {
                b.last_emit = Some(now);
            }
            due.then(|| b.text.clone())
        };
        if let Some(partial) = due {
            let shown = job.protector.restore_partial(&partial);
            inner.publish_partial(job.id, job.generation, Some(shown));
        }
    };
    let discard_partial = || {
        buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .text
            .clear();
        inner.publish_partial(job.id, job.generation, None);
    };

    let mut retries: u32 = 0;
    let mut placeholder_retried = false;
    let result = loop {
        let attempt = job.translator.translate(&job.request, &on_partial).await;
        let usage = attempt.as_ref().map(|out| out.usage).unwrap_or_default();
        inner.count_request(usage);
        match attempt {
            Ok(out) => {
                match job.protector.restore(&out.text) {
                    Ok(text) => break Ok(text),
                    // Models occasionally drop or mangle a placeholder; one
                    // immediate retry usually fixes it.
                    Err(e) if !placeholder_retried => {
                        placeholder_retried = true;
                        inner.sink.notice(&format!("{e}; retrying"));
                        discard_partial();
                    }
                    Err(e) => break Err(TranslateError::InvalidResponse(e.to_string())),
                }
            }
            Err(e) if e.is_retryable() && retries < job.retry.max_retries => {
                let delay = backoff_delay(retries, e.retry_after(), job.retry);
                inner
                    .sink
                    .notice(&format!("{e}; retrying in {:.1}s", delay.as_secs_f32()));
                // Don't leave the failed attempt's half output on screen.
                discard_partial();
                tokio::time::sleep(delay).await;
                retries += 1;
            }
            Err(e) => break Err(e),
        }
    };
    inner.finish_job(job, result);
}

/// One request for several segments. Transient errors retry the whole
/// batch with backoff. Each segment's answer is restored with its own
/// placeholders; a segment without a usable answer is sent again alone.
async fn run_batch(inner: Arc<Inner>, jobs: Vec<Job>) {
    let Some(first) = jobs.first() else {
        return;
    };
    let translator = Arc::clone(&first.translator);
    let retry = first.retry;
    let throttle = first.stream_throttle;
    let requests: Vec<TranslationRequest> = jobs.iter().map(|j| j.request.clone()).collect();
    let last_emit: Vec<Mutex<Option<Instant>>> = jobs.iter().map(|_| Mutex::new(None)).collect();
    let on_partial = |i: usize, partial: &str| {
        let (Some(job), Some(last)) = (jobs.get(i), last_emit.get(i)) else {
            return;
        };
        if job.fill {
            return;
        }
        let due = {
            let mut last = last.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let due = last.is_none_or(|t| now.duration_since(t) >= throttle);
            if due {
                *last = Some(now);
            }
            due
        };
        if due {
            let shown = job.protector.restore_partial(partial);
            inner.publish_partial(job.id, job.generation, Some(shown));
        }
    };
    let discard_partials = || {
        for job in &jobs {
            inner.publish_partial(job.id, job.generation, None);
        }
    };

    let mut retries: u32 = 0;
    let result = loop {
        let attempt = translator.translate_batch(&requests, &on_partial).await;
        let usage = attempt.as_ref().map(|out| out.usage).unwrap_or_default();
        inner.count_request(usage);
        match attempt {
            Ok(out) => break Ok(out.texts),
            Err(e) if e.is_retryable() && retries < retry.max_retries => {
                let delay = backoff_delay(retries, e.retry_after(), retry);
                inner
                    .sink
                    .notice(&format!("{e}; retrying in {:.1}s", delay.as_secs_f32()));
                discard_partials();
                tokio::time::sleep(delay).await;
                retries += 1;
            }
            Err(e) => break Err(e),
        }
    };
    let outcomes = match result {
        Ok(texts) => jobs
            .iter()
            .enumerate()
            .map(|(i, job)| match texts.get(i).cloned().flatten() {
                Some(text) => match job.protector.restore(&text) {
                    Ok(text) => Outcome::Done(text),
                    Err(_) => Outcome::Alone,
                },
                None => Outcome::Alone,
            })
            .collect(),
        Err(e) => jobs.iter().map(|_| Outcome::Failed(e.clone())).collect(),
    };
    inner.finish_batch(jobs, outcomes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy {
            max_retries: 5,
            base: Duration::from_secs(1),
            max: Duration::from_secs(10),
        };
        let d0 = backoff_delay(0, None, p);
        let d3 = backoff_delay(3, None, p);
        assert!(d0 >= Duration::from_secs(1) && d0 <= Duration::from_millis(1250));
        assert!(d3 >= Duration::from_secs(8) && d3 <= Duration::from_secs(10));
        assert_eq!(backoff_delay(30, None, p), Duration::from_secs(10));
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(3)), p),
            Duration::from_secs(3)
        );
    }
}
