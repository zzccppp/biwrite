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
            key_through: Default::default(),
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
            key_through: Default::default(),
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
            key_through: Default::default(),
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

const KEY_C: &str = "sk-keytest-cccccccccccccccccccc";
const KEY_D: &str = "sk-keytest-dddddddddddddddddddd";

/// A pool of `keys` for provider `p-1`, named and noted as given.
fn pool_state(dir: &std::path::Path, keys: &[&str]) -> tauri::App<tauri::test::MockRuntime> {
    use tauri::Manager;
    let app = tauri::test::mock_app();
    app.manage(app_state(dir));
    let state = app.state::<crate::state::AppState>();
    let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
    let parts = secrets::set_keys(state.secrets.as_ref(), "p-1", &keys, None).unwrap();
    let mut s = state.settings();
    s.providers.push(ProviderEntry {
        config: ProviderConfig {
            id: "p-1".into(),
            name: "Relay".into(),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "http://127.0.0.1:9/v1".into(),
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
        key_count: keys.len(),
        key_names: Default::default(),
        key_through: Default::default(),
        key_parts: parts,
    });
    drop(s);
    app
}

/// A key that never got through gives way to a new key of the same
/// account: in its place, with its name, and without the old key's record.
#[tokio::test]
async fn a_key_is_replaced_in_its_place_with_its_name() {
    use biwrite_providers::{key_fingerprint, key_tail};
    use tauri::Manager;

    use crate::settings_commands::{list_api_keys, replace_api_key};
    let dir = std::env::temp_dir().join(format!("biwrite-replace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let app = pool_state(&dir, &[KEY_A, KEY_B]);
    let state = app.state::<crate::state::AppState>();
    {
        let mut s = state.settings();
        let entry = s.provider_mut("p-1").unwrap();
        entry.key_names.insert(key_fingerprint(KEY_A), "lab".into());
        entry
            .key_names
            .insert(key_fingerprint(KEY_B), "main".into());
        entry
            .key_through
            .insert(key_fingerprint(KEY_A), 1_700_000_000_000);
        entry
            .key_through
            .insert(key_fingerprint(KEY_B), 1_600_000_000_000);
    }
    replace_api_key(
        state.clone(),
        "p-1".into(),
        2,
        key_tail(KEY_B),
        KEY_C.into(),
    )
    .await
    .unwrap();
    let vouched = state.settings().provider("p-1").unwrap().vouched();
    assert_eq!(
        secrets::get_keys(state.secrets.as_ref(), "p-1", vouched).unwrap(),
        [KEY_A, KEY_C]
    );
    {
        let s = state.settings();
        let entry = s.provider("p-1").unwrap();
        assert_eq!(
            entry
                .key_names
                .get(&key_fingerprint(KEY_C))
                .map(String::as_str),
            Some("main")
        );
        assert!(!entry.key_names.contains_key(&key_fingerprint(KEY_B)));
        assert!(!entry.key_through.contains_key(&key_fingerprint(KEY_B)));
        assert!(entry.key_through.contains_key(&key_fingerprint(KEY_A)));
    }
    // The list shows which keys never got through.
    let list = list_api_keys(state.clone(), "p-1".into()).await.unwrap();
    assert_eq!(list[0].last_through, Some(1_700_000_000_000));
    assert_eq!(list[1].last_through, None);
    assert_eq!(list[1].name, "main");

    // Refused: a key the pool has, a key that is no longer there, two keys.
    let err =
        |r: crate::error::CommandResult<crate::settings::SettingsView>| r.unwrap_err().to_string();
    assert!(
        err(replace_api_key(
            state.clone(),
            "p-1".into(),
            2,
            key_tail(KEY_C),
            KEY_A.into()
        )
        .await)
        .contains("already")
    );
    assert!(
        err(replace_api_key(
            state.clone(),
            "p-1".into(),
            2,
            key_tail(KEY_B),
            KEY_D.into()
        )
        .await)
        .contains("changed")
    );
    assert!(
        err(replace_api_key(
            state.clone(),
            "p-1".into(),
            2,
            key_tail(KEY_C),
            format!("{KEY_D}\n{KEY_B}")
        )
        .await)
        .contains("one key")
    );
    // A name written with the new key is taken.
    replace_api_key(
        state.clone(),
        "p-1".into(),
        1,
        key_tail(KEY_A),
        format!("backup {KEY_D}"),
    )
    .await
    .unwrap();
    let list = list_api_keys(state.clone(), "p-1".into()).await.unwrap();
    assert_eq!(list[0].name, "backup");
    assert_eq!(list[0].tail, key_tail(KEY_D));
    std::fs::remove_dir_all(&dir).ok();
}

/// The keys kept alive are noted in the settings (fingerprints, never the
/// keys), and forgotten there once keeping alive is turned off.
#[tokio::test]
async fn keys_kept_alive_are_noted_for_a_restart() {
    use biwrite_providers::key_fingerprint;
    use tauri::Manager;
    let dir = std::env::temp_dir().join(format!("biwrite-kept-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let app = pool_state(&dir, &[KEY_A, KEY_B]);
    let state = app.state::<crate::state::AppState>();
    let entry = state.settings().provider("p-1").cloned().unwrap();
    let board = state.squeeze.board_for(&entry).unwrap();
    board.allow_crowding();
    board.note_turned_away();
    board.note_turned_away();
    board.answered(&key_fingerprint(KEY_A));
    state.squeeze.set_forever(true);
    // The keeper keeps the key through.
    assert!(
        state
            .squeeze
            .dropped_keys(std::slice::from_ref(&entry))
            .is_empty()
    );
    crate::squeeze_commands::record_through(&state);
    assert_eq!(
        state.settings().kept_keys.as_ref().unwrap()["p-1"],
        [key_fingerprint(KEY_A)]
    );
    let file = std::fs::read_to_string(&state.paths.settings).unwrap();
    assert!(file.contains("keptKeys"));
    assert!(!file.contains(KEY_A));
    // Keeping alive turned off: nothing to squeeze back in after a restart.
    state.squeeze.keep_alive_off();
    crate::squeeze_commands::record_through(&state);
    assert_eq!(
        state.settings().kept_keys,
        Some(Default::default()),
        "noted as none, not as unknown"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Keys answered by a relay that turns keys away are noted with the time,
/// once an hour at most, so the pool shows the ones that never got through.
#[tokio::test]
async fn keys_that_got_through_are_noted() {
    use biwrite_providers::key_fingerprint;
    use tauri::Manager;
    let dir = std::env::temp_dir().join(format!("biwrite-noted-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let app = pool_state(&dir, &[KEY_A, KEY_B]);
    let state = app.state::<crate::state::AppState>();
    let entry = state.settings().provider("p-1").cloned().unwrap();
    let board = state
        .squeeze
        .board_for(&entry)
        .expect("a pool of two keys has a board");
    board.answered(&key_fingerprint(KEY_A));
    crate::squeeze_commands::record_through(&state);
    let noted = state
        .settings()
        .provider("p-1")
        .unwrap()
        .key_through
        .clone();
    assert_eq!(noted.len(), 1);
    let first = noted[&key_fingerprint(KEY_A)];
    assert!(first > 1_700_000_000_000);
    // Answered again soon after: the note stays as it was.
    board.answered(&key_fingerprint(KEY_A));
    crate::squeeze_commands::record_through(&state);
    assert_eq!(
        state.settings().provider("p-1").unwrap().key_through[&key_fingerprint(KEY_A)],
        first
    );
    // The settings file holds it, never the key.
    let file = std::fs::read_to_string(&state.paths.settings).unwrap();
    assert!(file.contains(&key_fingerprint(KEY_A)));
    assert!(!file.contains(KEY_A));
    std::fs::remove_dir_all(&dir).ok();
}
