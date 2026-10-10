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
  protect/               Placeholder protection: mask math, \cite, … as ⟦n⟧
  glossary.rs / csv.rs   Glossary matching per segment; CSV import/export
  bilingual.rs           Bilingual Markdown export
  lang.rs                Direction (en-zh / zh-en), CJK-aware tokens
  pair.rs                Pair a document with its hand-made mirror (alignment),
                         patch changed paragraphs back into the mirror
  assist.rs              Writing assistant prompts, answer parsing, diffs,
                         paragraph and sentence at a position
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
  responses.rs           OpenAI Responses API (streaming, AnyRouter needs)
  keys.rs                Key pools: per-key limit, cooldown, failover
  observe.rs             Request records for the log (no text, no keys)
  openai.rs              OpenAI-compatible chat completions (streaming)
  anthropic.rs           Anthropic Messages API (streaming, raw HTTP)
  http.rs                Client, lazy key, error mapping, redaction, SSE loop
  sse.rs / clean.rs      SSE parser; output cleanup + <think> stream filter
  prompt.rs              System prompt files + tagged user message
  config.rs / models.rs  Provider config, validation, presets; model listing
  tests/                 Against a scripted local HTTP server
crates/biwrite-latex/    LaTeX without Tauri: project root and engine, TeX
                         toolchain, latexmk builds (process group killed on
                         timeout), log parsing, SyncTeX, click location,
                         templates, the Chinese mirror as a document
src-tauri/               Tauri 2 shell: commands, events, dialogs, file IO,
                         keychain (secrets.rs), settings.json (settings.rs),
                         glossary and export commands (glossary_commands.rs)
src/                     Svelte 5 + CodeMirror 6 frontend; lib/math.ts renders
                         math on the right with KaTeX
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
  dispatch time from the current text, with protected spans masked (see
  *Placeholder protection*), prev/next context, the document note, the
  glossary entries the paragraph mentions, and, when the old and new source
  are more than 0.6 similar, a revise basis (old source + old translation;
  not when the old translation came from the mock). Up to `concurrency` jobs run at once.
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
* **Documents.** `Snapshot.document` changes whenever the document is
  replaced (load, `load_known`, swap). Every command that carries the
  editor's text carries this number too, and `AppState::sync_text` refuses
  text for an earlier document (`EngineError::Stale`), so a save or flush
  sent before a swap or an open is never applied to the new document.
  Saves and document changes (open, swap, retarget, pairing, the deferred
  mirror write) take `save_lock` in turn.

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

## Placeholder protection

Text the model must reproduce exactly is replaced by numbered placeholders
before a request and put back afterwards (`core/protect/`).

| Mode | Protected |
|---|---|
| LaTeX | `$…$`, `\(…\)`, `$$…$$`, `\[…\]`; `\cite…`/`\ref`/`\eqref`/`\cref`/`\autoref`/`\pageref`/`\label`/`\url` (with `*` and up to two `[…]` arguments); the URL of `\href{url}`; `\verb|…|`; an unescaped `%` comment to the end of the line |
| Markdown | inline code (any backtick run), `$…$` (Pandoc rules, so "$5 and $10" stays text), `$$…$$`, autolinks `<https://…>`, link destinations `(…)` after `]` |
| Plain | nothing |

In every mode, text already shaped like `⟦n⟧` is protected too, so it can't be
confused with a placeholder. Escapes (`\$`, `\%`, `\\`) are text; math
never spans a blank line, so an unmatched `$` stays text.

* Identical spans share a number. `restore` requires every placeholder back
  exactly as often as it was sent, with no unknown numbers. It tolerates
  spaces inside the brackets (`⟦ 0 ⟧`).
* A restored `%` comment that the model put mid-line gets a line break after
  it, so the following text isn't commented out. In LaTeX, a `%` the model
  wrote itself always means a percent sign (comments only come back through
  placeholders), so `restore` escapes it as `\%`. The right pane shows LaTeX
  prose escapes (`\%`, `\&`, `\$`, …) as plain characters.
* **Revise mode:** the new source is masked first. The previous source is
  masked with the same numbering, and the previous translation with
  `mask_known`, which scans it with the same rules and masks spans whose
  text is already known (so `\%` is never taken for a comment). Only the new
  source's spans are expected back.
* **Streaming:** partials are shown restored; a trailing incomplete `⟦1` is
  hidden.
