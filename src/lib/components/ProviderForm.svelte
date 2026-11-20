<script lang="ts">
  import { count, type MessageKey, t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import { isLoopbackBaseUrl, mergeModels, sameUrlOrigin, stepOptionIndex, visibleModels } from "../modelSelect";
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
    longContext: null,
  });
  let keyInput = $state("");
  let models = $state<string[]>([]);
  let customModels = $state<string[]>([]);
  let fetchedOrigin = $state("");
  let fetchedKind = $state<ProviderConfig["kind"] | "">("");
  let modelOpen = $state(false);
  let filtering = $state(false);
  let activeIndex = $state(-1);
  let comboEl = $state<HTMLElement>();
  let modelInput = $state<HTMLInputElement>();
  let menuEl = $state<HTMLElement>();
  let managing = $state(false);
  let busy = $state<"" | "save" | "key" | "test" | "models" | "delete">("");
  let message = $state<{ ok: boolean; text: string } | null>(null);

  let lastId: string | null = null;
  let fetchSeq = 0;
  $effect(() => {
    if (provider.id !== lastId || provider.id === "") {
      lastId = provider.id;
      fetchSeq++;
      draft = copy(provider);
      keyInput = "";
      models = [];
      customModels = [];
      fetchedOrigin = "";
      fetchedKind = "";
      modelOpen = false;
      filtering = false;
      activeIndex = -1;
      managing = false;
      message = null;
    }
  });

  // Models belong to one host and API family: changing either clears an earlier list.
  $effect(() => {
    if (fetchedOrigin && (draft.kind !== fetchedKind || !sameUrlOrigin(draft.baseUrl, fetchedOrigin))) {
      fetchSeq++;
      models = [];
      customModels = [];
      fetchedOrigin = "";
      fetchedKind = "";
      modelOpen = false;
      filtering = false;
      activeIndex = -1;
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
  const allModels = $derived(mergeModels(models, customModels));
  const shown = $derived(visibleModels(allModels, draft.model, filtering));
  const totalOptions = $derived(shown.options.length + (shown.canAddCustom ? 1 : 0));
  const showDropdown = $derived(modelOpen && totalOptions > 0);
  const canFetchModels = $derived(
    !busy &&
      draft.baseUrl.trim().length > 0 &&
      (!provider.needsKey ||
        pasted > 0 ||
        isLoopbackBaseUrl(draft.baseUrl) ||
        (!isNew && provider.hasKey && sameUrlOrigin(provider.baseUrl, draft.baseUrl))),
  );

  // Close the model dropdown on an outside click or Escape (capture phase so
  // Escape does not also close the settings drawer underneath).
  $effect(() => {
    if (!showDropdown) return;
    const onMouseDown = (e: MouseEvent) => {
      if (comboEl && !comboEl.contains(e.target as Node)) {
        modelOpen = false;
        filtering = false;
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.isComposing || e.keyCode === 229) return;
      e.preventDefault();
      e.stopPropagation();
      modelOpen = false;
      filtering = false;
    };
    window.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousedown", onMouseDown, true);
      window.removeEventListener("keydown", onKey, true);
    };
  });

  function scrollOptionIntoView(index: number): void {
    requestAnimationFrame(() => {
      const item = menuEl?.children[index] as HTMLElement | undefined;
      item?.scrollIntoView({ block: "nearest" });
    });
  }

  function openDropdown(filterByInput = false): void {
    filtering = filterByInput;
    modelOpen = true;
    const current = draft.model.trim();
    const idx = allModels.indexOf(current);
    activeIndex = idx;
    if (idx >= 0) scrollOptionIntoView(idx);
  }

  function toggleDropdown(): void {
    if (showDropdown && !filtering) {
      modelOpen = false;
      return;
    }
    openDropdown(false);
    modelInput?.focus();
  }

  function pickModel(m: string): void {
    draft.model = m;
    modelOpen = false;
    filtering = false;
  }

  function addCustomModel(m: string): void {
    const trimmed = m.trim();
    if (!trimmed) return;
    if (!models.includes(trimmed) && !customModels.includes(trimmed)) {
      customModels = [...customModels, trimmed];
    }
    draft.model = trimmed;
    modelOpen = false;
    filtering = false;
  }

  function onModelKeydown(e: KeyboardEvent): void {
    if (e.isComposing || e.keyCode === 229) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      if (allModels.length === 0 && !shown.canAddCustom) return;
      e.preventDefault();
      const delta = e.key === "ArrowDown" ? 1 : -1;
      if (!showDropdown) {
        openDropdown(false);
        if (activeIndex < 0) activeIndex = stepOptionIndex(-1, delta, allModels.length);
        if (activeIndex >= 0) scrollOptionIntoView(activeIndex);
        return;
      }
      activeIndex = stepOptionIndex(activeIndex, delta, totalOptions);
      if (activeIndex >= 0) scrollOptionIntoView(activeIndex);
    } else if (e.key === "Enter" && showDropdown) {
      e.preventDefault();
      if (activeIndex >= 0 && activeIndex < shown.options.length) {
        pickModel(shown.options[activeIndex]);
      } else if (shown.canAddCustom && (activeIndex < 0 || activeIndex === shown.options.length)) {
        addCustomModel(shown.customValue);
      } else {
        modelOpen = false;
        filtering = false;
      }
    } else if (e.key === "Tab" && showDropdown) {
      modelOpen = false;
      filtering = false;
    }
  }

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
      longContext: p.longContext ?? null,
    };
  }

  function setApi(next: Api): void {
    // The same host on another API: the Messages API takes the address
    // without the version, the OpenAI API with it.
    const base = draft.baseUrl.trim().replace(/\/+$/, "");
    if (next === "anthropic" && base.endsWith("/v1")) {
      draft.baseUrl = base.slice(0, -3);
    } else if (next !== "anthropic" && draft.kind === "anthropic") {
      try {
        if (new URL(base).pathname === "/") draft.baseUrl = `${base}/v1`;
      } catch {
        // Not an address yet: left as typed.
      }
    }
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
    const typedModel = draft.model.trim();
    modelOpen = false;
    filtering = false;
    await act("save", async () => {
      const view = await settingsIpc.saveProvider($state.snapshot(draft));
      const id = isNew ? (view.providers.at(-1)?.id ?? "") : draft.id;
      // Report the saved provider right away, so a failing key below can't
      // leave the form "new" (and a second click create a duplicate).
      onsaved(view, id);
      resetFrom(view, id);
      if (typedModel && models.length > 0 && !models.includes(typedModel) && !customModels.includes(typedModel)) {
        customModels = [...customModels, typedModel];
      }
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
    if (out !== undefined)
      message = { ok: true, text: `${t("provider.testOk")}\n${t("test.qa", { q: out.question, a: out.answer })}` };
  }

  async function fetchModels(): Promise<void> {
    const seq = ++fetchSeq;
    const requestedId = provider.id;
    const targetUrl = draft.baseUrl;
    const targetKind = draft.kind;
    const list = await act("models", () =>
      settingsIpc.listModels(requestedId, $state.snapshot(draft), keyInput),
    );
    if (
      !list ||
      seq !== fetchSeq ||
      provider.id !== requestedId ||
      draft.kind !== targetKind ||
      !sameUrlOrigin(draft.baseUrl, targetUrl)
    ) {
      return;
    }
    models = list;
    const savedModel = provider.model.trim();
    if (
      !isNew &&
      savedModel &&
      provider.kind === targetKind &&
      sameUrlOrigin(provider.baseUrl, targetUrl) &&
      !list.includes(savedModel) &&
      !customModels.includes(savedModel)
    ) {
      customModels = [...customModels, savedModel];
    }
    fetchedOrigin = targetUrl;
    fetchedKind = targetKind;
    if (list.length === 0 && customModels.length === 0) {
      modelOpen = false;
      message = { ok: true, text: t("provider.models.empty") };
    } else {
      message = {
        ok: true,
        text:
          list.length === 0
            ? t("provider.models.empty")
            : count(list.length, "provider.models.one", "provider.models.many"),
      };
      openDropdown(false);
      requestAnimationFrame(() => modelInput?.focus());
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
    { value: "xhigh", label: "provider.effort.xhigh" },
    { value: "max", label: "provider.effort.max" },
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
      <div class="combo" bind:this={comboEl}>
        <input
          bind:this={modelInput}
          class="input mono"
          class:with-chev={allModels.length > 0}
          role="combobox"
          aria-label={t("provider.model")}
          aria-expanded={showDropdown}
          aria-autocomplete="list"
          aria-controls={showDropdown ? listId : undefined}
          aria-activedescendant={showDropdown && activeIndex >= 0 && activeIndex < totalOptions ? `${listId}-opt-${activeIndex}` : undefined}
          autocomplete="off"
          bind:value={draft.model}
          spellcheck="false"
          placeholder={t("provider.modelPlaceholder")}
          onfocus={() => {
            if (allModels.length > 0 && !modelOpen) openDropdown(false);
          }}
          onclick={() => {
            if (allModels.length > 0 && !modelOpen) openDropdown(false);
          }}
          oninput={() => {
            activeIndex = -1;
            if (allModels.length > 0) {
              filtering = true;
              modelOpen = true;
            }
          }}
          onkeydown={onModelKeydown}
        />
        {#if allModels.length > 0}
          <button
            type="button"
            class="combo-btn"
            class:open={showDropdown}
            tabindex="-1"
            aria-label={t("provider.showModels")}
            title={t("provider.showModels")}
            onmousedown={(e) => e.preventDefault()}
            onclick={toggleDropdown}
          >
            <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 6.5 8 10l3.5-3.5" /></svg>
          </button>
        {/if}
        {#if showDropdown}
          <ul
            class="model-menu mono"
            id={listId}
            role="listbox"
            aria-label={t("provider.model")}
            bind:this={menuEl}
            onmousedown={(e) => {
              if (e.target !== e.currentTarget || e.offsetX < e.currentTarget.clientWidth) e.preventDefault();
            }}
          >
            {#each shown.options as m, i (m)}
              <li role="presentation">
                <button
                  type="button"
                  id={`${listId}-opt-${i}`}
                  role="option"
                  class="model-opt"
                  class:selected={draft.model.trim() === m}
                  class:active={activeIndex === i}
                  aria-selected={draft.model.trim() === m}
                  onmouseenter={() => (activeIndex = i)}
                  onclick={() => pickModel(m)}
                >
                  <span class="opt-name">{m}</span>
                  {#if draft.model.trim() === m}<span class="opt-check" aria-hidden="true">✓</span>{/if}
                </button>
              </li>
            {/each}
            {#if shown.canAddCustom}
              <li role="presentation" class:custom-sep={shown.options.length > 0}>
                <button
                  type="button"
                  id={`${listId}-opt-${shown.options.length}`}
                  role="option"
                  class="model-opt custom"
                  class:active={activeIndex === shown.options.length}
                  aria-selected={false}
                  onmouseenter={() => (activeIndex = shown.options.length)}
                  onclick={() => addCustomModel(shown.customValue)}
                >
                  <span class="opt-name">{t("provider.addCustomModel", { model: shown.customValue })}</span>
                </button>
              </li>
            {/if}
          </ul>
        {/if}
      </div>
      <button class="btn" onclick={fetchModels} disabled={!canFetchModels}>
        {busy === "models" ? t("common.loading") : t("provider.listModels")}
      </button>
    </div>
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

  {#if api === "anthropic"}
    <label class="check" title={t("provider.longContextNote")}>
      <input
        type="checkbox"
        checked={draft.longContext !== false}
        onchange={(e) => (draft.longContext = e.currentTarget.checked ? null : false)}
      />
      <span>{t("provider.longContext")}</span>
    </label>
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
    onchange={(view) => {
      // The limits set there are saved: Save here keeps them, and the
      // form's other edits stay as typed.
      const saved = view.providers.find((p) => p.id === provider.id);
      if (saved) {
        draft.keyConcurrency = saved.keyConcurrency ?? null;
        draft.maxRetries = saved.maxRetries ?? null;
      }
      onsaved(view, provider.id);
    }}
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
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 0 12px;
    font-size: 13px;
    color: var(--ink-2);
    cursor: pointer;
  }
  .check input {
    accent-color: var(--seal);
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
  .combo {
    position: relative;
    flex: 1;
    min-width: 0;
  }
  .input.with-chev {
    padding-right: 28px;
  }
  .combo-btn {
    position: absolute;
    top: 1px;
    right: 1px;
    bottom: 1px;
    width: 26px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    border: 0;
    border-radius: 0 3px 3px 0;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
    padding: 0;
  }
  .combo-btn:hover {
    color: var(--accent);
    background: var(--accent-wash);
  }
  .combo-btn svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 140ms var(--ease);
  }
  .combo-btn.open svg {
    transform: rotate(180deg);
  }
  .model-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    z-index: 30;
    max-height: 220px;
    overflow-y: auto;
    margin: 0;
    padding: 4px;
    list-style: none;
    background: var(--paper);
    border: 1px solid var(--rule-strong);
    border-radius: 6px;
    box-shadow: 0 10px 26px -8px rgba(0, 0, 0, 0.28);
  }
  .model-opt {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 5px 8px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--ink);
    font: inherit;
    font-size: 12px;
    text-align: left;
    cursor: pointer;
  }
  .model-opt:hover,
  .model-opt.active {
    background: var(--accent-wash);
    color: var(--accent);
  }
  .model-opt.selected {
    font-weight: 600;
  }
  .opt-name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .opt-check {
    flex: none;
    color: var(--accent);
    font-size: 11px;
  }
  .custom-sep {
    margin-top: 3px;
    padding-top: 3px;
    border-top: 1px solid var(--rule);
  }
  .model-opt.custom {
    font-family: var(--font-ui);
    font-size: 12.5px;
    color: var(--accent);
  }
  .actions {
    margin: 4px 0 10px;
  }
  .spacer {
    flex: 1;
  }
  .message {
    white-space: pre-line;
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
