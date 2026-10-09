<script lang="ts">
  import "katex/dist/katex.min.css";
  import { type Macros, renderToHtml } from "../math";
  import type { Mode, SegmentLayout, SegmentState } from "../types";
  import { SKIP_LABELS } from "../types";

  interface Props {
    segment: SegmentLayout;
    state: SegmentState | undefined;
    number: number;
    active: boolean;
    edited: boolean;
    preview: string;
    /** Language of the text shown in this pane. */
    lang: "zh" | "en";
    /** Document mode: decides which math delimiters are recognised. */
    mode: Mode;
    macros: Macros;
    /** A collapsed math block shown typeset; `source` is its LaTeX. */
    expanded: boolean;
    source: string;
    register: (node: HTMLElement, id: number) => { update(id: number): void; destroy(): void };
    onactivate: (id: number, el: HTMLElement) => void;
    onretry: (id: number) => void;
    ontoggle: (id: number) => void;
  }

  let {
    segment,
    state,
    number,
    active,
    edited,
    preview,
    lang,
    mode,
    macros,
    expanded,
    source,
    register,
    onactivate,
    onretry,
    ontoggle,
  }: Props = $props();

  let el: HTMLElement;

  const kind = $derived(segment.kind);
  const status = $derived(edited && state?.status === "translated" ? "edited" : (state?.status ?? "queued"));
  const pending = $derived(status === "queued" || status === "stale" || status === "edited");
  const streaming = $derived(status === "translating" && !!state?.partial);
  const text = $derived(state?.text?.trim() ?? "");
  const dim = $derived(pending || status === "error" || (status === "translating" && !streaming));
  const headingLevel = $derived(kind.type === "heading" ? Math.min(kind.level, 4) : 0);
  // Escaped prose plus KaTeX output (trust: false): the only HTML ever injected.
  const html = $derived(text ? renderToHtml(text, mode, macros) : "");
  const isMath = $derived(kind.type === "skipped" && kind.reason === "math");
  const mathHtml = $derived(isMath && expanded && source ? renderToHtml(source.trim(), mode, macros) : "");

  const statusLabel: Record<string, string> = {
    translated: "translated",
    edited: "edited — will retranslate",
    queued: "queued",
    stale: "stale — auto-translate paused",
    translating: "translating",
    error: "error",
    skipped: "not translated",
  };

  function onclick(): void {
    // Let users select and copy Chinese text without moving the cursor.
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed && el.contains(selection.anchorNode)) return;
    onactivate(segment.id, el);
  }

  function onkeydown(e: KeyboardEvent): void {
    // Keys on the buttons inside belong to them.
    if (e.target !== el) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onactivate(segment.id, el);
    }
  }
</script>

<div
  bind:this={el}
  use:register={segment.id}
  class="block"
  class:active
  class:dim
  class:skipped={kind.type === "skipped"}
  data-status={status}
  data-level={headingLevel || undefined}
  role="button"
  tabindex="-1"
  title={statusLabel[status]}
  {onclick}
  {onkeydown}
