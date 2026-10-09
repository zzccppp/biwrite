# BiWrite

A desktop editor for writing academic English with a live Chinese reading aid.
Write on the left (plain text, Markdown or LaTeX). The right pane shows a
read-only Chinese translation, one block per paragraph, kept in sync. Only
changed paragraphs are retranslated.

> **Status: M3.** Real LLM providers (OpenAI-compatible: OpenAI, DeepSeek,
> Qwen, Kimi, OpenRouter, local; and Anthropic) with streaming, keys in the
> system keychain, and a settings drawer. The offline **mock** provider
> (reverses text) stays available for trying things out. See
> [ARCHITECTURE.md](ARCHITECTURE.md).

## Requirements

* Rust ≥ 1.85 (tested with 1.96), Node ≥ 20 (tested with 26), npm
* macOS: Xcode command-line tools

## Run

```sh
npm install
npm run app                     # = tauri dev: Vite dev server + app window
```

Open a file from the command line (path must be absolute in dev mode, since
the app runs from `src-tauri/`):

```sh
npx tauri dev -- -- "$PWD/samples/paper.tex"
```

Or build a debug binary that embeds the frontend:

```sh
npx tauri build --debug --no-bundle
./target/debug/biwrite samples/paper.tex
```

### Set up a provider

1. **Settings** (toolbar, or `⌘,`) → *Add provider…* → pick a preset.
2. Check the base URL and model. *List models* fetches the available models
   once a key is saved.
3. Paste the API key and click *Add provider*. The key goes into the macOS
   Keychain; BiWrite never displays it again.
4. *Test* translates one sentence. Select the radio button to make the
   provider active.
5. Optional: set a note for the open document (e.g. "ML paper on in-context
   learning in graph models"), the number of parallel requests, and the
   system prompts (EN → 中 and 中 → EN). The prompts are files you can also
   edit directly (*Show files*).

Changing provider or prompt doesn't invalidate cached translations; use
*Retranslate all* to refresh them.

### Things to try

* Type in a paragraph: its block dims ("edited"). 800 ms after you stop typing,
  only that paragraph is retranslated (streamed). The status bar counts
  requests.
* Undo (`⌘Z`): the old translation comes back from the cache with no new
  request.
* Cut a paragraph and paste it elsewhere: no request.
* Click a Chinese block: the cursor jumps to that paragraph and the panes stay
  aligned. Scroll either pane and the other follows, anchored on segments.
* Open `samples/paper.tex`: the preamble, math, tables, comments and
  `\maketitle`-style lines are collapsed (not translated); captions are.
* **`EN ⇄ 中`** (toolbar centre): swap languages. You now edit the Chinese and
  the English follows on the right. Only paragraphs you change get new English
  (revised from your original); everything else keeps your exact wording. Save
  still writes the English file. Swap back to continue in English. A swap
  needs every paragraph translated first.
* Toolbar: Open (`⌘O`), Save (`⌘S`, `⌘⇧S` = Save As), mode (Plain / Markdown /
  LaTeX, auto-detected from the extension), retranslate segment / all,
  pause auto-translate, theme (auto / light / dark).

The translation cache persists in
`~/Library/Application Support/app.biwrite.desktop/cache.sqlite3`. Delete it to
start fresh.

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run check                   # svelte-check, fails on warnings
npm run build                   # frontend bundle
```
