<script lang="ts">
  import { count, type MessageKey, t } from "../i18n.svelte";
  import { errorMessage, logIpc } from "../ipc";
  import { compare, differs, formatClock, formatCount, formatDuration, summarize, type Comparison } from "../logFormat";
  import type { RequestLogStore } from "../requestLog.svelte";
  import type { RecordState, RequestRecord } from "../types";

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
    if (c === "not_returned") return t("log.notReturned");
    return declared ?? "";
  }

  const STATE_LABEL: Record<RecordState, MessageKey> = {
    in_flight: "log.state.in_flight",
    ok: "log.state.ok",
    error: "log.state.error",
    cancelled: "log.state.cancelled",
  };

  const PURPOSE_LABEL: Record<string, MessageKey | undefined> = {
    translate: "log.purpose.translate",
    polish: "log.purpose.polish",
    edit: "log.purpose.edit",
    ask: "log.purpose.ask",
    figure: "log.purpose.figure",
  };

  /** A known purpose in the interface language, any other as recorded. */
  function purposeLabel(purpose: string): string {
    const key = PURPOSE_LABEL[purpose];
    return key ? t(key) : purpose;
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") onclose();
  }

  const filters: { value: Filter; label: MessageKey }[] = [
    { value: "all", label: "log.filter.all" },
    { value: "translate", label: "log.filter.translate" },
    { value: "assistant", label: "log.filter.assistant" },
    { value: "differs", label: "log.filter.differs" },
    { value: "errors", label: "log.filter.errors" },
  ];
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet pair(r: RequestRecord, requested: string | null, declared: string | null)}
  {@const c = r.state === "in_flight" && declared === null ? (requested === null ? "absent" : "match") : compare(requested, declared)}
  <span
    class="pair"
    data-cmp={c}
    title={c === "absent"
      ? ""
      : t("log.asked", {
          requested: requested ?? t("log.notSentParen"),
          declared: declared ?? t("log.notReturnedParen"),
        })}
  >
    {#if c === "absent"}
      <span class="faint">—</span>
    {:else}
      <span class="req">{requested ?? t("log.notSent")}</span>
      {#if c !== "match"}
        <span class="arrow" aria-hidden="true">→</span>
        <span class="decl">{declaredLabel(c, declared)}</span>
      {/if}
    {/if}
  </span>
{/snippet}

{#snippet details(r: RequestRecord)}
  <dl class="details">
    <dt>{t("log.endpoint")}</dt>
    <dd class="mono">{r.endpoint || "—"} <span class="faint">({r.wire})</span></dd>
    <dt>{t("log.key")}</dt>
    <dd class="mono">
      {r.key ? t("log.keyValue", { n: r.key.number, count: r.key.count, tail: r.key.tail }) : "—"}
    </dd>
    <dt>{t("log.col.status")}</dt>
    <dd>{r.httpStatus ?? t("log.noResponse")} · {t(STATE_LABEL[r.state])}</dd>
    <dt>{t("log.timing")}</dt>
    <dd>
      {t("log.timingValue", { first: formatDuration(r.firstTokenMs), total: formatDuration(r.durationMs) })}
    </dd>
    <dt>{t("log.size")}</dt>
    <dd>
      {count(r.promptChars, "log.sizeValue.one", "log.sizeValue.many", {
        sent: r.promptChars.toLocaleString(),
        received: r.outputChars.toLocaleString(),
      })}
    </dd>
    <dt>{t("log.tokens")}</dt>
    <dd>
      {t("log.tokensValue", {
        in: formatCount(r.usage.inputTokens),
        cached: formatCount(r.usage.cachedTokens),
        out: formatCount(r.usage.outputTokens),
        reasoning: formatCount(r.usage.reasoningTokens),
      })}
    </dd>
    {#if r.error}
      <dt>{t("log.error")}</dt>
      <dd class="err">{r.error}</dd>
    {/if}
    {#each r.notes as note, i (i)}
      <dt>{i === 0 ? t("log.notes") : ""}</dt>
      <dd>{note}</dd>
    {/each}
  </dl>
{/snippet}

<div class="scrim" role="presentation" onclick={onclose}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="log-title">
  <header>
    <h2 id="log-title" class="smallcaps">{t("log.title")}</h2>
    <span class="count">
      <span>{t("log.recorded", { n: stats.total })}</span>
      {#if stats.inFlight}<span class="dot">·</span><span class="busy">{t("log.inFlight", { n: stats.inFlight })}</span
        >{/if}
      {#if stats.differs}<span class="dot">·</span><span class="seal"
          >{count(stats.differs, "log.differ.one", "log.differ.many")}</span
        >{/if}
      {#if stats.errors}<span class="dot">·</span><span class="err">{t("log.failed", { n: stats.errors })}</span>{/if}
    </span>
    <button class="close" onclick={onclose} aria-label={t("log.close")}>✕</button>
  </header>

  <div class="tools row">
    <button
      class="btn recording"
      class:on={log.settings.enabled}
      onclick={toggleRecording}
      aria-pressed={log.settings.enabled}
      title={log.settings.enabled ? t("log.pauseTitle") : t("log.resumeTitle")}
    >
      <span class="led" aria-hidden="true"></span>{log.settings.enabled ? t("log.recording") : t("log.paused")}
    </button>
    <label class="check" title={t("log.fileTitle")}>
      <input type="checkbox" checked={log.settings.persist} onchange={togglePersist} />
      <span>{t("log.file")}</span>
    </label>
    <span class="sep" aria-hidden="true"></span>
    <div class="filters" role="radiogroup" aria-label={t("log.show")}>
      {#each filters as f (f.value)}
        <button
          class="mode smallcaps"
          class:on={filter === f.value}
          role="radio"
          aria-checked={filter === f.value}
          onclick={() => (filter = f.value)}>{t(f.label)}</button
        >
      {/each}
    </div>
    <input
      class="input search"
      type="search"
      placeholder={t("log.search")}
      bind:value={query}
      aria-label={t("log.searchLabel")}
    />
    <span class="spacer"></span>
    <button class="btn" onclick={clear} disabled={log.list.length === 0}>{t("log.clear")}</button>
    <button class="btn" onclick={reveal} disabled={!log.file}>{t("log.reveal")}</button>
  </div>

  <div class="table">
    <div class="head smallcaps" aria-hidden="true">
      <span>{t("log.col.time")}</span>
      <span>{t("log.col.for")}</span>
      <span>{t("log.col.provider")}</span>
      <span>{t("log.col.model")}</span>
      <span>{t("log.col.effort")}</span>
      <span>{t("log.col.tier")}</span>
      <span>{t("log.col.status")}</span>
      <span class="num">{t("log.col.took")}</span>
      <span class="num">{t("log.col.tokens")}</span>
    </div>
    {#each shown as r (r.id)}
      <div class="entry" class:open={open === r.id} data-state={r.state}>
        <button
          class="line"
          onclick={() => (open = open === r.id ? null : r.id)}
          aria-expanded={open === r.id}
          title={t("log.showDetails")}
        >
          <span class="mono faint">{formatClock(r.startedAt)}</span>
          <span class="smallcaps purpose">{purposeLabel(r.purpose)}</span>
          <span
            class="provider"
            title={r.key
              ? t("log.providerKey", { provider: r.provider, n: r.key.number, count: r.key.count, tail: r.key.tail })
              : r.provider}
          >
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
            <span class="smallcaps">{t(STATE_LABEL[r.state])}</span>
          </span>
          <span class="num mono">{formatDuration(r.durationMs)}</span>
          <span class="num mono">
            {formatCount(r.usage.inputTokens)} / {formatCount(r.usage.outputTokens)}{#if r.usage.reasoningTokens}<span
                class="faint"
                title={t("log.reasoningTokens")}> {t("log.reasoningShort", { n: formatCount(r.usage.reasoningTokens) })}</span
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
          {log.settings.enabled ? t("log.empty") : t("log.emptyPaused")}
        {:else}
          {t("log.noMatch")}
        {/if}
      </p>
    {/each}
  </div>

  <footer>
    <p class="hint" class:error={!!error}>
      {#if error}
        {error}
      {:else}
        {t("log.summary", {
          mean: formatDuration(stats.meanMs),
          in: formatCount(stats.inputTokens),
          cached: formatCount(stats.cachedTokens),
          out: formatCount(stats.outputTokens),
          reasoning: formatCount(stats.reasoningTokens),
        })}
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
