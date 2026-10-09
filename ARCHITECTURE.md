# BiWrite architecture

BiWrite is a desktop editor for writing English with a live Chinese reading aid.
The English source file is the only deliverable; the Chinese pane is derived,
cached, and never written to the source. The panes can be swapped to edit the
Chinese instead, with the English following (see *Language swap*).

## Layout

```
crates/biwrite-core/     Pure logic. No Tauri, no async, no IO.
  segment/               Source → segments (plain, Markdown, LaTeX)
  document.rs            Stable segment IDs across edits (LCS alignment)
  compose.rs             Build the other-language document (language swap)
  lang.rs                Direction (en-zh / zh-en), CJK-aware tokens
  hash.rs                blake3 of normalized text = segment key
  textfile.rs            Byte-exact file round-trip (BOM, line endings)
  utf16.rs               Byte offsets → CodeMirror (UTF-16) positions
crates/biwrite-engine/   Translation engine. tokio, no Tauri.
  engine.rs              Public API + reconcile (cache lookup, enqueue)
  queue.rs               Dispatch, generations, streaming, backoff, results
  state.rs               Engine state behind one mutex; settings
  swap.rs                Language swap and composing the English file
  translator.rs          `Translator` trait + request/response types
  cache.rs               `TranslationCache` trait (+ in-memory impl)
  sqlite_cache.rs        Persistent cache (rusqlite, bundled SQLite)
  mock.rs                Mock translator (reverses text, 0.5–2 s)
  tests/                 Acceptance, queue and sample-paper tests (virtual time)
crates/biwrite-providers/  LLM providers. reqwest + SSE, no Tauri.
  openai.rs              OpenAI-compatible chat completions (streaming)
  anthropic.rs           Anthropic Messages API (streaming, raw HTTP)
  http.rs                Client, lazy key, error mapping, redaction, SSE loop
  sse.rs / clean.rs      SSE parser; output cleanup + <think> stream filter
  prompt.rs              System prompt files + tagged user message
  config.rs / models.rs  Provider config, validation, presets; model listing
  tests/                 Against a scripted local HTTP server
src-tauri/               Tauri 2 shell: commands, events, dialogs, file IO,
                         keychain (secrets.rs), settings.json (settings.rs)
src/                     Svelte 5 + CodeMirror 6 frontend
```

The workspace `Cargo.toml` is at the repo root; `cargo test --workspace` and
`cargo clippy --workspace --all-targets -- -D warnings` cover everything.

## Data flow

```
CodeMirror edit ──(800 ms debounce, single-flight)──▶ update_document(text)
   │                                                     │
   │ map segment positions through                       ▼
   │ local edits; dim touched blocks          DocumentModel::apply
   │                                          (segment → hash → LCS align)
   │                                                     │
   ◀──────────── Snapshot { layout, changed states } ◀──── reconcile:
                                                         translated? keep
                                                         cache hit?  apply
                                                         else        enqueue
                                                                │
   ◀── "segment-states" events (streamed partials, results) ◀── queue
```

