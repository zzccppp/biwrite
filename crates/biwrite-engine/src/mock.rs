//! Mock translator for development: returns the reversed source after a
//! random delay, streaming it in a few chunks. Placeholders (`⟦n⟧`) are kept
//! whole, so protected math and citations survive like with a real model.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use biwrite_core::protect::placeholder_at;

use crate::random::random_between;
use crate::translator::{
    BoxFuture, PartialFn, TokenUsage, TranslateError, TranslationOutput, TranslationRequest,
    Translator,
};

/// Number of streamed fragments per response.
const STREAM_CHUNKS: usize = 6;

pub struct MockTranslator {
    min_delay: Duration,
    max_delay: Duration,
    calls: AtomicU64,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl Default for MockTranslator {
    fn default() -> Self {
        Self::with_delay(Duration::from_millis(500), Duration::from_millis(2000))
    }
}

impl MockTranslator {
    /// A mock whose response time is uniform in `[min_delay, max_delay]`.
    pub fn with_delay(min_delay: Duration, max_delay: Duration) -> Self {
        Self {
            min_delay,
            max_delay: max_delay.max(min_delay),
            calls: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        }
    }

    /// Number of `translate` calls made so far.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Highest number of concurrent requests observed.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

/// Decrements the in-flight counter even if the future is dropped (cancelled).
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Translator for MockTranslator {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "reverse"
    }

    fn is_placeholder(&self) -> bool {
        true
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            let _guard = InFlight(&self.in_flight);

            let total = random_between(self.min_delay, self.max_delay);
            let reversed = reverse(&request.source);
            let chunks = split_chunks(&reversed, STREAM_CHUNKS);
            let step = total / u32::try_from(chunks.len().max(1)).unwrap_or(1);
            if chunks.is_empty() {
                tokio::time::sleep(total).await;
            }
            let mut shown = String::new();
            for chunk in &chunks {
                tokio::time::sleep(step).await;
                shown.push_str(chunk);
                on_partial(&shown);
            }
            Ok(TranslationOutput {
                usage: TokenUsage {
                    input_tokens: estimate_tokens(&request.source),
                    output_tokens: estimate_tokens(&reversed),
                },
                text: reversed,
            })
        })
    }
}

/// Reverse the characters, keeping each placeholder as one unit.
pub fn reverse(text: &str) -> String {
    let mut units = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = placeholder_at(rest).map_or(c.len_utf8(), |(len, _)| len);
        units.push(&rest[..len]);
        rest = &rest[len..];
    }
    units.reverse();
    units.concat()
}

/// Split into at most `n` pieces on char boundaries.
fn split_chunks(text: &str, n: usize) -> Vec<&str> {
    let chars = text.chars().count();
    if chars == 0 {
        return Vec::new();
    }
    let per = chars.div_ceil(n);
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    for (count, (idx, _)) in text.char_indices().enumerate() {
        if count > 0 && count % per == 0 {
            out.push(&text[start..idx]);
            start = idx;
        }
    }
    out.push(&text[start..]);
    out
}

/// Rough token estimate (~4 chars per token) for the mock's usage report.
fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count().div_ceil(4)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_reassemble() {
        let text = "数据集包含😀 emoji and text";
        assert_eq!(split_chunks(text, 6).concat(), text);
        assert!(split_chunks(text, 6).len() <= 6);
        assert!(split_chunks("", 6).is_empty());
        assert_eq!(split_chunks("ab", 6), vec!["a", "b"]);
    }

    #[test]
    fn placeholders_stay_whole() {
        assert_eq!(reverse("ab ⟦12⟧ c⟦0⟧"), "⟦0⟧c ⟦12⟧ ba");
        assert_eq!(reverse("⟦ x"), "x ⟦");
    }

    #[tokio::test(start_paused = true)]
    async fn reverses_and_streams() {
        let mock = MockTranslator::default();
        let req = TranslationRequest {
            source: "abc def".into(),
            ..Default::default()
        };
        let seen = std::sync::Mutex::new(String::new());
        let out = mock
            .translate(&req, &|d| *seen.lock().unwrap() = d.to_owned())
            .await
            .unwrap();
        assert_eq!(out.text, "fed cba");
        assert_eq!(*seen.lock().unwrap(), "fed cba");
        assert_eq!(mock.calls(), 1);
    }
}
