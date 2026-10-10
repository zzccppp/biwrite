//! Key changes while translating: added keys reach the provider in use
//! without cancelling its requests in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use biwrite_core::Mode;
use biwrite_engine::SegmentStatus;
use biwrite_providers::{Effort, ProviderConfig, ProviderKind, WireApi};
use tokio::net::TcpListener;

use crate::pair_tests::app_state;
use crate::secrets;
use crate::settings::ProviderEntry;

const KEY_A: &str = "sk-keytest-aaaaaaaaaaaaaaaaaaaa";
const KEY_B: &str = "sk-keytest-bbbbbbbbbbbbbbbbbbbb";

/// A server that takes requests and never answers. Counts connections.
async fn silent_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            seen.fetch_add(1, Ordering::SeqCst);
            held.push(socket);
        }
    });
    (base, count)
}

async fn until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..400 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn added_keys_leave_requests_in_flight_alone() {
    let dir = std::env::temp_dir().join(format!("biwrite-keys-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = app_state(&dir);
    let (base, connections) = silent_server().await;
    secrets::set_keys(state.secrets.as_ref(), "p-1", &[KEY_A.to_owned()], None).unwrap();
    {
        let mut s = state.settings();
        s.providers.push(ProviderEntry {
            config: ProviderConfig {
                id: "p-1".into(),
                name: "Relay".into(),
                kind: ProviderKind::OpenaiCompatible,
                base_url: base,
                model: "m".into(),
                temperature: 0.0,
                effort: Effort::Low,
                wire_api: WireApi::Chat,
                service_tier: None,
                key_concurrency: Some(1),
                max_retries: None,
            }
            .validated()
            .unwrap(),
            has_key: true,
            key_count: 1,
            key_names: Default::default(),
            key_parts: 0,
        });
        s.active_provider = "p-1".into();
        s.match_pool = true;
    }
    state.apply_active_provider().unwrap();
    state.engine.load("Hello world.".into(), Mode::Plain);
    until("the request", || connections.load(Ordering::SeqCst) == 1).await;

    // Add a key, as `add_api_keys` does.
    let keys = [KEY_A, KEY_B].map(String::from);
    let parts = secrets::set_keys(state.secrets.as_ref(), "p-1", &keys, None).unwrap();
    assert!(state.settings().set_key_count("p-1", 2, parts));
    state.reload_keys("p-1").unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "the request in flight was cancelled and sent again"
    );
    let snapshot = state.engine.snapshot();
    assert_eq!(snapshot.states[0].status, SegmentStatus::Translating);
    assert_eq!(
        state.engine.settings().concurrency,
        2,
        "keys × per-key limit"
    );

    // The next paragraph goes out with the added key.
    state
        .engine
        .update("Hello world.\n\nA second paragraph.".into());
    until("the second request", || {
        connections.load(Ordering::SeqCst) == 2
    })
    .await;
    let status = state.key_status("p-1").unwrap();
    assert_eq!(status.len(), 2);
    assert_eq!(
        status.iter().map(|s| s.in_flight).collect::<Vec<_>>(),
        [1, 1]
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_provider_built_for_another_host_is_not_reused() {
    let dir = std::env::temp_dir().join(format!("biwrite-keys-host-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = app_state(&dir);
    // Closed local ports: requests fail at once, nothing leaves the machine.
    let (host_a, host_b) = ("http://127.0.0.1:9/v1", "http://localhost:9/v1");
    let config = |base: &str| {
        ProviderConfig {
            id: "p-1".into(),
            name: "Relay".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: base.into(),
            model: "m".into(),
            temperature: 0.0,
            effort: Effort::Low,
            wire_api: WireApi::Chat,
            service_tier: None,
            key_concurrency: None,
            max_retries: Some(0),
        }
        .validated()
        .unwrap()
    };
    secrets::set_keys(state.secrets.as_ref(), "p-1", &[KEY_A.to_owned()], None).unwrap();
    {
        let mut s = state.settings();
        s.providers.push(ProviderEntry {
            config: config(host_a),
            has_key: true,
            key_count: 1,
            key_names: Default::default(),
            key_parts: 0,
        });
        s.assistant_provider = "p-1".into();
    }
    let old = state.assistant_model().unwrap();
    assert_eq!(old.config().base_url, host_a);
    // The provider moves to another host, and the cached assistant
    // provider was not dropped (it was being built meanwhile).
    state.settings().provider_mut("p-1").unwrap().config = config(host_b);
    *state.assistant_http.lock().unwrap() = Some(("p-1".into(), old.clone()));
    let fresh = state.assistant_model().unwrap();
    assert_eq!(fresh.config().base_url, host_b);
    // Keys added for the new host are not handed to the old provider.
    *state.assistant_http.lock().unwrap() = Some(("p-1".into(), old.clone()));
    let keys = [KEY_A, KEY_B].map(String::from);
    let parts = secrets::set_keys(state.secrets.as_ref(), "p-1", &keys, None).unwrap();
    assert!(state.settings().set_key_count("p-1", 2, parts));
    state.reload_keys("p-1").unwrap();
    let request = biwrite_engine::TranslationRequest {
        source: "x".into(),
        ..Default::default()
    };
    let attempt = biwrite_engine::Translator::translate(old.as_ref(), &request, &|_| {});
    assert!(attempt.await.is_err(), "nothing listens there");
    assert_eq!(
        old.key_status().unwrap().len(),
        1,
        "the old provider got the new pool"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn an_assistant_provider_built_across_a_key_change_is_not_kept() {
    let dir = std::env::temp_dir().join(format!("biwrite-keys-epoch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = app_state(&dir);
    let config = ProviderConfig {
        id: "p-1".into(),
        name: "Relay".into(),
        kind: ProviderKind::OpenaiCompatible,
        base_url: "http://127.0.0.1:9/v1".into(),
        model: "m".into(),
        temperature: 0.0,
        effort: Effort::Low,
        wire_api: WireApi::Chat,
        service_tier: None,
        key_concurrency: None,
        max_retries: Some(0),
    }
    .validated()
    .unwrap();
    {
        let mut s = state.settings();
        s.providers.push(ProviderEntry {
            config: config.clone(),
            has_key: true,
            key_count: 1,
            key_names: Default::default(),
            key_parts: 0,
        });
        s.assistant_provider = "p-1".into();
    }
    let built = |state: &crate::state::AppState| {
        biwrite_providers::build_http(
            &config,
            std::sync::Arc::new(|| Ok(vec![KEY_A.to_owned()])),
            std::sync::Arc::new(biwrite_providers::DefaultPrompts),
            state.request_log.clone(),
        )
        .unwrap()
    };
    // Keys added while it was built (settings read before): not kept.
    let epoch = state.assistant_epoch.load(Ordering::SeqCst);
    let http = built(&state);
    assert!(state.settings().set_key_count("p-1", 2, 1));
    state.reload_keys("p-1").unwrap();
    assert!(!state.keep_assistant(epoch, "p-1", &http));
    assert!(state.assistant_http.lock().unwrap().is_none());
    // The provider changed: not kept either.
    let epoch = state.assistant_epoch.load(Ordering::SeqCst);
    state.invalidate_assistant();
    assert!(!state.keep_assistant(epoch, "p-1", &http));
    // Nothing changed: kept.
    let epoch = state.assistant_epoch.load(Ordering::SeqCst);
    assert!(state.keep_assistant(epoch, "p-1", &http));
    std::fs::remove_dir_all(&dir).ok();
}
