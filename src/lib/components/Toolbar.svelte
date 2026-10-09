<script lang="ts">
  import type { Session } from "../session.svelte";
  import type { ThemePref } from "../theme";
  import { type MessageKey, t } from "../i18n.svelte";
  import { MODE_LABELS, type Mode, type ProjectView } from "../types";

  interface Props {
    session: Session;
    theme: ThemePref;
    /** The open document's LaTeX project (for switching files). */
    project: ProjectView | null;
    onnew: () => void;
    onfile: (file: string) => void;
    /** Pair the document with its translation in another file. */
    onimportmirror: () => void;
    onclosemirror: () => void;
    /** The swap waits for the remaining translations. */
    swapWaiting: boolean;
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
    onassistant: () => void;
    assistOpen: boolean;
    /** Assistant jobs running and waiting for a decision. */
    assistBusy: number;
    assistReady: number;
  }

  let {
    session,
    theme,
    project,
    onnew,
    onfile,
    onimportmirror,
    onclosemirror,
    swapWaiting,
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
    onassistant,
    assistOpen,
    assistBusy,
    assistReady,
  }: Props = $props();

  const zh = $derived(session.direction === "zh-en");

  const modes: Mode[] = ["plain", "markdown", "latex"];
  const themeLabel: Record<ThemePref, MessageKey> = {
    system: "toolbar.theme.system",
    light: "toolbar.theme.light",
    dark: "toolbar.theme.dark",
  };
  const mod = navigator.platform.toLowerCase().includes("mac") ? "⌘" : "Ctrl+";
</script>

