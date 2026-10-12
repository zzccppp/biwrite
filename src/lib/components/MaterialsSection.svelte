<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { assistIpc, errorMessage } from "../ipc";
  import type { MaterialsView } from "../types";

  let view = $state<MaterialsView | null>(null);
  let error = $state<string | null>(null);

  async function refresh(): Promise<void> {
    try {
      view = await assistIpc.materials();
      error = null;
    } catch (err) {
      error = errorMessage(err);
    }
  }

  async function reveal(): Promise<void> {
    try {
      await assistIpc.revealMaterials();
    } catch (err) {
      error = errorMessage(err);
    }
  }

  onMount(() => {
    void refresh();
    // Files put into the folder show when the window comes back.
    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });
</script>

<section class="materials">
  <h3 class="smallcaps">{t("settings.materials")}</h3>
  {#if view}
    <p class="path" title={view.inSkill ? t("settings.materialsInSkill") : undefined}>{view.folder}</p>
    <p class="counts">
      {#each view.counts as c, i (c.name)}{#if i > 0}<span class="dot">{" · "}</span>{/if}<span class="count" class:none={c.files === 0}
          ><code>{c.name}/</code> {c.files}{#if c.name === "icons" && view.unnamedIcons > 0}<span
              class="unnamed"
              title={t("settings.materialsUnnamedTitle")}>{t("settings.materialsUnnamed", { n: view.unnamedIcons })}</span
            >{/if}</span
        >{/each}
    </p>
  {/if}
  <div class="row">
    <button class="btn small" onclick={reveal}>{t("settings.materialsReveal")}</button>
  </div>
  {#if error}<p class="warn">{error}</p>{/if}
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
  .path {
    margin: 0 0 4px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .counts {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .count {
    white-space: nowrap;
  }
  .counts code {
    font-family: var(--font-mono);
    font-size: 12px;
  }
  .unnamed {
    color: var(--muted);
    cursor: help;
  }
  .counts .none {
    color: var(--faint);
  }
  .dot {
    color: var(--faint);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .warn {
    margin: 6px 0 0;
    font-size: 12.5px;
    color: var(--error);
  }
</style>
