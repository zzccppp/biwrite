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
    DefaultPrompts, Effort, KEEP_ALIVE, NoObserver, ProviderConfig, ProviderKind, RequestObserver,
    RequestRecord, Squeeze, SqueezeBoard, SqueezeChange, WireApi, build_http, key_fingerprint,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

/// Keys the relay answers. A key in `after` is answered from its n-th
/// request on, and a key in `refused` is invalid. A key in `cut_off` is
/// answered with an output cut off by its limit, one in `filtered` with the
/// content filter's refusal. With `no_model` every request is answered that
/// the model does not exist.
#[derive(Default)]
struct Rules {
    through: HashSet<String>,
    after: HashMap<String, usize>,
    refused: HashSet<String>,
    cut_off: HashSet<String>,
    filtered: HashSet<String>,
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
                        } else if rules.cut_off.contains(&key) {
                            Ok("length")
                        } else if rules.filtered.contains(&key) {
                            Ok("content_filter")
                        } else if rules.through.contains(&key)
                            || rules.after.get(&key).is_some_and(|&m| n >= m)
                        {
                            Ok("stop")
                        } else {
                            Err((500, "the model is at capacity, try again later"))
                        }
                    };
                    let response = match answer {
                        Ok(finish) => {
                            let last = format!(
                                r#"data: {{"choices":[{{"index":0,"delta":{{}},"finish_reason":"{finish}"}}]}}"#
                            );
                            let body = [
                                r#"data: {"choices":[{"index":0,"delta":{"content":"早上好。"}}]}"#,
                                last.as_str(),
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
        long_context: None,
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
    start_as(relay, board, false)
}

fn start_as(relay: &Relay, board: &Arc<SqueezeBoard>, unattended: bool) -> (Arc<Squeeze>, Changes) {
    let changes: Changes = Arc::default();
    let sink = Arc::clone(&changes);
    let squeeze = Squeeze::start(
        &config(&relay.base),
        keys(),
        Arc::clone(board),
        Arc::new(DefaultPrompts),
        Arc::new(move |c| sink.lock().unwrap().push(c)),
        &tokio::runtime::Handle::current(),
        unattended,
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
    assert!(
        board.used_within(Duration::from_secs(3600)),
        "got through for the author: kept alive"
    );
    let progress = squeeze.progress();
    assert!(progress[0].refused, "an invalid key is not tried again");
    assert!(board.gave_up(&key_fingerprint(A)), "nor squeezed back in");
    assert!(!board.gave_up(&key_fingerprint(B)));
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
    assert!(
        !board.used_within(Duration::from_secs(3600)),
        "tries are no use of the keys"
    );
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
    let board = SqueezeBoard::for_relay();
    let config = config(&relay.base);
    let log = Arc::new(Purposes::default());
    let ping_on = |board: &Arc<SqueezeBoard>, key: &str| {
        biwrite_providers::ping(
            &config,
            key.to_owned(),
            Arc::clone(board),
            Arc::new(DefaultPrompts),
            log.clone(),
        )
    };
    let ping = |key: &str| ping_on(&board, key);
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
    // Pings are no use of the keys by the author, and are logged as what
    // they are.
    assert!(!board.used_within(Duration::from_secs(3600)));
    let purposes = log.0.lock().unwrap().clone();
    assert!(!purposes.is_empty());
    assert!(purposes.iter().all(|p| p == KEEP_ALIVE), "{purposes:?}");
    // An ordinary pool's server errors never make it a crowded relay.
    let ordinary = SqueezeBoard::new();
    assert!(ping_on(&ordinary, A).await.is_err());
    assert!(ping_on(&ordinary, B).await.is_err());
    assert!(!ordinary.was_crowded());
}

/// A key kept alive keeps its place when a ping fails (the keeper sends it
/// again soon), and loses it for good when the relay refuses the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_ping_keeps_the_keys_place_unless_the_key_is_refused() {
    let relay = Relay::start().await;
    relay.rules.lock().unwrap().through.insert(C.into());
    let board = SqueezeBoard::for_relay();
    let config = config(&relay.base);
    let ping = |key: &str| {
        biwrite_providers::ping(
            &config,
            key.to_owned(),
            Arc::clone(&board),
            Arc::new(DefaultPrompts),
            Arc::new(Purposes::default()),
        )
    };
    let c = key_fingerprint(C);
    ping(C).await.unwrap();
    relay.rules.lock().unwrap().through.remove(C);
    for _ in 0..8 {
        assert!(ping(C).await.is_err());
    }
    assert!(board.is_through(&c), "turned away, it keeps its place");
    assert!(!board.gave_up(&c));
    relay.rules.lock().unwrap().refused.insert(C.into());
    assert!(ping(C).await.is_err());
    assert!(!board.is_through(&c), "refused, it loses it");
    assert!(board.gave_up(&c), "and is not squeezed back in");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twenty_consecutive_failed_pings_stop_requests_until_the_user_resumes_it_by_default() {
    let relay = Relay::start().await;
    let board = SqueezeBoard::for_relay();
    assert_eq!(board.keep_alive_failure_limit(), 20);
    let print = key_fingerprint(A);
    board.answered(&print);
    let config = config(&relay.base);
    let ping = || {
        biwrite_providers::ping(
            &config,
            A.into(),
            Arc::clone(&board),
            Arc::new(DefaultPrompts),
            Arc::new(NoObserver),
        )
    };
    for _ in 0..10 {
        assert!(ping().await.is_err());
    }
    assert_eq!(board.background_state(&print).0, 10);
    assert!(
        !board.background_stopped(&print),
        "ten failures still retry"
    );
    relay.rules.lock().unwrap().through.insert(A.into());
    ping().await.unwrap();
    assert_eq!(
        board.background_state(&print),
        (0, false, None),
        "a successful keep-alive clears consecutive failures"
    );
    relay.rules.lock().unwrap().through.remove(A);
    for failures in 1..=20 {
        assert!(ping().await.is_err());
        assert_eq!(board.background_state(&print).0, failures);
        assert_eq!(
            board.background_stopped(&print),
            failures == 20,
            "only twenty consecutive failures stop the key"
        );
    }
    let sent = relay.requests_with(A);
    assert_eq!(sent, 31, "count full requests exactly once");
    relay.rules.lock().unwrap().through.insert(A.into());
    assert!(ping().await.is_err());
    assert_eq!(
        relay.requests_with(A),
        sent,
        "stopped keys issue no background request"
    );
    assert!(board.resume_background(&print));
    ping().await.unwrap();
    assert_eq!(relay.requests_with(A), sent + 1);
    assert_eq!(board.background_state(&print), (0, false, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn selected_keep_alive_limits_are_independent_of_ordinary_request_retries() {
    for (limit, max_retries) in [(10, 30), (30, 1)] {
        let relay = Relay::start().await;
        let board = SqueezeBoard::for_relay();
        board.set_keep_alive_failure_limit(limit);
        assert_eq!(board.keep_alive_failure_limit(), limit);
        let print = key_fingerprint(A);
        board.answered(&print);
        let mut config = config(&relay.base);
        config.max_retries = Some(max_retries);
        let ping = || {
            biwrite_providers::ping(
                &config,
                A.into(),
                Arc::clone(&board),
                Arc::new(DefaultPrompts),
                Arc::new(NoObserver),
            )
        };
        for failures in 1..=limit {
            assert!(ping().await.is_err());
            assert_eq!(board.background_state(&print).0, failures);
            assert_eq!(
                board.background_stopped(&print),
                failures == limit,
                "the selected limit {limit} controls when the key stops"
            );
        }
        assert_eq!(
            relay.requests_with(A),
            limit as usize,
            "ordinary max_retries={max_retries} does not multiply keep-alive requests"
        );
        relay.rules.lock().unwrap().through.insert(A.into());
        assert!(ping().await.is_err());
        assert_eq!(
            relay.requests_with(A),
            limit as usize,
            "stopped keys issue no background request"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_key_is_not_tried_by_a_new_squeeze_without_explicit_rejoin() {
    let relay = Relay::start().await;
    let board = SqueezeBoard::new();
    board.set_pace(Some(Duration::from_millis(80)));
    let print = key_fingerprint(A);
    board.restore_stopped(&print, 10, "server unavailable");
    let (run, changes) = start(&relay, &board);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(relay.requests_with(A), 0);
    assert!(run.running());
    relay.rules.lock().unwrap().through.insert(A.into());
    assert!(run.rejoin_key(&board, &print));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !board.is_through(&print) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(relay.requests_with(A) > 0);
    assert_eq!(board.background_state(&print), (0, false, None));
    run.stop();
    assert!(!until_ended(&changes, Duration::from_secs(3)).await);
    board.restore_stopped(&print, 10, "server unavailable");
    assert!(
        !run.rejoin_key(&board, &print),
        "an ended squeeze never restarts itself"
    );
}

/// A relay that answers a key, though with an output cut off by its limit
/// or declined by its content filter, let the key through: the squeeze does
/// not ask it again every few seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_answered_with_an_unusable_output_got_through() {
    let relay = Relay::start().await;
    {
        let mut rules = relay.rules.lock().unwrap();
        rules.cut_off.insert(A.into());
        rules.filtered.insert(B.into());
        rules.through.insert(C.into());
    }
    let board = SqueezeBoard::new();
    let (squeeze, changes) = start(&relay, &board);
    assert!(until_ended(&changes, Duration::from_secs(10)).await);
    for key in [A, B, C] {
        assert!(board.is_through(&key_fingerprint(key)), "{key}");
        assert_eq!(relay.requests_with(key), 1, "{key}");
    }
    assert!(squeeze.progress().iter().all(|p| !p.refused));
}

/// A squeeze nobody watches (the keeper's) waits longer after every try
/// turned away; one the author started, or one while the author uses the
/// keys (and waits for them), tries every few seconds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unattended_squeeze_backs_off() {
    let (eager, patient, waited) = (
        Relay::start().await,
        Relay::start().await,
        Relay::start().await,
    );
    let (attended, _) = start_as(&eager, &SqueezeBoard::new(), false);
    let (unattended, _) = start_as(&patient, &SqueezeBoard::new(), true);
    let used = SqueezeBoard::new();
    used.note_use();
    let (in_use, _) = start_as(&waited, &used, true);
    tokio::time::sleep(Duration::from_millis(13_500)).await;
    attended.stop();
    unattended.stop();
    in_use.stop();
    // Tries at 0, 3, 6, 9 and 12 s, and at 0, 3 and 9 s.
    assert!(eager.requests_with(A) >= 5, "{}", eager.requests_with(A));
    assert!(waited.requests_with(A) >= 5, "{}", waited.requests_with(A));
    assert_eq!(patient.requests_with(A), 3);
}

/// The purposes of the requests reported.
#[derive(Default)]
struct Purposes(Mutex<Vec<String>>);

impl RequestObserver for Purposes {
    fn record(&self, record: &RequestRecord) {
        self.0.lock().unwrap().push(record.purpose.clone());
    }
}
