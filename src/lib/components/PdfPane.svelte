<script lang="ts">
  import { onMount, untrack } from "svelte";
  import type { PDFDocumentProxy, RenderTask, TextLayer } from "pdfjs-dist/legacy/build/pdf.mjs";
  import { count, t } from "../i18n.svelte";
  import { errorMessage, latexIpc } from "../ipc";
  import type { LatexStore } from "../latex.svelte";
  import { formatDuration } from "../logFormat";
  import { caretOffset, openPdf, pdfjs } from "../pdf";
  import type { IssueView, PdfBox, PdfLang, PdfMenu, PdfPick } from "../types";

  interface Props {
    store: LatexStore;
    /** Boxes to show, from the cursor (forward sync); `tick` repeats the same ones. */
    marks: { lang: PdfLang; boxes: PdfBox[]; tick: number };
    menu: PdfMenu | null;
    /** The document can be compiled (a saved .tex file). */
    compilable: boolean;
    oncompile: (lang: PdfLang) => void;
    onpick: (pick: PdfPick) => void;
    onlocate: () => void;
    onissue: (issue: IssueView) => void;
    onaction: (action: "polish" | "edit" | "ask") => void;
    onopenfile: (file: string, line: number) => void;
    onclosemenu: () => void;
    /** Show the PDF of another language (built if it never was). */
    onlang: (lang: PdfLang) => void;
    /** Save the Chinese version as a .tex file. */
    onexporttex: () => void;
  }

  let {
    store,
    marks,
    menu,
    compilable,
    oncompile,
    onpick,
    onlocate,
    onissue,
    onaction,
    onopenfile,
    onclosemenu,
    onlang,
    onexporttex,
  }: Props = $props();

  const ZOOM_KEY = "biwrite.pdf.zoom";
  const mod = navigator.platform.toLowerCase().includes("mac") ? "⌘" : "Ctrl+";
  const alt = navigator.platform.toLowerCase().includes("mac") ? "⌥" : "Alt+";

  let scroller = $state<HTMLElement>();
  let doc = $state<PDFDocumentProxy | null>(null);
  /** Page sizes at scale 1 (PDF points). */
  let sizes = $state<{ width: number; height: number }[]>([]);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  /** "fit" follows the pane's width. */
  let zoomMode = $state<"fit" | number>(loadZoom());
  let paneWidth = $state(0);
  let showProblems = $state(false);
  let showOutput = $state(false);
  /** Build id of the document on show, per language. */
  let shownBuild = 0;
  let shownLang: PdfLang | null = null;
  let docSerial = 0;
  let visibleMarks = $state<PdfBox[]>([]);
  let markTimer: ReturnType<typeof setTimeout> | undefined;

  const langs: PdfLang[] = ["en", "zh"];
  const pageEls = new Map<number, HTMLElement>();
  const rendered = new Map<number, { serial: number; zoom: number; task: RenderTask | null; text: TextLayer | null }>();
  let observer: IntersectionObserver | null = null;
  const visible = new Set<number>();

  const lang = $derived(store.lang);
  const build = $derived(store.builds[lang]);
  const building = $derived(store.building[lang]);
  const failure = $derived(store.failure[lang]);
  const maxWidth = $derived(sizes.reduce((m, s) => Math.max(m, s.width), 0) || 612);
  const zoom = $derived(
    zoomMode === "fit" ? Math.max(0.3, Math.min(4, (paneWidth - 36) / maxWidth)) : (zoomMode as number),
  );
  const errors = $derived(build?.issues.filter((i) => i.severity === "error") ?? []);
  const warnings = $derived(build?.issues.filter((i) => i.severity === "warning") ?? []);
  const boxes = $derived(build?.issues.filter((i) => i.severity === "box") ?? []);

  function loadZoom(): "fit" | number {
    try {
      const v = localStorage.getItem(ZOOM_KEY);
      const n = v === null ? NaN : parseFloat(v);
      return n >= 0.3 && n <= 4 ? n : "fit";
    } catch {
      return "fit";
    }
  }

  function setZoom(next: "fit" | number): void {
    zoomMode = next === "fit" ? "fit" : Math.max(0.3, Math.min(4, Math.round(next * 100) / 100));
    try {
      localStorage.setItem(ZOOM_KEY, String(zoomMode));
    } catch {
      // Not persisted; harmless.
    }
  }

  // Load the PDF of the latest build (or switch languages). A build
  // without a PDF keeps the last one on show.
  $effect(() => {
    const b = build;
    const l = lang;
    untrack(() => {
      if (!b?.hasPdf) {
        if (shownLang !== l || !b) {
          void replaceDoc(null);
          shownLang = l;
          shownBuild = 0;
        }
        return;
      }
      if (b.id === shownBuild && l === shownLang) return;
      shownBuild = b.id;
      shownLang = l;
      void load(l, b.id);
    });
  });

  async function load(l: PdfLang, id: number): Promise<void> {
    loading = true;
    loadError = null;
    try {
      const bytes = await latexIpc.pdf(l);
      const next = await openPdf(bytes);
      if (id !== shownBuild || l !== shownLang) {
        void next.loadingTask.destroy();
        return;
      }
      const pages: { width: number; height: number }[] = [];
      for (let n = 1; n <= next.numPages; n++) {
        const page = await next.getPage(n);
        const v = page.getViewport({ scale: 1 });
        pages.push({ width: v.width, height: v.height });
      }
      await replaceDoc(next, pages);
    } catch (err) {
      loadError = errorMessage(err);
    } finally {
      loading = false;
    }
  }

  async function replaceDoc(next: PDFDocumentProxy | null, pages: { width: number; height: number }[] = []): Promise<void> {
    const old = doc;
    for (const r of rendered.values()) {
      r.task?.cancel();
      r.text?.cancel();
    }
    rendered.clear();
    docSerial++;
    doc = next;
    sizes = pages;
    if (old) void old.loadingTask.destroy();
    // Pages already in view render now; the rest as they scroll in.
    requestAnimationFrame(() => visible.forEach((n) => void renderPage(n)));
  }

  async function renderPage(n: number): Promise<void> {
    const d = doc;
    const el = pageEls.get(n);
    if (!d || !el || n > d.numPages) return;
    const z = zoom;
    const serial = docSerial;
    const prev = rendered.get(n);
    if (prev && prev.serial === serial && prev.zoom === z) return;
    prev?.task?.cancel();
    prev?.text?.cancel();
    const entry = { serial, zoom: z, task: null as RenderTask | null, text: null as TextLayer | null };
    rendered.set(n, entry);
    const lib = await pdfjs();
    try {
      const page = await d.getPage(n);
      if (rendered.get(n) !== entry) return;
      const viewport = page.getViewport({ scale: z });
      const ratio = window.devicePixelRatio || 1;
      const canvas = document.createElement("canvas");
      canvas.width = Math.floor(viewport.width * ratio);
      canvas.height = Math.floor(viewport.height * ratio);
      canvas.style.width = `${Math.floor(viewport.width)}px`;
      canvas.style.height = `${Math.floor(viewport.height)}px`;
      entry.task = page.render({
        canvas,
        viewport,
        transform: ratio === 1 ? undefined : [ratio, 0, 0, ratio, 0, 0],
      });
      await entry.task.promise;
      if (rendered.get(n) !== entry) return;
      const text = document.createElement("div");
      text.className = "textLayer";
      entry.text = new lib.TextLayer({ textContentSource: page.streamTextContent(), container: text, viewport });
      await entry.text.render();
      if (rendered.get(n) !== entry) return;
      el.querySelector("canvas")?.remove();
      el.querySelector(".textLayer")?.remove();
      el.prepend(canvas);
      canvas.after(text);
    } catch (err) {
      if (!(err instanceof lib.RenderingCancelledException) && rendered.get(n) === entry) {
        rendered.delete(n);
      }
    }
  }

  // Zoom changes re-render the pages in view.
  $effect(() => {
    void zoom;
    void sizes;
    requestAnimationFrame(() => visible.forEach((n) => void renderPage(n)));
  });

  function registerPage(node: HTMLElement, n: number) {
    pageEls.set(n, node);
    observer?.observe(node);
    return {
      destroy() {
        observer?.unobserve(node);
        if (pageEls.get(n) === node) pageEls.delete(n);
        visible.delete(n);
      },
    };
  }

  // Boxes from the cursor: show them and bring the first into view.
  $effect(() => {
    const m = marks;
    if (m.lang !== lang || m.boxes.length === 0) {
      untrack(() => (visibleMarks = []));
      return;
    }
    untrack(() => {
      visibleMarks = m.boxes;
      clearTimeout(markTimer);
      markTimer = setTimeout(() => (visibleMarks = []), 4000);
      const first = m.boxes[0];
      const el = pageEls.get(first.page);
      if (el && scroller) {
        const top = el.offsetTop + first.top * zoom - scroller.clientHeight / 3;
        scroller.scrollTo({ top: Math.max(0, top), behavior: "smooth" });
      }
    });
  });

  function onPageClick(e: MouseEvent, n: number): void {
    const selection = window.getSelection();
    if (selection && !selection.isCollapsed) return; // selecting text to copy
    const el = e.currentTarget as HTMLElement;
    if ((e.target as HTMLElement).closest(".pdf-menu")) return;
    const rect = el.getBoundingClientRect();
    const target = e.target as HTMLElement;
    const span = target.tagName === "SPAN" && target.closest(".textLayer") ? target : null;
    onpick({
      page: n,
      x: (e.clientX - rect.left) / zoom,
      y: (e.clientY - rect.top) / zoom,
      span: span?.textContent ?? "",
      click: span ? caretOffset(span, e.clientX, e.clientY) : 0,
    });
  }

  function issueLabel(i: IssueView): string {
    const where = i.file
      ? i.line === null
        ? i.file
        : `${i.file}:${i.line}`
      : i.line === null
        ? ""
        : t("pdf.line", { n: i.line });
    return where ? `${where} · ${i.message}` : i.message;
  }

  onMount(() => {
    observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          const n = Number((e.target as HTMLElement).dataset.page);
          if (e.isIntersecting) {
            visible.add(n);
            void renderPage(n);
          } else {
            visible.delete(n);
          }
        }
      },
      { root: scroller, rootMargin: "600px 0px" },
    );
    for (const el of pageEls.values()) observer.observe(el);
    const resize = new ResizeObserver(() => {
      if (scroller) paneWidth = scroller.clientWidth;
    });
    if (scroller) resize.observe(scroller);
    return () => {
      observer?.disconnect();
      resize.disconnect();
      clearTimeout(markTimer);
      for (const r of rendered.values()) {
        r.task?.cancel();
        r.text?.cancel();
      }
      void doc?.loadingTask.destroy();
    };
  });
