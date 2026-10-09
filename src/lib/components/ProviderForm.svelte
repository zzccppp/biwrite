<script lang="ts">
  import { errorMessage, settingsIpc } from "../ipc";
  import { KIND_LABELS, type Effort, type ProviderConfig, type ProviderView, type SettingsView } from "../types";

  interface Props {
    /** Provider being edited; `id === ""` for a new one. */
    provider: ProviderView;
    onsaved: (view: SettingsView, id: string) => void;
    ondeleted: (view: SettingsView) => void;
  }

  let { provider, onsaved, ondeleted }: Props = $props();

  // Local draft, reset whenever another provider is selected.
  let draft = $state<ProviderConfig>({
    id: "",
    name: "",
    kind: "openai_compatible",
    baseUrl: "",
    model: "",
    temperature: 0,
    effort: "low",
  });
  let keyInput = $state("");
  let models = $state<string[]>([]);
  let busy = $state<"" | "save" | "key" | "test" | "models" | "delete">("");
  let message = $state<{ ok: boolean; text: string } | null>(null);

  let lastId: string | null = null;
  $effect(() => {
    if (provider.id !== lastId || provider.id === "") {
      lastId = provider.id;
      draft = copy(provider);
      keyInput = "";
      models = [];
      message = null;
    }
  });

  const isNew = $derived(provider.id === "");
  const anthropic = $derived(draft.kind === "anthropic");
  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(copy(provider)));
  const listId = $derived(`models-${provider.id || "new"}`);

  function copy(p: ProviderConfig): ProviderConfig {
    return {
      id: p.id,
      name: p.name,
      kind: p.kind,
      baseUrl: p.baseUrl,
      model: p.model,
      temperature: p.temperature,
      effort: p.effort,
    };
  }

  async function act<T>(kind: typeof busy, f: () => Promise<T>): Promise<T | undefined> {
    busy = kind;
    message = null;
    try {
      return await f();
    } catch (err) {
      message = { ok: false, text: errorMessage(err) };
      return undefined;
    } finally {
      busy = "";
    }
  }

  async function save(): Promise<void> {
    const key = keyInput.trim();
    await act("save", async () => {
      const view = await settingsIpc.saveProvider($state.snapshot(draft));
      const id = isNew ? (view.providers.at(-1)?.id ?? "") : draft.id;
      // Report the saved provider right away, so a failing key below can't
      // leave the form "new" (and a second click create a duplicate).
      onsaved(view, id);
      resetFrom(view, id);
      if (key && id) {
        onsaved(await settingsIpc.setApiKey(id, key), id);
        keyInput = "";
      }
      message = { ok: true, text: "Saved." };
    });
  }

  /** Take the normalized config back from Rust (e.g. trailing "/" removed). */
  function resetFrom(view: SettingsView, id: string): void {
    const saved = view.providers.find((p) => p.id === id);
    if (saved) {
      draft = copy(saved);
      lastId = id;
    }
  }

  async function saveKey(): Promise<void> {
    await act("key", async () => {
      const view = await settingsIpc.setApiKey(provider.id, keyInput);
      keyInput = "";
      message = { ok: true, text: "Key stored in the system keychain." };
      onsaved(view, provider.id);
    });
  }

  async function removeKey(): Promise<void> {
    await act("key", async () => onsaved(await settingsIpc.clearApiKey(provider.id), provider.id));
  }

  async function test(): Promise<void> {
    const out = await act("test", () => settingsIpc.test(provider.id));
    if (out !== undefined) message = { ok: true, text: out };
  }

  async function fetchModels(): Promise<void> {
    const list = await act("models", () => settingsIpc.listModels(provider.id));
    if (list) {
      models = list;
      message = { ok: true, text: `${list.length} models available — pick one from the model field.` };
    }
  }

  async function remove(): Promise<void> {
    await act("delete", async () => ondeleted(await settingsIpc.deleteProvider(provider.id)));
  }

  const efforts: { value: Effort; label: string }[] = [
    { value: "low", label: "Low (fast)" },
    { value: "medium", label: "Medium" },
    { value: "high", label: "High" },
    { value: "default", label: "Model default" },
  ];
</script>