* **Failure:** a mismatch is retried once immediately (no backoff; usage of
  both attempts is counted). A second mismatch is a non-retryable
  `InvalidResponse` ("the model changed protected text: missing ⟦3⟧
  `\cite{x}`"), which is sticky until the text changes or the user retries.
* Context paragraphs are sent unmasked; the request's `source` is masked and
  `SegMeta.translated_source` keeps the raw text. The mock reverses text
  with placeholders kept whole.
* Composing (swap/save while editing Chinese) still escapes a stray `%` as
  `\%` (translations cached before M4 may have one), but never inside
  protected spans (`\url{…%20…}`, `\verb|…%…|`, math) and never for the
  source paragraph's own comments, each as often as the source has it.

## Glossary

Entries `{ term, translation | null }` (null = keep in English) live in
`settings.json` and apply to every document. The settings drawer links to a
glossary sheet (also on the toolbar) with filter, add/remove, keep-English,
CSV import (merged into the table, reviewed, then saved) and CSV export.

* **Matching (`glossary::relevant`):** run on the masked source, so terms
  inside citation keys or math don't count.
  * EN→ZH matches the term case-insensitively as a whole word (ASCII
    letters or digits on neither side), plurals `-s`/`-es`/`-y→-ies`
    included. CJK next to the term is a boundary.
  * ZH→EN matches the Chinese rendering and sends it as `中文 → term`;
    keep-English entries match their English term.
  * At most 30 entries per request, in order of first mention.
  * `Glossary` indexes entries by their first two words (English) and first
    four characters (Chinese rendering), so a paragraph only checks entries
    that can occur in it. It is built outside the engine lock. At the
    10,000-entry cap, matching 500 paragraphs takes about 10 ms (English)
    and 100 ms (Chinese, all renderings sharing a prefix) in release builds.
* **Cache key:** the `glossary` part is a 63-bit fingerprint of exactly the
  entries sent with the request (0 = none), not a global version. Editing
  the glossary only invalidates paragraphs that mention a changed term, and
  removing a term brings the earlier cached translation back.
* **`Engine::set_glossary`:** segments whose translation was made with
  different entries (`SegMeta.glossary_fp`) lose `translated_hash` and are
  redone (respecting pause), as a minimal revision of the old translation.
  In-flight requests with outdated entries are cancelled and requeued. The
  user's own text from a swap (`exact`) is never retranslated.
* **CSV:** RFC 4180 (quotes, `""`, commas and line breaks in quotes), header
  `term,translation` optional, UTF-8 with or without BOM; non-UTF-8 files
  (e.g. GBK from Excel) get a "save as CSV UTF-8" error. An empty
  translation or `KEEP` means keep in English. Export writes a BOM and CRLF
  so Excel opens it correctly. Duplicate terms (case-insensitive) merge;
  the later entry wins. At most 10,000 entries and 2 MB per CSV; parsing runs
  off the async runtime. Saving persists first, then updates memory and the
  engine under the settings lock, so they can't disagree. The sheet renders
  at most 300 rows at a time (the filter finds the rest) and locks the rows
  while a save or import is running.

## Bilingual export

*Export* writes Markdown: every translatable segment's source, a blank line,
then its translation in the same markup (`# 引言`, `\section{引言}`), with
machine output shaped as for a swap. Skipped blocks are copied verbatim.
Paragraphs without an up-to-date translation get *（尚未翻译）* (or *(not
translated yet)*), and the status bar says how many. English always comes
first, so exporting before and after a swap gives the same file. The path is
chosen in a native Save dialog (default `<name>.bilingual.md` next to the
document).

## Cache

SQLite at `<app data>/cache.sqlite3` (macOS:
`~/Library/Application Support/app.biwrite.desktop/`), WAL mode, with a 250 ms
busy timeout because lookups run under the engine lock. Key:
`(hash, direction, provider, model, glossary fingerprint)`, shared across files
and sessions. (The SQL column is still called `glossary_version`; old rows
have 0, which is "no glossary entries".) Direction is part of the key because
the same text can be a source in either direction. If the database can't be
opened the app falls back to an in-memory cache.

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
   joined, and in LaTeX an unescaped `%` becomes `\%` unless it starts one
   of the source's own (protected) comments.
3. The file is always English. While editing Chinese, Save writes
   `compose_target()`, the English composed from the right pane. Swapping back
   without edits restores the original byte for byte (tested on both samples).
4. Retranslate (segment and all) is disabled while editing Chinese, because it
   would replace the user's English with machine translation (Rust refuses it
   too, for paragraphs whose translation is exact). The editor is read-only
   during a swap, and save, export, compile, continue and mode changes wait
   for it. Opening a file always returns to editing English.
