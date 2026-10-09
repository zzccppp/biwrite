<script lang="ts">
  import { onMount } from "svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { CacheView, Direction } from "../types";

  interface Props {
    /** Log folder, or null when logs only go to stderr. */
    logDir: string | null;
  }

  let { logDir }: Props = $props();

  let cache = $state<CacheView | null>(null);
  let confirming = $state(false);
  let busy = $state(false);
  let status = $state<string | null>(null);
  let error = $state<string | null>(null);
  let logError = $state<string | null>(null);

  const DIRECTION: Record<Direction, string> = { "en-zh": "EN → 中", "zh-en": "中 → EN" };

  function size(bytes: number): string {
    if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  async function run(f: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await f();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }

  function clear(): Promise<void> {
    return run(async () => {
      const cleared = await settingsIpc.clearCache();
      cache = cleared.cache;
      confirming = false;
      status = `Deleted ${cleared.removed} cached translation${cleared.removed === 1 ? "" : "s"}.`;
    });
  }

  onMount(() => {
    void run(async () => {
      cache = await settingsIpc.getCache();
    });
  });
</script>

<section>
  <h3 class="smallcaps">Translation cache</h3>
  {#if cache}
    <p class="summary">
      {cache.entries} translation{cache.entries === 1 ? "" : "s"} · {size(cache.bytes)}
      {#if !cache.location}<span class="warn"> · temporary (the cache file couldn't be opened)</span>{/if}
    </p>
    {#if cache.groups.length}
      <table>
        <tbody>
          {#each cache.groups as g (`${g.id}\u0000${g.model}\u0000${g.direction}`)}
            <tr>
              <td class="who">{g.provider} <span class="model">{g.model}</span></td>
              <td class="dir">{DIRECTION[g.direction]}</td>
              <td class="n">{g.entries}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    <div class="row actions">
      {#if confirming}
        <span class="ask">Delete all {cache.entries} cached translations?</span>
        <button class="btn" onclick={() => (confirming = false)} disabled={busy}>Cancel</button>
        <button class="btn danger" onclick={clear} disabled={busy}>Delete</button>
      {:else}
        <button
          class="btn"
          onclick={() => {
            status = null;
            confirming = true;
          }}
          disabled={busy || cache.entries === 0}>Clear cache…</button
        >
      {/if}
    </div>
  {:else if !error}
    <p class="summary">Loading…</p>
  {/if}
  <p class="hint" class:error={!!error}>
    {error ??
      status ??
      "Shared by all documents. After clearing, paragraphs on screen keep their translation; anything else is translated again (new requests) when needed."}
  </p>
  {#if cache?.location}<p class="path" title={cache.location}>{cache.location}</p>{/if}
</section>

<section>
  <h3 class="smallcaps">Logs</h3>
  <div class="row">
    <p class="hint grow" class:error={!!logError}>
      {logError ??
        "One file per day, at most 5 MB each; files older than 7 days are deleted. Never API keys; error messages may quote a short snippet (e.g. a formula)."}
    </p>
    <button
      class="btn"
      onclick={() => settingsIpc.revealLogs().catch((e) => (logError = errorMessage(e)))}
      disabled={!logDir}>Show logs</button
    >
  </div>
  {#if logDir}<p class="path" title={logDir}>{logDir}</p>{/if}
</section>

<style>
  section {
    padding: 14px 0 6px;
    border-bottom: 1px solid var(--rule);
  }
  h3 {
    margin: 0 0 10px;
    font-size: 14px;
    font-weight: 600;
    color: var(--seal);
  }
  .summary {
    margin: 0 0 8px;
    font-size: 14px;
  }
  .warn,
  .error {
    color: var(--error);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    margin: 0 0 10px;
    font-size: 13px;
  }
  td {
    padding: 3px 0;
    border-top: 1px solid var(--rule);
  }
  .model {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--muted);
  }
  .dir {
    color: var(--muted);
    white-space: nowrap;
    padding: 3px 12px;
  }
  .n {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .actions {
    margin-bottom: 6px;
  }
  .ask {
    flex: 1;
    font-size: 13px;
    color: var(--error);
  }
  .hint {
    margin: 4px 0;
    font-size: 12px;
    font-style: italic;
    color: var(--faint);
  }
  .hint.error {
    font-style: normal;
    color: var(--error);
  }
  .grow {
    flex: 1;
  }
  .path {
    margin: 2px 0 6px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    user-select: text;
    -webkit-user-select: text;
  }
</style>
