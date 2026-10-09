<script lang="ts">
  import "katex/dist/katex.min.css";
  import { type MessageKey, t } from "../i18n.svelte";
  import { type Macros, renderToHtml } from "../math";
  import type { Mode, SegmentLayout, SegmentState, SkipReason } from "../types";

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

  const statusLabel: Record<typeof status, MessageKey> = {
    translated: "segment.status.translated",
    edited: "segment.status.edited",
    queued: "segment.status.queued",
    stale: "segment.status.stale",
    translating: "segment.status.translating",
    error: "segment.status.error",
    skipped: "segment.status.skipped",
  };

  const skipLabel: Record<SkipReason, MessageKey> = {
    front_matter: "skip.front_matter",
    code: "skip.code",
    rule: "skip.rule",
    preamble: "skip.preamble",
    comment: "skip.comment",
    math: "skip.math",
    table: "skip.table",
    float: "skip.float",
    markup: "skip.markup",
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
  title={t(statusLabel[status])}
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
          aria-label={expanded ? t("segment.collapseEquation") : t("segment.showEquation")}
          title={expanded ? t("segment.collapse") : t("segment.showEquationTitle")}
          onclick={(e) => (e.stopPropagation(), ontoggle(segment.id))}>▸</button
        >
      {/if}
      <span class="skip-label smallcaps">{t(skipLabel[kind.reason])}</span>
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
        data-label={kind.type === "caption" ? t("segment.caption") : undefined}
        lang={lang === "en" ? "en" : "zh-CN"}
      >
        {@html html}{#if streaming}<span class="caret" aria-hidden="true"></span>{/if}
      </div>
    {:else if status === "error"}
      <div class="zh empty">—</div>
    {:else}
      <div class="skeleton" aria-label={t("segment.waiting")}>
        <span style="width: 92%"></span><span style="width: 74%"></span>
      </div>
    {/if}
    {#if status === "error"}
      <div class="error">
        <span class="smallcaps">{t("segment.failed")}</span>
        <span class="error-msg">{state?.error ?? t("segment.unknownError")}</span>
        <button class="retry smallcaps" onclick={(e) => (e.stopPropagation(), onretry(segment.id))}>
          {t("segment.retry")}
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
    content: attr(data-label);
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

  /* Markdown and LaTeX text markup (format.ts). */
  .zh :global(strong) {
    font-weight: 700;
  }
  /* Songti's bold barely differs from its regular weight: Chinese bold text
     is set in a bold sans (黑体), as in Chinese typesetting. */
  .zh:not(.en) :global(strong) {
    font-family: "PingFang SC", "Hiragino Sans GB", "Source Han Sans SC", "Noto Sans CJK SC", "Microsoft YaHei",
      var(--font-zh);
    font-weight: 600;
  }
  .zh :global(del) {
    color: var(--muted);
  }
  .zh :global(code) {
    font-family: var(--font-mono);
    font-size: 0.86em;
    background: var(--paper-2);
    border: 1px solid var(--rule);
    border-radius: 3px;
    padding: 0 3px;
  }
  .zh :global(.fmt-link) {
    color: var(--hl-link);
    text-decoration: underline;
    text-decoration-color: var(--accent-soft);
    text-underline-offset: 2px;
  }
  .zh :global(.fmt-image)::before {
    content: "▣ ";
    color: var(--faint);
  }
  .zh :global(.fmt-sc) {
    font-variant-caps: small-caps;
  }
  .zh :global(.fmt-note) {
    font-size: 0.85em;
    color: var(--muted);
  }
  .zh :global(.fmt-note)::before {
    content: "〔注 ";
  }
  .zh :global(.fmt-note)::after {
    content: "〕";
  }
  .zh.en :global(.fmt-note)::before {
    content: "[note: ";
  }
  .zh.en :global(.fmt-note)::after {
    content: "]";
  }
  .zh :global(.fmt-cite),
  .zh :global(.fmt-ref) {
    font-family: var(--font-prose);
    font-size: 0.9em;
    color: var(--hl-command);
  }
  .zh :global(.fmt-li),
  .zh :global(.fmt-quote) {
    display: block;
  }
  .zh :global(.fmt-li) {
    position: relative;
    padding-left: 1.4em;
  }
  .zh :global(.fmt-li.fmt-d1) {
    margin-left: 1.4em;
  }
  .zh :global(.fmt-li.fmt-d2),
  .zh :global(.fmt-li.fmt-d3) {
    margin-left: 2.8em;
  }
  .zh :global(.fmt-marker) {
    position: absolute;
    left: 0;
    color: var(--seal);
  }
  .zh :global(.fmt-quote) {
    padding-left: 0.8em;
    border-left: 3px solid var(--rule-strong);
    color: var(--ink-2);
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