5. Swapping back to the file's own language never keeps untranslated
   paragraphs (`keep` is ignored that way round): they would be saved into
   the file in the other language. The swap waits for every translation.

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
  provider is selected, from scratch (it is never sent as a translation to
  revise). Real translations are kept across real providers.
* **Usage:** every provider request is counted as soon as it returns, so a
  rejected answer still counts if its retry is then cancelled.

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
  writes are ignored. Panes re-align on resize (ResizeObserver on the editor
  scroller, the right pane and its content). While the user types or moves
  the cursor, the active block is also kept visible on the right with the
  smallest extra scroll; a wheel or scrollbar scroll turns that off until the
  cursor moves again.
* **Math rendering** (`lib/math.ts`): translated paragraphs, headings and
  captions are split into prose and math (LaTeX: `$…$`, `\(…\)`, `$$…$$`,
  `\[…\]` and display environments; Markdown: `$…$` with Pandoc rules and
  `$$…$$`; Plain: none) and math is typeset with KaTeX (bundled, fonts are
  local assets). Macros come from the preamble's `\newcommand`, `\def` and
  `\DeclareMathOperator`, re-parsed only when the preamble changes.
  `\label` is stripped; numbered environments render unnumbered. A KaTeX
  error falls back to the escaped source. Collapsed math blocks have a ▸
  toggle that typesets the equation; the expanded state survives a swap.
* **Text formatting** (`lib/format.ts`), deliberately small; anything else
  stays visible as escaped source:
  * Markdown: `**bold**`/`__bold__`, `*italic*`/`_italic_` (not inside
    words), `~~strike~~`, inline code, links and images (text underlined,
    URL as tooltip, never clickable, since an `<a href>` would navigate the
    app window), autolinks, `\`-escapes, list items (`-`/`*`/`+`/`1.`, up
    to three nesting levels) and `>` quotes as block lines.
  * LaTeX: `\textbf`, `\textit`/`\emph`/`\textsl`, `\texttt`,
    `\underline`, `\textsc`, `\footnote` (inline, muted), citations as
    `[keys]`, references as their label (`\eqref` in parentheses), `\label`
    hidden, `\url`/`\href` as links, ` ``…'' ` quotes, `--`/`---` dashes,
    `~`, `\\`, escaped characters; comments hidden and bare braces dropped as
    in the PDF; unknown commands shown as written.
  * Math is swapped for private-use stand-ins while formatting, so
    `**$x$ 很大**` and `\textbf{$x$ …}` work; stand-ins never go into
    attributes. Chinese bold uses a bold sans (Songti's bold is too weak).
* Clicking a block moves the cursor to the segment start and scrolls the editor
  so the segment sits at the same height as the clicked block.

## Security invariants

* All file IO and (from M3) network calls and API keys live in Rust. File
  paths are chosen in native dialogs on the Rust side; the webview cannot name
  a path to read or write.
* IPC payloads carry document text, segment layout and states, usage counters,
  file names and glossary entries only (`events.rs`, `commands.rs`,
  `glossary_commands.rs`). Glossary CSVs and exports are also picked in Rust
  dialogs; CSV reads are capped at 2 MB.
* `{@html}` only ever receives HTML-escaped prose, the formatter's fixed
  tags (`strong`, `em`, `del`, `code`, `u`, `span` with a class and an
  escaped `title`) and KaTeX output rendered with `trust: false` (no `\href`,
  `\url`, `\includegraphics` or HTML extensions). Nothing is clickable.

## Logs and cache maintenance

* **Logs** (`src-tauri/src/logging.rs`, on the `log` facade): one file per
  local day, `biwrite-YYYY-MM-DD.log`, in the app log folder (macOS
  `~/Library/Logs/app.biwrite.desktop/`, Windows
  `%LOCALAPPDATA%\app.biwrite.desktop\logs\`). At most 5 MB a day, then one
  line saying the rest of the day is dropped. Files older than 7 days are
  deleted at startup and when the day changes; only files named like ours
  are touched. BiWrite's messages from `info`, other crates' from `warn`.
  Logged: startup, files opened/saved/exported (names only), provider
  switches, glossary changes, cache clearing, notices (retries, cache
  problems), failed paragraphs, every error a command returns to the UI,
  and panics with a backtrace. No document text beyond what error
  messages quote (e.g. a placeholder error names the formula it lost);
  every line goes through the key redaction again. The logger is installed first thing in
  `run()` (stderr until the log folder is known), so failures before setup
  are reported too. Timestamps and the day use the local offset, read per
  line. Settings → *Show logs* opens the folder.
* **Translation cache:** Settings shows the number of entries, the size,
  and a breakdown by provider (configured name, or the API host), model
  and direction. *Clear cache…* (confirmed inline) deletes every entry,
  `VACUUM`s and truncates the WAL. Translations already on screen stay.
* Capability: `core:default` only. CSP restricts scripts to `'self'`.

## File round-trip

`TextFile::decode` accepts UTF-8 (with or without BOM), normalizes line endings
to `\n` for the editor, and remembers the original bytes. `encode(text)` returns
the original bytes when `text` is unchanged (byte-identical save, even with
mixed line endings). Otherwise it restores the BOM and the dominant line ending.
Saves are atomic: a temp file in the same directory, fsync, rename. Symlinks
resolve to their target and permissions are preserved.

## Version 0.2

* **LaTeX** (`biwrite-latex`, `src-tauri/src/latex_commands.rs`,
  `latex_sync.rs`). The English PDF is built from the files on disk with
  latexmk (`-f`, so a document with errors still gets its PDF). The Chinese
  PDF is the composed translation (`Engine::compose_mirror`, paragraphs not
  translated yet stay English) with `ctex` added on the `\begin{document}`
  line, so its lines are the composed text's lines, built into
  `.biwrite/zh/`. SyncTeX runs through the `synctex` tool. A PDF line maps
  to the editor through a line diff in the same language and paragraph by
  paragraph across languages. A click's words pick the spot near the
  SyncTeX line (`locate.rs`).
  * The root's name comes from the project (`% !TEX root`, file names), and
    latexmk runs the engine through the shell, so `compile` only takes roots
    whose path parts are letters, digits and ` ._-+,()[]@='`, none starting
    with `-` (a file named `-shell-escape` or `` `cmd`.tex `` ran commands).
    On Windows, `NoDefaultCurrentDirectoryInExePath` stops tools being found
    in the project folder. A project's own `latexmkrc` still runs, as in
    other editors, and the PDF pane warns that it did
    (`Compiled::project_rc`): it can run any command.
