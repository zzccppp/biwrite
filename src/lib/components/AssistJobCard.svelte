<script lang="ts">
  import { type AssistJob, ACTION_LABEL } from "../assist.svelte";
  import { t } from "../i18n.svelte";
  import { formatCount, formatDuration } from "../logFormat";
  import type { AssistResult } from "../types";

  interface Props {
    job: AssistJob;
    /** The job's document is no longer open. */
    stale: boolean;
    onaccept: (job: AssistJob) => void;
    /** Undo an applied job's change, wherever later edits put it. */
    onrevert: (job: AssistJob) => void;
    ondiscard: (job: AssistJob) => void;
    /** Take a finished card off the list. */
    onremove: (job: AssistJob) => void;
    onagain: (job: AssistJob) => void;
    onreapply: (job: AssistJob) => void;
    onfollowup: (job: AssistJob, question: string) => void;
    onfocus: (job: AssistJob) => void;
    onstop: (job: AssistJob) => void;
  }

  let { job, stale, onaccept, onrevert, ondiscard, onremove, onagain, onreapply, onfollowup, onfocus, onstop }: Props =
    $props();

  let followUp = $state("");
  let copied = $state(false);
  /** An applied, discarded or kept card shows its result again. */
  let expanded = $state(false);
  const finished = $derived(job.state === "applied" || job.state === "discarded");
  /** Shown as one line (dimmed): finished or kept, and not opened. A revert
   * that found the text edited lists the text from before, in full. */
  const folded = $derived(
    !expanded && ((finished && !(job.state === "applied" && job.revertBlocked)) || (job.state === "done" && job.kept)),
  );

  function keep(): void {
    job.kept = true;
    expanded = false;
  }

  const r = $derived(job.result);
  const unchanged = $derived(!!r?.revision && r.revision.trim() === job.target.text.trim());
  const preview = $derived(job.target.text.replace(/\s+/g, " ").slice(0, 90));

  async function copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard not available; nothing to do.
    }
  }

  function ask(): void {
    const q = followUp.trim();
    if (!q) return;
    followUp = "";
    onfollowup(job, q);
  }
</script>

