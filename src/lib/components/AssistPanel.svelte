<script lang="ts">
  import { type AssistJob, type AssistStore, ACTION_LABEL, PLACEHOLDER, SCOPE_LABEL } from "../assist.svelte";
  import { t } from "../i18n.svelte";
  import { ipc } from "../ipc";
  import type { AssistAction, AssistScope } from "../types";
  import AssistJobCard from "./AssistJobCard.svelte";

  interface Props {
    store: AssistStore;
    /** What the next run acts on, as shown in the composer. */
    target: {
      label: string;
      text: string;
      /** The paragraph, for the via-translation action (null for a selection). */
      key: string | null;
      /** The paragraph's current translation. */
      translation: string | null;
    } | null;
    /** The assistant has a model; otherwise the composer explains. */
    ready: boolean;
    model: string;
    skill: string;
    onrun: () => void;
    onaccept: (job: AssistJob) => void;
    onagain: (job: AssistJob) => void;
    onreapply: (job: AssistJob) => void;
    onfollowup: (job: AssistJob, question: string) => void;
    onfocus: (job: AssistJob) => void;
    onclose: () => void;
  }

  let { store, target, ready, model, skill, onrun, onaccept, onagain, onreapply, onfollowup, onfocus, onclose }: Props =
    $props();

  let instructionBox = $state<HTMLTextAreaElement>();
  let sampleDraft = $state("");

  /** Put the cursor in the instruction box (⌘K). */
  export function focus(): void {
    instructionBox?.focus();
  }
  let addingSample = $state(false);

  const actions: AssistAction[] = ["polish", "edit", "ask", "figure", "mirror"];
  /** The paragraph whose translation the box was last filled with. */
  let prefilled: string | null = null;

  // Via translation: the box starts with the paragraph's translation.
  $effect(() => {
    if (store.action !== "mirror" || !target?.key) return;
    if (target.key !== prefilled) {
      prefilled = target.key;
      store.instruction = target.translation ?? "";
    }
  });
  const scopes: AssistScope[] = ["target", "neighbors", "document"];
  const needsInstruction = $derived(store.action !== "polish");
  const mirrorReady = $derived(
    store.action !== "mirror" ||
      (!!target?.translation && store.instruction.trim().length > 0 && store.instruction.trim() !== target.translation.trim()),
  );
  const canRun = $derived(
    ready && !!target && (!needsInstruction || store.instruction.trim().length > 0) && mirrorReady,
  );

  function addSample(): void {
    const text = sampleDraft.trim();
    if (text) store.samples = [...store.samples, text];
    sampleDraft = "";
    addingSample = false;
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && canRun) {
      e.preventDefault();
      onrun();
    }
  }
</script>

