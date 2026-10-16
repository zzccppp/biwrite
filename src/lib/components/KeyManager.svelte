<script lang="ts">
  import { onMount } from "svelte";
  import { count, t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { KeyEntry, ProviderView, SettingsView, SqueezeKey, SqueezeView } from "../types";

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
  let squeeze = $state<SqueezeView | null>(null);
  /** The key a new one is being pasted for (by fingerprint), and the paste. */
  let swapping = $state<string | null>(null);
  let fresh = $state("");
  /** Keep the keys that got through alive (a setting for every provider). */
  let keepAlive = $state<boolean | null>(null);
  /** However long the author is away, or half an hour after the last use. */
  let forever = $state(true);
  /** Mean minutes between keep-alives, and seconds between tries. */
  let keepMinutes = $state(4);
  let trySeconds = $state(3);
  /** A squeeze stops once this many keys got through. */
  let enough = $state(2);

  async function setPacing(minutes: number, seconds: number, keys = enough): Promise<void> {
    if (!(minutes > 0) || !(seconds > 0) || !(keys >= 1)) return;
    const view = await act("pace", () => settingsIpc.setPacing(minutes, seconds, Math.round(keys)));
    if (view) {
      keepMinutes = view.keepAliveMinutes;
      trySeconds = view.squeezeSeconds;
      enough = view.squeezeEnough;
      onchange(view);
    }
  }

  /** Set key `k` aside, or use it again. */
  async function setIgnored(k: KeyEntry, ignored: boolean): Promise<void> {
    const view = await act(`ignore-${k.fingerprint}`, () => settingsIpc.setKeyIgnored(provider.id, k.fingerprint, ignored));
    if (view) {
      keys = keys.map((x) => (x.fingerprint === k.fingerprint ? { ...x, ignored } : x));
      onchange(view);
    }
  }
  const round = (n: number) => Math.round(n * 10) / 10;

  async function setKeepAlive(on: boolean): Promise<void> {
    const view = await act("keep", () => settingsIpc.setKeepAlive(on));
    if (view) {
      keepAlive = view.keepAlive;
      onchange(view);
    }
  }

  async function setForever(on: boolean): Promise<void> {
    const view = await act("forever", () => settingsIpc.setKeepAliveForever(on));
    if (view) {
      forever = view.keepAliveForever;
      onchange(view);
    }
  }
  /** Ticks every few seconds, so "3 min ago" moves on. */
  let now = $state(Date.now());

  const perKey = $derived(provider.keyConcurrency ?? null);
  /** Keys requests go to: those not set aside. */
  const usable = $derived(keys.filter((k) => !k.ignored).length);
  const parallel = $derived(perKey ? usable * perKey : null);

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

  async function refreshSqueeze(): Promise<void> {
    now = Date.now();
    if (!provider.squeezable && !squeeze?.running) {
      squeeze = null;
      return;
    }
    try {
      squeeze = await settingsIpc.squeezeStatus(provider.id);
    } catch {
      // The next tick tries again.
    }
  }

  async function toggleSqueeze(): Promise<void> {
    const view = await act("squeeze", () =>
      squeeze?.running ? settingsIpc.squeezeStop(provider.id) : settingsIpc.squeezeStart(provider.id),
    );
    if (view) squeeze = view;
  }

  function squeezeOf(k: KeyEntry): SqueezeKey | undefined {
    return squeeze?.keys.find((s) => s.fingerprint === k.fingerprint);
  }

  function ago(ms: number): string {
    const minutes = Math.floor((now - ms) / 60_000);
    if (minutes < 1) return t("squeeze.agoNow");
    if (minutes < 60) return t("squeeze.agoMin", { n: minutes });
    if (minutes < 48 * 60) return t("squeeze.agoHour", { n: Math.floor(minutes / 60) });
    return t("squeeze.agoDay", { n: Math.floor(minutes / (24 * 60)) });
  }

  /** A key that got through and was not answered for five minutes may
   * have lost its place. */
  const STALE_MS = 5 * 60_000;

  /** Keys that got through are shown while that matters: the relay turns
   * keys away, or a squeeze ran. A pool where every key answers shows its
   * usual states. */
  const showThrough = $derived(!!squeeze && (squeeze.running || squeeze.started !== null || squeeze.crowded));

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

  /** Paste a new key for this one (a new key of the same account). */
  function startSwap(k: KeyEntry): void {
    swapping = swapping === k.fingerprint ? null : k.fingerprint;
    fresh = "";
  }

  async function swap(k: KeyEntry): Promise<void> {
    const view = await act(`swap-${k.fingerprint}`, () =>
      settingsIpc.replaceKey(provider.id, k.number, k.tail, fresh),
    );
    if (!view) return;
    swapping = null;
    fresh = "";
    onchange(view);
    await reload();
    message = { ok: true, text: t("keys.swapped", { n: k.number }) };
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
    if (out !== undefined)
      message = { ok: true, text: `${t("keys.testOk", { n: k.number })}\n${t("test.qa", { q: out.question, a: out.answer })}` };
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
    void refreshSqueeze();
    settingsIpc
      .get()
      .then((view) => {
        keepAlive = view.keepAlive;
        forever = view.keepAliveForever;
        keepMinutes = view.keepAliveMinutes ?? 4;
        trySeconds = view.squeezeSeconds ?? 3;
        enough = view.squeezeEnough ?? 2;
      })
      .catch(() => {});
    const timer = setInterval(() => {
      void refreshState();
      void refreshSqueeze();
    }, 1500);
    return () => clearInterval(timer);
  });

  // A keep-alive is drawn within a minute of the mean either way, or a
  // quarter of a shorter mean (as the keeper draws it).
  const keepSpread = (minutes: number) => Math.min(1, minutes * 0.25);

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
      {#if parallel}· {t("keys.parallelNote", { n: parallel, keys: usable, per: perKey ?? 1 })}{/if}
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
        {#each [3, 5, 8, 10, 15, 20, 50, 100] as n (n)}<option value={n}>{n}</option>{/each}
      </select>
    </label>
    {#if provider.squeezable && keepAlive !== null}
      <label class="inline keep" title={t("squeeze.keepAliveHint")}>
        <input
          type="checkbox"
          checked={keepAlive}
          disabled={!!busy}
          onchange={(e) => void setKeepAlive(e.currentTarget.checked)}
        />
        <span class="label smallcaps">{t("squeeze.keepAlive")}</span>
      </label>
      <label class="inline keep forever" title={t("squeeze.foreverHint")}>
        <input
          type="checkbox"
          checked={forever}
          disabled={!!busy || !keepAlive}
          onchange={(e) => void setForever(e.currentTarget.checked)}
        />
        <span class="label smallcaps">{t("squeeze.forever")}</span>
      </label>
      <label class="inline pace" title={t("squeeze.keepEveryHint", { low: round(keepMinutes - keepSpread(keepMinutes)), high: round(keepMinutes + keepSpread(keepMinutes)) })}>
        <span class="label smallcaps">{t("squeeze.keepEvery")}</span>
        <input
          class="input"
          type="number"
          min="1"
          max="30"
          step="0.5"
          value={keepMinutes}
          disabled={!!busy}
          onchange={(e) => void setPacing(Number(e.currentTarget.value), trySeconds)}
        />
        <span class="label">{t("squeeze.minutes")}</span>
      </label>
      <label class="inline pace" title={t("squeeze.tryEveryHint", { low: round(trySeconds * 0.5), high: round(trySeconds * 1.5) })}>
        <span class="label smallcaps">{t("squeeze.tryEvery")}</span>
        <input
          class="input"
          type="number"
          min="1"
          max="120"
          step="1"
          value={trySeconds}
          disabled={!!busy}
          onchange={(e) => void setPacing(keepMinutes, Number(e.currentTarget.value))}
        />
        <span class="label">{t("squeeze.seconds")}</span>
      </label>
      <label class="inline pace" title={t("squeeze.enoughHint")}>
        <span class="label smallcaps">{t("squeeze.enough")}</span>
        <input
          class="input"
          type="number"
          min="1"
          max="99"
          step="1"
          value={enough}
          disabled={!!busy}
          onchange={(e) => void setPacing(keepMinutes, trySeconds, Number(e.currentTarget.value))}
        />
        <span class="label">{t("squeeze.enoughUnit")}</span>
      </label>
    {/if}
    {#if squeeze?.running || (provider.squeezable && keys.length > 0)}
      <span class="spacer"></span>
      <button class="btn squeeze" class:primary={!squeeze?.running} onclick={toggleSqueeze} disabled={!!busy}>
        {squeeze?.running ? t("squeeze.stop") : t("squeeze.start")}
      </button>
    {/if}
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
      {@const sq = squeezeOf(k)}
      {@const through = showThrough && !!sq?.throughSince && k.state !== "cooling" && k.state !== "rejected"}
      <div
        class="entry"
        class:ignored={k.ignored}
        data-state={k.state}
        data-squeeze={through
          ? "through"
          : sq?.refused
            ? "refused"
            : squeeze?.running && sq && !sq.throughSince
              ? "trying"
              : null}
        class:stale={through && !!sq?.lastOk && now - sq.lastOk > STALE_MS}
      >
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
        {#if k.ignored}
          <span class="state" title={t("keys.ignoreHint")}>{t("keys.ignored")}</span>
        {:else if through && sq?.throughSince}
          <!-- Got through, then kept alive (since the latest keep-alive), or
               its keep-alive being tried again. -->
          <span
            class="state"
            class:retrying={sq.keepRetrying}
            title={[
              t("squeeze.throughSince", { ago: ago(sq.throughSince) }),
              sq.lastOk ? t("squeeze.lastOk", { ago: ago(sq.lastOk) }) : "",
              sq.keptAt ? t("squeeze.keptAt", { ago: ago(sq.keptAt) }) : "",
            ]
              .filter(Boolean)
              .join(" · ")}
          >
            <!-- The time since it was last answered, by a keep-alive or any
                 other request. -->
            {sq.keepRetrying
              ? t("squeeze.keepRetrying", { ago: ago(sq.lastOk ?? sq.throughSince) })
              : sq.keptAt
                ? t("squeeze.kept", { ago: ago(Math.max(sq.keptAt, sq.lastOk ?? 0)) })
                : t("squeeze.through", { ago: ago(sq.throughSince) })}
          </span>
        {:else if squeeze?.running && sq && !sq.refused && !sq.throughSince}
          <span class="state" title={sq.lastError ?? ""}>{t("squeeze.trying", { n: sq.tries + (sq.trying ? 1 : 0) })}</span>
        {:else if provider.squeezable && k.lastThrough === null && k.state !== "ready" && !sq?.refused}
          <span class="state never" title={sq?.lastError ?? k.detail ?? ""}>{t("keys.neverThrough")}</span>
        {:else}
          <span
            class="state"
            title={sq?.lastError ??
              k.detail ??
              (provider.squeezable && k.lastThrough ? t("keys.lastThrough", { ago: ago(k.lastThrough) }) : "")}
          >
            {t(stateLabel[sq?.refused ? "rejected" : k.state])}
          </span>
        {/if}
        <span class="num mono">{k.inFlight}</span>
        <span class="actions">
          <button class="btn" class:on={swapping === k.fingerprint} onclick={() => startSwap(k)} disabled={!!busy}>
            {t("keys.swap")}
          </button>
          <button class="btn" onclick={() => test(k)} disabled={!!busy}>
            {busy === `test-${k.fingerprint}` ? t("common.testing") : t("common.test")}
          </button>
          <button class="btn ignore" onclick={() => void setIgnored(k, !k.ignored)} disabled={!!busy} title={t("keys.ignoreHint")}>
            {k.ignored ? t("keys.unignore") : t("keys.ignore")}
          </button>
          <button class="remove" onclick={() => remove(k)} disabled={!!busy} aria-label={t("common.remove")}>✕</button>
        </span>
      </div>
      {#if swapping === k.fingerprint}
        <div class="swap row">
          <input
            class="input mono"
            type="password"
            bind:value={fresh}
            placeholder={t("keys.swapPaste")}
            autocomplete="off"
            spellcheck="false"
            onkeydown={(e) => e.key === "Enter" && fresh.trim() && swap(k)}
          />
          <button class="btn primary" onclick={() => swap(k)} disabled={!!busy || !fresh.trim()}>{t("common.save")}</button>
          <button class="btn" onclick={() => startSwap(k)} disabled={!!busy}>{t("common.cancel")}</button>
        </div>
      {/if}
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
    <p class="hint" class:error={message && !message.ok} class:qa={message?.ok && message.text.includes("\n")}>
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
    flex-wrap: wrap;
    row-gap: 8px;
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
    grid-template-columns: 28px minmax(110px, 1fr) 90px minmax(110px, 0.8fr) 60px 236px;
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
  .entry .state.retrying {
    color: var(--accent);
  }
  .entry[data-state="rejected"] .state,
  .entry.ignored > :not(.actions) {
    opacity: 0.5;
  }
  .pace .input {
    width: 58px;
    padding: 2px 6px;
  }
  .entry[data-squeeze="refused"] .state {
    color: var(--error);
  }
  .entry[data-squeeze="through"] .state {
    color: #3f8f5a;
    font-style: normal;
  }
  .entry[data-squeeze="through"].stale .state {
    color: var(--accent);
  }
  .entry[data-squeeze="trying"] .state {
    color: var(--accent);
  }
  .state.never {
    color: var(--error);
  }
  .actions .btn.on {
    background: var(--paper-2);
  }
  .swap {
    gap: 6px;
    padding: 6px 0 8px 38px;
    border-bottom: 1px solid var(--rule);
  }
  .swap .input {
    flex: 1;
    min-width: 0;
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
  footer .row {
    flex-wrap: wrap;
    row-gap: 6px;
  }
  .spacer {
    flex: 1;
  }
  .hint.qa {
    white-space: pre-line;
    font-style: normal;
    color: var(--ink-2);
    padding: 6px 10px;
    border-left: 2px solid var(--accent);
    background: var(--accent-wash);
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
