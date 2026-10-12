<script lang="ts">
  import { onMount } from "svelte";
  import { t } from "../i18n.svelte";
  import { assistIpc, errorMessage } from "../ipc";
  import { openPdf } from "../pdf";
  import type { FigureView } from "../types";

  interface Props {
    /** The figure job. */
    id: number;
    figure: FigureView;
    /** A message for the card (a save, or a failure). */
    onnote: (text: string) => void;
  }

  let { id, figure, onnote }: Props = $props();

  let canvas = $state<HTMLCanvasElement>();
  let failed = $state(false);
  /** PNG export: pixels per PDF point (300 dpi). */
  const PNG_SCALE = 300 / 72;

  /** The figure's PDF drawn at `scale` (pixels per point). */
  async function drawPdf(target: HTMLCanvasElement, scale: number): Promise<void> {
    const doc = await openPdf(await assistIpc.figurePdf(id));
    try {
      const page = await doc.getPage(1);
      const viewport = page.getViewport({ scale });
      target.width = Math.ceil(viewport.width);
      target.height = Math.ceil(viewport.height);
      const ctx = target.getContext("2d");
      if (!ctx) return;
      ctx.fillStyle = "#fff";
      ctx.fillRect(0, 0, target.width, target.height);
      await page.render({ canvasContext: ctx, viewport, canvas: target }).promise;
    } finally {
      void doc.loadingTask.destroy();
    }
  }

  /** The diagram's SVG drawn at `scale` (pixels per CSS pixel). */
  async function drawSvg(target: HTMLCanvasElement, scale: number): Promise<void> {
    const img = new Image();
    img.src = svgUrl;
    await img.decode();
    target.width = Math.ceil(img.naturalWidth * scale);
    target.height = Math.ceil(img.naturalHeight * scale);
    const ctx = target.getContext("2d");
    if (!ctx) return;
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, target.width, target.height);
    ctx.drawImage(img, 0, 0, target.width, target.height);
  }

  const svgUrl = $derived(figure.svg ? `data:image/svg+xml;charset=utf-8,${encodeURIComponent(figure.svg)}` : "");

  onMount(() => {
    if (!figure.pdf || !canvas) return;
    drawPdf(canvas, 2 * (window.devicePixelRatio || 1)).catch(() => (failed = true));
  });

  async function save(kind: "pdf" | "pptx" | "png"): Promise<void> {
    try {
      let png: number[] | null = null;
      if (kind === "png") {
        const off = document.createElement("canvas");
        if (figure.pdf) await drawPdf(off, PNG_SCALE);
        else await drawSvg(off, 300 / 96);
        const blob = await new Promise<Blob | null>((done) => off.toBlob(done, "image/png"));
        if (!blob) return;
        png = Array.from(new Uint8Array(await blob.arrayBuffer()));
      }
      const path = await assistIpc.figureSave(id, kind, png);
      if (path) onnote(t("assist.figure.saved", { path }));
    } catch (err) {
      onnote(errorMessage(err));
    }
  }
</script>

<div class="figure">
  {#if figure.pdf}
    <canvas bind:this={canvas} class="preview" class:failed></canvas>
  {:else if figure.svg}
    <img class="preview" src={svgUrl} alt="" />
  {/if}
  <div class="row saves">
    <span class="label smallcaps">{t("assist.figure.saveAs")}</span>
    {#if figure.pptx}<button class="link" onclick={() => save("pptx")}>PPT</button>{/if}
    {#if figure.pdf}<button class="link" onclick={() => save("pdf")}>PDF</button>{/if}
    <button class="link" onclick={() => save("png")}>PNG</button>
  </div>
</div>

<style>
  .figure {
    display: grid;
    gap: 4px;
  }
  .preview {
    display: block;
    max-width: 100%;
    max-height: 260px;
    width: auto;
    height: auto;
    justify-self: center;
    border: 1px solid var(--rule);
    border-radius: 3px;
    background: #fff;
  }
  canvas.preview {
    width: 100%;
    object-fit: contain;
  }
  .preview.failed {
    display: none;
  }
  .saves {
    gap: 8px;
    font-size: 12.5px;
  }
  .label {
    font-size: 12px;
    color: var(--muted);
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
    padding: 0 2px;
  }
</style>
