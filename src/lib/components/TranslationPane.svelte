<script lang="ts">
  import { onMount } from "svelte";
  import type { Session } from "../session.svelte";
  import SegmentBlock from "./SegmentBlock.svelte";

  interface Props {
    session: Session;
    blocks: Map<number, HTMLElement>;
    preview: (from: number, to: number) => string;
    onactivate: (id: number, offsetY: number) => void;
    onretry: (id: number) => void;
    onscroll: () => void;
    onresize: () => void;
    pane?: HTMLElement;
  }

  let { session, blocks, preview, onactivate, onretry, onscroll, onresize, pane = $bindable() }: Props = $props();

  let list: HTMLElement;

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
    const observer = new ResizeObserver(() => onresize());
    observer.observe(list);
    return () => observer.disconnect();
  });
</script>

<section class="gloss" bind:this={pane} {onscroll} aria-label="Chinese translation (read-only)">
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
        {register}
        onactivate={activate}
        {onretry}
      />
    {:else}
      <div class="empty">
        {#if session.direction === "zh-en"}
          <p class="zh-hint en">The English appears here</p>
          <p class="hint" lang="zh-CN">在左侧用中文写作；保存时写入的是右侧的英文。</p>
        {:else}
          <p class="zh-hint" lang="zh-CN">译文将在此处显示</p>
          <p class="hint">Open a <em>.tex</em>, <em>.md</em> or <em>.txt</em> file, or start writing on the left.</p>
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
