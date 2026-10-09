//! Provider behaviour against a scripted local server: request shape,
//! streaming, usage, parameter fallbacks, error mapping and key redaction.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use biwrite_core::Direction;
use biwrite_engine::{TranslateError, TranslationRequest, Translator};
use biwrite_providers::{
    DefaultPrompts, Effort, KeyFn, ProviderConfig, ProviderKind, WireApi, build_translator,
    list_models,
};
use support::{Canned, MockServer};

const KEY: &str = "sk-test-0123456789abcdefSECRET";

fn key_fn(calls: Arc<AtomicUsize>) -> KeyFn {
    Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![KEY.to_owned()])
    })
}

fn config(kind: ProviderKind, base: &str, model: &str) -> ProviderConfig {
    ProviderConfig {
        id: "p".into(),
        name: "Test".into(),
        kind,
        base_url: base.into(),
        model: model.into(),
        temperature: 0.2,
        effort: Effort::Low,
        wire_api: WireApi::Chat,
        service_tier: None,
        key_concurrency: None,
        max_retries: None,
    }
    .validated()
    .unwrap()
}

fn translator(
    kind: ProviderKind,
    server: &MockServer,
    model: &str,
) -> (Arc<dyn Translator>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let base = match kind {
        ProviderKind::OpenaiCompatible => format!("{}/v1", server.base),
        _ => server.base.clone(),
    };
    let t = build_translator(
        &config(kind, &base, model),
        key_fn(calls.clone()),
        Arc::new(DefaultPrompts),
    )
    .unwrap();
    (t, calls)
}

fn request(source: &str) -> TranslationRequest {
    TranslationRequest {
        source: source.into(),
        context_before: Some("Previous paragraph.".into()),
        ..Default::default()
    }
}

async fn run(
    t: &Arc<dyn Translator>,
    req: &TranslationRequest,
) -> (Result<String, TranslateError>, String) {
    let seen = Mutex::new(String::new());
    let out = t
        .translate(req, &|d| *seen.lock().unwrap() = d.to_owned())
        .await
        .map(|o| o.text);
    (out, seen.into_inner().unwrap())
}

// ── OpenAI-compatible ────────────────────────────────────────────────

