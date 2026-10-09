<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { errorMessage, settingsIpc } from "../ipc";
  import type { Direction, PromptView } from "../types";

  let direction = $state<Direction>("en-zh");
  let prompt = $state<PromptView | null>(null);
  let text = $state("");
  let status = $state<string | null>(null);

  async function load(d: Direction): Promise<void> {
    direction = d;
    status = null;
    try {
      prompt = await settingsIpc.getPrompt(d);
      text = prompt.text;
    } catch (err) {
      status = errorMessage(err);
    }
  }

  async function save(reset = false): Promise<void> {
    try {
      prompt = await settingsIpc.savePrompt(direction, reset ? "" : text);
      text = prompt.text;
      status = reset ? t("prompt.restored") : t("prompt.saved");
    } catch (err) {
      status = errorMessage(err);
    }
  }

  onMount(() => void load("en-zh"));
</script>

<div class="tabs" role="tablist">
  {#each [["en-zh", "EN → 中"], ["zh-en", "中 → EN"]] as [d, label] (d)}
    <button
      class="tab"
      class:on={direction === d}
      role="tab"
      aria-selected={direction === d}
      onclick={() => load(d as Direction)}>{label}</button
    >
  {/each}
</div>

<textarea class="textarea" rows="7" bind:value={text} spellcheck="false"></textarea>
<div class="row actions">
  <button class="btn primary" onclick={() => save()} disabled={!prompt || text === prompt.text}>
    {t("common.save")}
  </button>
  <button class="btn" onclick={() => save(true)} disabled={!prompt || prompt.isDefault}>{t("common.default")}</button>
  <span class="spacer"></span>
  <button class="btn" onclick={() => settingsIpc.revealPrompts().catch((e) => (status = errorMessage(e)))}>
    {t("prompt.reveal")}
  </button>
</div>
<p class="hint">
  {status ?? t("prompt.hint")}
</p>

<style>
  .tabs {
    display: flex;
    gap: 4px;
    margin-bottom: 8px;
  }
  .tab {
    border: 0;
    background: transparent;
    padding: 2px 10px 4px;
    border-bottom: 2px solid transparent;
    font: inherit;
    font-size: 13.5px;
    color: var(--muted);
    cursor: pointer;
  }
  .tab.on {
    color: var(--ink);
    border-bottom-color: var(--seal);
  }
  .actions {
    margin: 8px 0 4px;
  }
  .spacer {
    flex: 1;
  }
  .hint {
    margin: 4px 0 0;
    font-size: 12px;
    font-style: italic;
    color: var(--faint);
  }
</style>
