# BiWrite

A desktop editor for writing academic English with a live Chinese reading aid.
Write on the left (plain text, Markdown or LaTeX). The right pane shows a
read-only Chinese translation, one block per paragraph, kept in sync. Only
changed paragraphs are retranslated.

> **Status: M4.** Real LLM providers (OpenAI-compatible: OpenAI, DeepSeek,
> Qwen, Kimi, OpenRouter, local; and Anthropic) with streaming and keys in
> the system keychain. Math, citations and references are protected from
> the model, a glossary steers terminology, math is typeset on the right,
> and the document exports as bilingual Markdown. The offline **mock**
> provider (reverses text) stays available for trying things out. See
> [ARCHITECTURE.md](ARCHITECTURE.md).

## Install

Download the installer from the
[Releases](https://github.com/zzccppp/biwrite/releases) page:
`BiWrite_<version>_x64-setup.exe` (Windows 10/11) or
`BiWrite_<version>_universal.dmg` (macOS 12+, Apple Silicon and Intel).
They aren't signed with a trusted certificate yet: on Windows choose *More
info → Run anyway*; on macOS right-click the app and choose *Open* the first
time (or run `xattr -dr com.apple.quarantine /Applications/BiWrite.app`).

### Releasing

`.github/workflows/build-installers.yml` builds both installers on every
push to `master` (as workflow artifacts). To publish a release, set the
version in `src-tauri/tauri.conf.json`, `package.json` and the workspace
`Cargo.toml`, commit, then push a matching tag:

```sh
git tag -a v0.1.0 -m "BiWrite 0.1.0"
git push origin v0.1.0
```

The workflow checks that the tag matches the app version, builds both
installers and creates the GitHub Release with them attached. A tag with a
suffix (`v0.2.0-beta.1`) is published as a pre-release.

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

### Glossary

**Glossary** (toolbar, or Settings → *Edit glossary…*) holds your preferred
Chinese for terms, or *Keep EN* to leave a term in English. Import a CSV
with the columns `term,translation` (UTF-8; in Excel use *CSV UTF-8*; an
empty translation or `KEEP` means keep in English), review it, then *Save*.
Only paragraphs that mention a changed term are translated again, and each
request carries only the terms its paragraph mentions.

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
  Inline math on the right is typeset (with the paper's `\newcommand`
  macros); click ▸ on a collapsed equation to see it rendered.
* Math, `\cite`, `\ref`, `\label`, URLs and `%` comments reach the model as
  `⟦0⟧`, `⟦1⟧`, … and come back byte for byte. If a model drops one, BiWrite
  retries once, then shows an error on that paragraph instead of a damaged
  translation.
* **Export** (toolbar): bilingual Markdown, each paragraph in English followed
  by its Chinese, next to the document as `<name>.bilingual.md`.
* **`EN ⇄ 中`** (toolbar centre): swap languages. You now edit the Chinese and
  the English follows on the right. Only paragraphs you change get new English
  (revised from your original); everything else keeps your exact wording. Save
  still writes the English file. Swap back to continue in English. A swap
  needs every paragraph translated first.
* Toolbar: Open (`⌘O`), Save (`⌘S`, `⌘⇧S` = Save As), Export, mode (Plain /
  Markdown / LaTeX, auto-detected from the extension), retranslate segment /
  all, pause auto-translate, Glossary, Settings, theme (auto / light / dark).
  In narrower windows the secondary buttons show icons only.

* The right pane formats common Markdown (bold, italic, code, links, lists,
  quotes) and LaTeX text markup (`\textbf`, `\emph`, citations, references,
  footnotes, quotes, dashes) as well as math.

The translation cache persists in
`~/Library/Application Support/app.biwrite.desktop/cache.sqlite3`. Settings →
*Translation cache* shows what it holds and can clear it.

Logs: one file per day in `~/Library/Logs/app.biwrite.desktop/` (Windows:
`%LOCALAPPDATA%\app.biwrite.desktop\logs\`), at most 5 MB a day, kept for 7
days. Settings → *Logs* → *Show logs*.

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
npm run check                   # svelte-check, fails on warnings
npm test                        # frontend unit tests (node --test, math.ts)
npm run build                   # frontend bundle
```
