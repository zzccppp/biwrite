<script lang="ts">
  import type { Session } from "../session.svelte";
  import type { ThemePref } from "../theme";
  import { MODE_LABELS, type Mode } from "../types";

  interface Props {
    session: Session;
    theme: ThemePref;
    onopen: () => void;
    onsave: () => void;
    onexport: () => void;
    onmode: (mode: Mode) => void;
    onretranslate: () => void;
    onretranslateall: () => void;
    ontoggleauto: () => void;
    ontheme: () => void;
    onswap: () => void;
    onglossary: () => void;
    onsettings: () => void;
    onlog: () => void;
    /** Model requests in flight (badge on the Log button). */
    inflight: number;
  }

  let {
    session,
    theme,
    onopen,
    onsave,
    onexport,
    onmode,
    onretranslate,
    onretranslateall,
    ontoggleauto,
    ontheme,
    onswap,
    onglossary,
    onsettings,
    onlog,
    inflight,
  }: Props = $props();

  const zh = $derived(session.direction === "zh-en");

  const modes: Mode[] = ["plain", "markdown", "latex"];
  const themeLabel: Record<ThemePref, string> = { system: "Auto", light: "Light", dark: "Dark" };
  const mod = navigator.platform.toLowerCase().includes("mac") ? "⌘" : "Ctrl+";
</script>

