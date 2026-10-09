<script lang="ts">
  import { onMount } from "svelte";
  import { count, t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { KeyEntry, ProviderView, SettingsView } from "../types";

  interface Props {
    provider: ProviderView;
    onchange: (view: SettingsView) => void;
    onclose: () => void;
  }

  let { provider, onchange, onclose }: Props = $props();

  let keys = $state<KeyEntry[]>([]);
  let names = $state<Record<string, string>>({});
  let paste = $state("");
  let busy = $state<string | null>(null);
  let message = $state<{ ok: boolean; text: string } | null>(null);
  let loaded = $state(false);

  const perKey = $derived(provider.keyConcurrency ?? null);
  const parallel = $derived(perKey ? keys.length * perKey : null);

  async function act<T>(what: string, f: () => Promise<T>): Promise<T | undefined> {
    busy = what;
    message = null;
    try {
      return await f();
    } catch (err) {
      message = { ok: false, text: errorMessage(err) };
      return undefined;
    } finally {
      busy = null;
    }
  }

  async function reload(): Promise<void> {
    const list = await act("load", () => settingsIpc.listKeys(provider.id));
    if (list) {
      keys = list;
      names = Object.fromEntries(list.map((k) => [k.fingerprint, k.name]));
    }
    loaded = true;
  }

  /** Live state from the provider in use (no keychain access). */
  async function refreshState(): Promise<void> {
    try {
      const live = await settingsIpc.keyStatus(provider.id);
      if (!live) return;
      keys = keys.map((k) => {
        const s = live.find((l) => l.fingerprint === k.fingerprint);
        return s ? { ...k, state: s.state, inFlight: s.inFlight, detail: s.detail } : k;
      });
    } catch {
      // The next tick tries again.
    }
  }

  async function rename(k: KeyEntry): Promise<void> {
    const name = (names[k.fingerprint] ?? "").trim();
    if (name === k.name) return;
    const view = await act("rename", () => settingsIpc.renameKey(provider.id, k.fingerprint, name));
    if (view) {
      onchange(view);
      k.name = name;
    }
  }

  async function store(mode: "add" | "replace"): Promise<void> {
    const view = await act(mode, () =>
      mode === "add" ? settingsIpc.addApiKeys(provider.id, paste) : settingsIpc.setApiKey(provider.id, paste),
    );
    if (!view) return;
    paste = "";
    onchange(view);
    const n = view.providers.find((p) => p.id === provider.id)?.keyCount ?? 0;
    message = { ok: true, text: t("keys.stored", { n }) };
    await reload();
  }

  /** Keys from a file picked in Rust (an exported pool, or any text with keys). */
  async function importFile(): Promise<void> {
    const view = await act("import", () => settingsIpc.importKeys(provider.id));
    if (!view) return;
    onchange(view);
    const n = view.providers.find((p) => p.id === provider.id)?.keyCount ?? 0;
    message = { ok: true, text: t("keys.stored", { n }) };
    await reload();
  }

  async function exportFile(): Promise<void> {
    const path = await act("export", () => settingsIpc.exportKeys(provider.id));
    if (path) message = { ok: true, text: t("keys.exported", { path }) };
  }

  async function test(k: KeyEntry): Promise<void> {
    const out = await act(`test-${k.fingerprint}`, () => settingsIpc.testKey(provider.id, k.fingerprint));
    if (out !== undefined) message = { ok: true, text: t("keys.testOk", { n: k.number, text: out }) };
    void refreshState();
  }

  async function remove(k: KeyEntry): Promise<void> {
    const view = await act(`remove-${k.fingerprint}`, () => settingsIpc.removeApiKey(provider.id, k.number, k.tail));
    if (view) {
      onchange(view);
      await reload();
    }
  }

  async function removeAll(): Promise<void> {
    const view = await act("clear", () => settingsIpc.clearApiKey(provider.id));
    if (view) {
      onchange(view);
      await reload();
    }
  }

  async function setLimits(patch: { keyConcurrency?: number | null; maxRetries?: number | null }): Promise<void> {
    const { hasKey: _h, keyCount: _c, keyNames: _n, builtin: _b, needsKey: _k, ...config } = provider;
    const view = await act("limits", () => settingsIpc.saveProvider({ ...config, ...patch }));
    if (view) onchange(view);
  }

  /** Runs in the capture phase, so Escape closes only this sheet and not
   * the settings drawer underneath. */
  function onKeydown(e: KeyboardEvent): void {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    onclose();
  }

  onMount(() => {
    void reload();
    const timer = setInterval(() => void refreshState(), 1500);
    return () => clearInterval(timer);
  });

  const stateLabel = {
    ready: "keys.state.ready",
    idle: "keys.state.idle",
    cooling: "keys.state.cooling",
    rejected: "keys.state.rejected",
  } as const;
</script>

<svelte:window onkeydowncapture={onKeydown} />

<div class="scrim" role="presentation" onclick={onclose}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="keys-title">
  <header>
    <h2 id="keys-title" class="smallcaps">{t("keys.title")}</h2>
    <span class="count">
      {provider.name} · {count(keys.length, "keys.count.one", "keys.count.many")}
      {#if parallel}· {t("keys.parallelNote", { n: parallel, keys: keys.length, per: perKey ?? 1 })}{/if}
    </span>
    <button class="close" onclick={onclose} aria-label={t("common.close")}>✕</button>
  </header>

  <div class="tools row">
    <label class="inline">
      <span class="label smallcaps">{t("keys.perKey")}</span>
      <select
        class="select"
        value={perKey ?? 0}
        disabled={!!busy}
        onchange={(e) => {
          const n = Number(e.currentTarget.value);
          void setLimits({ keyConcurrency: n > 0 ? n : null });
        }}
      >
        <option value={0}>{t("keys.unlimited")}</option>
        {#each [1, 2, 3, 4, 6, 8] as n (n)}<option value={n}>{n}</option>{/each}
      </select>
    </label>
    <label class="inline">
      <span class="label smallcaps">{t("keys.retries")}</span>
      <select
        class="select"
        value={provider.maxRetries ?? 0}
        disabled={!!busy}
        onchange={(e) => {
          const n = Number(e.currentTarget.value);
          void setLimits({ maxRetries: n > 0 ? n : null });
        }}
      >
        <option value={0}>{t("keys.retriesDefault")}</option>
        {#each [3, 5, 8, 10, 15, 20] as n (n)}<option value={n}>{n}</option>{/each}
      </select>
    </label>
  </div>

  <div class="table">
    <div class="head smallcaps" aria-hidden="true">
      <span>#</span>
      <span>{t("keys.colName")}</span>
      <span>{t("keys.colKey")}</span>
      <span>{t("keys.colState")}</span>
      <span class="num">{t("keys.colBusy")}</span>
      <span></span>
    </div>
    {#each keys as k (k.fingerprint)}
      <div class="entry" data-state={k.state}>
        <span class="mono faint">{k.number}</span>
        <input
          class="input name"
          bind:value={names[k.fingerprint]}
          placeholder={t("keys.namePlaceholder")}
          onblur={() => rename(k)}
          onkeydown={(e) => e.key === "Enter" && e.currentTarget.blur()}
          spellcheck="false"
        />
        <span class="mono">…{k.tail}</span>
        <span class="state" title={k.detail ?? ""}>{t(stateLabel[k.state])}</span>
        <span class="num mono">{k.inFlight}</span>
        <span class="actions">
          <button class="btn" onclick={() => test(k)} disabled={!!busy}>
            {busy === `test-${k.fingerprint}` ? t("common.testing") : t("common.test")}
          </button>
          <button class="remove" onclick={() => remove(k)} disabled={!!busy} aria-label={t("common.remove")}>✕</button>
        </span>
      </div>
    {:else}
      <p class="empty">{loaded ? t("keys.empty") : "…"}</p>
    {/each}
  </div>

  <footer>
    <textarea
      class="textarea mono"
      rows="4"
      bind:value={paste}
      autocomplete="off"
      spellcheck="false"
      placeholder={t("keys.paste")}
    ></textarea>
    <div class="row">
      {#if keys.length > 0}
        <button class="btn" onclick={() => store("add")} disabled={!!busy || !paste.trim()}>{t("keys.add")}</button>
        <button class="btn" onclick={() => store("replace")} disabled={!!busy || !paste.trim()}>{t("keys.replace")}</button>
      {:else}
        <button class="btn primary" onclick={() => store("replace")} disabled={!!busy || !paste.trim()}>
          {t("keys.saveFirst")}
        </button>
      {/if}
      <button class="btn" onclick={importFile} disabled={!!busy}>{t("keys.import")}</button>
      {#if keys.length > 0}
        <button class="btn" onclick={exportFile} disabled={!!busy}>{t("keys.export")}</button>
      {/if}
      <span class="spacer"></span>
      {#if keys.length > 0}
        <button class="btn danger" onclick={removeAll} disabled={!!busy}>{t("keys.removeAll")}</button>
      {/if}
    </div>
    <p class="hint" class:error={message && !message.ok}>
      {message ? message.text : t("keys.hint")}
    </p>
  </footer>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(20, 18, 14, 0.22);
    z-index: 60;
    animation: fade 160ms var(--ease);
  }
  .sheet {
    position: fixed;
    z-index: 70;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(760px, 94vw);
    max-height: min(680px, 90vh);
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    background: var(--paper);
    border: 1px solid var(--rule-strong);
    border-radius: 6px;
    box-shadow: 0 24px 60px -28px rgba(0, 0, 0, 0.45);
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding: 10px 16px 8px;
    border-bottom: 1px solid var(--rule);
    background: var(--chrome);
    border-radius: 6px 6px 0 0;
  }
  h2 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
  }
  .count {
    flex: 1;
    font-size: 13px;
    font-style: italic;
    color: var(--muted);
  }
  .close {
    border: 0;
    background: transparent;
    font-size: 15px;
    color: var(--muted);
    cursor: pointer;
  }
  .tools {
    padding: 10px 16px;
    border-bottom: 1px solid var(--rule);
    gap: 18px;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .inline .label {
    font-size: 13px;
    color: var(--muted);
    white-space: nowrap;
  }
  .inline .select {
    width: auto;
  }
  .table {
    overflow-y: auto;
    padding: 0 16px 8px;
    min-height: 120px;
  }
  .head,
  .entry {
    display: grid;
    grid-template-columns: 28px minmax(120px, 1fr) 90px minmax(110px, 0.8fr) 70px 110px;
    gap: 10px;
    align-items: center;
  }
  .head {
    position: sticky;
    top: 0;
    padding: 8px 0 5px;
    background: var(--paper);
    font-size: 13px;
    color: var(--muted);
    border-bottom: 1px solid var(--rule);
  }
  .entry {
    padding: 4px 0;
    border-bottom: 1px solid var(--rule);
    font-size: 13.5px;
  }
  .num {
    text-align: right;
  }
  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .faint {
    color: var(--faint);
  }
  .name {
    padding: 3px 6px;
  }
  .state {
    font-style: italic;
    color: var(--muted);
  }
  .entry[data-state="ready"] .state {
    color: #3f8f5a;
  }
  .entry[data-state="cooling"] .state {
    color: var(--accent);
  }
  .entry[data-state="rejected"] .state {
    color: var(--error);
  }
  .actions {
    display: inline-flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .remove {
    border: 0;
    background: transparent;
    color: var(--faint);
    cursor: pointer;
  }
  .remove:hover:not(:disabled) {
    color: var(--error);
  }
  .empty {
    padding: 24px 0;
    text-align: center;
    font-style: italic;
    color: var(--muted);
  }
  footer {
    padding: 10px 16px 12px;
    border-top: 1px solid var(--rule);
    display: grid;
    gap: 8px;
  }
  .spacer {
    flex: 1;
  }
  .hint {
    margin: 0;
    font-size: 12.5px;
    font-style: italic;
    color: var(--faint);
    overflow-wrap: anywhere;
    user-select: text;
    -webkit-user-select: text;
  }
  .hint.error {
    color: var(--error);
    font-style: normal;
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
</style>
