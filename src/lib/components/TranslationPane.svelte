<script lang="ts">
  import { onMount } from "svelte";
  import { language, parts, t } from "../i18n.svelte";
  import type { Session } from "../session.svelte";
  import SegmentBlock from "./SegmentBlock.svelte";

  interface Props {
    session: Session;
    blocks: Map<number, HTMLElement>;
    preview: (from: number, to: number) => string;
    /** Full source text of a range (for typesetting a collapsed equation). */
    slice: (from: number, to: number) => string;
    onactivate: (id: number, offsetY: number) => void;
    onretry: (id: number) => void;
    onedit: (id: number) => void;
    onscroll: () => void;
    onresize: () => void;
    pane?: HTMLElement;
  }

  let { session, blocks, preview, slice, onactivate, onretry, onedit, onscroll, onresize, pane = $bindable() }: Props =
    $props();

  let list: HTMLElement;

  /** The empty-state hint is in the interface language. */
  const hintLang = $derived(language.current === "zh" ? "zh-CN" : "en");

  function register(node: HTMLElement, id: number) {
    blocks.set(id, node);
    return {
      update(next: number) {
        if (blocks.get(id) === node) blocks.delete(id);
        id = next;
        blocks.set(id, node);
      },
      destroy() {
        if (blocks.get(id) === node) blocks.delete(id);
      },
    };
  }

  function activate(id: number, el: HTMLElement): void {
    if (!pane) return;
    onactivate(id, el.getBoundingClientRect().top - pane.getBoundingClientRect().top);
  }

  onMount(() => {
    // Content height (reflow, expanded equations) and viewport size (window
    // resize, splitter drag) both move the blocks.
    const observer = new ResizeObserver(() => onresize());
    observer.observe(list);
    if (pane) observer.observe(pane);
    return () => observer.disconnect();
  });
</script>

<section class="gloss" bind:this={pane} {onscroll} aria-label={t("pane.gloss")}>
  <div class="list" bind:this={list}>
    {#each session.layout as segment, i (segment.id)}
      <SegmentBlock
        {segment}
        state={session.states.get(segment.id)}
        number={i + 1}
        active={session.activeId === segment.id}
        edited={session.localStale.has(segment.id)}
        preview={segment.kind.type === "skipped" ? preview(segment.from, segment.to) : ""}
        lang={session.direction === "zh-en" ? "en" : "zh"}
        mode={session.mode}
        macros={session.macros}
        expanded={session.expanded.has(segment.id)}
        source={session.expanded.has(segment.id) ? slice(segment.from, segment.to) : ""}
        {register}
        onactivate={activate}
        {onretry}
        {onedit}
        ontoggle={(id) => session.toggleExpanded(id)}
      />
    {:else}
      <div class="empty">
        {#if session.direction === "zh-en"}
          <p class="zh-hint en">The English appears here</p>
          <p class="hint" lang={hintLang}>{t("pane.writeZhHint")}</p>
        {:else}
          <p class="zh-hint" lang="zh-CN">译文将在此处显示</p>
          <p class="hint" lang={hintLang}>
            {#each parts("pane.openHint") as part, j (j)}{#if j % 2}<em>.{part}</em>{:else}{part}{/if}{/each}
          </p>
        {/if}
      </div>
    {/each}
  </div>
</section>

<style>
  .gloss {
    position: relative;
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 36px 0 45vh;
    background:
      linear-gradient(90deg, var(--rule) 0, var(--rule) 1px, transparent 1px) 40px 0 / 100% 100% no-repeat,
      var(--paper-2);
  }
  .list {
    max-width: 760px;
  }
  .empty {
    padding: 18vh 48px 0 56px;
    color: var(--muted);
    animation: rise 500ms var(--ease) both 120ms;
  }
  .zh-hint {
    font-family: var(--font-zh);
    font-size: 22px;
    letter-spacing: 0.12em;
    color: var(--faint);
    margin: 0 0 10px;
  }
  .zh-hint.en {
    font-family: var(--font-prose);
    font-style: italic;
    letter-spacing: 0.02em;
  }
  .hint {
    font-style: italic;
    margin: 0;
  }
  .hint:lang(zh-CN) {
    font-family: var(--font-zh);
    font-style: normal;
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }
</style>
