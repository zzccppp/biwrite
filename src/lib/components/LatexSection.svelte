<script lang="ts">
  import { t } from "../i18n.svelte";
  import { errorMessage, latexIpc, settingsIpc } from "../ipc";
  import type { LatexStore } from "../latex.svelte";
  import type { SettingsView } from "../types";

  interface Props {
    latex: LatexStore;
    settings: SettingsView;
    onchange: (view: SettingsView) => void;
  }

  let { latex, settings, onchange }: Props = $props();

  let busy = $state(false);
  let error = $state<string | null>(null);

  const status = $derived(latex.status);
  const tools = $derived(
    status ? [...status.engines, ...(status.latexmk ? ["latexmk"] : []), ...(status.synctex ? ["synctex"] : [])] : [],
  );

  async function run(f: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await f();
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
    }
  }

  function setCompileOnSave(on: boolean): Promise<void> {
    return run(async () => {
      await latexIpc.setCompileOnSave(on);
      onchange(await settingsIpc.get());
    });
  }
</script>

<section>
  <h3 class="smallcaps">{t("settings.latex")}</h3>
  {#if status?.found}
    <p class="line">{t("settings.texFound", { dist: status.distribution || "TeX", bin: status.bin })}</p>
    <p class="hint">{t("settings.texTools", { tools: tools.join(", ") })}</p>
  {:else if status}
    <p class="line warn">{t("settings.texNone")}</p>
    <p class="hint">{t("pdf.noTexHint")}</p>
  {/if}
  <div class="row">
    <button class="btn small" onclick={() => run(() => latex.loadStatus(true))} disabled={busy}>{t("pdf.checkAgain")}</button>
    <button class="btn small" onclick={() => run(async () => void (latex.status = await latexIpc.chooseBin()))} disabled={busy}
      >{t("settings.texChoose")}</button
    >
    {#if status?.customBin}
      <button class="btn small" onclick={() => run(async () => void (latex.status = await latexIpc.resetBin()))} disabled={busy}
        >{t("settings.texAuto")}</button
      >
    {/if}
  </div>
  <label class="check">
    <input
      type="checkbox"
      checked={settings.latex.compileOnSave}
      onchange={(e) => setCompileOnSave(e.currentTarget.checked)}
    />
    <span>{t("settings.compileOnSave")}</span>
  </label>
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
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--error);
  }
  .hint {
    margin: 0 0 8px;
    font-size: 12.5px;
    font-style: italic;
    color: var(--muted);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
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
  .check input {
    accent-color: var(--seal);
  }
  .error {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--error);
  }
</style>
