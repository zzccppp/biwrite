<script lang="ts">
  import { errorMessage, logIpc } from "../ipc";
  import {
    compare,
    differs,
    formatClock,
    formatCount,
    formatDuration,
    STATE_LABELS,
    summarize,
    type Comparison,
  } from "../logFormat";
  import type { RequestLogStore } from "../requestLog.svelte";
  import type { RequestRecord } from "../types";

  interface Props {
    log: RequestLogStore;
    onclose: () => void;
  }

  let { log, onclose }: Props = $props();

  type Filter = "all" | "translate" | "assistant" | "errors" | "differs";

  let filter = $state<Filter>("all");
  let query = $state("");
  let open = $state<number | null>(null);
  let error = $state<string | null>(null);

  const ASSISTANT = new Set(["polish", "edit", "ask"]);

  const shown = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return log.list.filter((r) => {
      if (filter === "translate" && r.purpose !== "translate") return false;
      if (filter === "assistant" && !ASSISTANT.has(r.purpose)) return false;
      if (filter === "errors" && r.state !== "error") return false;
      if (filter === "differs" && (r.state === "in_flight" || !differs(r))) return false;
      if (!q) return true;
      return [r.provider, r.purpose, r.request.model, r.response.model, r.endpoint, r.error]
        .filter(Boolean)
        .some((v) => (v as string).toLowerCase().includes(q));
    });
  });
  const stats = $derived(summarize(log.list));

  async function run(f: () => Promise<void>): Promise<void> {
    error = null;
    try {
      await f();
    } catch (err) {
      error = errorMessage(err);
    }
  }

  function toggleRecording(): Promise<void> {
    return run(async () => log.load(await logIpc.set({ ...log.settings, enabled: !log.settings.enabled })));
  }

  function togglePersist(): Promise<void> {
    return run(async () => log.load(await logIpc.set({ ...log.settings, persist: !log.settings.persist })));
  }

  function clear(): Promise<void> {
    return run(async () => log.load(await logIpc.clear()));
  }

  function reveal(): Promise<void> {
    return run(() => logIpc.reveal());
  }

  /** Label of the declared side of a comparison. */
  function declaredLabel(c: Comparison, declared: string | null): string {
    if (c === "not_returned") return "not returned";
    return declared ?? "";
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") onclose();
  }

  const filters: { value: Filter; label: string }[] = [
    { value: "all", label: "All" },
    { value: "translate", label: "Translation" },
    { value: "assistant", label: "Assistant" },
    { value: "differs", label: "Differences" },
    { value: "errors", label: "Errors" },
  ];
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet pair(r: RequestRecord, requested: string | null, declared: string | null)}
  {@const c = r.state === "in_flight" && declared === null ? (requested === null ? "absent" : "match") : compare(requested, declared)}
  <span
    class="pair"
    data-cmp={c}
    title={c === "absent" ? "" : `asked for ${requested ?? "(not sent)"} · declared ${declared ?? "(not returned)"}`}
  >
    {#if c === "absent"}
      <span class="faint">—</span>
    {:else}
      <span class="req">{requested ?? "not sent"}</span>
      {#if c !== "match"}
        <span class="arrow" aria-hidden="true">→</span>
        <span class="decl">{declaredLabel(c, declared)}</span>
      {/if}
    {/if}
  </span>
{/snippet}

{#snippet details(r: RequestRecord)}
  <dl class="details">
    <dt>Endpoint</dt>
    <dd class="mono">{r.endpoint || "—"} <span class="faint">({r.wire})</span></dd>
    <dt>Key</dt>
    <dd class="mono">{r.key ? `#${r.key.number} of ${r.key.count} · …${r.key.tail}` : "—"}</dd>
    <dt>Status</dt>
    <dd>{r.httpStatus ?? "no response"} · {STATE_LABELS[r.state]}</dd>
    <dt>Timing</dt>
    <dd>first text after {formatDuration(r.firstTokenMs)} · total {formatDuration(r.durationMs)}</dd>
    <dt>Size</dt>
    <dd>{r.promptChars.toLocaleString()} characters sent · {r.outputChars.toLocaleString()} received</dd>
    <dt>Tokens</dt>
    <dd>
      in {formatCount(r.usage.inputTokens)} (cached {formatCount(r.usage.cachedTokens)}) · out
      {formatCount(r.usage.outputTokens)} (reasoning {formatCount(r.usage.reasoningTokens)})
    </dd>
    {#if r.error}
      <dt>Error</dt>
      <dd class="err">{r.error}</dd>
    {/if}
    {#each r.notes as note, i (i)}
      <dt>{i === 0 ? "Notes" : ""}</dt>
      <dd>{note}</dd>
    {/each}
  </dl>
{/snippet}

<div class="scrim" role="presentation" onclick={onclose}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="log-title">
  <header>
    <h2 id="log-title" class="smallcaps">Requests</h2>
    <span class="count">
      <span>{stats.total} recorded</span>
      {#if stats.inFlight}<span class="dot">·</span><span class="busy">{stats.inFlight} in flight</span>{/if}
      {#if stats.differs}<span class="dot">·</span><span class="seal">{stats.differs} differ</span>{/if}
      {#if stats.errors}<span class="dot">·</span><span class="err">{stats.errors} failed</span>{/if}
    </span>
    <button class="close" onclick={onclose} aria-label="Close the request log">✕</button>
  </header>

  <div class="tools row">
    <button
      class="btn recording"
      class:on={log.settings.enabled}
      onclick={toggleRecording}
      aria-pressed={log.settings.enabled}
      title={log.settings.enabled
        ? "Pause: new requests are not recorded (requests still run)"
        : "Resume recording new requests"}
    >
      <span class="led" aria-hidden="true"></span>{log.settings.enabled ? "Recording" : "Paused"}
    </button>
    <label class="check" title="Append finished records to requests.jsonl (metadata only)">
      <input type="checkbox" checked={log.settings.persist} onchange={togglePersist} />
      <span>Log file</span>
    </label>
    <span class="sep" aria-hidden="true"></span>
    <div class="filters" role="radiogroup" aria-label="Show">
      {#each filters as f (f.value)}
        <button
          class="mode smallcaps"
          class:on={filter === f.value}
          role="radio"
          aria-checked={filter === f.value}
          onclick={() => (filter = f.value)}>{f.label}</button
        >
      {/each}
    </div>
    <input class="input search" type="search" placeholder="Search…" bind:value={query} aria-label="Search requests" />
    <span class="spacer"></span>
    <button class="btn" onclick={clear} disabled={log.list.length === 0}>Clear</button>
    <button class="btn" onclick={reveal} disabled={!log.file}>Show file</button>
  </div>

  <div class="table">
    <div class="head smallcaps" aria-hidden="true">
      <span>Time</span>
      <span>For</span>
      <span>Provider · key</span>
      <span>Model</span>
      <span>Effort</span>
      <span>Tier</span>
      <span>Status</span>
      <span class="num">Took</span>
      <span class="num">Tokens in / out</span>
    </div>
    {#each shown as r (r.id)}
      <div class="entry" class:open={open === r.id} data-state={r.state}>
        <button
          class="line"
          onclick={() => (open = open === r.id ? null : r.id)}
          aria-expanded={open === r.id}
          title="Show details"
        >
          <span class="mono faint">{formatClock(r.startedAt)}</span>
          <span class="smallcaps purpose">{r.purpose}</span>
          <span class="provider" title={r.key ? `${r.provider} · key #${r.key.number} of ${r.key.count} (…${r.key.tail})` : r.provider}>
            {#if r.key && r.key.count > 1}<span class="keyno mono">#{r.key.number}</span>{/if}{r.provider}
          </span>
          {@render pair(r, r.request.model, r.response.model)}
          {@render pair(r, r.request.effort, r.response.effort)}
          {@render pair(r, r.request.serviceTier, r.response.serviceTier)}
          <span class="status">
            {#if r.state === "in_flight"}
              <span class="spinner" aria-hidden="true"></span>
            {/if}
            <span class="mono">{r.httpStatus ?? ""}</span>
            <span class="smallcaps">{STATE_LABELS[r.state]}</span>
          </span>
          <span class="num mono">{formatDuration(r.durationMs)}</span>
          <span class="num mono">
            {formatCount(r.usage.inputTokens)} / {formatCount(r.usage.outputTokens)}{#if r.usage.reasoningTokens}<span
                class="faint"
                title="reasoning tokens"> ({formatCount(r.usage.reasoningTokens)} r)</span
              >{/if}
          </span>
        </button>
        {#if open === r.id}
          {@render details(r)}
        {/if}
      </div>
    {:else}
      <p class="empty">
        {#if log.list.length === 0}
          {log.settings.enabled
            ? "No requests yet. Translations and assistant requests appear here as they are sent."
            : "Recording is paused. Requests still run but are not recorded."}
        {:else}
          No request matches.
        {/if}
      </p>
    {/each}
  </div>

  <footer>
    <p class="hint" class:error={!!error}>
      {#if error}
        {error}
      {:else}
        Mean {formatDuration(stats.meanMs)} · {formatCount(stats.inputTokens)} in ({formatCount(stats.cachedTokens)} cached) ·
        {formatCount(stats.outputTokens)} out ({formatCount(stats.reasoningTokens)} reasoning). Declared values are what the
        server reports, which can differ from what actually ran. Records hold no text and no keys.
      {/if}
    </p>
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
    width: min(1180px, 96vw);
    height: min(720px, 90vh);
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
    display: flex;
    gap: 6px;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  .count .dot {
    color: var(--faint);
    font-style: normal;
  }
  .keyno {
    margin-right: 5px;
    color: var(--accent);
  }
  .close {
    border: 0;
    background: transparent;
    font-size: 15px;
    color: var(--muted);
    cursor: pointer;
  }
  .tools {
    padding: 9px 16px;
    border-bottom: 1px solid var(--rule);
    flex-wrap: wrap;
  }
  .recording .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--faint);
  }
  .recording.on .led {
    background: var(--seal);
    box-shadow: 0 0 0 3px var(--seal-wash);
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 13.5px;
    font-variant-caps: all-small-caps;
    letter-spacing: 0.05em;
    color: var(--ink-2);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--seal);
  }
  .sep {
    width: 1px;
    height: 18px;
    margin: 0 4px;
    background: var(--rule);
  }
  .filters {
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
  .mode.on {
    background: var(--ink);
    color: var(--paper);
  }
  .search {
    width: 170px;
  }
  .spacer {
    flex: 1;
  }
  .table {
    overflow: auto;
    padding: 0 16px 8px;
    min-height: 0;
  }
  .head,
  .line {
    display: grid;
    grid-template-columns: 66px 86px minmax(110px, 1.1fr) minmax(130px, 1.4fr) minmax(90px, 0.9fr) minmax(110px, 1fr) 112px 62px 126px;
    gap: 10px;
    align-items: baseline;
  }
  .head {
    position: sticky;
    top: 0;
    padding: 8px 6px 5px;
    background: var(--paper);
    font-size: 13px;
    color: var(--muted);
    border-bottom: 1px solid var(--rule);
    z-index: 1;
  }
  .num {
    text-align: right;
  }
  .entry {
    border-bottom: 1px solid var(--rule);
  }
  .entry.open {
    background: var(--paper-2);
  }
  .line {
    width: 100%;
    padding: 6px 6px;
    border: 0;
    background: transparent;
    text-align: left;
    font: inherit;
    font-size: 13.5px;
    color: var(--ink);
    cursor: pointer;
  }
  .line:hover {
    background: var(--accent-wash);
  }
  .line > span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .entry[data-state="cancelled"] .line {
    color: var(--muted);
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .faint {
    color: var(--faint);
  }
  .purpose {
    color: var(--accent);
    font-size: 13.5px;
  }
  .provider {
    color: var(--ink-2);
  }
  .pair {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .pair .arrow {
    margin: 0 3px;
    color: var(--faint);
  }
  .pair[data-cmp="differs"] .decl {
    color: var(--seal);
    font-weight: 600;
  }
  .pair[data-cmp="not_returned"] .decl,
  .pair[data-cmp="not_sent"] .req {
    color: var(--faint);
    font-style: italic;
    font-family: var(--font-ui);
  }
  .status {
    display: inline-flex;
    align-items: baseline;
    gap: 5px;
  }
  .entry[data-state="error"] .status {
    color: var(--error);
  }
  .entry[data-state="ok"] .status .smallcaps {
    color: #3f8f5a;
  }
  .spinner {
    width: 9px;
    height: 9px;
    border: 1.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
    align-self: center;
  }
  .busy {
    color: var(--accent);
  }
  .seal {
    color: var(--seal);
  }
  .err {
    color: var(--error);
  }
  .details {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 3px 12px;
    margin: 0;
    padding: 2px 6px 10px 84px;
    font-size: 13px;
    color: var(--ink-2);
    user-select: text;
    -webkit-user-select: text;
  }
  .details dt {
    font-variant-caps: all-small-caps;
    letter-spacing: 0.05em;
    color: var(--muted);
  }
  .details dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .empty {
    padding: 40px 0;
    text-align: center;
    font-style: italic;
    color: var(--muted);
  }
  footer {
    padding: 8px 16px 10px;
    border-top: 1px solid var(--rule);
  }
  .hint {
    margin: 0;
    font-size: 12.5px;
    font-style: italic;
    color: var(--faint);
  }
  .hint.error {
    color: var(--error);
    font-style: normal;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-50% + 8px));
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
</style>
