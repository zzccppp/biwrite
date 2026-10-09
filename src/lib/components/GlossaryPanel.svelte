<script lang="ts">
  import { onMount, tick } from "svelte";
  import { errorMessage, glossaryIpc } from "../ipc";
  import type { GlossaryEntry } from "../types";

  interface Props {
    onclose: () => void;
  }

  let { onclose }: Props = $props();

  interface Row {
    key: number;
    term: string;
    translation: string;
    keep: boolean;
  }

  let rows = $state<Row[]>([]);
  let saved = $state("[]");
  let filter = $state("");
  let status = $state<string | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let loaded = $state(false);
  let confirmClose = $state(false);
  let list = $state<HTMLElement>();
  let nextKey = 0;

  function toRows(entries: GlossaryEntry[]): Row[] {
    return entries.map((e) => ({
      key: nextKey++,
      term: e.term,
      translation: e.translation ?? "",
      keep: e.translation === null,
    }));
  }

  function toEntries(rs: Row[]): GlossaryEntry[] {
    return rs
      .filter((r) => r.term.trim())
      .map((r) => ({
        term: r.term.trim(),
        translation: r.keep || !r.translation.trim() ? null : r.translation.trim(),
      }));
  }

  /** Rows rendered at once; a filter finds the rest (glossaries can hold 10,000 terms). */
  const MAX_SHOWN = 300;

  const dirty = $derived(JSON.stringify(toEntries(rows)) !== saved);
  const count = $derived(toEntries(rows).length);
  const matching = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => r.term.toLowerCase().includes(q) || r.translation.toLowerCase().includes(q));
  });
  const shown = $derived(matching.slice(0, MAX_SHOWN));

  function reset(entries: GlossaryEntry[]): void {
    rows = toRows(entries);
    saved = JSON.stringify(entries);
  }

  async function run(f: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    status = null;
    try {
      await f();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }

  async function addRow(): Promise<void> {
    filter = "";
    // New rows go first so they are visible however long the list is.
    rows.unshift({ key: nextKey++, term: "", translation: "", keep: false });
    await tick();
    list?.scrollTo({ top: 0 });
    list?.querySelector<HTMLInputElement>("input.term")?.focus();
  }

  function removeRow(key: number): void {
    rows = rows.filter((r) => r.key !== key);
  }

  function save(): Promise<void> {
    return run(async () => {
      const entries = await glossaryIpc.save(toEntries(rows));
      reset(entries);
      status = `Saved ${entries.length} term${entries.length === 1 ? "" : "s"}. Paragraphs that use a changed term are translated again.`;
    });
  }

  /** Imported terms replace rows with the same term and are added otherwise; nothing is saved yet. */
  function importCsv(): Promise<void> {
    return run(async () => {
      const imported = await glossaryIpc.importCsv();
      if (!imported) return;
      let added = 0;
      for (const entry of imported) {
        const [row] = toRows([entry]);
        const i = rows.findIndex((r) => r.term.trim().toLowerCase() === entry.term.toLowerCase());
        if (i >= 0) rows[i] = row;
        else {
          rows.push(row);
          added++;
        }
      }
      filter = "";
      status = `Imported ${imported.length} term${imported.length === 1 ? "" : "s"} (${added} new, added at the end). Review them, then Save.`;
    });
  }

  function exportCsv(): Promise<void> {
    return run(async () => {
      const name = await glossaryIpc.exportCsv();
      if (name) status = `Exported ${name}.`;
    });
  }

  /** Unsaved edits are only ever dropped with the explicit Discard button. */
  function close(): void {
    if (dirty) confirmClose = true;
    else onclose();
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") {
      e.preventDefault();
      if (confirmClose) confirmClose = false;
      else close();
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      if (dirty) void save();
    }
  }

  onMount(() => {
    glossaryIpc
      .get()
      .then((entries) => {
        reset(entries);
        loaded = true;
      })
      .catch((err) => (error = errorMessage(err)));
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="scrim" role="presentation" onclick={close}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="glossary-title">
  <header>
    <h2 id="glossary-title" class="smallcaps">Glossary</h2>
    <span class="count">{count} term{count === 1 ? "" : "s"}{dirty ? " · unsaved" : ""}</span>
    <button class="close" onclick={close} aria-label="Close glossary">✕</button>
  </header>

  <div class="tools row">
    <input class="input filter" type="search" placeholder="Filter…" bind:value={filter} aria-label="Filter terms" />
    <button class="btn" onclick={addRow} disabled={!loaded || busy}>+ Add term</button>
    <span class="spacer"></span>
    <button class="btn" onclick={importCsv} disabled={busy || !loaded}>Import CSV</button>
    <button
      class="btn"
      onclick={exportCsv}
      disabled={busy || dirty || !loaded}
      title={dirty ? "Save first: the saved glossary is exported" : "Export as term,translation CSV"}>Export CSV</button
    >
  </div>

  <div class="table" bind:this={list}>
    <div class="head smallcaps" aria-hidden="true">
      <span>English term</span>
      <span lang="zh-CN" class="zh-head">中文</span>
      <span class="center" title="Keep the term in English">Keep EN</span>
      <span></span>
    </div>
    {#each shown as row (row.key)}
      <div class="entry">
        <input
          class="input term"
          bind:value={row.term}
          disabled={busy}
          placeholder="term"
          aria-label="English term"
          spellcheck="false"
        />
        <input
          class="input zh"
          lang="zh-CN"
          bind:value={row.translation}
          disabled={row.keep || busy}
          placeholder={row.keep ? "keep in English" : "译名"}
          aria-label="Chinese rendering"
        />
        <input
          class="keep"
          type="checkbox"
          bind:checked={row.keep}
          disabled={busy}
          aria-label="Keep {row.term || 'term'} in English"
        />
        <button class="remove" onclick={() => removeRow(row.key)} disabled={busy} aria-label="Remove {row.term || 'term'}">✕</button>
      </div>
    {:else}
      <p class="empty">
        {#if !loaded && error}
          The glossary could not be loaded.
        {:else if !loaded}
          Loading…
        {:else if filter}
          No term matches “{filter}”.
        {:else}
          No terms yet. Add one, or import a CSV with the columns <code>term,translation</code>.
        {/if}
      </p>
    {/each}
    {#if matching.length > shown.length}
      <p class="more">Showing {shown.length} of {matching.length}. Type in the filter to find the others.</p>
    {/if}
  </div>

  <footer>
    {#if confirmClose}
      <p class="warn">{error ?? "You have unsaved changes."}</p>
      <div class="row">
        <button class="btn" onclick={() => (confirmClose = false)}>Keep editing</button>
        <button class="btn danger" onclick={onclose}>Discard</button>
        <button
          class="btn primary"
          disabled={busy}
          onclick={async () => {
            await save();
            if (!dirty) onclose();
          }}>Save & close</button
        >
      </div>
    {:else}
      <p class="hint" class:error={!!error}>
        {error ??
          status ??
          "Terms match whole words, any case, plurals included. Each paragraph is sent only the terms it mentions; when you write Chinese, the Chinese rendering is matched."}
      </p>
      <div class="row">
        <button class="btn" onclick={close}>Close</button>
        <button class="btn primary" onclick={save} disabled={busy || !dirty}>Save</button>
      </div>
    {/if}
  </footer>
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
    width: min(720px, 94vw);
    height: min(640px, 88vh);
    display: grid;
    grid-template-rows: auto auto 1fr auto;
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
  .count {
    flex: 1;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  .close {
    border: 0;
    background: transparent;
    font-size: 15px;
    color: var(--muted);
    cursor: pointer;
  }
  .tools {
    padding: 10px 16px;
    border-bottom: 1px solid var(--rule);
  }
  .filter {
    width: 200px;
  }
  .spacer {
    flex: 1;
  }
  .table {
    overflow-y: auto;
    padding: 0 16px 8px;
    min-height: 0;
  }
  .head,
  .entry {
    display: grid;
    grid-template-columns: 1fr 1fr 64px 24px;
    gap: 8px;
    align-items: center;
  }
  .head {
    position: sticky;
    top: 0;
    padding: 8px 0 4px;
    background: var(--paper);
    font-size: 13px;
    color: var(--muted);
    z-index: 1;
  }
  .center {
    text-align: center;
    white-space: nowrap;
  }
  .zh-head {
    font-family: var(--font-zh);
    font-variant-caps: normal;
    letter-spacing: 0.1em;
  }
  .entry {
    padding: 3px 0;
  }
  .entry .input {
    padding: 3px 7px;
  }
  .term {
    font-family: var(--font-prose);
  }
  .zh {
    font-family: var(--font-zh);
  }
  .zh:disabled {
    background: var(--paper-2);
    color: var(--faint);
    font-style: italic;
  }
  .keep {
    justify-self: center;
    accent-color: var(--seal);
  }
  .remove {
    border: 0;
    background: transparent;
    color: var(--faint);
    font-size: 12px;
    cursor: pointer;
    border-radius: 3px;
    height: 24px;
  }
  .remove:hover {
    color: var(--error);
    background: var(--seal-wash);
  }
  .more {
    margin: 10px 0 4px;
    font-size: 12.5px;
    font-style: italic;
    color: var(--muted);
    text-align: center;
  }
  .empty {
    margin: 28px 0;
    text-align: center;
    font-style: italic;
    color: var(--muted);
  }
  code {
    font-family: var(--font-mono);
    font-size: 12.5px;
    font-style: normal;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    border-top: 1px solid var(--rule);
  }
  footer p {
    flex: 1;
    margin: 0;
    font-size: 12.5px;
  }
  .hint {
    font-style: italic;
    color: var(--faint);
  }
  .hint.error,
  .warn {
    font-style: normal;
    color: var(--error);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-50% + 10px));
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
</style>
