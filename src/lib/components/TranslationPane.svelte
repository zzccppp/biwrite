<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { language, parts, t } from "../i18n.svelte";
  import type { Session } from "../session.svelte";
  import { anchorsAgree, placeOf, samePlace, sentenceBounds, shownText, textRange } from "../sentences";
  import type { PaperPart as Part, PaperView, SegmentLayout, SentenceMark, SentencePick } from "../types";
  import PaperPart from "./PaperPart.svelte";
  import SegmentBlock from "./SegmentBlock.svelte";

  interface Props {
    session: Session;
    blocks: Map<number, HTMLElement>;
    preview: (from: number, to: number) => string;
    /** Full source text of a range (for typesetting a collapsed equation). */
    slice: (from: number, to: number) => string;
    /** A click on block `id`, `offsetY` px below the pane's top, on `sentence` of its translation. */
    onactivate: (id: number, offsetY: number, sentence: SentencePick | null) => void;
    /** Show where the cursor is: sentence `index` of `count` in block `id`,
     * at `place` of its paragraph (`tick` repeats it). */
    mark?: SentenceMark | null;
    onretry: (id: number) => void;
    onedit: (id: number) => void;
    onscroll: () => void;
    onresize: () => void;
    pane?: HTMLElement;
    /** The whole paper around the open document (another file's paragraph
     * opens that file there), or null for the open document alone. */
    paper?: PaperView | null;
    onopenpart?: (file: string, from: number, to: number) => void;
  }

  let {
    session,
    blocks,
    preview,
    slice,
    onactivate,
    onretry,
    onedit,
    onscroll,
    onresize,
    mark = null,
    pane = $bindable(),
    paper = null,
    onopenpart = () => {},
  }: Props = $props();

  const paneLang = $derived(session.direction === "zh-en" ? "en" : "zh");

  /** The parts of the paper a segment of the open document includes (its
   * `\input` and `\include` lines). */
  function inside(segment: SegmentLayout): Part[] {
    if (!paper || paper.inside.length === 0) return [];
    const out: Part[] = [];
    for (const line of slice(segment.from, segment.to).split("\n")) {
      const code = line.replace(/(^|[^\\])%.*$/, "$1");
      for (const m of code.matchAll(/\\(?:input|include|subfile)\s*\{([^{}]+)\}/g)) {
        const name = m[1].trim().replace(/^\.\//, "").replace(/\.tex$/, "");
        out.push(...(paper.inside.find((i) => i.name === name)?.parts ?? []));
      }
    }
    return out;
  }

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

  function activate(id: number, el: HTMLElement, sentence: SentencePick | null): void {
    if (!pane) return;
    onactivate(id, el.getBoundingClientRect().top - pane.getBoundingClientRect().top, sentence);
  }

  // The cursor's sentence, marked for a moment: the same sentence of the
  // translation when both have as many, it lies in about the same place and
  // its numbers and names are the source sentence's, else the whole block.
  // A translation of older text has none.
  let unmark: (() => void) | null = null;
  $effect(() => {
    const m = mark;
    if (!m || !pane) return;
    const block = blocks.get(m.id);
    if (!block) return;
    unmark?.();
    const view = pane.getBoundingClientRect();
    const at = block.getBoundingClientRect();
    if (at.bottom < view.top || at.top > view.bottom) block.scrollIntoView({ block: "center" });
    const box = block.querySelector<HTMLElement>(".zh");
    const current = untrack(
      () => session.states.get(m.id)?.status === "translated" && !session.localStale.has(m.id),
    );
    const shown = box && current ? shownText(box) : "";
    const bounds = sentenceBounds(shown);
    const candidate = bounds.length === m.count ? bounds[m.index] : undefined;
    const sentence =
      candidate &&
      samePlace(placeOf(bounds, m.index), m.place) &&
      anchorsAgree(shown.slice(candidate.start, candidate.end), m.text)
        ? candidate
        : undefined;
    const range = box && sentence ? textRange(box, sentence.start, sentence.end) : null;
    if (range && "highlights" in CSS) {
      CSS.highlights.set("bw-locate", new Highlight(range));
      unmark = () => CSS.highlights.delete("bw-locate");
    } else {
      block.classList.add("located");
      unmark = () => block.classList.remove("located");
    }
    const timer = setTimeout(() => {
      unmark?.();
      unmark = null;
    }, 2500);
    return () => {
      clearTimeout(timer);
      unmark?.();
      unmark = null;
    };
  });

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
    {#if paper}
      {#each paper.before as part, k (k)}<PaperPart {part} lang={paneLang} macros={session.macros} onopen={onopenpart} />{/each}
    {/if}
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
      {#each inside(segment) as part, k (k)}<PaperPart {part} lang={paneLang} macros={session.macros} onopen={onopenpart} />{/each}
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
    {#if paper}
      {#each paper.after as part, k (k)}<PaperPart {part} lang={paneLang} macros={session.macros} onopen={onopenpart} />{/each}
    {/if}
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
