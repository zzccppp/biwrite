<script lang="ts">
  import { errorMessage, settingsIpc } from "../ipc";
  import type { Preset, ProviderView, SettingsView } from "../types";
  import { KIND_LABELS } from "../types";
  import PromptEditor from "./PromptEditor.svelte";
  import ProviderForm from "./ProviderForm.svelte";

  interface Props {
    settings: SettingsView;
    onchange: (view: SettingsView) => void;
    onclose: () => void;
    onglossary: () => void;
  }

  let { settings, onchange, onclose, onglossary }: Props = $props();

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
      hasKey: false,
      keyCount: 0,
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
</script>

<svelte:window onkeydown={onKeydown} />

<div class="scrim" role="presentation" onclick={onclose}></div>
<aside class="drawer" aria-label="Settings">
  <header>
    <h2 class="smallcaps">Settings</h2>
    <button class="close" onclick={onclose} aria-label="Close settings">✕</button>
  </header>

  <div class="body">
    <section>
      <h3 class="smallcaps">Translation provider</h3>
      <ul class="providers" role="radiogroup" aria-label="Active provider">
        {#each settings.providers as p (p.id)}
          <li class:selected={!draftNew && editing?.id === p.id}>
            <input
              type="radio"
              name="active-provider"
              checked={settings.activeProvider === p.id}
              aria-label="Use {p.name}"
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
                {p.builtin ? "offline" : describe(p)}
                {#if p.needsKey && !p.hasKey}<span class="nokey"> · no key</span>{:else if p.keyCount > 1}
                  · {p.keyCount} keys{/if}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      <select class="select add" onchange={onPreset} aria-label="Add a provider">
        <option value="" selected>+ Add provider…</option>
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
        <p class="explain">
          The mock provider reverses each paragraph after a short delay. Use it to try BiWrite without an API key. Add a
          real provider above.
        </p>
      {/if}
    </section>

    <section>
      <h3 class="smallcaps">Writing assistant</h3>
      <label class="field">
        <span class="label">Model for polishing, edits and questions</span>
        <select
          class="select"
          value={settings.assistantProvider}
          onchange={(e) => run(() => settingsIpc.setAssistantProvider(e.currentTarget.value))}
        >
          <option value="">Same as translation ({settings.activeLabel})</option>
          {#each assistantChoices as p (p.id)}
            <option value={p.id}>{p.name} · {p.model}</option>
          {/each}
        </select>
        <span class="hint">
          {settings.assistantReady
            ? `Uses ${settings.assistantLabel}. A fast model can translate while a stronger one writes.`
            : "The offline mock cannot write. Add a provider above and choose it here."}
        </span>
      </label>
    </section>

    <section>
      <h3 class="smallcaps">This document</h3>
      <label class="field">
        <span class="label">Note for the translator</span>
        <textarea
          class="textarea"
          rows="2"
          bind:value={note}
          onblur={saveNote}
          placeholder="e.g. ML paper on in-context learning in graph models"
        ></textarea>
        <span class="hint">Sent with every paragraph of this file.</span>
      </label>
      <label class="field">
        <span class="label">Parallel requests · {settings.concurrency}</span>
        <input
          type="range"
          min="1"
          max="8"
          step="1"
          value={settings.concurrency}
          onchange={(e) => run(() => settingsIpc.setConcurrency(Number(e.currentTarget.value)))}
        />
      </label>
    </section>

    <section>
      <h3 class="smallcaps">Glossary</h3>
      <div class="row glossary">
        <p class="explain grow">Preferred Chinese for your terms, or keep them in English. Used for every document.</p>
        <button class="btn" onclick={onglossary}>Edit glossary…</button>
      </div>
    </section>

    <section>
      <h3 class="smallcaps">System prompt</h3>
      <PromptEditor />
    </section>

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
