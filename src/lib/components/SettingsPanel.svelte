<script lang="ts">
  import { onMount } from "svelte";
  import { count, language, t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { LatexStore } from "../latex.svelte";
  import type { Preset, ProviderView, SettingsView, SkillInfo } from "../types";
  import { KIND_LABELS } from "../types";
  import LatexSection from "./LatexSection.svelte";
  import PromptEditor from "./PromptEditor.svelte";
  import ProviderForm from "./ProviderForm.svelte";
  import SkillSection from "./SkillSection.svelte";
  import StorageSection from "./StorageSection.svelte";
  import UpdateSection from "./UpdateSection.svelte";

  interface Props {
    settings: SettingsView;
    latex: LatexStore;
    /** Unsaved changes in the open document. */
    dirty: boolean;
    /** Scroll to a section when opening (e.g. "updates"). */
    focus?: string | null;
    onchange: (view: SettingsView) => void;
    onskill: (skill: SkillInfo) => void;
    onclose: () => void;
    onglossary: () => void;
  }

  let { settings, latex, dirty, focus = null, onchange, onskill, onclose, onglossary }: Props = $props();
  let body = $state<HTMLElement>();

  let selectedId = $state<string | null>(null);
  let draftNew = $state<ProviderView | null>(null);
  let error = $state<string | null>(null);
  let note = $state("");
  let noteSaved = $state("");

  // Track the document note coming from Rust (e.g. after opening a file).
  $effect(() => {
    note = settings.docNote;
    noteSaved = settings.docNote;
  });

  const editing = $derived(
    draftNew ?? settings.providers.find((p) => p.id === (selectedId ?? settings.activeProvider)) ?? null,
  );

  async function run(f: () => Promise<SettingsView | void>): Promise<void> {
    error = null;
    try {
      const view = await f();
      if (view) onchange(view);
    } catch (err) {
      error = errorMessage(err);
    }
  }

  function startNew(preset: Preset): void {
    draftNew = {
      id: "",
      name: preset.name,
      kind: preset.kind,
      baseUrl: preset.baseUrl,
      model: preset.model,
      temperature: 0,
      effort: preset.effort,
      wireApi: preset.wireApi,
      serviceTier: preset.serviceTier,
      keyConcurrency: preset.keyConcurrency,
      maxRetries: preset.maxRetries,
      hasKey: false,
      keyCount: 0,
      keyNames: {},
      builtin: false,
      needsKey: true,
    };
  }

  /** "OpenAI Responses · gpt-6-astra", "Anthropic · claude-opus-5-5". */
  function describe(p: ProviderView): string {
    const api = p.kind === "openai_compatible" && p.wireApi === "responses" ? "OpenAI Responses" : KIND_LABELS[p.kind];
    return `${api} · ${p.model}`;
  }

  const assistantChoices = $derived(settings.providers.filter((p) => !p.builtin));
  /** The active provider, if its keys carry a per-key limit. */
  const pooled = $derived(
    settings.providers.find((p) => p.id === settings.activeProvider && p.keyConcurrency && p.keyCount > 0) ?? null,
  );

  function onPreset(e: Event): void {
    const select = e.currentTarget as HTMLSelectElement;
    const preset = settings.presets[Number(select.value)];
    select.value = "";
    if (preset) startNew(preset);
  }

  function saveNote(): void {
    if (note === noteSaved) return;
    const value = note;
    void run(async () => {
      await settingsIpc.setDocNote(value);
      noteSaved = value;
    });
  }

  function onKeydown(e: KeyboardEvent): void {
    if (e.key === "Escape") onclose();
  }

  onMount(() => {
    if (focus) requestAnimationFrame(() => body?.querySelector(`#${focus}`)?.scrollIntoView({ block: "start" }));
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="scrim" role="presentation" onclick={onclose}></div>
<aside class="drawer" aria-label={t("settings.title")}>
  <header>
    <h2 class="smallcaps">{t("settings.title")}</h2>
    <button class="close" onclick={onclose} aria-label={t("common.close")}>✕</button>
  </header>

  <div class="body" bind:this={body}>
    <section>
      <h3 class="smallcaps">{t("settings.language")}</h3>
      <div class="row">
        <div class="langs" role="radiogroup" aria-label={t("settings.language")}>
          <button
            class="lang smallcaps"
            class:on={language.current === "en"}
            role="radio"
            aria-checked={language.current === "en"}
            onclick={() => language.set("en")}>English</button
          >
          <button
            class="lang"
            class:on={language.current === "zh"}
            role="radio"
            aria-checked={language.current === "zh"}
            lang="zh-CN"
            onclick={() => language.set("zh")}>中文</button
          >
        </div>
        <p class="explain grow">{t("settings.languageHint")}</p>
      </div>
    </section>

    <section>
      <h3 class="smallcaps">{t("settings.provider")}</h3>
      <ul class="providers" role="radiogroup" aria-label={t("settings.provider")}>
        {#each settings.providers as p (p.id)}
          <li class:selected={!draftNew && editing?.id === p.id}>
            <input
              type="radio"
              name="active-provider"
              checked={settings.activeProvider === p.id}
              aria-label={t("settings.useProvider", { name: p.name })}
              onchange={() => run(() => settingsIpc.setActive(p.id))}
            />
            <button
              class="pick"
              onclick={() => {
                draftNew = null;
                selectedId = p.id;
              }}
            >
              <span class="name">{p.name}</span>
              <span class="meta">
                {p.builtin ? t("settings.offline") : describe(p)}
                {#if p.needsKey && !p.hasKey}<span class="nokey"> · {t("settings.noKey")}</span>{:else if p.keyCount > 1}
                  · {count(p.keyCount, "keys.count.one", "keys.count.many")}{/if}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      <select class="select add" onchange={onPreset} aria-label={t("settings.addProvider")}>
        <option value="" selected>{t("settings.addProvider")}</option>
        {#each settings.presets as preset, i (preset.name)}
          <option value={i}>{preset.name}</option>
        {/each}
      </select>

      {#if editing && !editing.builtin}
        <ProviderForm
          provider={editing}
          onsaved={(view, id) => {
            onchange(view);
            draftNew = null;
            selectedId = id;
          }}
          ondeleted={(view) => {
            onchange(view);
            selectedId = null;
          }}
        />
      {:else if editing?.builtin}
        <p class="explain">{t("settings.mockHint")}</p>
      {/if}
    </section>

    <section>
      <h3 class="smallcaps">{t("settings.assistant")}</h3>
      <label class="field">
        <span class="label">{t("settings.assistantModel")}</span>
        <select
          class="select"
          value={settings.assistantProvider}
          onchange={(e) => run(() => settingsIpc.setAssistantProvider(e.currentTarget.value))}
        >
          <option value="">{t("settings.assistantSame", { label: settings.activeLabel })}</option>
          {#each assistantChoices as p (p.id)}
            <option value={p.id}>{p.name} · {p.model}</option>
          {/each}
        </select>
        <span class="hint">
          {settings.assistantReady
            ? t("settings.assistantUses", { label: settings.assistantLabel })
            : t("settings.assistantMock")}
        </span>
      </label>
    </section>

    <SkillSection onchange={onskill} />

    <LatexSection {latex} {settings} {onchange} />

    <section>
      <h3 class="smallcaps">{t("settings.document")}</h3>
      <label class="field">
        <span class="label">{t("settings.note")}</span>
        <textarea
          class="textarea"
          rows="2"
          bind:value={note}
          onblur={saveNote}
          placeholder={t("settings.notePlaceholder")}
        ></textarea>
        <span class="hint">{t("settings.noteHint")}</span>
      </label>
    </section>

    <section>
      <h3 class="smallcaps">{t("settings.pacing")}</h3>
      <label class="field">
        <span class="label">{t("settings.batch", { n: settings.batchSize })}</span>
        <input
          type="range"
          min="1"
          max="8"
          step="1"
          value={settings.batchSize}
          onchange={(e) => run(() => settingsIpc.setBatchSize(Number(e.currentTarget.value)))}
        />
        <span class="hint">{t("settings.batchHint")}</span>
      </label>
      <label class="field">
        <span class="label">{t("settings.parallel", { n: settings.effectiveConcurrency })}</span>
        <input
          type="range"
          min="1"
          max="32"
          step="1"
          value={settings.concurrency}
          disabled={pooled !== null && settings.matchPool}
          onchange={(e) => run(() => settingsIpc.setConcurrency(Number(e.currentTarget.value)))}
        />
        {#if pooled}
          <label class="check">
            <input
              type="checkbox"
              checked={settings.matchPool}
              onchange={(e) => run(() => settingsIpc.setMatchPool(e.currentTarget.checked))}
            />
            <span>{t("settings.matchPool", { keys: pooled.keyCount, per: pooled.keyConcurrency ?? 1, n: pooled.keyCount * (pooled.keyConcurrency ?? 1) })}</span>
          </label>
        {/if}
      </label>
    </section>

    <section>
      <h3 class="smallcaps">{t("glossary.title")}</h3>
      <div class="row glossary">
        <p class="explain grow">{t("settings.glossaryHint")}</p>
        <button class="btn" onclick={onglossary}>{t("settings.glossaryEdit")}</button>
      </div>
    </section>

    <section>
      <h3 class="smallcaps">{t("settings.prompt")}</h3>
      <PromptEditor />
    </section>

    <StorageSection logDir={settings.logDir} />

    <UpdateSection {settings} {onchange} {dirty} />

    {#if error}<p class="error">{error}</p>{/if}
  </div>
</aside>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(20, 18, 14, 0.18);
    z-index: 40;
    animation: fade 160ms var(--ease);
  }
  .drawer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(460px, 92vw);
    z-index: 50;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    border-left: 1px solid var(--rule-strong);
    box-shadow: -18px 0 40px -24px rgba(0, 0, 0, 0.35);
    animation: slide 220ms var(--ease);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 44px;
    padding: 0 16px;
    border-bottom: 1px solid var(--rule);
    background: var(--chrome);
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
    flex: 1;
    overflow-y: auto;
    padding: 6px 18px 30px;
  }
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
  .providers {
    list-style: none;
    margin: 0 0 8px;
    padding: 0;
  }
  .providers li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .providers li.selected {
    background: var(--accent-wash);
  }
  .providers input {
    accent-color: var(--seal);
  }
  .pick {
    flex: 1;
    display: grid;
    text-align: left;
    border: 0;
    background: transparent;
    padding: 2px 0;
    font: inherit;
    cursor: pointer;
    min-width: 0;
  }
  .name {
    font-size: 14.5px;
    color: var(--ink);
  }
  .meta {
    font-size: 12px;
    color: var(--muted);
    font-family: var(--font-mono);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .nokey {
    color: var(--error);
  }
  .add {
    margin-bottom: 12px;
  }
  .glossary {
    margin-bottom: 10px;
  }
  .langs {
    display: inline-flex;
    padding: 2px;
    border: 1px solid var(--rule);
    border-radius: 5px;
    background: var(--paper);
    margin: 0 10px 10px 0;
  }
  .lang {
    border: 0;
    background: transparent;
    padding: 1px 10px 3px;
    border-radius: 3px;
    font-size: 13.5px;
    color: var(--muted);
    cursor: pointer;
  }
  .lang:lang(zh-CN) {
    font-family: var(--font-zh);
  }
  .lang.on {
    background: var(--ink);
    color: var(--paper);
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--ink-2);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--seal);
  }
  .explain.grow {
    flex: 1;
    margin: 0;
  }
  .explain {
    margin: 0 0 12px;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .error {
    margin: 12px 0 0;
    color: var(--error);
    font-size: 13px;
  }
  @keyframes slide {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
</style>
