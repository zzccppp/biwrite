<script lang="ts">
  import { count, type MessageKey, t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { Effort, ProviderConfig, ProviderView, ServiceTier, SettingsView } from "../types";
  import KeyManager from "./KeyManager.svelte";

  interface Props {
    /** Provider being edited; `id === ""` for a new one. */
    provider: ProviderView;
    onsaved: (view: SettingsView, id: string) => void;
    ondeleted: (view: SettingsView) => void;
  }

  let { provider, onsaved, ondeleted }: Props = $props();

  /** The three request formats, as one choice. */
  type Api = "chat" | "responses" | "anthropic";

  // Local draft, reset whenever another provider is selected.
  let draft = $state<ProviderConfig>({
    id: "",
    name: "",
    kind: "openai_compatible",
    baseUrl: "",
    model: "",
    temperature: 0,
    effort: "low",
    wireApi: "chat",
    serviceTier: null,
    keyConcurrency: null,
    maxRetries: null,
  });
  let keyInput = $state("");
  let models = $state<string[]>([]);
  let managing = $state(false);
  let busy = $state<"" | "save" | "key" | "test" | "models" | "delete">("");
  let message = $state<{ ok: boolean; text: string } | null>(null);

  let lastId: string | null = null;
  $effect(() => {
    if (provider.id !== lastId || provider.id === "") {
      lastId = provider.id;
      draft = copy(provider);
      keyInput = "";
      models = [];
      managing = false;
      message = null;
    }
  });

  const isNew = $derived(provider.id === "");
  const api = $derived<Api>(
    draft.kind === "anthropic" ? "anthropic" : draft.wireApi === "responses" ? "responses" : "chat",
  );
  const dirty = $derived(JSON.stringify(draft) !== JSON.stringify(copy(provider)));
  const listId = $derived(`models-${provider.id || "new"}`);
  const pasted = $derived(
    keyInput
      .split(/[\s,]+/)
      .map((k) => k.trim())
      .filter(Boolean).length,
  );

  function copy(p: ProviderConfig): ProviderConfig {
    return {
      id: p.id,
      name: p.name,
      kind: p.kind,
      baseUrl: p.baseUrl,
      model: p.model,
      temperature: p.temperature,
      effort: p.effort,
      wireApi: p.wireApi ?? "chat",
      serviceTier: p.serviceTier ?? null,
      keyConcurrency: p.keyConcurrency ?? null,
      maxRetries: p.maxRetries ?? null,
    };
  }

  function setApi(next: Api): void {
    draft.kind = next === "anthropic" ? "anthropic" : "openai_compatible";
    draft.wireApi = next === "responses" ? "responses" : "chat";
    if (next === "anthropic") draft.serviceTier = null;
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
      message = { ok: true, text: t("common.saved") };
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

  /** Store the pasted keys (the first ones of this provider). */
  async function storeKeys(): Promise<void> {
    await act("key", async () => {
      const view = await settingsIpc.setApiKey(provider.id, keyInput);
      keyInput = "";
      const n = view.providers.find((p) => p.id === provider.id)?.keyCount ?? 0;
      message = { ok: true, text: t("keys.stored", { n }) };
      onsaved(view, provider.id);
    });
  }

  async function test(): Promise<void> {
    const out = await act("test", () => settingsIpc.test(provider.id));
    if (out !== undefined) message = { ok: true, text: out };
  }

  async function fetchModels(): Promise<void> {
    const list = await act("models", () => settingsIpc.listModels(provider.id));
    if (list) {
      models = list;
      message = { ok: true, text: count(list.length, "provider.models.one", "provider.models.many") };
    }
  }

  async function remove(): Promise<void> {
    await act("delete", async () => ondeleted(await settingsIpc.deleteProvider(provider.id)));
  }

  const apis: { value: Api; label: MessageKey }[] = [
    { value: "chat", label: "provider.api.chat" },
    { value: "responses", label: "provider.api.responses" },
    { value: "anthropic", label: "provider.api.anthropic" },
  ];

  const efforts: { value: Effort; label: MessageKey }[] = [
    { value: "low", label: "provider.effort.low" },
    { value: "medium", label: "provider.effort.medium" },
    { value: "high", label: "provider.effort.high" },
    { value: "default", label: "provider.effort.default" },
  ];

  const tiers: { value: ServiceTier | ""; label: MessageKey }[] = [
    { value: "", label: "provider.tier.none" },
    { value: "priority", label: "provider.tier.priority" },
    { value: "flex", label: "provider.tier.flex" },
    { value: "default", label: "common.default" },
  ];

</script>

<div class="form">
  <label class="field">
    <span class="label">{t("provider.name")}</span>
    <input class="input" bind:value={draft.name} placeholder={t("provider.namePlaceholder")} />
  </label>

  <div class="two">
    <label class="field">
      <span class="label">{t("provider.api")}</span>
      <select class="select" value={api} onchange={(e) => setApi(e.currentTarget.value as Api)}>
        {#each apis as a (a.value)}<option value={a.value}>{t(a.label)}</option>{/each}
      </select>
    </label>
    <label class="field">
      <span class="label">{t("provider.temperature", { value: draft.temperature.toFixed(2) })}</span>
      <input type="range" min="0" max="0.3" step="0.05" bind:value={draft.temperature} disabled={api !== "chat"} />
    </label>
  </div>
  {#if api === "anthropic"}
    <p class="note">{t("provider.noteAnthropic")}</p>
  {:else if api === "responses"}
    <p class="note">{t("provider.noteResponses")}</p>
  {/if}

  <label class="field">
    <span class="label">{t("provider.baseUrl")}</span>
    <input class="input mono" bind:value={draft.baseUrl} spellcheck="false" placeholder="https://api.example.com/v1" />
  </label>

  <div class="field">
    <span class="label">{t("provider.model")}</span>
    <div class="row">
      <input
        class="input mono"
        list={listId}
        bind:value={draft.model}
        spellcheck="false"
        placeholder={t("provider.modelPlaceholder")}
      />
      <button class="btn" onclick={fetchModels} disabled={isNew || !!busy || (provider.needsKey && !provider.hasKey)}>
        {busy === "models" ? t("common.loading") : t("provider.listModels")}
      </button>
    </div>
    <datalist id={listId}>
      {#each models as m (m)}<option value={m}></option>{/each}
    </datalist>
  </div>

  {#if api !== "chat" || draft.kind === "openai_compatible"}
    <div class="two">
      {#if api !== "chat"}
        <label class="field">
          <span class="label">{api === "anthropic" ? t("provider.effort") : t("provider.reasoningEffort")}</span>
          <select class="select" bind:value={draft.effort}>
            {#each efforts as e (e.value)}<option value={e.value}>{t(e.label)}</option>{/each}
          </select>
        </label>
      {/if}
      {#if api !== "anthropic"}
        <label class="field">
          <span class="label">{t("provider.tier")}</span>
          <select
            class="select"
            value={draft.serviceTier ?? ""}
            onchange={(e) => (draft.serviceTier = (e.currentTarget.value || null) as ServiceTier | null)}
          >
            {#each tiers as tier (tier.value)}<option value={tier.value}>{t(tier.label)}</option>{/each}
          </select>
        </label>
      {/if}
    </div>
    {#if api === "responses"}
      <p class="note">{t("provider.tierNote")}</p>
    {/if}
  {/if}

  <div class="field">
    <span class="label">
      {t("provider.keys")}{#if provider.keyCount > 0}&nbsp;· {t("keys.inKeychain", { n: provider.keyCount })}{/if}
    </span>
    {#if !isNew && provider.keyCount > 0}
      <div class="row">
        <span class="stored">
          {provider.keyCount === 1 ? t("keys.count.one") : t("keys.count.many", { n: provider.keyCount })}{#if provider.keyConcurrency}
            · {t("keys.perKey")} {provider.keyConcurrency}{/if}
        </span>
        <button class="btn" onclick={() => (managing = true)} disabled={!!busy}>{t("keys.manage")}</button>
      </div>
    {:else}
      <textarea
        class="textarea mono"
        rows={pasted > 1 ? 4 : 2}
        autocomplete="off"
        spellcheck="false"
        bind:value={keyInput}
        placeholder={t("keys.paste")}
      ></textarea>
      <span class="hint">{t("keys.hint")}</span>
      {#if !isNew && pasted > 0}
        <div><button class="btn" onclick={storeKeys} disabled={!!busy}>{t("keys.saveFirst")}</button></div>
      {/if}
    {/if}
  </div>

  <div class="row actions">
    <button class="btn primary" onclick={save} disabled={!!busy || (!dirty && !isNew && !keyInput.trim())}>
      {busy === "save" ? t("common.saving") : isNew ? t("provider.add") : t("common.save")}
    </button>
    <button class="btn" onclick={test} disabled={isNew || dirty || !!busy}>
      {busy === "test" ? t("provider.testing") : t("common.test")}
    </button>
    <span class="spacer"></span>
    {#if !isNew}
      <button class="btn danger" onclick={remove} disabled={!!busy}>{t("common.delete")}</button>
    {/if}
  </div>

  {#if message}
    <p class="message" class:error={!message.ok} lang={message.ok ? "zh-CN" : "en"}>{message.text}</p>
  {/if}
</div>

{#if managing}
  <KeyManager
    {provider}
    onchange={(view) => onsaved(view, provider.id)}
    onclose={() => (managing = false)}
  />
{/if}

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
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
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
