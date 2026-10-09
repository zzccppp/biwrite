//! Live checks against real endpoints. Ignored by default because they need
//! a key and network access and they spend quota. Run them with, e.g.
//!
//! ```sh
//! BIWRITE_LIVE_ANYROUTER_KEY=sk-... cargo test -p biwrite-providers --test live -- --ignored --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_engine::{TranslationRequest, Translator};
use biwrite_providers::config::ANYROUTER;
use biwrite_providers::{
    DefaultPrompts, ProviderConfig, RecordState, RequestObserver, RequestRecord, build_http,
};

#[derive(Default)]
struct Records(Mutex<Vec<RequestRecord>>);

impl RequestObserver for Records {
    fn record(&self, record: &RequestRecord) {
        self.0.lock().unwrap().push(record.clone());
    }
}

#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEY and network access"]
async fn anyrouter_translates_with_the_preset() {
    let Ok(key) = std::env::var("BIWRITE_LIVE_ANYROUTER_KEY") else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEY is not set; skipping");
        return;
    };
    let config = ProviderConfig {
        id: "live-anyrouter".into(),
        name: ANYROUTER.name.into(),
        kind: ANYROUTER.kind,
        base_url: ANYROUTER.base_url.into(),
        model: ANYROUTER.model.into(),
        temperature: 0.0,
        effort: ANYROUTER.effort,
        wire_api: ANYROUTER.wire_api,
        service_tier: ANYROUTER.service_tier,
        key_concurrency: ANYROUTER.key_concurrency,
        max_retries: ANYROUTER.max_retries,
    }
    .validated()
    .unwrap();
    let records = Arc::new(Records::default());
    let provider = build_http(
        &config,
        Arc::new(move || Ok(vec![key.clone()])),
        Arc::new(DefaultPrompts),
        records.clone(),
    )
    .unwrap();
    let request = TranslationRequest {
        source: "Placeholder protection keeps citations intact.".into(),
        ..Default::default()
    };
    // The relay's upstream rate-limits often; retry like the engine does.
    let mut last = None;
    for attempt in 0..6u32 {
        match provider.translate(&request, &|_| {}).await {
            Ok(out) => {
                println!("translation: {}", out.text);
                assert!(out.text.chars().any(biwrite_core::lang::is_cjk));
                last = None;
                break;
            }
            Err(e) if e.is_retryable() => {
                println!("attempt {attempt}: {e}");
                last = Some(e);
                tokio::time::sleep(Duration::from_secs(2 + 2 * u64::from(attempt))).await;
            }
            Err(e) => {
                last = Some(e);
                break;
            }
        }
    }
    for r in records
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.state != RecordState::InFlight)
    {
        println!(
            "record {}: {:?} http={:?} req=({:?},{:?},{:?}) resp=({:?},{:?},{:?}) usage={:?} {}ms",
            r.id,
            r.state,
            r.http_status,
            r.request.model,
            r.request.effort,
            r.request.service_tier,
            r.response.model,
            r.response.effort,
            r.response.service_tier,
            r.usage,
            r.duration_ms.unwrap_or(0)
        );
        if let Some(err) = &r.error {
            println!("  error: {err}");
        }
    }
    assert!(last.is_none(), "no translation: {last:?}");
}

/// Keys from `BIWRITE_LIVE_ANYROUTER_KEYS`: one per line; lines that do not
/// look like keys (account names) are skipped.
fn pool_keys() -> Option<Vec<String>> {
    let text = std::env::var("BIWRITE_LIVE_ANYROUTER_KEYS").ok()?;
    let keys: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("sk-"))
        .map(str::to_owned)
        .collect();
    (!keys.is_empty()).then_some(keys)
}

fn anyrouter_config(id: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.into(),
        name: ANYROUTER.name.into(),
        kind: ANYROUTER.kind,
        base_url: ANYROUTER.base_url.into(),
        model: ANYROUTER.model.into(),
        temperature: 0.0,
        effort: ANYROUTER.effort,
        wire_api: ANYROUTER.wire_api,
        service_tier: ANYROUTER.service_tier,
        key_concurrency: ANYROUTER.key_concurrency,
        max_retries: ANYROUTER.max_retries,
    }
    .validated()
    .unwrap()
}