>
  <span class="num" aria-hidden="true">{number}</span>

  {#if kind.type === "skipped"}
    <div class="skip">
      {#if isMath}
        <button
          class="toggle"
          class:open={expanded}
          aria-expanded={expanded}
          aria-label={expanded ? "Collapse equation" : "Show equation"}
          title={expanded ? "Collapse" : "Show the equation"}
          onclick={(e) => (e.stopPropagation(), ontoggle(segment.id))}>▸</button
        >
      {/if}
      <span class="skip-label smallcaps">{SKIP_LABELS[kind.reason]}</span>
      <span class="skip-preview">{preview}</span>
    </div>
    {#if mathHtml}
      <div class="math-block">{@html mathHtml}</div>
    {/if}
  {:else}
    {#if text}
      <div
        class="zh"
        class:en={lang === "en"}
        class:heading={headingLevel > 0}
        class:caption={kind.type === "caption"}
        lang={lang === "en" ? "en" : "zh-CN"}
      >
        {@html html}{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}
      </div>
    {:else if status === "error"}
      <div class="zh empty">—</div>
    {:else}
      <div class="skeleton" aria-label="waiting for translation">
        <span style="width: 92%"></span><span style="width: 74%"></span>
      </div>
    {/if}
    {#if status === "error"}
      <div class="error">
        <span class="smallcaps">failed</span>
        <span class="error-msg">{state?.error ?? "unknown error"}</span>
        <button class="retry smallcaps" onclick={(e) => (e.stopPropagation(), onretry(segment.id))}>
          retry
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .block {
    position: relative;
    padding: 10px 28px 12px 54px;
    margin: 0 0 10px;
    border-left: 2px solid transparent;
    transition:
      background-color 160ms var(--ease),
      border-color 200ms var(--ease);
    cursor: pointer;
  }
  .block:hover {
    background: var(--accent-wash);
  }
  .block.active {
    background: var(--seal-wash);
    border-left-color: var(--seal);
  }

  .num {
    position: absolute;
    left: 14px;
    top: 13px;
    width: 26px;
    text-align: right;
    font-family: var(--font-ui);
    font-size: 11px;
    font-variant-numeric: oldstyle-nums;
    color: var(--faint);
    transition: color 160ms var(--ease);
  }
  .block.active .num {
    color: var(--seal);
  }
  /* Status tick in the margin next to the number. */
  .num::after {
    content: "";
    position: absolute;
    right: -10px;
    top: 5px;
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: transparent;
  }
  [data-status="queued"] .num::after,
  [data-status="stale"] .num::after,
  [data-status="edited"] .num::after {
    background: var(--faint);
  }
  [data-status="translating"] .num::after {
    background: var(--accent);
    animation: pulse 1.1s ease-in-out infinite;
  }
  [data-status="error"] .num::after {
    background: var(--error);
  }

  .zh {
    font-family: var(--font-zh);
    font-size: var(--zh-size);
    line-height: var(--zh-leading);
    color: var(--ink);
    white-space: pre-line;
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
    transition: opacity 220ms var(--ease);
    animation: ink-in 280ms var(--ease);
  }
  .dim .zh {
    opacity: 0.4;
  }
  .zh.empty {
    color: var(--faint);
  }
  .zh.en {
    font-family: var(--font-prose);
    font-size: 15.5px;
    line-height: 1.7;
  }
  .zh.caption {
    font-size: 0.92em;
    color: var(--ink-2);
  }
  .zh.caption::before {
    content: "caption";
    display: block;
    font-family: var(--font-ui);
    font-variant-caps: all-small-caps;
    letter-spacing: 0.06em;
    font-size: 12px;
    color: var(--seal);
  }
  .zh.heading {
    font-weight: 700;
    letter-spacing: 0.02em;
    color: var(--hl-heading);
  }
  [data-level="1"] .zh {
    font-size: 1.45em;
  }
  [data-level="2"] .zh {
    font-size: 1.28em;
  }
  [data-level="3"] .zh {
    font-size: 1.12em;
  }
  [data-level] {
    padding-top: 16px;
  }

  .caret {
    display: inline-block;
    width: 0.5em;
    height: 1em;
    margin-left: 2px;
    vertical-align: -0.12em;
    border-bottom: 2px solid var(--accent);
    animation: blink 0.9s steps(2) infinite;
  }

  [data-status="translating"] {
    border-left-color: var(--accent-soft);
  }
  [data-status="translating"]:not(.active)::before {
    content: "";
    position: absolute;
    left: -2px;
    top: 0;
    width: 2px;
    height: 34%;
    background: var(--accent);
    animation: travel 1.3s var(--ease) infinite;
  }
  [data-status="error"]:not(.active) {
    border-left-color: var(--error);
  }

  .skeleton {
    display: grid;
    gap: 9px;
    padding: 8px 0 4px;
  }
  .skeleton span {
    height: 9px;
    border-radius: 5px;
    background: linear-gradient(90deg, var(--rule) 0%, var(--paper-2) 50%, var(--rule) 100%);
    background-size: 200% 100%;
    animation: shimmer 1.6s linear infinite;
    opacity: 0.8;
  }

  .error {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-top: 6px;
    font-size: 12.5px;
    color: var(--error);
  }
  .error-msg {
    flex: 1;
    font-style: italic;
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .retry {
    border: 1px solid currentColor;
    background: transparent;
    border-radius: 3px;
    padding: 0 8px 1px;
    cursor: pointer;
  }
  .retry:hover {
    background: var(--seal-wash);
  }

  .block.skipped {
    padding-top: 4px;
    padding-bottom: 5px;
    margin-bottom: 8px;
  }
  .skip {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 3px 10px 4px;
    border: 1px dashed var(--rule-strong);
    border-radius: 4px;
    color: var(--muted);
    min-width: 0;
  }
  .skip-label {
    flex: none;
    font-size: 12px;
    color: var(--seal);
  }
  .skip-preview {
    font-family: var(--font-mono);
    font-size: 11.5px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .toggle {
    flex: none;
    align-self: center;
    width: 16px;
    height: 16px;
    margin: 0 -4px 0 -4px;
    padding: 0;
    border: 0;
    border-radius: 3px;
    background: transparent;
    color: var(--seal);
    font-size: 10px;
    line-height: 16px;
    cursor: pointer;
    transition: transform 160ms var(--ease);
  }
  .toggle:hover,
  .toggle:focus-visible {
    background: var(--seal-wash);
  }
  .toggle.open {
    transform: rotate(90deg);
  }
  .math-block {
    margin: 6px 0 2px;
    padding: 2px 10px;
    overflow-x: auto;
    overflow-y: hidden;
    color: var(--ink);
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
    animation: ink-in 280ms var(--ease);
  }
  .math-block :global(.katex-display) {
    margin: 0.5em 0;
  }

  /* Typeset math inside translations. */
  .zh :global(.katex) {
    font-size: 1.08em;
  }
  .zh.en :global(.katex) {
    font-size: 1.12em;
  }
  .zh :global(.katex-display) {
    margin: 0.4em 0;
    overflow-x: auto;
    overflow-y: hidden;
    white-space: normal;
  }
  .zh :global(.math-error),
  .math-block :global(.math-error) {
    font-family: var(--font-mono);
    font-size: 0.85em;
    color: var(--error);
    background: var(--seal-wash);
    border-radius: 3px;
    padding: 0 3px;
    white-space: pre-wrap;
  }

  @keyframes ink-in {
    from {
      opacity: 0;
      filter: blur(1.5px);
    }
  }
  @keyframes blink {
    to {
      visibility: hidden;
    }
  }
  @keyframes pulse {
    50% {
      opacity: 0.25;
    }
  }
  @keyframes travel {
    0% {
      top: 0;
      height: 0;
    }
    50% {
      top: 20%;
      height: 50%;
    }
    100% {
      top: 100%;
      height: 0;
    }
  }
  @keyframes shimmer {
    to {
      background-position: -200% 0;
    }
  }
</style>