* **Pairs** (`biwrite-core::pair`, `src-tauri/src/pairing.rs`). Opening a
  file looks for its counterpart by name (`_zh`, `sections_en` and
  `sections_zh`, …). Paragraphs align with a dynamic program over kind and
  shared anchors plus a pass for moved floats. `Engine::load_known` seeds
  the mirror's paragraphs as exact translations. Saving patches only
  changed paragraphs into the mirror, and swapping edits the mirror.
  * The two files must read as two languages (`lang::chinese_of_two`: their
    prose's shares of Chinese differ by 0.15 or more; the one with more is
    Chinese), so a same-language copy is never written into. A file is
    never its own counterpart (`same_file`, through links).
  * The mirror is written only as it was read: if it changed on disk, it is
    not overwritten (`MirrorProblem::ChangedOnDisk`), and swapping onto it
    is refused. A patch must segment as planned (`pair::patch_checked`),
    else nothing is written (`MirrorProblem::Structure`). A new paragraph
    goes next to a paired paragraph or heading it sits next to in the
    document (only blank lines between, other new paragraphs aside), right
    after or before that one's counterpart, with a blank line on both
    sides: so it lands in the same place, inside a list or a wrapper around
    the body alike. New headings, captions, list items and paragraphs with
    no such neighbour are left out and reported. A Save As whose mirror
    can't be written moves the pair anyway (`PairState.exists` false): the
    next save creates it, and the old mirror is never patched again.
  * `PairState.behind` marks a mirror waiting for translations or not
    written; it counts as unsaved (close asks), like a pair's text that
    differs from its file after a swap. The mirror follows the saved
    document: written later (when its translations arrive) only while the
    document is still the saved text, else with the next save
    (`MirrorSaved.deferred`).
  * A pair keeps its mode, and retranslating a paragraph whose translation
    is the mirror's own text is refused. Save As names the mirror where
    opening the new file looks for it.
