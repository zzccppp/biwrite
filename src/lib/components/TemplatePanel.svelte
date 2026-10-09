<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { errorMessage, latexIpc } from "../ipc";
  import type { SessionView, TemplateView } from "../types";

  interface Props {
    /** The open document is a LaTeX project that can be exported. */
    canExport: boolean;
    /** A document was opened (a new paper or a project folder). */
    onopened: (view: SessionView) => void;
    onclose: () => void;
  }

  let { canExport, onopened, onclose }: Props = $props();

  let templates = $state<TemplateView[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let note = $state<string | null>(null);
  let confirmDelete = $state<string | null>(null);

  const builtin = $derived(templates.filter((x) => x.builtin));
  const mine = $derived(templates.filter((x) => !x.builtin));

  function size(bytes: number): string {
    if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }

  function host(url: string | undefined): string {
    if (!url) return "";
    try {
      const u = new URL(url);
      return `${u.host}${u.pathname}`.replace(/\/$/, "");
    } catch {
      return url;
    }
  }

  async function run(f: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    note = null;
    try {
      await f();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }

  async function reload(): Promise<void> {
    templates = await latexIpc.templates();
  }

  function create(id: string): Promise<void> {
    return run(async () => {
      const view = await latexIpc.newPaper(id);
      if (view) onopened(view);
    });
  }

  function openFolder(): Promise<void> {
    return run(async () => {
      const view = await latexIpc.openFolder();
      if (view) onopened(view);
    });
  }

  function importTemplate(zip: boolean): Promise<void> {
    return run(async () => {
      const added = await latexIpc.importTemplate(zip);
      if (added) {
        await reload();
        note = t("templates.imported", { name: added.name });
      }
    });
  }

  function exportProject(): Promise<void> {
    return run(async () => {
      const path = await latexIpc.exportTemplate();
      if (path) note = t("templates.exported", { path });
    });
  }

  function remove(id: string): Promise<void> {
    if (confirmDelete !== id) {
      confirmDelete = id;
      return Promise.resolve();
    }
    confirmDelete = null;
    return run(async () => {
      await latexIpc.deleteTemplate(id);
      await reload();
    });
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.stopPropagation();
      onclose();
    }
  }

  onMount(() => {
    void run(reload);
  });
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet card(x: TemplateView)}
  <li class="card">
    <div class="head">
      <h3>{x.name}</h3>
      {#if x.engine}<span class="engine">{x.engine}</span>{/if}
    </div>
    {#if x.description}<p class="desc">{x.description}</p>{/if}
    <p class="meta">
      {t("templates.size", { n: x.files, size: size(x.bytes) })} · <span class="mono">{x.main}</span>
      {#if x.source}<br /><span class="faint">{t("templates.source")} · {host(x.source)}</span>{/if}
    </p>
    <div class="row">
      <button class="btn small primary" onclick={() => create(x.id)} disabled={busy}>{t("templates.create")}</button>
      {#if !x.builtin}
        <span class="spacer"></span>
        <button class="btn small" class:danger={confirmDelete === x.id} onclick={() => remove(x.id)} disabled={busy}
          >{confirmDelete === x.id ? t("templates.confirmDelete") : t("templates.delete")}</button
        >
      {/if}
    </div>
  </li>
{/snippet}

<div class="scrim" role="presentation" onclick={onclose}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-label={t("templates.title")}>
  <header>
    <h2>{t("templates.title")}</h2>
    <span class="spacer"></span>
    <button class="close" onclick={onclose} aria-label={t("common.close")}>✕</button>
  </header>

  <div class="body">
    <p class="hint">{t("templates.hint")}</p>
    <h4 class="smallcaps">{t("templates.builtin")}</h4>
    <ul class="grid">
      {#each builtin as x (x.id)}{@render card(x)}{/each}
    </ul>
    <h4 class="smallcaps">{t("templates.mine")}</h4>
    {#if mine.length}
      <ul class="grid">
        {#each mine as x (x.id)}{@render card(x)}{/each}
      </ul>
    {:else}
      <p class="empty">{t("templates.empty")}</p>
    {/if}
  </div>

  <footer>
    <button class="btn" onclick={openFolder} disabled={busy}>{t("templates.openFolder")}</button>
    <span class="sep" aria-hidden="true"></span>
    <button class="btn" onclick={() => importTemplate(false)} disabled={busy}>{t("templates.importFolder")}</button>
    <button class="btn" onclick={() => importTemplate(true)} disabled={busy}>{t("templates.importZip")}</button>
    <button class="btn" onclick={exportProject} disabled={busy || !canExport}>{t("templates.export")}</button>
    <span class="spacer"></span>
    <button class="link" onclick={() => run(() => latexIpc.revealTemplates())}>{t("templates.reveal")}</button>
  </footer>
  {#if error || note}
    <p class="status" class:error={!!error}>{error ?? note}</p>
  {/if}
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(20, 18, 14, 0.22);
    z-index: 40;
    animation: fade 160ms var(--ease);
  }
  .sheet {
    position: fixed;
    z-index: 50;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(920px, 94vw);
    max-height: 88vh;
    display: grid;
    grid-template-rows: auto 1fr auto auto;
    background: var(--paper);
    border: 1px solid var(--rule-strong);
    border-radius: 6px;
    box-shadow: 0 24px 60px -28px rgba(0, 0, 0, 0.45);
    animation: rise 200ms var(--ease);
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding: 10px 16px 8px;
    border-bottom: 1px solid var(--rule);
    background: var(--chrome);
    border-radius: 6px 6px 0 0;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
  }
  .close {
    border: 0;
    background: transparent;
    font-size: 15px;
    color: var(--muted);
    cursor: pointer;
  }
  .body {
    overflow-y: auto;
    padding: 6px 18px 14px;
    min-height: 0;
  }
  .hint {
    margin: 6px 0 4px;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  h4 {
    margin: 14px 0 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
  }
  .grid {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 10px;
  }
  .card {
    display: grid;
    gap: 6px;
    align-content: start;
    padding: 10px 12px;
    border: 1px solid var(--rule);
    border-radius: 6px;
    background: var(--paper);
    transition: border-color 120ms var(--ease);
  }
  .card:hover {
    border-color: var(--rule-strong);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h3 {
    flex: 1;
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .engine {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--muted);
    border: 1px solid var(--rule);
    border-radius: 3px;
    padding: 0 4px;
  }
  .desc {
    margin: 0;
    font-size: 13px;
    color: var(--ink-2);
  }
  .meta {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .faint {
    color: var(--faint);
  }
  .empty {
    margin: 0;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  footer {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 10px 16px;
    border-top: 1px solid var(--rule);
    background: var(--chrome);
  }
  .sep {
    width: 1px;
    height: 18px;
    background: var(--rule);
  }
  .spacer {
    flex: 1;
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .danger {
    color: var(--error);
    border-color: var(--error);
  }
  .status {
    margin: 0;
    padding: 6px 16px 10px;
    font-size: 13px;
    color: var(--muted);
    background: var(--chrome);
    border-radius: 0 0 6px 6px;
  }
  .status.error {
    color: var(--error);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-50% + 8px));
    }
  }
</style>
