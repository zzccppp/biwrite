//! Getting through to a crowded relay, against a local relay that answers
//! some keys and turns the others away: a squeeze tries the keys until all
//! that can be used got through, stops when asked, and the key pool sends
//! translations to the keys that got through.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_engine::{TranslationRequest, Translator};
use biwrite_providers::{
    DefaultPrompts, Effort, NoObserver, ProviderConfig, ProviderKind, Squeeze, SqueezeBoard,
    SqueezeChange, WireApi, build_http, key_fingerprint,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Keys the relay answers. A key in `after` is answered from its n-th
/// request on, and a key in `refused` is invalid. With `no_model` every
/// request is answered that the model does not exist.
#[derive(Default)]
struct Rules {
    through: HashSet<String>,
    after: HashMap<String, usize>,
    refused: HashSet<String>,
    no_model: bool,
}

#[derive(Clone)]
struct Relay {
    base: String,
    rules: Arc<Mutex<Rules>>,
    /// Requests per key, in order of arrival.
    seen: Arc<Mutex<Vec<String>>>,
}

impl Relay {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay = Self {
            base: format!("http://{}/v1", listener.local_addr().unwrap()),
            rules: Arc::default(),
            seen: Arc::default(),
        };
        let r = relay.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let r = r.clone();
                tokio::spawn(async move {
                    let Some(req) = support::read_request(&mut socket).await else {
                        return;
                    };
                    let key = req
                        .header("authorization")
                        .unwrap_or_default()
                        .trim_start_matches("Bearer ")
                        .to_owned();
                    let n = {
                        let mut seen = r.seen.lock().unwrap();
                        seen.push(key.clone());
                        seen.iter().filter(|k| **k == key).count()
                    };
                    let answer = {
                        let rules = r.rules.lock().unwrap();
                        if rules.no_model {
                            Err((404, "model not found"))
                        } else if rules.refused.contains(&key) {
                            Err((401, "invalid token"))
                        } else if rules.through.contains(&key)
                            || rules.after.get(&key).is_some_and(|&m| n >= m)
                        {
                            Ok(())
                        } else {
                            Err((500, "the model is at capacity, try again later"))
                        }
                    };
                    let response = match answer {
                        Ok(()) => {
                            let body = [
                                r#"data: {"choices":[{"index":0,"delta":{"content":"早上好。"}}]}"#,
                                r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
                                "data: [DONE]",
                            ]
                            .map(|e| format!("{e}\n\n"))
                            .concat();
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                            )
                        }
                        Err((status, message)) => {
                            let body = format!(
                                r#"{{"error":{{"message":"{message}","code":"get_channel_failed"}}}}"#
                            );
                            format!(
                                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            )
                        }
                    };
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        relay
    }

    fn requests_with(&self, key: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|k| *k == key)
            .count()
    }
}

fn config(base: &str) -> ProviderConfig {
    ProviderConfig {
        id: "relay".into(),
        name: "Relay".into(),
        kind: ProviderKind::OpenaiCompatible,
        base_url: base.into(),
        model: "relay-model".into(),
        temperature: 0.0,
        effort: Effort::Default,
        wire_api: WireApi::Chat,
        service_tier: None,
        key_concurrency: None,
        max_retries: None,
    }
    .validated()
    .unwrap()
}

const A: &str = "sk-relay-key-aaaa";
const B: &str = "sk-relay-key-bbbb";
const C: &str = "sk-relay-key-cccc";

fn keys() -> Vec<String> {
    [A, B, C].map(String::from).to_vec()
}

type Changes = Arc<Mutex<Vec<SqueezeChange>>>;

fn start(relay: &Relay, board: &Arc<SqueezeBoard>) -> (Arc<Squeeze>, Changes) {
    let changes: Changes = Arc::default();
    let sink = Arc::clone(&changes);
    let squeeze = Squeeze::start(
        &config(&relay.base),
        keys(),
        Arc::clone(board),
        Arc::new(DefaultPrompts),
        Arc::new(move |c| sink.lock().unwrap().push(c)),
        &tokio::runtime::Handle::current(),
    )
    .unwrap();
    (squeeze, changes)
}

