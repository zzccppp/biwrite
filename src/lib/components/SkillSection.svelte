<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { assistIpc, errorMessage, ipc } from "../ipc";
  import type { SkillInfo } from "../types";

  interface Props {
    /** The skill changed (the assistant shows its version). */
    onchange: (skill: SkillInfo) => void;
  }

  let { onchange }: Props = $props();

  let skill = $state<SkillInfo | null>(null);
  let busy = $state(false);
  let updating = $state(false);
  let note = $state<string | null>(null);
  let error = $state<string | null>(null);

  async function run(f: () => Promise<SkillInfo | null | void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    note = null;
    try {
      const next = await f();
      if (next) {
        skill = next;
        onchange(next);
      }
    } catch (err) {
      error = errorMessage(err);
    } finally {
      busy = false;
      updating = false;
    }
  }

  function update(): Promise<void> {
    updating = true;
    return run(async () => {
      const next = await assistIpc.updateSkill();
      note = t("settings.skillUpdated", { version: next.version });
      return next;
    });
  }

  onMount(() => {
    void run(() => assistIpc.getSkill());
  });
</script>

<section>
  <h3 class="smallcaps">{t("settings.skill")}</h3>
  <p class="hint">{t("settings.skillHint")}</p>
  {#if skill}
    <p class="line">
      {t("settings.skillInfo", { name: skill.name, version: skill.version, date: skill.date, files: skill.files })}
      · {t(`settings.skillOrigin.${skill.origin}`)}
    </p>
    {#if skill.folder}<p class="path">{skill.folder}</p>{/if}
    {#if skill.missing.length}<p class="warn">{t("settings.skillMissing", { files: skill.missing.join(", ") })}</p>{/if}
  {/if}
  <div class="row">
    <button class="btn small" onclick={update} disabled={busy}>
      {updating ? t("settings.skillUpdating") : t("settings.skillUpdate")}
    </button>
    {#if skill?.origin === "folder"}
      <button class="btn small" onclick={() => run(() => assistIpc.resetSkillFolder())} disabled={busy}
        >{t("settings.skillReset")}</button
      >
    {:else}
      <button class="btn small" onclick={() => run(() => assistIpc.chooseSkillFolder())} disabled={busy}
        >{t("settings.skillChoose")}</button
      >
    {/if}
    <button class="btn small" onclick={() => run(() => assistIpc.revealSkill())} disabled={busy}>{t("settings.skillReveal")}</button>
    <button class="link" onclick={() => ipc.openLink("skill")}>github.com/qzkinhit/research-builder</button>
  </div>
  {#if note}<p class="note">{note}</p>{/if}
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
  .hint {
    margin: 0 0 8px;
    font-size: 12.5px;
    font-style: italic;
    color: var(--muted);
  }
  .line {
    margin: 0 0 4px;
    font-size: 13px;
    color: var(--ink-2);
  }
  .path {
    margin: 0 0 4px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .link {
    border: 0;
    background: transparent;
    padding: 0 2px;
    color: var(--accent);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }
  .note {
    margin: 8px 0 0;
    font-size: 12.5px;
    color: var(--accent);
  }
  .warn {
    margin: 6px 0 0;
    font-size: 12.5px;
    color: var(--error);
  }
</style>
