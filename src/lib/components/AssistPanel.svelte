<script lang="ts">
  import { type AssistJob, type AssistStore, ACTION_LABEL, FIGURE_LABEL, PLACEHOLDER, SCOPE_LABEL } from "../assist.svelte";
  import { t } from "../i18n.svelte";
  import { onMount } from "svelte";
  import { assistIpc, errorMessage, ipc } from "../ipc";
  import type { AssistAction, AssistScope, FigureFormat, ImageUse, MaterialEntry } from "../types";
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
    /** A LaTeX file that compiles: fixes and Accept build it. */
    latex: boolean;
    /** A LaTeX document: a figure can be a file of its own. */
    latexDoc: boolean;
    onrun: () => void;
    onaccept: (job: AssistJob) => void;
    onrevert: (job: AssistJob) => void;
    onagain: (job: AssistJob) => void;
    onreapply: (job: AssistJob) => void;
    onfollowup: (job: AssistJob, question: string) => Promise<boolean>;
    onfocus: (job: AssistJob) => void;
    onclose: () => void;
  }

  let {
    store,
    target,
    ready,
    model,
    skill,
    latex,
    latexDoc,
    onrun,
    onaccept,
    onrevert,
    onagain,
    onreapply,
    onfollowup,
    onfocus,
    onclose,
  }: Props = $props();

  let instructionBox = $state<HTMLTextAreaElement>();
  let sampleDraft = $state("");

  /** Put the cursor in the instruction box (⌘K). */
  export function focus(): void {
    instructionBox?.focus();
  }
  let addingSample = $state(false);

  // Polishing takes an optional instruction, so it also does what "edit" did.
  // Fixing builds the document, so only a LaTeX file has it.
  const actions = $derived<AssistAction[]>(
    latex ? ["polish", "ask", "write", "figure", "mirror", "fix"] : ["polish", "ask", "write", "figure", "mirror"],
  );
  $effect(() => {
    if (!latex && store.action === "fix") store.action = "polish";
  });
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
  const formats: FigureFormat[] = ["latex", "pdf", "pptx"];
  const USES: ImageUse[] = ["trace", "style"];

  /** The papers and figures of the materials folder, read again whenever
   * the window comes back (files may have been put in meanwhile). */
  let materials = $state<MaterialEntry[] | null>(null);
  let materialError = $state<string | null>(null);
  async function loadMaterials(): Promise<void> {
    try {
      materials = await assistIpc.listMaterials();
    } catch {
      materials = [];
    }
  }
  onMount(() => {
    void loadMaterials();
    const onFocus = () => void loadMaterials();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });
  /** The materials by folder: figures first for a figure, texts first otherwise. */
  const materialGroups = $derived.by(() => {
    const order =
      store.action === "figure"
        ? ["figures", "domain", "general", "writing-samples"]
        : ["writing-samples", "domain", "general", "figures"];
    return order
      .map((name) => ({ name, items: (materials ?? []).filter((m) => m.folder === name) }))
      .filter((g) => g.items.length > 0);
  });
  /** Not a path of the list: those start with a folder name. */
  const OPEN_FOLDER = "#open";
  async function pickMaterial(path: string): Promise<void> {
    materialError = null;
    if (path === OPEN_FOLDER) {
      await assistIpc.revealMaterials().catch((err) => (materialError = errorMessage(err)));
      return;
    }
    const entry = materials?.find((m) => m.path === path);
    if (!entry) return;
    try {
      await store.addMaterial(entry);
    } catch (err) {
      materialError = errorMessage(err);
    }
  }
  const fixing = $derived(store.action === "fix");
  const needsInstruction = $derived(store.action !== "polish" && !fixing);
  const mirrorReady = $derived(
    store.action !== "mirror" ||
      (!!target?.translation && store.instruction.trim().length > 0 && store.instruction.trim() !== target.translation.trim()),
  );
  const canRun = $derived(
    ready && (fixing || !!target) && (!needsInstruction || store.instruction.trim().length > 0) && mirrorReady,
  );

  function addSample(): void {
    const text = sampleDraft.trim();
    if (text) store.addText(text);
    sampleDraft = "";
    addingSample = false;
  }

  /** "12,345 characters" / "1.2 万字", for a reference from a file. */
  function size(text: string): string {
    const n = [...text].length;
    return t("assist.chars", { n: n >= 10000 ? `${(n / 10000).toFixed(1)}${t("assist.tenThousand")}` : String(n) });
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

    {#if fixing}
      <div class="target">
        <span class="label smallcaps">{t("assist.target.document")}</span>
      </div>
    {:else}
      <div class="target" class:none={!target}>
        <span class="label smallcaps">{target ? target.label : t("assist.target")}</span>
        <span class="text">{target ? target.text : t("assist.target.none")}</span>
      </div>
    {/if}

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

    {#if store.action === "figure" && latexDoc}
      <div class="row options">
        <span class="label smallcaps">{t("assist.figure.format")}</span>
        <select
          class="select"
          value={store.figureFormat}
          onchange={(e) => store.setFigureFormat(e.currentTarget.value as FigureFormat)}
        >
          {#each formats as f (f)}<option value={f}>{t(FIGURE_LABEL[f])}</option>{/each}
        </select>
      </div>
    {/if}

    {#if !fixing}
    <div class="row options">
      <span class="label smallcaps">{t("assist.scope")}</span>
      <select class="select" value={store.scope} onchange={(e) => store.setScope(e.currentTarget.value as AssistScope)}>
        {#each scopes as s (s)}<option value={s}>{t(SCOPE_LABEL[s])}</option>{/each}
      </select>
    </div>

    <div class="refs">
      <div class="row adds-row">
        <span class="label smallcaps">{t("assist.references")}</span>
        <span class="spacer"></span>
        <span class="adds">
          <button class="link" onclick={() => (addingSample = !addingSample)}>{t("assist.addText")}</button>
          <button class="link" onclick={() => store.addFile()} title={t("assist.addFileTitle")}>{t("assist.addFile")}</button>
          <button class="link" onclick={() => store.attach()}>{t("assist.addImage")}</button>
          <span class="pick-wrap">
            <span class="link" aria-hidden="true">{t("assist.addMaterial")}</span>
            <select
              class="pick"
              aria-label={t("assist.addMaterial")}
              onchange={(e) => {
                const path = e.currentTarget.value;
                e.currentTarget.value = "";
                void pickMaterial(path);
              }}
            >
              <option value="" disabled selected hidden>{t("assist.addMaterial")}</option>
              {#each materialGroups as g (g.name)}
                <optgroup label={`${g.name}/`}>
                  {#each g.items as m (m.path)}<option value={m.path}>{m.path.slice(g.name.length + 1)}</option>{/each}
                </optgroup>
              {/each}
              {#if materials && materialGroups.length === 0}<option value="" disabled>{t("assist.materialsEmpty")}</option>{/if}
              <option value={OPEN_FOLDER}>{t("assist.materialsOpen")}</option>
            </select>
          </span>
        </span>
      </div>
      {#if materialError}<p class="material-error">{materialError}</p>{/if}
      {#if addingSample}
        <textarea class="textarea" rows="3" bind:value={sampleDraft} placeholder={t("assist.samplePlaceholder")}
        ></textarea>
        <div class="row"><button class="btn small" onclick={addSample} disabled={!sampleDraft.trim()}>{t("common.ok")}</button></div>
      {/if}
      {#if store.samples.length || store.attachments.length}
        <ul class="chips">
          {#each store.samples as s, i (i)}
            <li title={s.truncated ? t("assist.truncated") : undefined}>
              {#if s.name}
                <span class="chip-text"
                  ><span class="file">{s.name}</span> · {size(s.text)}{s.truncated ? ` · ${t("assist.cut")}` : ""}</span
                >
              {:else}
                <span class="chip-text">“{s.text.slice(0, 40)}{s.text.length > 40 ? "…" : ""}”</span>
              {/if}
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
              {#if store.action === "figure" && latex}
                <span class="use" role="group">
                  {#each USES as u (u)}
                    <button
                      class:on={a.use === u}
                      aria-pressed={a.use === u}
                      title={t(`assist.image.${u}Title`)}
                      onclick={() => store.setImageUse(a.id, u)}>{t(`assist.image.${u}`)}</button
                    >
                  {/each}
                </span>
              {/if}
              <button class="x" aria-label={t("common.remove")} onclick={() => store.detach(a.id)}>✕</button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
    {/if}

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
        held={store.adopting}
        {onaccept}
        oncancelcheck={(j) => store.cancelCheck(j)}
        onstoptidy={(j) => store.stopTidy(j)}
        {onrevert}
        ondiscard={(j) => store.discard(j)}
        onremove={(j) => store.remove(j)}
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
    flex-wrap: wrap;
    padding: 2px;
    border: 1px solid var(--rule);
    border-radius: 5px;
    background: var(--paper-2);
  }
  .mode {
    flex: 1 0 auto;
    border: 0;
    background: transparent;
    padding: 2px 4px 4px;
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
  /* The links stay together: in a narrow dock they go under the label. */
  .adds-row {
    flex-wrap: wrap;
    row-gap: 2px;
  }
  .adds {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-left: auto;
    white-space: nowrap;
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
  /* The menu of materials opens from its label: the select lies over it,
     transparent. */
  .pick-wrap {
    position: relative;
    display: inline-block;
  }
  .pick {
    position: absolute;
    inset: 0;
    width: 100%;
    opacity: 0;
    cursor: pointer;
    font-size: 13px;
  }
  .material-error {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--error);
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
    max-width: 260px;
  }
  .use {
    display: inline-flex;
    flex: none;
    border: 1px solid var(--rule);
    border-radius: 3px;
    overflow: hidden;
  }
  .use button {
    border: 0;
    background: transparent;
    padding: 0 5px;
    font-size: 11.5px;
    line-height: 18px;
    color: var(--muted);
    cursor: pointer;
    white-space: nowrap;
  }
  .use button.on {
    background: var(--ink);
    color: var(--paper);
  }
  .chip-text .file {
    font-family: var(--font-mono);
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
