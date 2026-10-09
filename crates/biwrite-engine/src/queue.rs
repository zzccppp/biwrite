//! Job dispatch: concurrency limit, per-segment generations, streaming,
//! exponential backoff, and applying results.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use biwrite_core::{ContentHash, SegmentId, similarity};
use tokio::time::Instant;

use crate::cache::CacheKey;
use crate::engine::Inner;
use crate::events::SegmentStatus;
use crate::random::random_between;
use crate::state::{Running, SegError, State};
use crate::translator::{
    Revision, TranslateError, TranslationOutput, TranslationRequest, Translator,
};

/// A dispatched translation request.
pub(crate) struct Job {
    id: SegmentId,
    generation: u64,
    hash: ContentHash,
    key: CacheKey,
    request: TranslationRequest,
    translator: Arc<dyn Translator>,
    retry: RetryPolicy,
    stream_throttle: Duration,
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
    /// Start queued jobs until the concurrency limit is reached.
    pub(crate) fn pump(self: &Arc<Self>, st: &mut State) {
        while st.running.len() < st.settings.concurrency.max(1) {
            let Some(id) = st.queue.pop_front() else {
                break;
            };
            let Some(job) = prepare_job(st, id) else {
                continue;
            };
            let (generation, hash) = (job.generation, job.hash);
            let handle = self.runtime.spawn(run_job(Arc::clone(self), job));
            st.running.insert(
                id,
                Running {
                    generation,
                    hash,
                    abort: handle.abort_handle(),
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

    fn finish_job(
        self: &Arc<Self>,
        job: Job,
        result: Result<TranslationOutput, TranslateError>,
        attempts: u64,
    ) {
        let mut st = self.lock();
        if st
            .running
            .get(&job.id)
            .is_some_and(|r| r.generation == job.generation)
        {
            st.running.remove(&job.id);
        }
        st.usage.requests += attempts;
        if let Ok(out) = &result {
            st.usage.input_tokens += out.usage.input_tokens;
            st.usage.output_tokens += out.usage.output_tokens;
        }
        let current = st
            .meta
            .get(&job.id)
            .is_some_and(|m| m.generation == job.generation)
            && st.doc.get(job.id).is_some_and(|s| s.hash == job.hash);

        match result {
            Ok(mut out) => {
                out.text = out.text.trim().to_owned();
                // A finished translation is valid for its source even if the
                // segment has moved on (e.g. the user will undo).
                if let Err(e) = self.cache.put(&job.key, &out.text) {
                    st.notices.push(e.to_string());
                }
                if current {
                    let placeholder = job.translator.is_placeholder();
                    st.update(job.id, |m| {
                        m.exact = false;
                        m.placeholder = placeholder;
                        m.translation = Some(out.text);
                        m.translated_hash = Some(job.hash);
                        m.translated_source = Some(job.request.source);
                        m.status = SegmentStatus::Translated;
                        m.partial = None;
                        m.error = None;
                        m.forced = false;
                    });
                }
            }
            Err(e) => {
                if current {
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

        self.pump(&mut st);
        let states = st.take_touched();
        let usage = st.usage;
        self.release(st);
        if !states.is_empty() {
            self.sink.segment_states(&states);
        }
        self.sink.usage(&usage);
    }
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

    let meta = st.meta.get(&id)?;
    let revision = match (&meta.translation, &meta.translated_source) {
        (Some(old_translation), Some(old_source))
            if meta.translated_hash != Some(hash)
                && !old_translation.is_empty()
                && similarity(old_source, &source) > st.settings.revise_threshold =>
        {
            Some(Revision {
                old_source: old_source.clone(),
                old_translation: old_translation.clone(),
            })
        }
        _ => None,
    };
    let request = TranslationRequest {
        direction: st.direction,
        source,
        context_before,
        context_after,
        glossary: Vec::new(),
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
        key: st.cache_key(hash),
        request,
        translator: Arc::clone(&st.translator),
        retry: RetryPolicy {
            max_retries: st.settings.max_retries,
            base: st.settings.backoff_base,
            max: st.settings.backoff_max,
        },
        stream_throttle: st.settings.stream_throttle,
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
            inner.publish_partial(job.id, job.generation, Some(partial));
        }
    };

    let mut attempt: u32 = 0;
    let result = loop {
        match job.translator.translate(&job.request, &on_partial).await {
            Ok(out) => break Ok(out),
            Err(e) if e.is_retryable() && attempt < job.retry.max_retries => {
                let delay = backoff_delay(attempt, e.retry_after(), job.retry);
                inner
                    .sink
                    .notice(&format!("{e}; retrying in {:.1}s", delay.as_secs_f32()));
                buffer
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .text
                    .clear();
                // Don't leave the failed attempt's half output on screen.
                inner.publish_partial(job.id, job.generation, None);
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
            Err(e) => break Err(e),
        }
    };
    inner.finish_job(job, result, u64::from(attempt) + 1);
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
