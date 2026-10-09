<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { errorMessage, settingsIpc, updateIpc } from "../ipc";
  import type { InstalledView, ReleaseView, SettingsView, UpdateProgress } from "../types";

  interface Props {
    settings: SettingsView;
    onchange: (view: SettingsView) => void;
    /** Unsaved changes: BiWrite does not quit or restart over them. */
    dirty: boolean;
  }

  let { settings, onchange, dirty }: Props = $props();

  let releases = $state<ReleaseView[] | null>(null);
  let selected = $state<string | null>(null);
  let checking = $state(false);
  let installing = $state<string | null>(null);
  let progress = $state<UpdateProgress | null>(null);
  let installed = $state<InstalledView | null>(null);
  let error = $state<string | null>(null);

  const chosen = $derived(releases?.find((r) => r.tag === selected) ?? null);

  function size(bytes: number): string {
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }

  async function check(): Promise<void> {
    if (checking) return;
    checking = true;
    error = null;
    try {
      const view = await updateIpc.list();
      releases = view.releases;
      selected =
        view.releases.find((r) => r.relation === "newer" && !r.prerelease)?.tag ??
        view.releases.find((r) => r.relation === "current")?.tag ??
        view.releases[0]?.tag ??
        null;
    } catch (err) {
      error = errorMessage(err);
    } finally {
      checking = false;
    }
  }

  async function install(): Promise<void> {
    if (!chosen || installing) return;
    installing = chosen.tag;
    error = null;
    progress = null;
    try {
      installed = await updateIpc.install(chosen.tag);
    } catch (err) {
      error = errorMessage(err);
    } finally {
      installing = null;
    }
  }

  async function relaunch(): Promise<void> {
    error = null;
    try {
      await updateIpc.relaunch();
    } catch (err) {
      error = errorMessage(err);
    }
  }

  async function setAuto(on: boolean): Promise<void> {
    try {
      onchange(await settingsIpc.setCheckUpdates(on));
    } catch (err) {
      error = errorMessage(err);
    }
  }

  onMount(() => {
    let off: (() => void) | undefined;
    let disposed = false;
    void updateIpc.onProgress((p) => (progress = p)).then((fn) => {
      if (disposed) fn();
      else off = fn;
    });
    return () => {
      disposed = true;
      off?.();
    };
  });
</script>