async fn until_ended(changes: &Changes, within: Duration) -> bool {
    let wait = async {
        loop {
            let ended = changes.lock().unwrap().iter().find_map(|c| match c {
                SqueezeChange::Ended { all_through } => Some(*all_through),
                _ => None,
            });
            if let Some(all) = ended {
                return all;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(within, wait)
        .await
        .expect("the squeeze ended")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_squeeze_goes_on_until_every_usable_key_got_through() {
    let relay = Relay::start().await;
    {
        let mut rules = relay.rules.lock().unwrap();
        rules.through.insert(B.into());
        rules.after.insert(C.into(), 3);
        rules.refused.insert(A.into());
    }
    let board = SqueezeBoard::new();
    let (squeeze, changes) = start(&relay, &board);
    assert!(squeeze.running());
    assert!(until_ended(&changes, Duration::from_secs(30)).await);
    assert!(!squeeze.running());
    assert!(board.is_through(&key_fingerprint(B)));
    assert!(board.is_through(&key_fingerprint(C)));
    assert!(!board.is_through(&key_fingerprint(A)));
    let progress = squeeze.progress();
    assert!(progress[0].refused, "an invalid key is not tried again");
    assert_eq!(progress[0].tries, 1);
    assert_eq!(progress[1].tries, 1);
    assert_eq!(progress[2].tries, 3, "the third try got through");
    assert_eq!(relay.requests_with(A), 1);
    assert_eq!(relay.requests_with(C), 3);
    let numbers: Vec<usize> = changes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            SqueezeChange::Through { number } => Some(*number),
            _ => None,
        })
        .collect();
    assert_eq!(numbers, vec![2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_a_squeeze_sends_nothing_more() {
    let relay = Relay::start().await;
    let board = SqueezeBoard::new();
    let (squeeze, changes) = start(&relay, &board);
    tokio::time::sleep(Duration::from_millis(500)).await;
    squeeze.stop();
    assert!(!until_ended(&changes, Duration::from_secs(5)).await);
    let sent = relay.seen.lock().unwrap().len();
    assert_eq!(sent, 3, "one try per key before the stop");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(relay.seen.lock().unwrap().len(), sent);
    assert!(board.through().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn translations_go_to_the_key_that_got_through_and_a_new_provider_knows_it() {
    let relay = Relay::start().await;
    relay.rules.lock().unwrap().through.insert(C.into());
    let board = SqueezeBoard::for_relay();
    let pool = keys();
    let provider = || {
        let pool = pool.clone();
        let http = build_http(
            &config(&relay.base),
            Arc::new(move || Ok(pool.clone())),
            Arc::new(DefaultPrompts),
            Arc::new(NoObserver),
        )
        .unwrap();
        http.join_board(Arc::clone(&board));
        http
    };
    let http = provider();
    let request = TranslationRequest {
        source: "Good morning.".into(),
        ..Default::default()
    };
    // Turned away until a request reaches the third key (the queue's
    // retries, a few at once here).
    let mut answered = false;
    for _ in 0..6 {
        if Translator::translate(http.as_ref(), &request, &|_| {})
            .await
            .is_ok()
        {
            answered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(2100)).await;
    }
    assert!(answered);
    assert!(board.is_through(&key_fingerprint(C)));
    let (a, b) = (relay.requests_with(A), relay.requests_with(B));
    for _ in 0..5 {
        Translator::translate(http.as_ref(), &request, &|_| {})
            .await
            .unwrap();
    }
    assert_eq!((relay.requests_with(A), relay.requests_with(B)), (a, b));
    // Another provider for the same relay (after switching away and back)
    // starts with the key that got through.
    let again = provider();
    let before = relay.seen.lock().unwrap().len();
    Translator::translate(again.as_ref(), &request, &|_| {})
        .await
        .unwrap();
    assert_eq!(relay.seen.lock().unwrap()[before..], [C.to_owned()]);
}

fn throughs(changes: &Changes) -> usize {
    changes
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, SqueezeChange::Through { .. }))
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pool_of_invalid_keys_ends_without_any_key_through() {
    let relay = Relay::start().await;
    relay
        .rules
        .lock()
        .unwrap()
        .refused
        .extend([A, B, C].map(String::from));
    let board = SqueezeBoard::new();
    let (squeeze, changes) = start(&relay, &board);
    assert!(!until_ended(&changes, Duration::from_secs(10)).await);
    assert_eq!(throughs(&changes), 0);
    assert!(squeeze.progress().iter().all(|p| p.refused && p.tries == 1));
    assert_eq!(relay.seen.lock().unwrap().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_the_relay_cannot_serve_is_not_tried_forever() {
    let relay = Relay::start().await;
    relay.rules.lock().unwrap().no_model = true;
    let board = SqueezeBoard::new();
    let (squeeze, changes) = start(&relay, &board);
    assert!(!until_ended(&changes, Duration::from_secs(10)).await);
    let progress = squeeze.progress();
    assert!(
        progress.iter().all(|p| p.refused && p.tries == 1),
        "{progress:?}"
    );
    assert!(
        progress[0]
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("404")),
        "{progress:?}"
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        relay.seen.lock().unwrap().len(),
        3,
        "nothing sent after it ended"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ping_keeps_a_key_that_got_through_answered_and_notes_keys_turned_away() {
    let relay = Relay::start().await;
    relay.rules.lock().unwrap().through.insert(C.into());
    let board = SqueezeBoard::new();
    let config = config(&relay.base);
    let ping = |key: &str| {
        biwrite_providers::ping(
            &config,
            key.to_owned(),
            Arc::clone(&board),
            Arc::new(DefaultPrompts),
        )
    };
    ping(C).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let idle = board.idle(&key_fingerprint(C)).unwrap();
    assert!(idle >= Duration::from_millis(300));
    ping(C).await.unwrap();
    assert!(
        board.idle(&key_fingerprint(C)).unwrap() < idle,
        "answered again"
    );
    // Keys turned away show the relay is crowded.
    assert!(!board.was_crowded());
    assert!(ping(A).await.is_err());
    assert!(ping(B).await.is_err());
    assert!(board.was_crowded());
    assert!(!board.is_through(&key_fingerprint(A)));
    assert_eq!(relay.requests_with(C), 2);
}
