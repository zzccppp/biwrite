<script lang="ts">
  // Paragraphs of another file of the paper in the translation pane's
  // whole-paper view: read only, a click opens the file there.
  import { t } from "../i18n.svelte";
  import { type Macros, renderToHtml } from "../math";
  import type { PaperPart } from "../types";

  interface Props {
    part: PaperPart;
    lang: "zh" | "en";
    macros: Macros;
    onopen: (file: string, from: number, to: number) => void;
  }

  let { part, lang, macros, onopen }: Props = $props();
</script>

<div class="part" data-file={part.file}>
  <p class="file smallcaps">{part.file}</p>
  {#each part.blocks as block, i (i)}
    <div
      class="block"
      class:missing={!block.translation}
      role="button"
      tabindex="-1"
      title={t("paper.open", { file: part.file })}
      onclick={() => onopen(part.file, block.from, block.to)}
      onkeydown={(e) => (e.key === "Enter" || e.key === " ") && onopen(part.file, block.from, block.to)}
    >
      <div
        class="text"
        class:en={lang === "en" && !!block.translation}
        class:heading={block.heading}
        lang={block.translation ? (lang === "en" ? "en" : "zh-CN") : undefined}
      >
        {@html renderToHtml(block.translation ?? block.source, "latex", macros)}
      </div>
    </div>
  {/each}
</div>

<style>
  .part {
    margin: 0 0 10px;
    border-left: 2px solid var(--rule);
    margin-left: 40px;
  }
  .file {
    margin: 0;
    padding: 6px 28px 2px 12px;
    font-size: 12px;
    color: var(--faint);
    letter-spacing: 0.04em;
  }
  .block {
    padding: 6px 28px 8px 12px;
    cursor: pointer;
    transition: background-color 160ms var(--ease);
  }
  .block:hover {
    background: var(--accent-wash);
  }
  .text {
    font-family: var(--font-zh);
    font-size: var(--zh-size);
    line-height: var(--zh-leading);
    color: var(--ink-2);
    white-space: pre-line;
    overflow-wrap: anywhere;
  }
  .text.en {
    font-family: var(--font-prose);
    font-size: 15.5px;
    line-height: 1.7;
  }
  .text.heading {
    font-weight: 700;
    color: var(--hl-heading);
  }
  .missing .text {
    font-family: var(--font-prose);
    font-size: 14.5px;
    color: var(--faint);
  }
</style>