<header class="toolbar">
  <div class="group">
    <button class="tool" onclick={onopen} title="Open… ({mod}O)">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2z" /></svg>
      <span class="smallcaps">Open</span>
    </button>
    <button class="tool" onclick={onsave} title="Save ({mod}S) · Save As ({mod}⇧S)">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 2.5h8l2 2v9H3zM5.5 2.5v3h5v-3M5 13.5v-4h6v4" /></svg>
      <span class="smallcaps">Save</span>
    </button>
    <button class="tool secondary" onclick={onexport} title="Export bilingual Markdown: each paragraph in English, then Chinese">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 10V2.5M5.5 5 8 2.5 10.5 5M3 9v4.5h10V9" /></svg>
      <span class="smallcaps">Export</span>
    </button>

    <span class="sep" aria-hidden="true"></span>

    <div class="modes" role="radiogroup" aria-label="Document mode">
      {#each modes as m (m)}
        <button
          class="mode smallcaps"
          class:on={session.mode === m}
          role="radio"
          aria-checked={session.mode === m}
          onclick={() => onmode(m)}>{MODE_LABELS[m]}</button
        >
      {/each}
    </div>
  </div>

  <div class="title">
    <button
      class="swap"
      class:zh
      onclick={onswap}
      title={zh
        ? "Editing Chinese; English (saved to the file) follows on the right. Click to edit English again."
        : "Swap: edit the Chinese and let the English follow. Unchanged paragraphs keep your exact English."}
    >
      <span class="lang" lang={zh ? "zh-CN" : "en"}>{zh ? "中" : "EN"}</span>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 5.5h10l-2.5-2.5M13.5 10.5h-10l2.5 2.5" /></svg>
      <span class="lang" lang={zh ? "en" : "zh-CN"}>{zh ? "EN" : "中"}</span>
    </button>
    <span class="name" class:dirty={session.dirty}>{session.name}</span>
  </div>

  <div class="group">
    <button
      class="tool secondary"
      onclick={onretranslate}
      disabled={session.activeId === null || zh}
      title={zh
        ? "Disabled while editing Chinese: it would replace your English with machine translation"
        : "Retranslate the segment at the cursor"}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v2.5h-2.5" /></svg>
      <span class="smallcaps">Segment</span>
    </button>
    <button
      class="tool secondary"
      onclick={onretranslateall}
      disabled={zh}
      title={zh
        ? "Disabled while editing Chinese: it would replace your English with machine translation"
        : "Retranslate every segment"}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v2.5h-2.5M6 8h4M8 6v4" /></svg
      >
      <span class="smallcaps">All</span>
    </button>
    <button
      class="tool"
      class:paused={!session.autoTranslate}
      onclick={ontoggleauto}
      aria-pressed={!session.autoTranslate}
      title={session.autoTranslate ? "Pause auto-translate" : "Resume auto-translate"}
    >
      {#if session.autoTranslate}
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5.5 3.5v9M10.5 3.5v9" /></svg>
        <span class="smallcaps">Pause</span>
      {:else}
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3l8 5-8 5z" /></svg>
        <span class="smallcaps">Resume</span>
      {/if}
    </button>

    <span class="sep" aria-hidden="true"></span>

    <button class="tool secondary" onclick={onglossary} title="Glossary: preferred translations of terms">
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M3 2.8h7.5a1.5 1.5 0 0 1 1.5 1.5v9H4.5A1.5 1.5 0 0 1 3 11.8zM3 11.8a1.5 1.5 0 0 1 1.5-1.5H12M6 5.5h3.5" /></svg
      >
      <span class="smallcaps">Glossary</span>
    </button>
    <button class="tool" onclick={onsettings} title="Settings ({mod},)">
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><circle cx="8" cy="8" r="2.2" /><path
          d="M8 1.8v1.6M8 12.6v1.6M14.2 8h-1.6M3.4 8H1.8M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1M12.4 12.4l-1.1-1.1M4.7 4.7 3.6 3.6"
        /></svg
      >
      <span class="smallcaps">Settings</span>
    </button>
    <button class="tool secondary" onclick={ontheme} title="Theme: {themeLabel[theme]}">
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="5" /><path d="M8 3a5 5 0 0 0 0 10z" class="fill" /></svg>
      <span class="smallcaps">{themeLabel[theme]}</span>
    </button>
    <button
      class="tool log"
      onclick={onlog}
      title="Request log: model, reasoning effort and service tier of every request, as sent and as declared ({mod}⇧L)"
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M3 3.5h10M3 6.5h10M3 9.5h6M3 12.5h4M11.5 9.5v4M9.5 11.5h4" /></svg
      >
      <span class="smallcaps">Log</span>
      {#if inflight > 0}<span class="badge" aria-label="{inflight} in flight">{inflight}</span>{/if}
    </button>
  </div>
</header>

<style>
  .toolbar {
    display: grid;
    /* The button groups never shrink; the file name gives way (ellipsis). */
    grid-template-columns: minmax(max-content, 1fr) minmax(96px, auto) minmax(max-content, 1fr);
    align-items: center;
    gap: 12px;
    height: 44px;
    padding: 0 12px;
    background: var(--chrome);
    border-bottom: 1px solid var(--rule);
  }
  .group {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
  }
  .group:last-child {
    justify-content: flex-end;
  }
  .title {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    min-width: 0;
  }
  .swap {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 26px;
    padding: 0 10px 1px;
    border: 1px solid var(--rule-strong);
    border-radius: 13px;
    background: var(--paper);
    color: var(--ink-2);
    cursor: pointer;
    transition:
      background-color 150ms var(--ease),
      border-color 150ms var(--ease),
      color 150ms var(--ease);
  }
  .swap:hover {
    border-color: var(--seal);
    color: var(--seal);
  }
  .swap.zh {
    background: var(--seal-wash);
    border-color: var(--seal);
    color: var(--seal);
  }
  .swap .lang {
    font-size: 12.5px;
    letter-spacing: 0.04em;
    min-width: 1.2em;
    text-align: center;
  }
  .swap .lang:first-child {
    font-weight: 700;
  }
  .swap .lang:lang(zh-CN) {
    font-family: var(--font-zh);
  }
  .swap svg {
    width: 14px;
    height: 14px;
  }
  .name {
    font-style: italic;
    font-size: 15px;
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name.dirty::before {
    content: "●";
    margin-right: 7px;
    font-style: normal;
    font-size: 9px;
    vertical-align: 2px;
    color: var(--seal);
  }

  .tool {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 9px 1px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--ink-2);
    font-size: 14px;
    cursor: pointer;
    transition:
      background-color 120ms var(--ease),
      color 120ms var(--ease);
  }
  .tool:hover:not(:disabled) {
    background: var(--accent-wash);
    color: var(--accent);
  }
  .tool:active:not(:disabled) {
    transform: translateY(0.5px);
  }
  .tool:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .tool.paused {
    color: var(--seal);
    background: var(--seal-wash);
  }
  .tool.log {
    position: relative;
  }
  .badge {
    min-width: 15px;
    height: 15px;
    padding: 0 4px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--paper);
    font-family: var(--font-mono);
    font-size: 10px;
    line-height: 15px;
    text-align: center;
  }
  svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  svg .fill {
    fill: currentColor;
    stroke: none;
  }

  .modes {
    display: inline-flex;
    padding: 2px;
    border: 1px solid var(--rule);
    border-radius: 5px;
    background: var(--paper);
  }
  .mode {
    border: 0;
    background: transparent;
    padding: 1px 9px 3px;
    border-radius: 3px;
    font-size: 13.5px;
    color: var(--muted);
    cursor: pointer;
  }
  .mode:hover {
    color: var(--ink);
  }
  .mode.on {
    background: var(--ink);
    color: var(--paper);
  }

  .sep {
    width: 1px;
    height: 18px;
    margin: 0 6px;
    background: var(--rule);
  }

  /* Narrower windows: icons only, secondary actions first. */
  @media (max-width: 1240px) {
    .tool.secondary span {
      display: none;
    }
  }
  @media (max-width: 980px) {
    .tool span {
      display: none;
    }
  }
</style>
