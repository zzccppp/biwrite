// pdf.js setup: loaded on first use (it is large), with its worker and
// run-time assets (character maps, standard fonts, image decoders) served
// from /pdfjs/. Also helpers for the viewer.

import type { PDFDocumentProxy } from "pdfjs-dist/legacy/build/pdf.mjs";
import workerUrl from "pdfjs-dist/legacy/build/pdf.worker.min.mjs?url";

export type PdfJs = typeof import("pdfjs-dist/legacy/build/pdf.mjs");

let loaded: Promise<PdfJs> | null = null;

/** The pdf.js module, imported once. */
export function pdfjs(): Promise<PdfJs> {
  loaded ??= import("pdfjs-dist/legacy/build/pdf.mjs").then((m) => {
    m.GlobalWorkerOptions.workerSrc = workerUrl;
    return m;
  });
  return loaded;
}

function asset(dir: string): string {
  return new URL(`/pdfjs/${dir}/`, window.location.href).href;
}

/** Open a PDF from its bytes. */
export async function openPdf(bytes: ArrayBuffer): Promise<PDFDocumentProxy> {
  const { getDocument } = await pdfjs();
  return getDocument({
    data: new Uint8Array(bytes),
    cMapUrl: asset("cmaps"),
    cMapPacked: true,
    standardFontDataUrl: asset("standard_fonts"),
    wasmUrl: asset("wasm"),
    enableXfa: false,
  }).promise;
}

/** Both caret lookups: WebKit has only the older `caretRangeFromPoint`. */
interface CaretLookup {
  caretPositionFromPoint?(x: number, y: number): { offsetNode: Node; offset: number } | null;
  caretRangeFromPoint?(x: number, y: number): Range | null;
}

/** Character offset (UTF-16) of the point (`x`, `y`) in the text of `span`. */
export function caretOffset(span: HTMLElement, x: number, y: number): number {
  const doc = document as unknown as CaretLookup;
  const pos = doc.caretPositionFromPoint?.(x, y);
  if (pos && span.contains(pos.offsetNode)) return pos.offset;
  const range = doc.caretRangeFromPoint?.(x, y);
  if (range && span.contains(range.startContainer)) return range.startOffset;
  // Proportional guess along the span.
  const r = span.getBoundingClientRect();
  const length = span.textContent?.length ?? 0;
  return Math.max(0, Math.min(length, Math.round(((x - r.left) / Math.max(r.width, 1)) * length)));
}