<header class="toolbar">
  <div class="group">
    <button class="tool" onclick={onnew} title={t("toolbar.newTitle")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 2.5h5.5L12 5v8.5H4zM9.5 2.5V5H12M8 7.5v4M6 9.5h4" /></svg>
      <span class="smallcaps">{t("toolbar.new")}</span>
    </button>
    <button class="tool" onclick={onopen} title={t("toolbar.openTitle", { key: `${mod}O` })}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2z" /></svg>
      <span class="smallcaps">{t("toolbar.open")}</span>
    </button>
    <button class="tool" onclick={onsave} title={t("toolbar.saveTitle", { save: `${mod}S`, saveAs: `${mod}⇧S` })}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 2.5h8l2 2v9H3zM5.5 2.5v3h5v-3M5 13.5v-4h6v4" /></svg>
      <span class="smallcaps">{t("common.save")}</span>
    </button>
    <button class="tool secondary" onclick={onexport} title={t("toolbar.exportTitle")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 10V2.5M5.5 5 8 2.5 10.5 5M3 9v4.5h10V9" /></svg>
      <span class="smallcaps">{t("toolbar.export")}</span>
    </button>

    <span class="sep" aria-hidden="true"></span>

    <div class="modes" role="radiogroup" aria-label={t("toolbar.mode")}>
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
      class:waiting={swapWaiting}
      onclick={onswap}
      title={swapWaiting ? t("toolbar.swapWaiting") : zh ? t("toolbar.swapBack") : t("toolbar.swap")}
    >
      <span class="lang" lang={zh ? "zh-CN" : "en"}>{zh ? "中" : "EN"}</span>
      {#if swapWaiting}
        <span class="spin" aria-hidden="true"></span>
      {:else}
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 5.5h10l-2.5-2.5M13.5 10.5h-10l2.5 2.5" /></svg>
      {/if}
      <span class="lang" lang={zh ? "en" : "zh-CN"}>{zh ? "EN" : "中"}</span>
    </button>
    {#if project && project.files.length > 1}
      <label class="files" class:dirty={session.dirty} title={t("toolbar.files")}>
        <span class="name">{project.current}</span>
        <svg class="chev" viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 6.5 8 10l3.5-3.5" /></svg>
        <select
          value={project.current}
          aria-label={t("toolbar.files")}
          onchange={(e) => {
            const next = e.currentTarget.value;
            e.currentTarget.value = project.current;
            if (next !== project.current) onfile(next);
          }}
        >
          {#each project.files as f (f)}<option value={f}>{f === project.root ? t("toolbar.main", { file: f }) : f}</option>{/each}
        </select>
      </label>
    {:else}
      <span class="name" class:dirty={session.dirty}>{session.path ? session.name : t("doc.untitled")}</span>
    {/if}
    {#if session.pair}
      <span class="pair" title={t("pair.title", { name: session.pair.name, paired: session.pair.paired, units: session.pair.units })}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6.5 9.5a2.5 2.5 0 0 0 3.5 0l2.5-2.5a2.5 2.5 0 0 0-3.5-3.5l-.8.8M9.5 6.5a2.5 2.5 0 0 0-3.5 0L3.5 9a2.5 2.5 0 0 0 3.5 3.5l.8-.8" /></svg>
        <span class="pair-name">{session.pair.name}</span>
        <button class="unpair" onclick={onclosemirror} aria-label={t("pair.close")} title={t("pair.close")}>✕</button>
      </span>
    {:else if session.path}
      <button class="tool secondary import" onclick={onimportmirror} title={t("pair.importTitle")}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6.5 9.5a2.5 2.5 0 0 0 3.5 0l2.5-2.5a2.5 2.5 0 0 0-3.5-3.5l-.8.8M9.5 6.5a2.5 2.5 0 0 0-3.5 0L3.5 9a2.5 2.5 0 0 0 3.5 3.5l.8-.8" /></svg>
        <span class="smallcaps">{t("pair.import")}</span>
      </button>
    {/if}
  </div>

  <div class="group">
    <button
      class="tool secondary"
      onclick={onretranslate}
      disabled={session.activeId === null || zh}
      title={zh ? t("toolbar.retranslateOff") : t("toolbar.segmentTitle")}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v2.5h-2.5" /></svg>
      <span class="smallcaps">{t("toolbar.segment")}</span>
    </button>
    <button
      class="tool secondary"
      onclick={onretranslateall}
      disabled={zh}
      title={zh ? t("toolbar.retranslateOff") : t("toolbar.allTitle")}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v2.5h-2.5M6 8h4M8 6v4" /></svg
      >
      <span class="smallcaps">{t("toolbar.all")}</span>
    </button>
    <button
      class="tool"
      class:paused={!session.autoTranslate}
      onclick={ontoggleauto}
      aria-pressed={!session.autoTranslate}
      title={session.autoTranslate ? t("toolbar.pauseTitle") : t("toolbar.resumeTitle")}
    >
      {#if session.autoTranslate}
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5.5 3.5v9M10.5 3.5v9" /></svg>
        <span class="smallcaps">{t("toolbar.pause")}</span>
      {:else}
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3l8 5-8 5z" /></svg>
        <span class="smallcaps">{t("toolbar.resume")}</span>
      {/if}
    </button>

    <span class="sep" aria-hidden="true"></span>

    <button
      class="tool assistant"
      class:on={assistOpen}
      onclick={onassistant}
      aria-pressed={assistOpen}
      title={t("toolbar.assistantTitle")}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M3 12.5 9.8 5.7M8.6 4.5l1.2-1.2 2.9 2.9-1.2 1.2zM3 12.5l-.5 1 1-.5M11.5 10.5l.6 1.4 1.4.6-1.4.6-.6 1.4-.6-1.4-1.4-.6 1.4-.6z" /></svg
      >
      <span class="smallcaps">{t("toolbar.assistant")}</span>
      {#if assistReady > 0}<span class="badge ready">{assistReady}</span>{:else if assistBusy > 0}<span class="badge"
          >{assistBusy}</span
        >{/if}
    </button>

    <span class="sep" aria-hidden="true"></span>

    <button class="tool secondary" onclick={onglossary} title={t("toolbar.glossaryTitle")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M3 2.8h7.5a1.5 1.5 0 0 1 1.5 1.5v9H4.5A1.5 1.5 0 0 1 3 11.8zM3 11.8a1.5 1.5 0 0 1 1.5-1.5H12M6 5.5h3.5" /></svg
      >
      <span class="smallcaps">{t("glossary.title")}</span>
    </button>
    <button class="tool" onclick={onsettings} title={t("toolbar.settingsTitle", { key: `${mod},` })}>
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><circle cx="8" cy="8" r="2.2" /><path
          d="M8 1.8v1.6M8 12.6v1.6M14.2 8h-1.6M3.4 8H1.8M12.4 3.6l-1.1 1.1M4.7 11.3l-1.1 1.1M12.4 12.4l-1.1-1.1M4.7 4.7 3.6 3.6"
        /></svg
      >
      <span class="smallcaps">{t("settings.title")}</span>
    </button>
    <button class="tool secondary" onclick={ontheme} title={t("toolbar.themeTitle", { theme: t(themeLabel[theme]) })}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="5" /><path d="M8 3a5 5 0 0 0 0 10z" class="fill" /></svg>
      <span class="smallcaps">{t(themeLabel[theme])}</span>
    </button>
    <button
      class="tool secondary log"
      onclick={onlog}
      title={t("toolbar.logTitle", { key: `${mod}⇧L` })}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M3 3.5h10M3 6.5h10M3 9.5h6M3 12.5h4M11.5 9.5v4M9.5 11.5h4" /></svg
      >
      <span class="smallcaps">{t("toolbar.log")}</span>
      {#if inflight > 0}<span class="badge" aria-label={t("toolbar.inFlight", { n: inflight })}>{inflight}</span>{/if}
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
  .files {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    cursor: pointer;
  }
  .files .name {
    pointer-events: none;
  }
  .files.dirty .name::before {
    content: "●";
    margin-right: 7px;
    font-style: normal;
    font-size: 9px;
    vertical-align: 2px;
    color: var(--seal);
  }
  .files .chev {
    flex: none;
    width: 12px;
    height: 12px;
    color: var(--muted);
  }
  .files select {
    position: absolute;
    inset: 0;
    opacity: 0;
    cursor: pointer;
    font: inherit;
  }
  .swap.waiting {
    border-style: dashed;
  }
  .spin {
    width: 10px;
    height: 10px;
    border: 1.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .pair {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    max-width: 220px;
    padding: 1px 4px 1px 7px;
    border: 1px solid var(--rule-strong);
    border-radius: 11px;
    font-size: 12.5px;
    color: var(--ink-2);
    background: var(--paper);
  }
  .pair svg {
    flex: none;
    width: 12px;
    height: 12px;
  }
  .pair-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .unpair {
    border: 0;
    background: transparent;
    color: var(--faint);
    font-size: 10px;
    cursor: pointer;
    padding: 0 2px;
  }
  .unpair:hover {
    color: var(--error);
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
  .tool.assistant.on {
    color: var(--accent);
    background: var(--accent-wash);
  }
  .badge.ready {
    background: var(--seal);
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
  @media (max-width: 1440px) {
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