</script>

<section class="pdf" aria-label="PDF">
  <header class="bar">
    <div class="langs" role="radiogroup" aria-label="PDF">
      {#each langs as l (l)}
        <button
          class="lang smallcaps"
          class:on={lang === l}
          role="radio"
          aria-checked={lang === l}
          onclick={() => onlang(l)}>{t(l === "en" ? "pdf.lang.en" : "pdf.lang.zh")}</button
        >
      {/each}
    </div>
    {#if building}
      <span class="state busy"><span class="spinner" aria-hidden="true"></span>{t("pdf.compiling")}</span>
      <button class="btn small" onclick={() => store.cancel(lang)}>{t("pdf.stop")}</button>
    {:else}
      <button
        class="btn small primary"
        onclick={() => oncompile(lang)}
        disabled={!compilable || !store.ready}
        title={t("pdf.compileTitle", { key: `${mod}B` })}>{t("pdf.compile")}</button
      >
      {#if build}
        <button class="state" class:bad={build.outcome !== "ok"} onclick={() => (showProblems = !showProblems)}>
          {#if build.outcome === "ok"}
            {t("pdf.ok", { time: formatDuration(build.durationMs) })}
          {:else if build.outcome === "timed_out"}
            {t("pdf.timedOut")}
          {:else if build.outcome === "failed"}
            {t("pdf.failed")}
          {/if}
          {#if errors.length}<span class="count err">{count(errors.length, "pdf.error", "pdf.errors")}</span>{/if}
          {#if warnings.length}<span class="count warn">{count(warnings.length, "pdf.warning", "pdf.warnings")}</span
            >{/if}
        </button>
      {/if}
    {/if}
    <span class="spacer"></span>
    <button class="icon" onclick={onlocate} disabled={!doc} title={t("pdf.locate", { key: `${mod}${alt}J` })} aria-label={t("pdf.locate", { key: `${mod}${alt}J` })}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="4.5" /><path d="M8 1.5v3M8 11.5v3M1.5 8h3M11.5 8h3" /></svg>
    </button>
    <button class="icon" onclick={() => setZoom(zoom / 1.15)} disabled={!doc} title={t("pdf.zoomOut")} aria-label={t("pdf.zoomOut")}>−</button>
    <button class="zoom" class:on={zoomMode === "fit"} onclick={() => setZoom("fit")} disabled={!doc} title={t("pdf.fit")}
      >{Math.round(zoom * 100)}%</button
    >
    <button class="icon" onclick={() => setZoom(zoom * 1.15)} disabled={!doc} title={t("pdf.zoomIn")} aria-label={t("pdf.zoomIn")}>+</button>
    <button class="icon" onclick={() => latexIpc.savePdf(lang).catch(() => {})} disabled={!doc} title={t("pdf.save")} aria-label={t("pdf.save")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.5V10M5.5 7.5 8 10l2.5-2.5M3 11v2.5h10V11" /></svg>
    </button>
    {#if lang === "zh"}
      <button class="icon" onclick={onexporttex} disabled={!compilable} title={t("pdf.exportTex")} aria-label={t("pdf.exportTex")}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 2.5h5.5L12 5v8.5H4zM9.5 2.5V5H12M6 9.5h4M8 7.5v4" /></svg>
      </button>
    {/if}
    <button class="icon" onclick={() => latexIpc.revealPdf(lang).catch(() => {})} disabled={!doc} title={t("pdf.reveal")} aria-label={t("pdf.reveal")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2z" /></svg>
    </button>
  </header>

  {#if build && (showProblems || build.outcome !== "ok") && (build.issues.length || build.outcome !== "ok")}
    <div class="problems">
      {#if build.stale && build.outcome !== "ok" && build.hasPdf}<p class="note">{t("pdf.stale")}</p>{/if}
      {#if lang === "zh" && build.untranslated > 0}<p class="note">{t("pdf.untranslated", { n: build.untranslated })}</p>{/if}
      <ul>
        {#each build.issues.slice(0, 80) as issue, i (i)}
          <li>
            <button class="issue" data-severity={issue.severity} onclick={() => onissue(issue)} title={issue.message}>
              <span class="dot" aria-hidden="true"></span>
              <span class="text">{issueLabel(issue)}</span>
            </button>
          </li>
        {/each}
      </ul>
      {#if boxes.length}<p class="note">{t("pdf.boxes", { n: boxes.length })}</p>{/if}
      {#if build.output && (build.outcome !== "ok" || showOutput)}
        <button class="link" onclick={() => (showOutput = !showOutput)}>{t("pdf.output")}</button>
        {#if showOutput}<pre class="output">{build.output.slice(-6000)}</pre>{/if}
      {/if}
    </div>
  {:else if lang === "zh" && build && build.untranslated > 0}
    <div class="problems"><p class="note">{t("pdf.untranslated", { n: build.untranslated })}</p></div>
  {/if}

  <div class="scroller" bind:this={scroller} style:--zoom={zoom}>
    {#if store.status && !store.status.found}
      <div class="empty">
        <p class="title">{t("pdf.noTex")}</p>
        <p>{t("pdf.noTexHint")}</p>
        <button class="btn small" onclick={() => store.loadStatus(true)}>{t("pdf.checkAgain")}</button>
      </div>
    {:else if !compilable}
      <div class="empty"><p>{t("pdf.unsaved")}</p></div>
    {:else if failure}
      <div class="empty"><p class="error">{failure}</p></div>
    {:else if loadError}
      <div class="empty"><p class="error">{t("pdf.loadFailed", { error: loadError })}</p></div>
    {:else if !doc}
      <div class="empty">
        <p>{loading || building ? t("pdf.loading") : t("pdf.empty")}</p>
      </div>
    {/if}

    {#if doc}
      <div class="pages">
        {#each sizes as size, i (i)}
          {@const n = i + 1}
          <div
            class="page"
            data-page={n}
            use:registerPage={n}
            style:width="{Math.floor(size.width * zoom)}px"
            style:height="{Math.floor(size.height * zoom)}px"
            style:--scale-factor={zoom}
            onclick={(e) => onPageClick(e, n)}
            role="presentation"
          >
            {#each visibleMarks.filter((b) => b.page === n) as b, j (j)}
              <div
                class="mark"
                style:left="{b.left * zoom - 2}px"
                style:top="{b.top * zoom - 1}px"
                style:width="{b.width * zoom + 4}px"
                style:height="{b.height * zoom + 2}px"
              ></div>
            {/each}
            {#if menu && menu.page === n}
              <div
                class="pdf-menu"
                style:left="{Math.max(0, Math.min(menu.x * zoom, size.width * zoom - 260))}px"
                style:top="{menu.y * zoom + 14}px"
                role="dialog"
              >
                {#if menu.here}
                  <span class="label">{menu.paragraph ? t("pdf.paragraph") : t("pdf.sentence")}</span>
                  <button class="btn small primary" onclick={() => onaction("polish")}>{t("pdf.polish")}</button>
                  <button class="btn small" onclick={() => onaction("edit")}>{t("pdf.edit")}</button>
                  <button class="btn small" onclick={() => onaction("ask")}>{t("pdf.ask")}</button>
                {:else if menu.file}
                  <span class="label">{t("pdf.elsewhere", { file: menu.file, line: menu.line })}</span>
                  <button class="btn small primary" onclick={() => onopenfile(menu.file, menu.line)}>{t("pdf.openFile")}</button>
                {:else}
                  <span class="label">{t("pdf.noSource")}</span>
                {/if}
                <button class="x" onclick={onclosemenu} aria-label={t("common.close")}>✕</button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</section>

<style>
  .pdf {
    height: 100%;
    display: grid;
    grid-template-rows: auto auto 1fr;
    grid-template-columns: minmax(0, 1fr);
    background: var(--paper-2);
    min-width: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--rule);
    background: var(--chrome);
    min-width: 0;
    flex-wrap: wrap;
  }
  .langs {
    display: inline-flex;
    padding: 2px;
    border: 1px solid var(--rule);
    border-radius: 5px;
    background: var(--paper);
  }
  .lang {
    border: 0;
    background: transparent;
    padding: 1px 8px 3px;
    border-radius: 3px;
    font-size: 13px;
    color: var(--muted);
    cursor: pointer;
  }
  .lang.on {
    background: var(--ink);
    color: var(--paper);
  }
  .state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: transparent;
    font: inherit;
    font-size: 12.5px;
    color: var(--muted);
    cursor: pointer;
    padding: 2px 4px;
    white-space: nowrap;
  }
  .state.busy {
    color: var(--accent);
    cursor: default;
  }
  .state.bad {
    color: var(--error);
  }
  .count {
    padding: 0 6px;
    border-radius: 8px;
    font-size: 11.5px;
  }
  .count.err {
    background: var(--seal-wash);
    color: var(--error);
  }
  .count.warn {
    background: var(--paper-2);
    color: var(--ink-2);
    border: 1px solid var(--rule);
  }
  .spinner {
    width: 10px;
    height: 10px;
    border: 1.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
  }
  .spacer {
    flex: 1;
  }
  .icon,
  .zoom {
    height: 24px;
    min-width: 24px;
    border: 0;
    border-radius: 4px;
    background: transparent;
    color: var(--ink-2);
    font: inherit;
    font-size: 14px;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .zoom {
    font-family: var(--font-mono);
    font-size: 11.5px;
    min-width: 44px;
    color: var(--muted);
  }
  .zoom.on {
    color: var(--accent);
  }
  .icon:hover:not(:disabled),
  .zoom:hover:not(:disabled) {
    background: var(--accent-wash);
    color: var(--accent);
  }
  .icon:disabled,
  .zoom:disabled {
    opacity: 0.35;
    cursor: default;
  }
  svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .problems {
    max-height: 30vh;
    overflow-y: auto;
    padding: 6px 12px 8px;
    border-bottom: 1px solid var(--rule);
    background: var(--paper);
    font-size: 12.5px;
  }
  .problems ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .issue {
    display: flex;
    align-items: baseline;
    gap: 7px;
    width: 100%;
    border: 0;
    background: transparent;
    padding: 2px 0;
    text-align: left;
    font: inherit;
    font-size: 12.5px;
    color: var(--ink-2);
    cursor: pointer;
  }
  .issue:hover .text {
    color: var(--accent);
  }
  .issue .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 11.5px;
  }
  .dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--faint);
    transform: translateY(-1px);
  }
  .issue[data-severity="error"] .dot {
    background: var(--error);
  }
  .issue[data-severity="warning"] .dot {
    background: var(--seal);
  }
  .note {
    margin: 2px 0 4px;
    color: var(--muted);
    font-style: italic;
  }
  .link {
    border: 0;
    background: transparent;
    padding: 0;
    color: var(--accent);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .output {
    max-height: 200px;
    overflow: auto;
    margin: 4px 0 0;
    padding: 6px 8px;
    background: var(--paper-2);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: 11px;
    white-space: pre-wrap;
    user-select: text;
    -webkit-user-select: text;
  }
  .scroller {
    overflow: auto;
    min-height: 0;
    position: relative;
  }
  .pages {
    display: grid;
    justify-items: center;
    gap: 14px;
    padding: 16px 18px 40vh;
  }
  .page {
    position: relative;
    background: white;
    box-shadow:
      0 1px 2px rgba(0, 0, 0, 0.12),
      0 8px 24px -12px rgba(0, 0, 0, 0.35);
    --user-unit: 1;
    --total-scale-factor: calc(var(--scale-factor) * var(--user-unit));
    --scale-round-x: 1px;
    --scale-round-y: 1px;
    cursor: text;
  }
  .page :global(canvas) {
    position: absolute;
    inset: 0;
    display: block;
  }
  .mark {
    position: absolute;
    z-index: 3;
    border-radius: 3px;
    background: rgba(255, 196, 0, 0.28);
    box-shadow: 0 0 0 1.5px rgba(214, 150, 0, 0.55);
    pointer-events: none;
    animation: pulse 4s var(--ease) both;
  }
  .pdf-menu {
    position: absolute;
    z-index: 4;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 6px 6px 10px;
    border: 1px solid var(--rule-strong);
    border-radius: 6px;
    background: var(--paper);
    box-shadow: 0 10px 28px -12px rgba(0, 0, 0, 0.45);
    cursor: default;
    white-space: nowrap;
    animation: rise 140ms var(--ease);
  }
  .pdf-menu .label {
    font-size: 12.5px;
    color: var(--muted);
    margin-right: 2px;
  }
  .pdf-menu .x {
    border: 0;
    background: transparent;
    color: var(--faint);
    cursor: pointer;
    font-size: 11px;
  }
  .empty {
    padding: 14vh 36px 0;
    color: var(--muted);
    font-size: 14px;
    max-width: 520px;
  }
  .empty .title {
    font-size: 16px;
    color: var(--ink-2);
    font-weight: 600;
  }
  .error {
    color: var(--error);
    white-space: pre-wrap;
  }
  /* pdf.js text layer: transparent text over the canvas, for clicks and selection. */
  .page :global(.textLayer) {
    position: absolute;
    text-align: initial;
    inset: 0;
    overflow: clip;
    opacity: 1;
    line-height: 1;
    letter-spacing: normal;
    word-spacing: normal;
    text-size-adjust: none;
    -webkit-text-size-adjust: none;
    forced-color-adjust: none;
    transform-origin: 0 0;
    caret-color: transparent;
    z-index: 2;
    --min-font-size: 1;
    --text-scale-factor: calc(var(--total-scale-factor) * var(--min-font-size));
    --min-font-size-inv: calc(1 / var(--min-font-size));
  }
  .page :global(.textLayer :is(span, br)) {
    color: transparent;
    position: absolute;
    white-space: pre;
    cursor: pointer;
    transform-origin: 0% 0%;
    user-select: text;
    -webkit-user-select: text;
  }
  .page :global(.textLayer > :not(.markedContent)),
  .page :global(.textLayer .markedContent span:not(.markedContent)) {
    z-index: 1;
    --font-height: 0;
    font-size: calc(var(--text-scale-factor) * var(--font-height));
    --scale-x: 1;
    --rotate: 0deg;
    transform: rotate(var(--rotate)) scaleX(var(--scale-x)) scale(var(--min-font-size-inv));
  }
  .page :global(.textLayer .markedContent) {
    display: contents;
  }
  .page :global(.textLayer span:hover) {
    background: rgba(40, 90, 200, 0.08);
  }
  .page :global(.textLayer ::selection) {
    background: rgba(40, 90, 200, 0.25);
    color: transparent;
  }
  .page :global(.textLayer .endOfContent) {
    display: block;
    position: absolute;
    inset: 100% 0 0;
    z-index: 0;
    cursor: default;
    user-select: none;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes pulse {
    0% {
      opacity: 0;
    }
    8% {
      opacity: 1;
    }
    80% {
      opacity: 1;
    }
    100% {
      opacity: 0;
    }
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translateY(3px);
    }
  }
</style>
