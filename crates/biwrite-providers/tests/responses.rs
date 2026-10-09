//! OpenAI Responses API, key pools and request records, against a scripted
//! local server. The event shapes follow what OpenAI and Codex relays send,
//! including a rate limit reported inside a stream that began with HTTP 200.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::{Arc, Mutex};

use biwrite_engine::{TranslateError, TranslationRequest, Translator};
use biwrite_providers::{
    ChatModel, ChatRequest, DefaultPrompts, Effort, KeyFn, ProviderConfig, ProviderKind,
    RecordState, RequestObserver, RequestRecord, ServiceTier, WireApi, build_http,
};
use support::{Canned, MockServer};

const KEY_A: &str = "sk-test-aaaaaaaaaaaaaaaaaaaaAAAA";
const KEY_B: &str = "sk-test-bbbbbbbbbbbbbbbbbbbbBBBB";

#[derive(Default)]
struct Records(Mutex<Vec<RequestRecord>>);

impl RequestObserver for Records {
    fn record(&self, record: &RequestRecord) {
        self.0.lock().unwrap().push(record.clone());
    }
}

impl Records {
    /// The final state of every request, in start order.
    fn finished(&self) -> Vec<RequestRecord> {
        let all = self.0.lock().unwrap();
        let mut out: Vec<RequestRecord> = Vec::new();
        for r in all.iter() {
            match out.iter_mut().find(|o| o.id == r.id) {
                Some(slot) => *slot = r.clone(),
                None => out.push(r.clone()),
            }
        }
        out
    }
}

fn keys(list: &[&str]) -> KeyFn {
    let list: Vec<String> = list.iter().map(|k| (*k).to_owned()).collect();
    Arc::new(move || Ok(list.clone()))
}

fn config(server: &MockServer) -> ProviderConfig {
    ProviderConfig {
        id: "p-relay".into(),
        name: "Relay".into(),
        kind: ProviderKind::OpenaiCompatible,
        base_url: format!("{}/v1", server.base),
        model: "gpt-6-astra".into(),
        temperature: 0.0,
        effort: Effort::High,
        wire_api: WireApi::Responses,
        service_tier: Some(ServiceTier::Priority),
        key_concurrency: None,
        max_retries: None,
    }
    .validated()
    .unwrap()
}

fn provider(
    server: &MockServer,
    pool: &[&str],
) -> (Arc<biwrite_providers::HttpProvider>, Arc<Records>) {
    let records = Arc::new(Records::default());
    let p = build_http(
        &config(server),
        keys(pool),
        Arc::new(DefaultPrompts),
        records.clone(),
    )
    .unwrap();
    (p, records)
}

fn request(source: &str) -> TranslationRequest {
    TranslationRequest {
        source: source.into(),
        ..Default::default()
    }
}

/// A complete Responses stream answering `parts`.
fn ok(parts: &[&str]) -> Canned {
    let mut events = vec![
        r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1","status":"in_progress","model":"gpt-6-astra","reasoning":{"effort":"high"},"service_tier":"auto"}}"#.to_owned(),
        r#"event: response.output_item.added
data: {"type":"response.output_item.added","output_index":0,"item":{"type":"message","role":"assistant","content":[]}}"#.to_owned(),
    ];
    for p in parts {
        events.push(format!(
            "event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":{}}}",
            serde_json::to_string(p).unwrap()
        ));
    }
    events.push(
        r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","model":"gpt-6-astra-2026","reasoning":{"effort":"high"},"service_tier":"default","usage":{"input_tokens":120,"input_tokens_details":{"cached_tokens":96},"output_tokens":30,"output_tokens_details":{"reasoning_tokens":18},"total_tokens":150}}}"#.to_owned(),
    );
    let refs: Vec<&str> = events.iter().map(String::as_str).collect();
    Canned::sse(&refs)
}

/// HTTP 200, then a rate limit inside the stream (as AnyRouter sends it).
fn stream_rate_limited() -> Canned {
    Canned::sse(&[
        r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_x","status":"in_progress"}}"#,
        r#"event: error
data: {"type":"error","error":{"type":"too_many_requests","code":"rate_limit_exceeded","message":"Your requests to gpt-6-astra have exceeded token rate limit.","param":null},"sequence_number":1}"#,
        r#"event: response.failed
data: {"type":"response.failed","response":{"id":"resp_x","status":"failed","error":{"code":"rate_limit_exceeded","message":"Your requests to gpt-6-astra have exceeded token rate limit."}}}"#,
    ])
}

async fn translate(t: &dyn Translator, source: &str) -> Result<String, TranslateError> {
    t.translate(&request(source), &|_| {}).await.map(|o| o.text)
}