/// Translate like the engine does: retry transient errors with backoff.
async fn translate_with_retries(
    t: &dyn Translator,
    source: &str,
    retries: u32,
) -> Result<String, biwrite_engine::TranslateError> {
    let request = TranslationRequest {
        source: source.into(),
        ..Default::default()
    };
    let mut attempt = 0;
    loop {
        match t.translate(&request, &|_| {}).await {
            Ok(out) => return Ok(out.text),
            Err(e) if e.is_retryable() && attempt < retries => {
                let wait = e
                    .retry_after()
                    .unwrap_or(Duration::from_secs(1 << attempt.min(3)));
                tokio::time::sleep(wait.min(Duration::from_secs(10))).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEYS and network access"]
async fn anyrouter_each_key_answers() {
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    for (i, key) in keys.iter().enumerate() {
        let key = key.clone();
        let provider = build_http(
            &anyrouter_config(&format!("live-key-{i}")),
            Arc::new(move || Ok(vec![key.clone()])),
            Arc::new(DefaultPrompts),
            Arc::new(Records::default()),
        )
        .unwrap();
        let out = translate_with_retries(provider.as_ref(), "Keys rotate per request.", 6).await;
        println!("key {}: {:?}", i + 1, out);
        assert!(out.is_ok(), "key {} failed: {out:?}", i + 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEYS and network access"]
async fn anyrouter_pool_carries_parallel_paragraphs() {
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    let n_keys = keys.len();
    let records = Arc::new(Records::default());
    let provider = build_http(
        &anyrouter_config("live-pool"),
        Arc::new(move || Ok(keys.clone())),
        Arc::new(DefaultPrompts),
        records.clone(),
    )
    .unwrap();
    let paragraphs: Vec<String> = (1..=24)
        .map(|i| format!("Paragraph {i}: the pool spreads requests over every key."))
        .collect();
    let started = std::time::Instant::now();
    let handles: Vec<_> = paragraphs
        .into_iter()
        .map(|p| {
            let provider = provider.clone();
            tokio::spawn(async move { translate_with_retries(provider.as_ref(), &p, 10).await })
        })
        .collect();
    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }
    let elapsed = started.elapsed();
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let finished: Vec<RequestRecord> = {
        let all = records.0.lock().unwrap();
        all.iter()
            .filter(|r| r.state != RecordState::InFlight)
            .cloned()
            .collect()
    };
    let mut per_key = vec![(0usize, 0usize); n_keys];
    for r in &finished {
        if let Some(k) = &r.key {
            let slot = &mut per_key[k.number - 1];
            if r.state == RecordState::Ok {
                slot.0 += 1;
            } else {
                slot.1 += 1;
            }
        }
    }
    println!(
        "{ok}/{} paragraphs translated in {:.1}s with {} requests",
        results.len(),
        elapsed.as_secs_f32(),
        finished.len()
    );
    for (i, (good, bad)) in per_key.iter().enumerate() {
        println!("key {}: {good} ok, {bad} failed attempts", i + 1);
    }
    for r in results.iter().filter_map(|r| r.as_ref().err()) {
        println!("failed: {r}");
    }
    assert_eq!(ok, results.len());
}

#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEYS and network access"]
async fn anyrouter_translates_three_paragraphs_in_one_request() {
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    let records = Arc::new(Records::default());
    let provider = build_http(
        &anyrouter_config("live-batch"),
        Arc::new(move || Ok(keys.clone())),
        Arc::new(DefaultPrompts),
        records.clone(),
    )
    .unwrap();
    let reqs: Vec<TranslationRequest> = [
        "We evaluate on ⟦0⟧ datasets and report the mean over ⟦1⟧ runs.",
        "Results",
        "The method keeps ⟦0⟧ fixed and tunes only the threshold.",
    ]
    .iter()
    .map(|s| TranslationRequest {
        source: (*s).into(),
        ..Default::default()
    })
    .collect();
    let mut last = None;
    for attempt in 0..8u32 {
        match provider.translate_batch(&reqs, &|_, _| {}).await {
            Ok(out) => {
                for (i, t) in out.texts.iter().enumerate() {
                    println!("{}: {:?}", i + 1, t);
                }
                assert!(out.texts.iter().all(Option::is_some), "{:?}", out.texts);
                assert!(out.texts[0].as_deref().unwrap().contains("⟦1⟧"));
                last = None;
                break;
            }
            Err(e) if e.is_retryable() => {
                println!("attempt {attempt}: {e}");
                last = Some(e);
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(last.is_none(), "{last:?}");
}