<aside class="dock" aria-label={t("assist.title")}>
  <header>
    <h2 class="smallcaps">{t("assist.title")}</h2>
    <span class="model" title={model}>{model}</span>
    <button class="close" onclick={onclose} aria-label={t("common.close")}>✕</button>
  </header>

  <div class="composer">
    <div class="actions" role="radiogroup" aria-label={t("assist.title")}>
      {#each actions as a (a)}
        <button
          class="mode smallcaps"
          class:on={store.action === a}
          role="radio"
          aria-checked={store.action === a}
          onclick={() => store.setAction(a)}>{t(ACTION_LABEL[a])}</button
        >
      {/each}
    </div>

    <div class="target" class:none={!target}>
      <span class="label smallcaps">{target ? target.label : t("assist.target")}</span>
      <span class="text">{target ? target.text : t("assist.target.none")}</span>
    </div>

    <textarea
      class="textarea"
      bind:this={instructionBox}
      rows={store.action === "mirror" ? 6 : needsInstruction ? 3 : 2}
      bind:value={store.instruction}
      placeholder={t(PLACEHOLDER[store.action])}
      onkeydown={onKeydown}
    ></textarea>

    {#if store.action === "mirror" && !target?.translation}
      <p class="hint">{t("assist.mirrorNeedsParagraph")}</p>
    {/if}

    <div class="row options">
      <span class="label smallcaps">{t("assist.scope")}</span>
      <select class="select" value={store.scope} onchange={(e) => store.setScope(e.currentTarget.value as AssistScope)}>
        {#each scopes as s (s)}<option value={s}>{t(SCOPE_LABEL[s])}</option>{/each}
      </select>
    </div>

    <div class="refs">
      <div class="row">
        <span class="label smallcaps">{t("assist.references")}</span>
        <span class="spacer"></span>
        <button class="link" onclick={() => (addingSample = !addingSample)}>{t("assist.addText")}</button>
        <button class="link" onclick={() => store.attach()}>{t("assist.addImage")}</button>
      </div>
      {#if addingSample}
        <textarea class="textarea" rows="3" bind:value={sampleDraft} placeholder={t("assist.samplePlaceholder")}
        ></textarea>
        <div class="row"><button class="btn small" onclick={addSample} disabled={!sampleDraft.trim()}>{t("common.ok")}</button></div>
      {/if}
      {#if store.samples.length || store.attachments.length}
        <ul class="chips">
          {#each store.samples as s, i (i)}
            <li>
              <span class="chip-text">“{s.slice(0, 40)}{s.length > 40 ? "…" : ""}”</span>
              <button
                class="x"
                aria-label={t("common.remove")}
                onclick={() => (store.samples = store.samples.filter((_, j) => j !== i))}>✕</button
              >
            </li>
          {/each}
          {#each store.attachments as a (a.id)}
            <li>
              <img src={a.dataUrl} alt={a.name} />
              <span class="chip-text">{a.name}</span>
              <button class="x" aria-label={t("common.remove")} onclick={() => store.detach(a.id)}>✕</button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <div class="row run">
      <button class="skill" onclick={() => ipc.openLink("skill")} title="github.com/qzkinhit/research-builder"
        >{t("assist.skill", { skill })}</button
      >
      <span class="spacer"></span>
      <button class="btn primary" onclick={onrun} disabled={!canRun} title="⌘↩">{t("assist.run")}</button>
    </div>
  </div>

  <div class="jobs">
    {#each store.jobs as job (job.id)}
      <AssistJobCard
        {job}
        stale={job.epoch !== store.epoch}
        {onaccept}
        ondiscard={(j) => store.discard(j)}
        {onagain}
        {onreapply}
        {onfollowup}
        {onfocus}
        onstop={(j) => store.cancel(j)}
      />
    {:else}
      <p class="empty">{t("assist.empty")}</p>
    {/each}
    {#if store.jobs.some((j) => !j.open)}
      <button class="link clear" onclick={() => store.clearFinished()}>{t("assist.clear")}</button>
    {/if}
  </div>
</aside>

<style>
  .dock {
    height: 100%;
    width: 100%;
    display: grid;
    grid-template-rows: auto auto 1fr;
    grid-template-columns: minmax(0, 1fr);
    background: var(--paper);
    border-left: 1px solid var(--rule-strong);
    min-width: 0;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 9px 14px 7px;
    border-bottom: 1px solid var(--rule);
    background: var(--chrome);
  }
  h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    white-space: nowrap;
  }
  .model {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    color: var(--muted);
    font-family: var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .close {
    border: 0;
    background: transparent;
    font-size: 14px;
    color: var(--muted);
    cursor: pointer;
  }
  .composer {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
    padding: 10px 14px 10px;
    border-bottom: 1px solid var(--rule);
  }
  .actions {
    display: flex;
    padding: 2px;
    border: 1px solid var(--rule);
    border-radius: 5px;
    background: var(--paper-2);
  }
  .mode {
    flex: 1;
    border: 0;
    background: transparent;
    padding: 2px 6px 4px;
    border-radius: 3px;
    font-size: 13.5px;
    color: var(--muted);
    cursor: pointer;
    white-space: nowrap;
  }
  .mode.on {
    background: var(--ink);
    color: var(--paper);
  }
  .target {
    display: grid;
    gap: 2px;
    padding: 6px 8px;
    border-radius: 4px;
    background: var(--accent-wash);
    border-left: 2px solid var(--accent);
  }
  .target.none {
    background: var(--paper-2);
    border-left-color: var(--rule-strong);
  }
  .target .text {
    font-size: 13px;
    color: var(--ink-2);
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .label {
    font-size: 12.5px;
    color: var(--muted);
    white-space: nowrap;
  }
  .options .select {
    flex: 1;
  }
  .refs {
    display: grid;
    gap: 6px;
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
    padding: 0 2px;
  }
  .chips {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chips li {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    max-width: 100%;
    padding: 2px 4px 2px 6px;
    border: 1px solid var(--rule);
    border-radius: 4px;
    background: var(--paper-2);
    font-size: 12px;
  }
  .chips img {
    width: 22px;
    height: 22px;
    object-fit: cover;
    border-radius: 2px;
  }
  .chip-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 220px;
  }
  .x {
    border: 0;
    background: transparent;
    color: var(--faint);
    cursor: pointer;
    font-size: 11px;
  }
  .skill {
    border: 0;
    background: transparent;
    padding: 0;
    font: inherit;
    font-size: 12px;
    color: var(--muted);
    border-bottom: 1px dotted var(--rule-strong);
    cursor: pointer;
  }
  .skill:hover {
    color: var(--accent);
  }
  .spacer {
    flex: 1;
  }
  .jobs {
    overflow-y: auto;
    overflow-x: hidden;
    grid-template-columns: minmax(0, 1fr);
    padding: 10px 12px 40px;
    display: grid;
    align-content: start;
    gap: 10px;
    min-height: 0;
  }
  .empty {
    margin: 12px 4px;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  .clear {
    justify-self: center;
    color: var(--muted);
  }
  .hint {
    margin: 0;
    font-size: 12.5px;
    font-style: italic;
    color: var(--muted);
  }
</style>