* **Segments.** A segment has a byte range and a content range (the text sent to
  the translator: the paragraph, or a heading's title). Its key is
  `blake3(normalize(content))`, where normalizing trims and collapses whitespace,
  so re-wrapping a paragraph never triggers a request.
* **Stable IDs.** `DocumentModel::apply` diffs old and new key lists with
  `similar`'s LCS. Similarity is token-based, with one token per CJK
  character, so Chinese edits pair too. Pairing is budgeted (≤256 pairs,
  ≤50k tokens compared, 20 ms per diff). Equal runs keep their IDs. Inserted segments that match a
  deleted key take over that ID (moves). Within replaced hunks, segments are
  paired by word-level similarity (order-preserving DP, threshold 0.3), so an
  edited paragraph keeps its block. Everything else gets a fresh ID.
* **Translation state** lives in the engine per ID: last translation, the hash
  and source it belongs to, status, generation, and the error (tied to a
  hash). A segment is up to date iff `translated_hash == hash`. This makes
  undo free: the old hash is either still current or in the cache.
* **Queue.** It holds segment IDs, not requests. The request is built at
  dispatch time from the current text, with prev/next context, the document
  note, and, when the old and new source are more than 0.6 similar, a revise
  basis (old source + old translation). Up to `concurrency` jobs run at once.
  Every dispatch or cancellation bumps the segment's generation. A result is
  applied only if the generation and hash still match, so stale results are
  discarded but still cached. Edits abort in-flight requests for that segment
  (`AbortHandle`; dropping the future cancels the HTTP request). Retryable
  errors (429, 5xx, network) back off exponentially with jitter and honour
  `Retry-After`.
* **Errors are sticky per hash.** A failed segment is not retried on unrelated
  edits, only when its own text changes or the user clicks *retry*.
* **Events.** Every state change bumps a global version; the frontend drops
  updates older than what it has, so invoke responses and events may arrive in
  any order.

## Segmentation

| Mode | Translatable | Skipped (collapsed on the right) |
|---|---|---|
| Plain | paragraphs | — |
| Markdown | paragraphs, ATX/setext headings | front matter, fenced code, `$$` math, `<!-- -->`, rules |
| LaTeX | paragraphs, `\section`… headings, `\caption{}` in floats, `\item` text | preamble (through `\begin{document}`), `\end{document}` and after, `%` lines, math envs (`equation`, `align`, `gather`, …, `\[ \]`, `$$`), `tabular`, `algorithm`, `verbatim`, `lstlisting`, non-caption float content, structural-only lines (`\maketitle`, `\begin{abstract}`, `\label{}`, …) |

Run-in `\paragraph{X} text` stays a paragraph so no prose is hidden. Invariants
(tested on the samples): ordered, disjoint, every non-blank line covered,
`content ⊆ range`. `segment/latex_tests.rs` pins the full segmentation of
`samples/paper.tex` (35 segments, 22 translatable).

## Cache

SQLite at `<app data>/cache.sqlite3` (macOS:
`~/Library/Application Support/app.biwrite.desktop/`), WAL mode, with a 250 ms
busy timeout because lookups run under the engine lock. Key:
`(hash, direction, provider, model, glossary_version)`, shared across files and
sessions. Direction is part of the key because the same text can be a source
in either direction. If the database can't be opened the app falls back to an
in-memory cache.

## Language swap

The toolbar's `EN ⇄ 中` button swaps which language is edited.

1. `compose` splices each translatable segment's current translation into its
   *content* range and copies everything else byte for byte: gaps, skipped
   blocks, `\section{`…`}` markup, `\item ` markers. It then re-segments the
   result and refuses (`ComposeError::Structure`) unless it yields the same
   kinds with exactly the inserted contents. A swap needs every paragraph
   translated (`NotReady` otherwise).
2. The composed text is loaded as the new source with the flipped direction.
   Each original becomes the *seed* of the segment it was spliced into. This
   is positional, so paragraphs whose translations coincide (e.g. "Methods" and
   "Method" → 方法) keep distinct originals. A hash-keyed fallback covers only
   unambiguous text. Reconcile consults seeds before the cache, so every
   untouched paragraph translates back to its exact original with no request,
   and seeds are spliced back *verbatim* (`Insert::exact`). An edited paragraph
   is translated in revise mode: old Chinese + original English + new Chinese.
   Machine output is shaped minimally by `fit`: blank lines dropped, headings
   joined, and in LaTeX an unescaped `%` becomes `\%`.
3. The file is always English. While editing Chinese, Save writes
   `compose_target()`, the English composed from the right pane. Swapping back
   without edits restores the original byte for byte (tested on both samples).
4. Retranslate (segment and all) is disabled while editing Chinese, because it
   would replace the user's English with machine translation. The editor is
   read-only during a swap. Opening a file always returns to editing English.

Known segmenter limits (not translated, or translated as a whole):
`\item[label]` labels, `\subfloat[caption]`, a second `\caption` on the same
line, `$$…$$ text` on one line (the line is skipped as math), and
`\section{…}` split over two lines (it becomes a paragraph).

## Providers (M3)

`Translator` implementations live in `biwrite-providers`:

* **OpenAI-compatible:** `POST {base_url}/chat/completions` with
  `stream: true` and `stream_options.include_usage`. This covers OpenAI,
  DeepSeek, Qwen (DashScope compatible mode), Kimi, OpenRouter and local
  servers.
* **Anthropic:** `POST {base_url}/v1/messages` with `anthropic-version:
  2023-06-01`. There is no official Rust SDK, so this is raw HTTP.
  * `temperature` is not sent to current Claude models, which reject
    sampling parameters.
  * `output_config.effort` defaults to `low`.
  * On `api.anthropic.com`, Opus 5.5 / Opus 5 / Fable 5.1 / Sonnet 5.5 opt
    into server-side refusal fallbacks (`fallbacks: "default"`, beta
    `server-side-fallback-2026-07-01`). A `fallback` block in the stream
    discards the declined partial, and `stop_reason: "refusal"` becomes an
    error.
* **Prompts:** the system prompt comes from
  `<config>/prompts/{en-zh,zh-en}.txt`, created with defaults and re-read per
  request. The user message is assembled in code from tagged blocks:
  `<document_note>`, `<glossary>`, `<context_before>`/`<context_after>` ("do
  not translate"), `<previous_source>`/`<previous_translation>` (revise
  mode) and `<source>`, followed by one instruction. Editing the prompt file
  can't break the input structure.
* **Output:** the stream filter withholds a leading `<think>` block. The
  final text is cleaned of think blocks, code fences, wrapper tags,
  "Translation:"/"译文：" labels and enclosing quotes (unless the source was
  quoted).
* **Errors:**
  * 429 → `RateLimited` (honours `retry-after` / `retry-after-ms`);
    408/409/5xx/529 → `Server`. Both are retried with backoff by the engine.
  * `insufficient_quota` 429, 4xx, refusals, content filters and cut-off
    outputs → non-retryable errors. A stream that ends without its terminator
    → `Network`, which is retried.
  * A missing key → `Config`. Switching provider or adding a key retries
    failed segments.
  * Optional parameters an endpoint rejects with a 400 (`temperature`,
    `stream_options`, `effort`, `fallbacks`) are dropped once and remembered
    for that translator.
* **Timeouts:** connect 15 s, headers 90 s, mid-stream silence 90 s.
  Dropping the future (edit/cancel) aborts the HTTP request.
* **No redirects:** clients never follow redirects. reqwest strips only
  `Authorization` across origins, so a redirect could otherwise carry
  Anthropic's `x-api-key` elsewhere.
* **Streaming partials:** the `PartialFn` callback receives the whole visible
  output so far, not deltas, so a provider can restart it (Anthropic
  fallback). A stream cut mid-event drops the unfinished event, which ends
  as a retryable `Network` error.
* **Switching translator:** in-flight work is re-run with the new
  translator. Output from the placeholder mock is redone when a real
  provider is selected. Real translations are kept across real providers.

### Keys and settings

* Keys live in the OS credential store (`keyring`; service
  `app.biwrite.desktop`, account = provider id). `set_api_key` is
  write-only: no command or event returns a key, and views carry `hasKey`
  flags.
* The translator reads the key lazily on first use, on a blocking thread, and
  caches it in memory. Every error message passes through `redact()`, which
  removes the key and anything `sk-…`-shaped. Tested in
  `providers/tests/providers.rs` and `src-tauri/src/leak_tests.rs`.
* The one exposure is that the key is typed into the settings form and sent
  once, webview → Rust, over `set_api_key`. It is then cleared from the
  form.
* `settings.json` (in the config dir) holds providers (no keys), the active
  provider, concurrency and per-file document notes. A corrupt file is moved
  to `settings.json.bad`.
* Base URLs must be https, or http on localhost, with no credentials. The
  cache identity is `kind:base_url`, so the same model name on another host
  is a different cache entry.
* Changing a provider's API type or origin (scheme, host, port) deletes its
  stored key, so a key is never sent to a host it wasn't entered for.
* A document's note is set before `load` starts its first requests.

## Frontend

* `Session` (Svelte runes) holds the layout (render order and kinds) and a
  `SvelteMap` of states keyed by ID. Blocks are keyed by ID, so surviving
  segments never re-render or move.
* `SegmentIndex` keeps segment positions for the *current* editor document.
  Layouts arrive for the text that was sent; they are mapped through the
  `ChangeSet` of edits made since sending, then through every later edit.
* `ScrollSync` aligns the segment at the reading line (top edge plus content
  padding) of the driving pane with its counterpart, proportionally within the
  segment. The pane under the pointer or keyboard drives, and our own scroll
  writes are ignored.
* Clicking a block moves the cursor to the segment start and scrolls the editor
  so the segment sits at the same height as the clicked block.

## Security invariants

* All file IO and (from M3) network calls and API keys live in Rust. File
  paths are chosen in native dialogs on the Rust side; the webview cannot name
  a path to read or write.
* IPC payloads carry document text, segment layout and states, usage counters
  and file names only (`events.rs`, `commands.rs`).
* Capability: `core:default` only. CSP restricts scripts to `'self'`.

## File round-trip

`TextFile::decode` accepts UTF-8 (with or without BOM), normalizes line endings
to `\n` for the editor, and remembers the original bytes. `encode(text)` returns
the original bytes when `text` is unchanged (byte-identical save, even with
mixed line endings). Otherwise it restores the BOM and the dominant line ending.
Saves are atomic: a temp file in the same directory, fsync, rename. Symlinks
resolve to their target and permissions are preserved.

## Milestones

| | Scope | Status |
|---|---|---|
| M1 | Scaffold, open/save, panes, mock translator, full diff/queue/UI flow | done |
| M2 | LaTeX segmenter, SQLite cache, tests on a sample paper; language swap | done |
| M3 | OpenAI-compatible + Anthropic providers, keychain, SSE streaming, settings UI | done |
| M4 | Placeholder protection, glossary, revise prompts, scroll-sync polish | |
| M5 | macOS then Windows packaging | |

Notes for later milestones:
* M4 fills `TranslationRequest.glossary` (the prompt already renders it) and
  adds placeholder protection (⟦n⟧). That also protects swap safety:
  `compose` already refuses translations that would change LaTeX structure.
* Revise-mode prompting is in place (M3). M4 tunes it.