<div class="form">
  <label class="field">
    <span class="label">Name</span>
    <input class="input" bind:value={draft.name} placeholder="e.g. DeepSeek" />
  </label>

  <div class="two">
    <label class="field">
      <span class="label">API type</span>
      <select class="select" bind:value={draft.kind}>
        <option value="openai_compatible">{KIND_LABELS.openai_compatible}</option>
        <option value="anthropic">{KIND_LABELS.anthropic}</option>
      </select>
    </label>
    <label class="field">
      <span class="label">Temperature · {draft.temperature.toFixed(2)}</span>
      <input type="range" min="0" max="0.3" step="0.05" bind:value={draft.temperature} disabled={anthropic} />
    </label>
  </div>
  {#if anthropic}
    <p class="note">Current Claude models fix sampling, so temperature is not sent to them.</p>
  {/if}

  <label class="field">
    <span class="label">Base URL</span>
    <input class="input mono" bind:value={draft.baseUrl} spellcheck="false" placeholder="https://api.example.com/v1" />
  </label>

  <div class="field">
    <span class="label">Model</span>
    <div class="row">
      <input class="input mono" list={listId} bind:value={draft.model} spellcheck="false" placeholder="model id" />
      <button class="btn" onclick={fetchModels} disabled={isNew || !!busy || (provider.needsKey && !provider.hasKey)}>
        {busy === "models" ? "Loading…" : "List models"}
      </button>
    </div>
    <datalist id={listId}>
      {#each models as m (m)}<option value={m}></option>{/each}
    </datalist>
  </div>

  {#if anthropic}
    <label class="field">
      <span class="label">Effort</span>
      <select class="select" bind:value={draft.effort}>
        {#each efforts as e (e.value)}<option value={e.value}>{e.label}</option>{/each}
      </select>
      <span class="hint">Low is plenty for translation and keeps latency down.</span>
    </label>
  {/if}

  <div class="field">
    <span class="label">API key</span>
    {#if provider.hasKey}
      <div class="row">
        <span class="stored">Stored in the system keychain</span>
        <button class="btn danger" onclick={removeKey} disabled={!!busy}>Remove</button>
      </div>
      <input
        class="input mono"
        type="password"
        autocomplete="off"
        bind:value={keyInput}
        placeholder="Paste a new key to replace it"
      />
    {:else}
      <input class="input mono" type="password" autocomplete="off" bind:value={keyInput} placeholder="Paste the key" />
    {/if}
    <span class="hint">Kept in the keychain; BiWrite never shows it again.</span>
    {#if !isNew && keyInput.trim()}
      <div><button class="btn" onclick={saveKey} disabled={!!busy}>Save key</button></div>
    {/if}
  </div>

  <div class="row actions">
    <button class="btn primary" onclick={save} disabled={!!busy || (!dirty && !isNew && !keyInput.trim())}>
      {busy === "save" ? "Saving…" : isNew ? "Add provider" : "Save"}
    </button>
    <button class="btn" onclick={test} disabled={isNew || dirty || !!busy}>
      {busy === "test" ? "Translating…" : "Test"}
    </button>
    <span class="spacer"></span>
    {#if !isNew}
      <button class="btn danger" onclick={remove} disabled={!!busy}>Delete</button>
    {/if}
  </div>

  {#if message}
    <p class="message" class:error={!message.ok} lang={message.ok ? "zh-CN" : "en"}>{message.text}</p>
  {/if}
</div>

<style>
  .form {
    padding: 14px 16px 6px;
    border: 1px solid var(--rule);
    border-radius: 6px;
    background: var(--paper-2);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  input[type="range"] {
    width: 100%;
    accent-color: var(--accent);
  }
  .note {
    margin: -6px 0 12px;
    font-size: 12px;
    font-style: italic;
    color: var(--faint);
  }
  .stored {
    flex: 1;
    font-size: 13px;
    color: var(--ink-2);
  }
  .stored::before {
    content: "●";
    margin-right: 6px;
    font-size: 9px;
    vertical-align: 2px;
    color: #3f8f5a;
  }
  .row .input {
    flex: 1;
  }
  .actions {
    margin: 4px 0 10px;
  }
  .spacer {
    flex: 1;
  }
  .message {
    margin: 0 0 10px;
    padding: 6px 9px;
    border-left: 2px solid var(--accent);
    background: var(--accent-wash);
    font-size: 13px;
    user-select: text;
    -webkit-user-select: text;
    overflow-wrap: anywhere;
  }
  .message:lang(zh-CN) {
    font-family: var(--font-zh);
  }
  .message.error {
    border-color: var(--error);
    background: var(--seal-wash);
    color: var(--error);
    font-family: var(--font-ui);
  }
</style>