#[tokio::test]
async fn streams_text_with_the_codex_compatible_request_shape() {
    let server = MockServer::start().await;
    server.push(ok(&["占位符", "保护。"]));
    let (p, records) = provider(&server, &[KEY_A]);
    let seen = Mutex::new(String::new());
    let out = p
        .translate(&request("Placeholder protection."), &|d| {
            *seen.lock().unwrap() = d.to_owned()
        })
        .await
        .unwrap();
    assert_eq!(out.text, "占位符保护。");
    assert_eq!(*seen.lock().unwrap(), "占位符保护。");
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (120, 30));

    let req = &server.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/v1/responses")
    );
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {KEY_A}").as_str())
    );
    let body = req.json();
    assert_eq!(body["model"], "gpt-6-astra");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert_eq!(body["include"][0], "reasoning.encrypted_content");
    assert!(
        body["prompt_cache_key"]
            .as_str()
            .unwrap()
            .starts_with("biwrite-translate-")
    );
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["service_tier"], "priority");
    assert!(
        body["instructions"]
            .as_str()
            .unwrap()
            .contains("academic English")
    );
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert!(body.get("temperature").is_none());

    let rec = &records.finished()[0];
    assert_eq!(rec.state, RecordState::Ok);
    assert_eq!(rec.purpose, "translate");
    assert_eq!(rec.wire, "responses");
    assert!(rec.endpoint.ends_with("/v1/responses"), "{}", rec.endpoint);
    assert_eq!(rec.request.model.as_deref(), Some("gpt-6-astra"));
    assert_eq!(rec.request.effort.as_deref(), Some("high"));
    assert_eq!(rec.request.service_tier.as_deref(), Some("priority"));
    // The response's own claims, as the last event stated them.
    assert_eq!(rec.response.model.as_deref(), Some("gpt-6-astra-2026"));
    assert_eq!(rec.response.effort.as_deref(), Some("high"));
    assert_eq!(rec.response.service_tier.as_deref(), Some("default"));
    assert_eq!(rec.usage.cached_tokens, Some(96));
    assert_eq!(rec.usage.reasoning_tokens, Some(18));
    assert_eq!(rec.http_status, Some(200));
    assert_eq!(rec.output_chars, "占位符保护。".chars().count());
    assert!(rec.first_token_ms.is_some() && rec.duration_ms.is_some());
    let key = rec.key.as_ref().unwrap();
    assert_eq!((key.number, key.count, key.tail.as_str()), (1, 1, "AAAA"));
    let json = serde_json::to_string(rec).unwrap();
    assert!(
        !json.contains(KEY_A) && !json.contains("Placeholder"),
        "{json}"
    );
}

#[tokio::test]
async fn a_rate_limit_inside_the_stream_is_retryable() {
    let server = MockServer::start().await;
    server.push(stream_rate_limited());
    let (p, records) = provider(&server, &[KEY_A]);
    let err = translate(p.as_ref(), "x").await.unwrap_err();
    assert!(matches!(err, TranslateError::RateLimited { .. }), "{err:?}");
    assert!(err.is_retryable());
    let rec = &records.finished()[0];
    assert_eq!(rec.state, RecordState::Error);
    assert_eq!(rec.http_status, Some(200));
}

#[tokio::test]
async fn a_rate_limited_key_hands_over_to_the_next_one() {
    let server = MockServer::start().await;
    server.push(stream_rate_limited());
    server.push(ok(&["好"]));
    let (p, records) = provider(&server, &[KEY_A, KEY_B]);
    assert_eq!(translate(p.as_ref(), "Good").await.unwrap(), "好");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs[0].header("authorization"),
        Some(format!("Bearer {KEY_A}").as_str())
    );
    assert_eq!(
        reqs[1].header("authorization"),
        Some(format!("Bearer {KEY_B}").as_str())
    );
    let recs = records.finished();
    assert_eq!(recs[0].state, RecordState::Error);
    assert!(
        recs[0].notes.iter().any(|n| n.contains("another key")),
        "{:?}",
        recs[0].notes
    );
    assert_eq!(recs[1].state, RecordState::Ok);
    assert_eq!(recs[1].key.as_ref().unwrap().number, 2);
}

#[tokio::test]
async fn an_invalid_key_is_set_aside_and_later_requests_skip_it() {
    let server = MockServer::start().await;
    server.push(Canned::json(
        401,
        r#"{"error":{"message":"invalid api key","type":"authentication_error"}}"#,
    ));
    server.push(ok(&["一"]));
    server.push(ok(&["二"]));
    let (p, _) = provider(&server, &[KEY_A, KEY_B]);
    assert_eq!(translate(p.as_ref(), "One").await.unwrap(), "一");
    assert_eq!(translate(p.as_ref(), "Two").await.unwrap(), "二");
    let auth: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.header("authorization").unwrap().to_owned())
        .collect();
    assert_eq!(
        auth,
        [
            format!("Bearer {KEY_A}"),
            format!("Bearer {KEY_B}"),
            format!("Bearer {KEY_B}")
        ]
    );
    let status = p.key_status().unwrap();
    assert_eq!(status[0].state, "rejected");
    assert_eq!(status[1].state, "ready");
    assert!(!format!("{status:?}").contains(KEY_A));
}