<section id="updates">
  <h3 class="smallcaps">{t("updates.title")}</h3>
  <p class="line">{t("updates.current", { version: settings.version })}</p>
  <p class="hint">{t("updates.hint")}</p>
  <div class="row">
    <button class="btn small" onclick={check} disabled={checking || !!installing}>
      {checking ? t("updates.checking") : t("updates.check")}
    </button>
    <label class="check">
      <input type="checkbox" checked={settings.checkUpdates} onchange={(e) => setAuto(e.currentTarget.checked)} />
      <span>{t("updates.auto")}</span>
    </label>
  </div>

  {#if releases}
    {#if releases.length === 0}
      <p class="hint">{t("updates.none")}</p>
    {:else}
      <ul class="releases" role="radiogroup" aria-label={t("updates.title")}>
        {#each releases as r (r.tag)}
          <li>
            <label class:on={selected === r.tag}>
              <input type="radio" name="release" value={r.tag} checked={selected === r.tag} onchange={() => (selected = r.tag)} />
              <span class="version">{r.version}</span>
              <span class="tag" data-relation={r.relation}
                >{t(r.relation === "current" ? "updates.current.tag" : r.relation === "newer" ? "updates.newer" : "updates.older")}</span
              >
              {#if r.prerelease}<span class="tag pre">{t("updates.pre")}</span>{/if}
              <span class="date">{r.date}</span>
            </label>
          </li>
        {/each}
      </ul>
      {#if chosen}
        {#if chosen.notes.trim()}
          <details class="notes">
            <summary>{t("updates.notes")}</summary>
            <pre>{chosen.notes.trim()}</pre>
          </details>
        {/if}
        {#if chosen.asset}
          <div class="row">
            <button
              class="btn small primary"
              onclick={install}
              disabled={!!installing || chosen.relation === "current" || (dirty && !installed)}
            >
              {t("updates.install", { version: chosen.version })}
            </button>
            <span class="hint inline">{chosen.asset.name} · {size(chosen.asset.size)}</span>
          </div>
          {#if dirty && chosen.relation !== "current"}<p class="hint">{t("updates.saveFirst")}</p>{/if}
        {:else}
          <p class="hint">{t("updates.noAsset")}</p>
        {/if}
      {/if}
    {/if}
  {/if}

  {#if installing && progress && progress.tag === installing}
    <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax={progress.total} aria-valuenow={progress.received}>
      <span style:width="{Math.min(100, (progress.received / Math.max(1, progress.total)) * 100)}%"></span>
    </div>
    <p class="hint">{t("updates.downloading", { done: size(progress.received), total: size(progress.total) })}</p>
  {/if}
  {#if installed}
    {#if installed.relaunch}
      <div class="row">
        <p class="ok">{t("updates.installed", { version: installed.version })}</p>
        <button class="btn small primary" onclick={relaunch} disabled={dirty}>{t("updates.restart")}</button>
      </div>
    {:else if installed.quitting}
      <p class="ok">{t("updates.quitting")}</p>
    {:else if installed.file}
      <p class="ok">{t("updates.saved", { file: installed.file })}</p>
    {/if}
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</section>

<style>
  section {
    padding: 14px 0 10px;
    border-bottom: 1px solid var(--rule);
  }
  h3 {
    margin: 0 0 10px;
    font-size: 14px;
    font-weight: 600;
    color: var(--seal);
  }
  .line {
    margin: 0 0 2px;
    font-size: 13.5px;
    color: var(--ink-2);
  }
  .hint {
    margin: 0 0 8px;
    font-size: 12.5px;
    font-style: italic;
    color: var(--muted);
  }
  .hint.inline {
    margin: 0;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--ink-2);
    cursor: pointer;
  }
  .check input,
  .releases input {
    accent-color: var(--seal);
  }
  .releases {
    list-style: none;
    margin: 4px 0 8px;
    padding: 0;
    max-height: 200px;
    overflow-y: auto;
    border: 1px solid var(--rule);
    border-radius: 5px;
  }
  .releases label {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .releases label.on {
    background: var(--accent-wash);
  }
  .version {
    font-family: var(--font-mono);
    font-size: 12.5px;
    min-width: 92px;
  }
  .tag {
    font-size: 11px;
    padding: 0 6px;
    border-radius: 8px;
    border: 1px solid var(--rule);
    color: var(--muted);
  }
  .tag[data-relation="newer"] {
    color: var(--accent);
    border-color: var(--accent-soft);
  }
  .tag[data-relation="current"] {
    color: var(--ink);
    border-color: var(--rule-strong);
  }
  .tag.pre {
    color: var(--seal);
    border-color: var(--seal);
  }
  .date {
    margin-left: auto;
    font-size: 11.5px;
    color: var(--faint);
  }
  .notes summary {
    font-size: 12.5px;
    color: var(--accent);
    cursor: pointer;
  }
  .notes pre {
    max-height: 160px;
    overflow: auto;
    margin: 4px 0 8px;
    padding: 6px 8px;
    background: var(--paper-2);
    border-radius: 4px;
    font-size: 11.5px;
    white-space: pre-wrap;
  }
  .progress {
    height: 6px;
    border-radius: 3px;
    background: var(--paper-2);
    overflow: hidden;
    margin: 4px 0;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width 120ms linear;
  }
  .ok {
    margin: 0;
    font-size: 13px;
    color: var(--accent);
  }
  .error {
    margin: 6px 0 0;
    font-size: 12.5px;
    color: var(--error);
  }
</style>
