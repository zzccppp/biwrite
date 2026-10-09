<script lang="ts">
  import { t } from "../i18n.svelte";
  import type { Session } from "../session.svelte";

  interface Props {
    session: Session;
    /** Active provider, e.g. "DeepSeek · deepseek-chat". */
    provider: string;
    /** A newer release, found at startup. */
    update: string | null;
    onupdate: () => void;
    ondismisserror: () => void;
  }

  let { session, provider, update, onupdate, ondismisserror }: Props = $props();

  function compact(n: number): string {
    if (n < 1000) return String(n);
    if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
    return `${(n / 1_000_000).toFixed(1)}M`;
  }

  const eol = $derived(session.lineEnding.toUpperCase());
  const c = $derived(session.counts);
</script>

<footer class="status">
  <div class="left">
    <span class="smallcaps">{session.direction === "zh-en" ? "zh → en" : "en → zh"}</span>
    <span class="dot">·</span>
    <span class="smallcaps">{session.mode}</span>
    <span class="dot">·</span>
    <span class="smallcaps">{eol}</span>
    {#if !session.autoTranslate}
      <span class="badge smallcaps">auto-translate paused</span>
    {/if}
  </div>

  <div class="mid">
    {#if session.error}
      <button class="msg error" onclick={ondismisserror} title="Dismiss">{session.error} ✕</button>
    {:else if session.notice}
      <span class="msg notice">{session.notice}</span>
    {:else}
      <span>{c.translated} translated</span>
      {#if c.translating}<span class="dot">·</span><span class="busy">{c.translating} in flight</span>{/if}
      {#if c.pending}<span class="dot">·</span><span>{c.pending} pending</span>{/if}
      {#if c.error}<span class="dot">·</span><span class="err">{c.error} failed</span>{/if}
      {#if c.skipped}<span class="dot">·</span><span class="muted">{c.skipped} skipped</span>{/if}
    {/if}
  </div>

  <div class="right" title="Session usage">
    {#if update}
      <button class="update" onclick={onupdate}>{t("updates.available", { version: update })}</button>
      <span class="dot">·</span>
    {/if}
    {#if provider}<span class="provider">{provider}</span><span class="dot">·</span>{/if}
    <span>{compact(session.usage.requests)} <span class="smallcaps">req</span></span>
    <span class="dot">·</span>
    <span
      >{compact(session.usage.inputTokens)} <span class="smallcaps">in</span> / {compact(session.usage.outputTokens)}
      <span class="smallcaps">out</span></span
    >
    <span class="dot">·</span>
    <span>{compact(session.usage.cacheHits)} <span class="smallcaps">cached</span></span>
  </div>
</footer>

<style>
  .update {
    border: 0;
    background: transparent;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
    white-space: nowrap;
  }
  .update:hover {
    text-decoration: underline;
  }
  .status {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    gap: 16px;
    height: 26px;
    padding: 0 14px;
    background: var(--chrome);
    border-top: 1px solid var(--rule);
    color: var(--muted);
    font-size: 12.5px;
    font-variant-numeric: oldstyle-nums tabular-nums;
    white-space: nowrap;
  }
  .left,
  .right,
  .mid {
    display: flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
  }
  .right {
    justify-content: flex-end;
  }
  .mid {
    overflow: hidden;
  }
  .dot {
    color: var(--faint);
  }
  .provider {
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .badge {
    margin-left: 6px;
    color: var(--seal);
  }
  .busy {
    color: var(--accent);
  }
  .err {
    color: var(--error);
  }
  .muted {
    color: var(--faint);
  }
  .msg {
    overflow: hidden;
    text-overflow: ellipsis;
    font-style: italic;
  }
  .notice {
    color: var(--ink-2);
  }
  .error {
    border: 0;
    background: transparent;
    padding: 0;
    color: var(--error);
    cursor: pointer;
    font-size: inherit;
  }
</style>