#[tokio::test]
async fn an_exhausted_quota_is_final_for_a_single_key() {
    let server = MockServer::start().await;
    server.push(Canned::sse(&[
        r#"data: {"type":"response.failed","response":{"status":"failed","error":{"code":"insufficient_quota","message":"You exceeded your current quota."}}}"#,
    ]));
    let (p, _) = provider(&server, &[KEY_A]);
    let err = translate(p.as_ref(), "x").await.unwrap_err();
    assert!(
        matches!(err, TranslateError::Rejected { status: 429, .. }),
        "{err:?}"
    );
    assert!(!err.is_retryable());
}

#[tokio::test]
async fn openai_style_error_events_and_incomplete_responses() {
    let server = MockServer::start().await;
    server.push(Canned::sse(&[
        r#"data: {"type":"error","code":"server_error","message":"The server had an error","param":null,"sequence_number":2}"#,
    ]));
    server.push(Canned::sse(&[
        r#"data: {"type":"response.output_text.delta","delta":"长"}"#,
        r#"data: {"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#,
    ]));
    server.push(Canned::sse(&[
        r#"data: {"type":"response.output_text.delta","delta":"半"}"#,
    ]));
    let (p, _) = provider(&server, &[KEY_A]);
    let server_error = translate(p.as_ref(), "x").await.unwrap_err();
    assert!(
        matches!(server_error, TranslateError::Server { status: 500, .. }),
        "{server_error:?}"
    );
    let cut = translate(p.as_ref(), "x").await.unwrap_err();
    assert!(
        matches!(&cut, TranslateError::InvalidResponse(m) if m.contains("cut off")),
        "{cut:?}"
    );
    let early = translate(p.as_ref(), "x").await.unwrap_err();
    assert!(matches!(early, TranslateError::Network(_)), "{early:?}");
}

#[tokio::test]
async fn rejected_optional_parameters_are_dropped_once() {
    let server = MockServer::start().await;
    server.push(Canned::json(
        400,
        r#"{"error":{"message":"Encrypted content is not supported with this model.","param":"include"}}"#,
    ));
    server.push(ok(&["好"]));
    server.push(ok(&["好"]));
    let (p, records) = provider(&server, &[KEY_A]);
    assert_eq!(translate(p.as_ref(), "Good").await.unwrap(), "好");
    assert_eq!(translate(p.as_ref(), "Good").await.unwrap(), "好");
    let reqs = server.requests();
    assert!(reqs[0].json().get("include").is_some());
    assert!(reqs[1].json().get("include").is_none());
    assert!(reqs[2].json().get("include").is_none());
    assert!(reqs[1].json().get("prompt_cache_key").is_some());
    let first = &records.finished()[0];
    assert!(
        first.notes.iter().any(|n| n.contains("`include`")),
        "{:?}",
        first.notes
    );
}

#[tokio::test]
async fn the_assistant_call_strips_reasoning_and_names_its_purpose() {
    let server = MockServer::start().await;
    server.push(ok(&["<think>plan</think>\n", "答复"]));
    let (p, records) = provider(&server, &[KEY_A]);
    let chat: Arc<dyn ChatModel> = p.clone();
    assert_eq!(chat.label(), "Relay · gpt-6-astra");
    let out = chat
        .complete(
            &ChatRequest {
                purpose: "polish".into(),
                system: "Rules.".into(),
                user: "<target>x</target>".into(),
            },
            &|_| {},
        )
        .await
        .unwrap();
    assert_eq!(out.text, "答复");
    let body = server.requests()[0].json();
    assert_eq!(body["instructions"], "Rules.");
    assert!(
        body["prompt_cache_key"]
            .as_str()
            .unwrap()
            .starts_with("biwrite-polish-")
    );
    assert_eq!(records.finished()[0].purpose, "polish");
}

#[tokio::test]
async fn a_dropped_request_is_recorded_as_cancelled() {
    let server = MockServer::start().await;
    let mut slow = ok(&["慢"]);
    slow.delay = std::time::Duration::from_secs(5);
    server.push(slow);
    let (p, records) = provider(&server, &[KEY_A]);
    let req = request("x");
    let fut = p.translate(&req, &|_| {});
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), fut)
            .await
            .is_err()
    );
    let recs = records.finished();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].state, RecordState::Cancelled);
}
