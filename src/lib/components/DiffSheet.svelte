<script lang="ts">
  import { t } from "../i18n.svelte";
  import type { Hunk } from "../types";

  interface Props {
    /** The files compared, each from the version the app read or last
     * wrote to the one on disk now. */
    files: { name: string; hunks: Hunk[] }[];
    onclose: () => void;
  }

  let { files, onclose }: Props = $props();

  let box = $state<HTMLElement>();
  // It takes the keys while open: Escape closes it and nothing behind it
  // (the question about the file keeps its answer to the author).
  $effect(() => {
    box?.focus();
  });

  function onkeydown(e: KeyboardEvent): void {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    onclose();
  }

  /** Most changes shown of one file. */
  const MAX_HUNKS = 60;
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="backdrop" onclick={onclose}></div>
<div class="sheet diff-sheet" role="dialog" aria-modal="true" tabindex="-1" bind:this={box} {onkeydown}>
  <header>
    <h2>{t("disk.diffTitle")}</h2>
    <button class="close" aria-label={t("common.close")} onclick={onclose}>✕</button>
  </header>
  <p class="legend">
    <span class="delete">{t("disk.diffOld")}</span>
    <span class="insert">{t("disk.diffNew")}</span>
  </p>
  <div class="body">
    {#each files as f (f.name)}
      <section>
        <h3>{f.name}</h3>
        {#if f.hunks.length === 0}
          <p class="none">{t("disk.diffNone")}</p>
        {/if}
        {#each f.hunks.slice(0, MAX_HUNKS) as h, i (i)}
          <h4 class="smallcaps">{t("assist.line", { n: h.line })}</h4>
          <pre class="code">{#each h.diff as part, k (k)}<span class={part.kind}>{part.text}</span>{/each}</pre>
        {/each}
        {#if f.hunks.length > MAX_HUNKS}
          <p class="none">{t("disk.diffMore", { n: f.hunks.length - MAX_HUNKS })}</p>
        {/if}
      </section>
    {/each}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: rgba(0, 0, 0, 0.25);
  }
  .sheet {
    position: fixed;
    z-index: 61;
    top: 6vh;
    left: 50%;
    transform: translateX(-50%);
    width: min(860px, calc(100vw - 32px));
    max-height: 88vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    border: 1px solid var(--rule-strong);
    border-radius: 8px;
    box-shadow: 0 24px 60px -24px rgba(0, 0, 0, 0.5);
  }
  header {
    display: flex;
    align-items: center;
    padding: 12px 16px 6px;
  }
  h2 {
    flex: 1;
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }
  .close {
    border: 0;
    background: transparent;
    color: var(--muted);
    font-size: 14px;
    cursor: pointer;
  }
  .legend {
    display: flex;
    gap: 12px;
    margin: 0;
    padding: 0 16px 8px;
    font-size: 12.5px;
    border-bottom: 1px solid var(--rule);
  }
  .body {
    overflow-y: auto;
    padding: 8px 16px 16px;
  }
  h3 {
    margin: 10px 0 4px;
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 600;
  }
  h4 {
    margin: 10px 0 3px;
    font-size: 11.5px;
    color: var(--muted);
  }
  .code {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    padding: 6px 8px;
    border-radius: 4px;
    background: var(--paper-2);
  }
  .delete {
    color: var(--error);
    text-decoration: line-through;
    text-decoration-thickness: 1px;
    background: var(--seal-wash);
  }
  .insert {
    color: var(--accent);
    background: var(--accent-wash);
    border-radius: 2px;
  }
  .none {
    margin: 4px 0;
    font-size: 12.5px;
    color: var(--muted);
  }
</style>