fn openai_ok(parts: &[&str]) -> Canned {
    let mut events: Vec<String> = parts
        .iter()
        .map(|p| {
            format!(
                r#"data: {{"choices":[{{"index":0,"delta":{{"content":{}}}}}]}}"#,
                serde_json::to_string(p).unwrap()
            )
        })
        .collect();
    events.push(r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#.into());
    events
        .push(r#"data: {"choices":[],"usage":{"prompt_tokens":42,"completion_tokens":7}}"#.into());
    events.push("data: [DONE]".into());
    let refs: Vec<&str> = events.iter().map(String::as_str).collect();
    Canned::sse(&refs)
}

#[tokio::test]
async fn openai_streams_text_and_usage() {
    let server = MockServer::start().await;
    server.push(openai_ok(&["图神经", "网络", "很强。"]));
    let (t, calls) = translator(ProviderKind::OpenaiCompatible, &server, "deepseek-chat");
    let seen = Mutex::new(String::new());
    let out = t
        .translate(&request("GNNs are strong."), &|d| {
            *seen.lock().unwrap() = d.to_owned()
        })
        .await
        .unwrap();
    assert_eq!(out.text, "图神经网络很强。");
    assert_eq!(*seen.lock().unwrap(), "图神经网络很强。");
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (42, 7));

    let req = &server.requests()[0];
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("POST", "/v1/chat/completions")
    );
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let body = req.json();
    assert_eq!(body["model"], "deepseek-chat");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!((body["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert_eq!(body["messages"][0]["role"], "system");
    let user = body["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains("<source>\nGNNs are strong.\n</source>"));
    assert!(user.contains("<context_before>"));

    // The key is fetched once and cached.
    server.push(openai_ok(&["二"]));
    t.translate(&request("Two."), &|_| {}).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(t.provider(), format!("openai:{}/v1", server.base));
}

#[tokio::test]
async fn openai_retries_without_rejected_parameters_and_remembers() {
    let server = MockServer::start().await;
    server.push(Canned::json(400, r#"{"error":{"message":"Unsupported value: 'temperature' does not support 0.2 with this model."}}"#));
    server.push(Canned::json(
        400,
        r#"{"error":{"message":"Unrecognized request argument: stream_options"}}"#,
    ));
    server.push(openai_ok(&["好"]));
    server.push(openai_ok(&["好"]));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "o-model");
    assert_eq!(run(&t, &request("Good")).await.0.unwrap(), "好");
    assert_eq!(run(&t, &request("Good")).await.0.unwrap(), "好");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 4);
    assert!(reqs[0].json().get("temperature").is_some());
    assert!(reqs[2].json().get("temperature").is_none());
    assert!(reqs[2].json().get("stream_options").is_none());
    assert!(
        reqs[3].json().get("temperature").is_none(),
        "dropped params are remembered"
    );
}

#[tokio::test]
async fn openai_error_mapping() {
    let server = MockServer::start().await;
    server.push(
        Canned::json(429, r#"{"error":{"message":"Rate limit reached"}}"#)
            .with_header("retry-after", "7"),
    );
    server.push(Canned::json(503, "upstream unavailable"));
    server.push(Canned::json(
        429,
        r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota"}}"#,
    ));
    server.push(Canned::json(
        400,
        r#"{"error":{"message":"model not found"}}"#,
    ));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "m");
    let req = request("x");
    assert_eq!(
        run(&t, &req).await.0.unwrap_err(),
        TranslateError::RateLimited {
            retry_after: Some(Duration::from_secs(7))
        }
    );
    assert!(matches!(
        run(&t, &req).await.0.unwrap_err(),
        TranslateError::Server { status: 503, .. }
    ));
    assert!(matches!(
        run(&t, &req).await.0.unwrap_err(),
        TranslateError::Rejected { status: 429, .. }
    ));
    let err = run(&t, &req).await.0.unwrap_err();
    assert_eq!(
        err,
        TranslateError::Rejected {
            status: 400,
            message: "model not found".into()
        }
    );
}

#[tokio::test]
async fn api_key_is_redacted_from_errors() {
    let server = MockServer::start().await;
    server.push(Canned::json(
        401,
        &format!(r#"{{"error":{{"message":"Incorrect API key provided: {KEY}"}}}}"#),
    ));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "m");
    let err = run(&t, &request("x")).await.0.unwrap_err().to_string();
    assert!(!err.contains(KEY), "{err}");
    assert!(!err.contains("SECRET"), "{err}");
    assert!(err.contains("check the API key"));
}

#[tokio::test]
async fn reasoning_and_wrappers_are_stripped() {
    let server = MockServer::start().await;
    server.push(openai_ok(&[
        "<think>The user wants",
        " Chinese.</think>\n\n",
        "译文：",
        "结果",
    ]));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "qwen3");
    let (out, seen) = run(&t, &request("Result")).await;
    assert_eq!(out.unwrap(), "结果");
    assert!(
        !seen.contains("think"),
        "reasoning must not stream into the pane: {seen:?}"
    );
}

#[tokio::test]
async fn truncated_streams_and_cutoffs_are_errors() {
    let server = MockServer::start().await;
    server.push(Canned::sse(&[
        r#"data: {"choices":[{"index":0,"delta":{"content":"半"}}]}"#,
    ]));
    server.push(Canned::sse(&[
        r#"data: {"choices":[{"index":0,"delta":{"content":"长"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#,
        "data: [DONE]",
    ]));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "m");
    assert!(matches!(
        run(&t, &request("x")).await.0.unwrap_err(),
        TranslateError::Network(_)
    ));
    assert!(matches!(
        run(&t, &request("x")).await.0.unwrap_err(),
        TranslateError::InvalidResponse(_)
    ));
}

#[tokio::test]
async fn missing_key_is_a_config_error() {
    let server = MockServer::start().await;
    let cfg = config(
        ProviderKind::OpenaiCompatible,
        &format!("{}/v1", server.base),
        "m",
    );
    let t = build_translator(&cfg, Arc::new(|| Ok(vec![])), Arc::new(DefaultPrompts)).unwrap();
    let err = run(&t, &request("x")).await.0.unwrap_err();
    assert!(matches!(&err, TranslateError::Config(m) if m.contains("No API key")));
    assert!(!err.is_retryable());
    assert!(server.requests().is_empty());
}

// ── Anthropic ────────────────────────────────────────────────────────

fn anthropic_ok(parts: &[&str], stop: &str) -> Canned {
    let mut events = vec![
        r#"event: message_start
data: {"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":30,"cache_read_input_tokens":5,"output_tokens":1}}}"#.to_owned(),
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#.to_owned(),
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#.to_owned(),
        "event: ping\ndata: {\"type\":\"ping\"}".to_owned(),
        r#"event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#.to_owned(),
    ];
    for p in parts {
        events.push(format!(
            "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":1,\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}",
            serde_json::to_string(p).unwrap()
        ));
    }
    events.push(format!(
        "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{stop}\"}},\"usage\":{{\"output_tokens\":9}}}}"
    ));
    events.push("event: message_stop\ndata: {\"type\":\"message_stop\"}".to_owned());
    let refs: Vec<&str> = events.iter().map(String::as_str).collect();
    Canned::sse(&refs)
}