* **Assistant** (`biwrite-core::assist`, `src-tauri/src/assist_commands.rs`,
  `skills.rs`). Prompts carry the research-builder skill's files for the
  task. Jobs stream in the background as events, the frontend keeps each
  job's target mapped through later edits and applies a revision where its
  text is now.
  * New text (Write, Figure) goes after the whole segment of the cursor or
    the selection's end (`assist::insertion_point`): never inside a
    heading's braces or a math block, after the whole figure or table when
    the cursor is in one, and never after `\end{document}`. On
    accept it is placed at a line end with a blank line on each side
    (`assistText.ts`), and text typed at that spot meanwhile stays before
    it.
  * Accept hands over translations first, then works out positions from
    the document as it is and edits in one go; a second Accept meanwhile
    does nothing. A target that moved (its range collapsed) is found again
    if it occurs once; one that was edited is a conflict to re-apply.
    Edits made while a job starts are mapped onto its target, and its
    events are kept until the job is known. Jobs of a document no longer
    open can't be applied, run again or followed up.
* **Key pools and the request log** (`biwrite-providers::keys`,
  `observe`, `src-tauri/src/request_log.rs`). Keys stay in the keychain
  (primary account plus `#pool`), records hold metadata only.
* **Updates** (`src-tauri/src/updater.rs`). GitHub releases, SHA-256 checked
  downloads, an in-place bundle swap on macOS and the installer on Windows.
* **Interface language** (`src/lib/messages.ts`, `i18n.svelte.ts`). Every
  string has English and Chinese, and `t()` follows the reactive language.

## Version 0.2.2

* **The file's own language** (`FileState::home`). The direction in which
  the editor holds the open file's text: `EnZh` for an English file, `ZhEn`
  for a Chinese one, judged on opening by `biwrite_core::lang::written_in`
  (translatable paragraphs, protected spans masked, Chinese must prevail).
  Saving (`commands::home_text`), both PDFs, the exported `.tex` and SyncTeX
  follow it. While the other language is edited, the file's text is
  composed from the translations. `retarget_language` reads the file as the other
  language, from the file's own text.
* **Fills** (`SegMeta::fill`, `State::fill`, the `segment-fills` event). A
  swap made before every paragraph is translated keeps those paragraphs in
  the file's language and marks them. Each is translated the other way
  round (from the cache when it can, its own batches otherwise, never
  streamed to the right pane) and sent to the editor, which replaces the
  paragraph if it still reads as before. Its translation stays the exact
  original through a seed on the new text's hash.
  `Engine::continue_translation` takes up failed and paused paragraphs and,
  swapped, paragraphs still in the other language: plainly so
  (`lang::plainly_written_in`; English must be nearly free of Chinese,
  since Chinese often carries many English names), and not translated into
  the other language already. `State::fill` refuses a fill whose answer did
  not move into the edited language (`lang::moved_into`) or would replace
  an exact original; that paragraph is translated the usual way instead.
* **PDF clicks** (`latex_commands::checked_point`). The words under a click
  are checked against the line SyncTeX names. When they are not there (a
  line-number ruler drawn over the page), `biwrite_latex::find_words` finds
  them in the document as compiled and the project's other files.
* **Writing and figures** (`Action::Write`, `biwrite_latex::packages`).
  Write inserts new paragraphs after the target in the manner of reference
  texts, which can be files (`load_reference` keeps a `.tex` body without
  comments). Figure requests list the packages the document loads, and the
  packages their code still needs are added before `\begin{document}` on
  insert when the open file holds the preamble.
* **Tray** (`src-tauri/src/tray.rs`). Closing the window hides it when the
  setting is on, the icon's menu shows it or quits (asking about unsaved
  changes), and a second launch on Windows shows the running window
  (single-instance plugin).

## Milestones

| | Scope | Status |
|---|---|---|
| M1 | Scaffold, open/save, panes, mock translator, full diff/queue/UI flow | done |
| M2 | LaTeX segmenter, SQLite cache, tests on a sample paper; language swap | done |
| M3 | OpenAI-compatible + Anthropic providers, keychain, SSE streaming, settings UI | done |
| M4 | Placeholder protection, glossary, revise prompts, scroll-sync polish, KaTeX math on the right, bilingual Markdown export | done |
| M5 | macOS then Windows packaging | done |
| 0.2 | LaTeX PDF with SyncTeX and templates, pairs with a hand-made translation, writing assistant on research-builder, key pools, request log, updates, Chinese interface | done |
| 0.2.2 | The file's own language, early swaps filled in, continue, writing in the manner of a reference paper, figure packages, PDF clicks through rulers, tray | done |

Revise prompting (M4 tuning): a glossary-only change (same source, new
entries) sends just `<previous_translation>` with "revise it minimally so it
follows the glossary"; an edit that only changes whitespace or punctuation
asks for the previous translation with just that change; a source with
placeholders gets an instruction to copy every `⟦n⟧` exactly.