<article class="card" class:folded data-state={job.state} data-action={job.action}>
  <button class="head" onclick={() => onfocus(job)} title={job.target.text}>
    <span class="action smallcaps">{t(ACTION_LABEL[job.action])}</span>
    {#if job.reapplies}<span class="tag">{t("assist.reapplying")}</span>{/if}
    <span class="snippet">{preview || "—"}</span>
  </button>

  {#if job.instruction}
    <p class="instruction">{job.instruction.split("\n")[0]}</p>
  {/if}

  {#if job.state === "running"}
    <div class="streaming">
      <span class="spinner" aria-hidden="true"></span>
      <span class="label"
        >{job.retry ? t("assist.retrying", { n: job.retry.attempt, of: job.retry.of }) : t("assist.running")}</span
      >
      <button class="btn small" onclick={() => onstop(job)}>{t("assist.stop")}</button>
    </div>
    {#if job.partial}<pre class="partial">{job.partial}</pre>{/if}
  {:else if job.state === "failed"}
    <p class="error">{t("assist.failed", { error: job.error ?? "" })}</p>
    <div class="row">
      {#if !stale}<button class="btn small" disabled={job.restarting} onclick={() => onagain(job)}>{t("assist.again")}</button>{/if}
      <button class="btn small" onclick={() => ondiscard(job)}>{t("assist.discard")}</button>
    </div>
  {:else if finished}
    <div class="row">
      <p class="note">{t(job.state === "applied" ? "assist.applied" : "assist.discarded")}</p>
      <span class="spacer"></span>
      {#if r}
        <button class="btn small" aria-expanded={expanded} onclick={() => (expanded = !expanded)}
          >{expanded ? t("assist.hide") : t("assist.show")}</button
        >
      {/if}
      {#if job.state === "applied" && job.applied && !stale}
        <button class="btn small" onclick={() => onrevert(job)}>{t("assist.revert")}</button>
      {/if}
      <button class="btn small" onclick={() => onremove(job)}>{t("common.delete")}</button>
    </div>
    {#if job.state === "applied" && job.revertBlocked && !job.target.insert}
      <section class="original">
        <h4 class="smallcaps">{t("assist.original")}</h4>
        <p class="answer">{job.target.text}</p>
        <div class="row">
          <button class="btn small" onclick={() => copy(job.target.text)}>
            {copied ? t("assist.copied") : t("assist.copy")}
          </button>
        </div>
      </section>
    {/if}
    {#if expanded && r}
      {@render body(r)}
      {@render follow()}
      {@render meta(r)}
    {/if}
  {:else if r && job.kept && !expanded}
    <div class="row">
      <p class="note">{t("assist.kept")}</p>
      <span class="spacer"></span>
      <button class="btn small" aria-expanded={false} onclick={() => (expanded = true)}>{t("assist.show")}</button>
      <button class="btn small" onclick={() => onremove(job)}>{t("common.delete")}</button>
    </div>
  {:else if r}
    {@render body(r)}

    {#if stale}
      <p class="warn">{t("assist.stale")}</p>
      <div class="row">
        <span class="spacer"></span>
        <button class="btn small" onclick={() => onremove(job)}>{t("common.delete")}</button>
      </div>
    {:else if job.conflict}
      <p class="warn">{job.to > job.from ? t("assist.conflict") : t("assist.gone")}</p>
      <div class="row">
        {#if job.to > job.from}
          <button class="btn small primary" disabled={job.restarting} onclick={() => onreapply(job)}>{t("assist.reapply")}</button>
        {/if}
        <button class="btn small" onclick={() => ondiscard(job)}>{t("assist.discard")}</button>
      </div>
    {:else}
      <div class="row">
        {#if r.revision && !unchanged && job.action !== "ask"}
          <button class="btn small primary" disabled={job.applying || job.restarting} onclick={() => onaccept(job)}>
            {job.target.insert ? t("assist.insert") : t("assist.accept")}
          </button>
        {/if}
        <button class="btn small" disabled={job.restarting} onclick={() => onagain(job)}>{t("assist.again")}</button>
        {#if r.revision || r.answer}
          <button class="btn small" onclick={() => copy(r.revision ?? r.answer ?? "")}>
            {copied ? t("assist.copied") : t("assist.copy")}
          </button>
        {/if}
        <span class="spacer"></span>
        {#if job.kept}
          <button class="btn small" aria-expanded={true} onclick={() => (expanded = false)}>{t("assist.hide")}</button>
        {:else}
          <button class="btn small" onclick={keep}>{t("assist.keep")}</button>
        {/if}
        <!-- A question has nothing to accept or discard, so it is deleted when done with. -->
        {#if job.action === "ask"}
          <button class="btn small" onclick={() => onremove(job)}>{t("common.delete")}</button>
        {:else}
          <button class="btn small" onclick={() => ondiscard(job)}>{t("assist.discard")}</button>
        {/if}
      </div>
    {/if}

    {@render follow()}
    {@render meta(r)}
  {/if}
</article>

{#snippet body(r: AssistResult)}
  {#if r.answer}
    <section>
      <h4 class="smallcaps">{t("assist.answer")}</h4>
      <p class="answer">{r.answer}</p>
    </section>
  {/if}

  {#if r.revision}
    {#if job.action === "figure"}
      <pre class="code">{r.revision}</pre>
      {#if r.translation}
        <section>
          <h4 class="smallcaps">{t("assist.caption")}</h4>
          <p class="translation" lang="zh-CN">{r.translation}</p>
        </section>
      {/if}
      {#if r.missingPackages.length}
        <p class="warn">
          {r.preambleHere
            ? t("assist.packagesAdded", { packages: r.missingPackages.join(", ") })
            : t("assist.packagesNeeded", { packages: r.missingPackages.map((p) => `\\usepackage{${p}}`).join(" ") })}
        </p>
      {/if}
    {:else if job.action === "write"}
      <p class="written">{r.revision}</p>
      {#if r.translation}
        <section>
          <h4 class="smallcaps">{t("assist.translation")}</h4>
          <p class="translation written">{r.translation}</p>
        </section>
      {/if}
    {:else if unchanged}
      <p class="note">{t("assist.noChange")}</p>
    {:else}
      <p class="diff">
        {#each r.diff as part, i (i)}<span class={part.kind}>{part.text}</span>{/each}
      </p>
      {#if r.translation}
        <section>
          <h4 class="smallcaps">{t("assist.translation")}</h4>
          <p class="translation">{r.translation}</p>
        </section>
      {/if}
    {/if}
  {/if}

  {#if r.changesZh.length || r.changesEn.length}
    <section class="changes">
      <h4 class="smallcaps">{t("assist.changes")}</h4>
      {#if r.changesZh.length}
        <ul lang="zh-CN">{#each r.changesZh as c, i (i)}<li>{c}</li>{/each}</ul>
      {/if}
      {#if r.changesEn.length}
        <ul lang="en">{#each r.changesEn as c, i (i)}<li>{c}</li>{/each}</ul>
      {/if}
    </section>
  {/if}

  {#if r.removed.length}
    <p class="warn">{t("assist.removed", { items: r.removed.join(", ") })}</p>
  {/if}
  {#if r.repeated.length}
    <p class="warn">{t("assist.repeated", { items: r.repeated.join(", ") })}</p>
  {/if}
  {#if r.revision && job.action !== "figure" && job.action !== "write" && r.translation && !r.translationMatches}
    <p class="warn">{t("assist.translationDiffers")}</p>
  {/if}
{/snippet}

{#snippet follow()}
  {#if job.action === "ask" && !stale}
    <div class="row follow">
      <input
        class="input"
        bind:value={followUp}
        placeholder={t("assist.followUp")}
        onkeydown={(e) => e.key === "Enter" && ask()}
      />
      <button class="btn small" onclick={ask} disabled={!followUp.trim()}>{t("assist.followUpRun")}</button>
    </div>
  {/if}
{/snippet}

{#snippet meta(r: AssistResult)}
  <p class="meta">
    {job.model} · {t("assist.usage", {
      in: formatCount(r.usage.inputTokens),
      out: formatCount(r.usage.outputTokens),
      time: formatDuration(r.durationMs),
    })}
  </p>
{/snippet}

<style>
  .card {
    min-width: 0;
    border: 1px solid var(--rule);
    border-radius: 6px;
    background: var(--paper);
    padding: 8px 10px 6px;
    display: grid;
    gap: 6px;
    animation: rise 180ms var(--ease);
  }
  .card[data-state="done"] {
    border-color: var(--rule-strong);
    box-shadow: 0 6px 18px -14px rgba(0, 0, 0, 0.4);
  }
  .card.folded {
    opacity: 0.6;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    border: 0;
    background: transparent;
    padding: 0;
    text-align: left;
    font: inherit;
    cursor: pointer;
    min-width: 0;
  }
  .action {
    color: var(--seal);
    font-size: 13.5px;
    font-weight: 600;
    white-space: nowrap;
  }
  .tag {
    font-size: 11.5px;
    color: var(--accent);
    white-space: nowrap;
  }
  .snippet {
    flex: 1;
    min-width: 0;
    color: var(--muted);
    font-size: 13px;
    font-style: italic;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .instruction {
    margin: 0;
    font-size: 13px;
    color: var(--ink-2);
    border-left: 2px solid var(--rule-strong);
    padding-left: 7px;
  }
  .streaming {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--accent);
  }
  .streaming .label {
    flex: 1;
  }
  .spinner {
    width: 10px;
    height: 10px;
    border: 1.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
  }
  .partial,
  .code {
    margin: 0;
    max-height: 220px;
    overflow: auto;
    white-space: pre-wrap;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--ink-2);
    background: var(--paper-2);
    border-radius: 4px;
    padding: 6px 8px;
    user-select: text;
    -webkit-user-select: text;
  }
  .partial {
    color: var(--muted);
  }
  .diff,
  .answer,
  .translation {
    margin: 0;
    font-size: 14px;
    line-height: 1.6;
    white-space: pre-wrap;
    user-select: text;
    -webkit-user-select: text;
  }
  .written {
    margin: 0;
    max-height: 320px;
    overflow: auto;
    font-size: 14px;
    line-height: 1.6;
    white-space: pre-wrap;
    user-select: text;
    -webkit-user-select: text;
  }
  .diff .delete {
    color: var(--error);
    text-decoration: line-through;
    text-decoration-thickness: 1px;
    background: var(--seal-wash);
  }
  .diff .insert {
    color: var(--accent);
    background: var(--accent-wash);
    border-radius: 2px;
  }
  .translation,
  .changes ul:lang(zh-CN) {
    font-family: var(--font-zh);
  }
  h4 {
    margin: 0 0 2px;
    font-size: 12.5px;
    font-weight: 600;
    color: var(--muted);
  }
  .changes ul {
    margin: 0 0 2px;
    padding-left: 18px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .warn,
  .error {
    margin: 0;
    font-size: 12.5px;
    color: var(--error);
  }
  .note {
    margin: 0;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  .meta {
    margin: 0;
    font-size: 11.5px;
    color: var(--faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row {
    flex-wrap: wrap;
  }
  .follow .input {
    flex: 1;
  }
  .spacer {
    flex: 1;
  }
  :global(.btn.small) {
    height: 24px;
    padding: 0 9px 1px;
    font-size: 13px;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(4px);
    }
  }
</style>