#[tokio::test]
async fn anthropic_streams_text_and_usage() {
    let server = MockServer::start().await;
    server.push(anthropic_ok(&["We study ", "graphs."], "end_turn"));
    let (t, _) = translator(ProviderKind::Anthropic, &server, "claude-opus-5-5");
    let req = TranslationRequest {
        direction: Direction::ZhEn,
        source: "我们研究图。".into(),
        ..Default::default()
    };
    let seen = Mutex::new(String::new());
    let out = t
        .translate(&req, &|d| *seen.lock().unwrap() = d.to_owned())
        .await
        .unwrap();
    assert_eq!(out.text, "We study graphs.");
    assert_eq!(*seen.lock().unwrap(), "We study graphs.");
    assert_eq!((out.usage.input_tokens, out.usage.output_tokens), (35, 9));

    let r = &server.requests()[0];
    assert_eq!(r.path, "/v1/messages");
    assert_eq!(r.header("x-api-key"), Some(KEY));
    assert_eq!(r.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(r.header("authorization"), None);
    // Not the first-party host: no fallback beta.
    assert_eq!(r.header("anthropic-beta"), None);
    let body = r.json();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert!(
        body.get("temperature").is_none(),
        "current Claude models reject temperature"
    );
    assert_eq!(body["output_config"]["effort"], "low");
    assert!(body["system"].as_str().unwrap().contains("Chinese draft"));
    assert!(
        body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("into English")
    );
}

#[tokio::test]
async fn anthropic_older_models_get_temperature_and_unsupported_effort_is_dropped() {
    let server = MockServer::start().await;
    server.push(Canned::json(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.effort: not supported on this model"}}"#));
    server.push(anthropic_ok(&["好"], "end_turn"));
    let (t, _) = translator(ProviderKind::Anthropic, &server, "claude-haiku-4-5");
    assert_eq!(run(&t, &request("Good")).await.0.unwrap(), "好");
    let reqs = server.requests();
    assert!((reqs[0].json()["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert!(reqs[1].json().get("output_config").is_none());
}

#[tokio::test]
async fn anthropic_refusal_and_stream_errors() {
    let server = MockServer::start().await;
    server.push(anthropic_ok(&[], "refusal"));
    server.push(Canned::sse(&[
        r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
        r#"event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    ]));
    server.push(Canned::json(
        529,
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
    ));
    let (t, _) = translator(ProviderKind::Anthropic, &server, "claude-sonnet-5-5");
    let req = request("x");
    assert!(matches!(
        run(&t, &req).await.0.unwrap_err(),
        TranslateError::Rejected { status: 200, .. }
    ));
    let overloaded = run(&t, &req).await.0.unwrap_err();
    assert!(matches!(
        overloaded,
        TranslateError::Server { status: 529, .. }
    ));
    assert!(overloaded.is_retryable());
    assert!(matches!(
        run(&t, &req).await.0.unwrap_err(),
        TranslateError::Server { status: 529, .. }
    ));
}

#[tokio::test]
async fn anthropic_fallback_switch_discards_declined_partial() {
    let server = MockServer::start().await;
    server.push(Canned::sse(&[
        r#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":3}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"declined partial"}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"fallback","from":{"model":"a"},"to":{"model":"b"}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":2,"content_block":{"type":"text","text":""}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"最终译文"}}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
        "event: message_stop\ndata: {\"type\":\"message_stop\"}",
    ]));
    let (t, _) = translator(ProviderKind::Anthropic, &server, "claude-opus-5-5");
    assert_eq!(run(&t, &request("x")).await.0.unwrap(), "最终译文");
}

// ── Model listing ────────────────────────────────────────────────────

#[tokio::test]
async fn lists_models_for_both_kinds() {
    let server = MockServer::start().await;
    server.push(Canned::json(
        200,
        r#"{"object":"list","data":[{"id":"b-model"},{"id":"a-model"}]}"#,
    ));
    server.push(Canned::json(200, r#"{"data":[{"id":"claude-opus-5-5","display_name":"Claude Opus 5.5"},{"id":"claude-haiku-5-5"}],"has_more":false}"#));
    let oa = config(
        ProviderKind::OpenaiCompatible,
        &format!("{}/v1", server.base),
        "x",
    );
    assert_eq!(list_models(&oa, KEY).await.unwrap(), ["a-model", "b-model"]);
    let an = config(ProviderKind::Anthropic, &server.base, "x");
    assert_eq!(
        list_models(&an, KEY).await.unwrap(),
        ["claude-haiku-5-5", "claude-opus-5-5"]
    );
    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/v1/models");
    assert_eq!(reqs[1].path, "/v1/models?limit=1000");
    assert_eq!(reqs[1].header("x-api-key"), Some(KEY));
}

// ── Review regressions ───────────────────────────────────────────────

#[tokio::test]
async fn redirects_are_not_followed_so_keys_stay_put() {
    let server = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    let location = format!("{}/v1/messages", elsewhere.base);
    server.push(Canned::json(307, "").with_header("Location", &location));
    let (t, _) = translator(ProviderKind::Anthropic, &server, "claude-opus-5-5");
    let err = run(&t, &request("x")).await.0.unwrap_err();
    assert!(
        matches!(&err, TranslateError::Rejected { status: 307, message } if message.contains("redirect"))
    );
    assert!(
        elsewhere.requests().is_empty(),
        "the key must not follow a redirect"
    );

    server.push(Canned::json(302, "").with_header("Location", &location));
    let cfg = config(ProviderKind::Anthropic, &server.base, "x");
    assert!(list_models(&cfg, KEY).await.is_err());
    assert!(elsewhere.requests().is_empty());
}

#[tokio::test]
async fn concurrent_requests_all_retry_a_rejected_parameter() {
    let server = MockServer::start().await;
    let reject = r#"{"error":{"message":"Unsupported parameter: 'temperature'"}}"#;
    server.push(Canned::json(400, reject));
    server.push(Canned::json(400, reject));
    server.push(openai_ok(&["一"]));
    server.push(openai_ok(&["二"]));
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "o-model");
    let (a, b) = (request("One"), request("Two"));
    let (ra, rb) = tokio::join!(run(&t, &a), run(&t, &b));
    assert!(ra.0.is_ok(), "{:?}", ra.0);
    assert!(rb.0.is_ok(), "{:?}", rb.0);
}

#[tokio::test]
async fn key_read_failures_are_remembered() {
    let server = MockServer::start().await;
    let reads = Arc::new(AtomicUsize::new(0));
    let counter = reads.clone();
    let key: KeyFn = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Err("user denied keychain access".into())
    });
    let cfg = config(
        ProviderKind::OpenaiCompatible,
        &format!("{}/v1", server.base),
        "m",
    );
    let t = build_translator(&cfg, key, Arc::new(DefaultPrompts)).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            run(&t, &request("x")).await.0.unwrap_err(),
            TranslateError::Config(_)
        ));
    }
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn stream_cut_mid_event_is_a_retryable_early_end() {
    let server = MockServer::start().await;
    let mut cut = Canned::sse(&[r#"data: {"choices":[{"index":0,"delta":{"content":"半"}}]}"#]);
    cut.chunks
        .push(r#"data: {"choices":[{"index":0,"del"#.into());
    server.push(cut);
    let (t, _) = translator(ProviderKind::OpenaiCompatible, &server, "m");
    let err = run(&t, &request("x")).await.0.unwrap_err();
    assert!(matches!(err, TranslateError::Network(_)), "{err:?}");
    assert!(err.is_retryable());
}

/// A figure in the style of a reference image: the image reaches the model
/// with the text, in each API's own form.
#[tokio::test]
async fn reference_images_reach_the_model_in_each_api() {
    use biwrite_providers::{ChatModel, ChatRequest, ImageInput, NoObserver, build_http};
    let image = ImageInput {
        media_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    };
    let chat = |images: Vec<ImageInput>| ChatRequest {
        purpose: "figure".into(),
        system: "Rules.".into(),
        user: "<instruction>bars like the image</instruction>".into(),
        images,
    };

    // OpenAI-compatible Chat Completions: text first, then image_url parts.
    let server = MockServer::start().await;
    server.push(openai_ok(&["<revision>x</revision>"]));
    let calls = Arc::new(AtomicUsize::new(0));
    let p = build_http(
        &config(ProviderKind::OpenaiCompatible, &format!("{}/v1", server.base), "gpt-x"),
        key_fn(calls),
        Arc::new(DefaultPrompts),
        Arc::new(NoObserver),
    )
    .unwrap();
    let model: Arc<dyn ChatModel> = p;
    model.complete(&chat(vec![image.clone()]), &|_| {}).await.unwrap();
    let body = server.requests()[0].json();
    let content = &body["messages"][1]["content"];
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,iVBORw0KGgo=");
    // Without images, the content stays a plain string.
    server.push(openai_ok(&["<revision>x</revision>"]));
    model.complete(&chat(Vec::new()), &|_| {}).await.unwrap();
    assert!(server.requests()[1].json()["messages"][1]["content"].is_string());

    // Anthropic Messages: images first, as base64 sources, then the text.
    let server = MockServer::start().await;
    server.push(anthropic_ok(&["<revision>x</revision>"], "end_turn"));
    let calls = Arc::new(AtomicUsize::new(0));
    let p = build_http(
        &config(ProviderKind::Anthropic, &server.base, "claude-x"),
        key_fn(calls),
        Arc::new(DefaultPrompts),
        Arc::new(NoObserver),
    )
    .unwrap();
    let model: Arc<dyn ChatModel> = p;
    model.complete(&chat(vec![image]), &|_| {}).await.unwrap();
    let body = server.requests()[0].json();
    let content = &body["messages"][0]["content"];
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["type"], "base64");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], "iVBORw0KGgo=");
    assert_eq!(content[1]["type"], "text");
}
