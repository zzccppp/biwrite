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
use biwrite_providers::config::{ANYROUTER, presets};
use biwrite_providers::{
    ChatModel, ChatRequest, DefaultPrompts, ProviderConfig, RecordState, RequestObserver,
    RequestRecord, build_http,
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
        long_context: None,
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
        long_context: None,
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

/// The bundled research-builder files, as the app loads them.
fn bundled_skill() -> biwrite_core::assist::Skill {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/resources/skills/research-builder");
    let files = ["writing-deai.md", "writing-playbook.md"]
        .iter()
        .map(|f| {
            (
                (*f).to_owned(),
                std::fs::read_to_string(root.join(f)).unwrap(),
            )
        })
        .collect();
    biwrite_core::assist::Skill {
        name: "research-builder".into(),
        source: "https://github.com/qzkinhit/research-builder".into(),
        version: "d227e92".into(),
        files,
        icons: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEYS and network access"]
async fn anyrouter_polishes_with_the_research_builder_rules() {
    use biwrite_core::assist::{self, Action};
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    let provider = build_http(
        &anyrouter_config("live-assist"),
        Arc::new(move || Ok(keys.clone())),
        Arc::new(DefaultPrompts),
        Arc::new(Records::default()),
    )
    .unwrap();
    let target = "It is worth noting that our method — which leverages a novel pipeline — significantly outperforms \\citet{smith2020}; it achieves $92.4\\%$ accuracy on \\textsc{Hospital}, which is crucially important.";
    let prompt = assist::build(
        &assist::Request {
            action: Action::Polish,
            mode: biwrite_core::Mode::Latex,
            target,
            ..Default::default()
        },
        &bundled_skill(),
    );
    println!("system prompt: {} chars", prompt.system.chars().count());
    let request = biwrite_providers::ChatRequest {
        purpose: "polish".into(),
        system: prompt.system,
        user: prompt.user,
        images: Vec::new(),
    };
    let mut out = None;
    for attempt in 0..8u32 {
        match biwrite_providers::ChatModel::complete(provider.as_ref(), &request, &|_| {}).await {
            Ok(o) => {
                out = Some(o);
                break;
            }
            Err(e) if e.is_retryable() => {
                println!("attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let out = out.expect("an answer");
    let answer = assist::parse(Action::Polish, &out.text, &prompt.protector).unwrap();
    println!("revision: {}", answer.revision.as_deref().unwrap_or(""));
    println!(
        "translation: {}",
        answer.translation.as_deref().unwrap_or("")
    );
    println!("changes_zh: {:?}", answer.changes_zh);
    println!("changes_en: {:?}", answer.changes_en);
    println!(
        "removed: {:?} translation_matches: {}",
        answer.removed, answer.translation_matches
    );
    let revision = answer.revision.unwrap();
    assert!(revision.contains("\\citet{smith2020}"));
    assert!(revision.contains("$92.4\\%$"));
    assert!(
        !revision.contains('—') && !revision.contains(';'),
        "{revision}"
    );
    assert!(answer.translation.is_some());
}

/// The author rewrites a paragraph's Chinese; the English follows.
#[tokio::test]
#[ignore = "live: needs BIWRITE_LIVE_ANYROUTER_KEYS"]
async fn anyrouter_revises_english_from_edited_chinese() {
    use biwrite_core::assist::{self, Action};
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    let provider = build_http(
        &anyrouter_config("live-mirror"),
        Arc::new(move || Ok(keys.clone())),
        Arc::new(DefaultPrompts),
        Arc::new(Records::default()),
    )
    .unwrap();
    let target = "We evaluate the method on four public datasets and report the mean accuracy over five runs \\cite{smith2020}.";
    let new_chinese =
        "我们在六个公开数据集上评测该方法，并报告十次运行的平均准确率 \\cite{smith2020}。";
    let prompt = assist::build(
        &assist::Request {
            action: Action::Mirror,
            mode: biwrite_core::Mode::Latex,
            target,
            instruction: new_chinese,
            ..Default::default()
        },
        &bundled_skill(),
    );
    let request = biwrite_providers::ChatRequest {
        purpose: "mirror".into(),
        system: prompt.system,
        user: prompt.user,
        images: Vec::new(),
    };
    let mut out = None;
    for attempt in 0..8u32 {
        match biwrite_providers::ChatModel::complete(provider.as_ref(), &request, &|_| {}).await {
            Ok(o) => {
                out = Some(o);
                break;
            }
            Err(e) if e.is_retryable() => {
                println!("attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let answer = assist::parse(
        Action::Mirror,
        &out.expect("an answer").text,
        &prompt.protector,
    )
    .unwrap();
    let revision = answer.revision.unwrap();
    println!("revision: {revision}");
    println!("changes_zh: {:?}", answer.changes_zh);
    assert!(revision.contains("\\cite{smith2020}"));
    let lower = revision.to_lowercase();
    assert!(lower.contains("six"), "{revision}");
    assert!(lower.contains("ten"), "{revision}");
    assert!(!lower.contains("four"), "{revision}");
}

/// DeepSeek with its preset, through the app's provider code: a
/// translation into Chinese that keeps the citation, and an answer to a
/// free-form request as the writing assistant sends it.
#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_DEEPSEEK_KEY and network access"]
async fn deepseek_translates_and_answers() {
    let Ok(key) = std::env::var("BIWRITE_LIVE_DEEPSEEK_KEY") else {
        eprintln!("BIWRITE_LIVE_DEEPSEEK_KEY is not set; skipping");
        return;
    };
    let preset = presets()
        .into_iter()
        .find(|p| p.name == "DeepSeek")
        .expect("the DeepSeek preset");
    let config = ProviderConfig {
        id: "live-deepseek".into(),
        name: preset.name.into(),
        kind: preset.kind,
        base_url: preset.base_url.into(),
        model: preset.model.into(),
        temperature: 0.0,
        effort: preset.effort,
        wire_api: preset.wire_api,
        service_tier: preset.service_tier,
        key_concurrency: preset.key_concurrency,
        max_retries: preset.max_retries,
        long_context: None,
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
        source: "Our method improves accuracy on twelve tables over prior work \\cite{smith2020}."
            .into(),
        ..Default::default()
    };
    let out = provider.translate(&request, &|_| {}).await.unwrap();
    println!("translation: {}", out.text);
    assert!(out.text.chars().any(biwrite_core::lang::is_cjk));
    assert!(out.text.contains("smith2020"), "the citation is kept");
    let chat = ChatRequest {
        purpose: "ask".into(),
        system: "Answer with one number and nothing else.".into(),
        user: "What is 2 + 3?".into(),
        images: Vec::new(),
    };
    let answer = provider.complete(&chat, &|_| {}).await.unwrap();
    println!("answer: {}", answer.text);
    assert!(answer.text.contains('5'));
    for r in records.0.lock().unwrap().iter() {
        println!(
            "record {}: {:?} http={:?} {}ms",
            r.id,
            r.state,
            r.http_status,
            r.duration_ms.unwrap_or(0)
        );
    }
}

/// AnyRouter's Claude models through the Anthropic API: the relay asks for
/// the 1M-token context, and the request goes again with it. What comes
/// back then is the model's answer or the relay at capacity, never the
/// 1M request again. `BIWRITE_LIVE_ANYROUTER_KEY`, `BIWRITE_LIVE_CLAUDE_MODEL`.
#[tokio::test]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEY and network access"]
async fn anyrouter_claude_gets_the_1m_context() {
    let Ok(key) = std::env::var("BIWRITE_LIVE_ANYROUTER_KEY") else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEY is not set; skipping");
        return;
    };
    let model =
        std::env::var("BIWRITE_LIVE_CLAUDE_MODEL").unwrap_or_else(|_| "claude-opus-4-7".into());
    let config = ProviderConfig {
        id: "live-anyrouter-claude".into(),
        name: "AnyRouter (Claude)".into(),
        kind: biwrite_providers::ProviderKind::Anthropic,
        base_url: "https://anyrouter.top".into(),
        model,
        temperature: 0.0,
        effort: Default::default(),
        wire_api: Default::default(),
        service_tier: None,
        key_concurrency: None,
        max_retries: None,
        long_context: None,
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
    let outcome = provider.translate(&request, &|_| {}).await;
    println!("outcome: {outcome:?}");
    let done: Vec<_> = records
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.state != RecordState::InFlight)
        .cloned()
        .collect();
    for r in &done {
        println!(
            "record {}: {:?} http={:?} error={:?} notes={:?}",
            r.id, r.state, r.http_status, r.error, r.notes
        );
    }
    if let Err(e) = &outcome {
        assert!(
            !e.to_string().contains("1m"),
            "the 1M context was asked for again: {e}"
        );
    }
}

/// A squeeze of a pool at AnyRouter's GPT preset: within the time given it
/// gets at least one key through, and the translations afterwards go to keys
/// that got through. Keys from `BIWRITE_LIVE_ANYROUTER_KEYS` (see
/// [`pool_keys`]), `BIWRITE_LIVE_SQUEEZE_MINUTES` bounds the wait (10 by
/// default).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs BIWRITE_LIVE_ANYROUTER_KEYS and network access"]
async fn anyrouter_squeeze_gets_a_key_through() {
    let Some(keys) = pool_keys() else {
        eprintln!("BIWRITE_LIVE_ANYROUTER_KEYS is not set; skipping");
        return;
    };
    let minutes: u64 = std::env::var("BIWRITE_LIVE_SQUEEZE_MINUTES")
        .ok()
        .and_then(|m| m.parse().ok())
        .unwrap_or(10);
    let config = ProviderConfig {
        id: "live-anyrouter-pool".into(),
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
        long_context: None,
    }
    .validated()
    .unwrap();
    let board = biwrite_providers::SqueezeBoard::new();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&changes);
    let started = std::time::Instant::now();
    let squeeze = biwrite_providers::Squeeze::start(
        &config,
        keys.clone(),
        Arc::clone(&board),
        Arc::new(DefaultPrompts),
        Arc::new(move |c| sink.lock().unwrap().push(c)),
        &tokio::runtime::Handle::current(),
        false,
    )
    .unwrap();
    let deadline = started + Duration::from_secs(minutes * 60);
    while board.through().is_empty() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let tries: u32 = squeeze.progress().iter().map(|p| p.tries).sum();
    println!(
        "{} of {} keys through after {:.0}s and {tries} tries",
        board.through().len(),
        keys.len(),
        started.elapsed().as_secs_f64()
    );
    squeeze.stop();
    assert!(
        !board.through().is_empty(),
        "no key got through in {minutes} min"
    );
    let records = Arc::new(Records::default());
    let pool = keys.clone();
    let provider = build_http(
        &config,
        Arc::new(move || Ok(pool.clone())),
        Arc::new(DefaultPrompts),
        records.clone(),
    )
    .unwrap();
    provider.join_board(Arc::clone(&board));
    let request = TranslationRequest {
        source: "Placeholder protection keeps citations intact.".into(),
        ..Default::default()
    };
    let through_before: Vec<String> = board.through().into_iter().map(|t| t.fingerprint).collect();
    let mut answered = 0;
    for _ in 0..5 {
        if provider.translate(&request, &|_| {}).await.is_ok() {
            answered += 1;
        }
    }
    let answered_with: Vec<String> = records
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.state == RecordState::Ok)
        .filter_map(|r| r.key.as_ref().map(|k| k.fingerprint.clone()))
        .collect();
    println!("{answered} of 5 translations answered");
    assert!(answered >= 3, "{answered} of 5 answered");
    assert!(
        answered_with.iter().all(|f| through_before.contains(f)),
        "answered by keys that had not got through: {answered_with:?} vs {through_before:?}"
    );
}
