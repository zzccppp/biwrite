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
