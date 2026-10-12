// Sentences of a shown text, found by the rules the source side uses
// (biwrite_core::assist::sentence_at): a sentence ends at 。！？, or at
// . ! ? before whitespace or the end, unless the period ends a common
// abbreviation. A sentence of the translation is matched to the source
// sentence in the same place of its paragraph.

const ABBREVIATIONS = [
  "e.g.", "i.e.", "et al.", "etc.", "vs.", "Fig.", "Figs.", "Eq.", "Eqs.", "Sec.", "Secs.",
  "Tab.", "Ref.", "Refs.", "No.", "cf.", "approx.", "resp.", "Dr.", "Prof.",
];

export interface Bounds {
  start: number;
  end: number;
}

/** The sentences of `text` in order, without the blank text around them. */
export function sentenceBounds(text: string): Bounds[] {
  const out: Bounds[] = [];
  let start = 0;
  const close = (end: number) => {
    const slice = text.slice(start, end);
    if (slice.trim()) {
      const lead = slice.length - slice.trimStart().length;
      const trail = slice.length - slice.trimEnd().length;
      out.push({ start: start + lead, end: end - trail });
    }
    start = end;
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    let closes = "。！？".includes(c);
    if (".!?".includes(c)) {
      const next = text[i + 1];
      const spaceAfter = next === undefined || /\s/.test(next);
      const abbreviation = c === "." && ABBREVIATIONS.some((a) => text.slice(0, i + 1).endsWith(a));
      closes = spaceAfter && !abbreviation;
    }
    if (closes) close(i + 1);
  }
  close(text.length);
  return out;
}

/** The sentence at `offset`: the one containing it, else the last one before it. */
export function sentenceIndexAt(bounds: Bounds[], offset: number): number {
  let index = 0;
  for (let i = 0; i < bounds.length; i++) {
    if (bounds[i].start <= offset) index = i;
  }
  return index;
}

/** The text position under a point of the window (node and offset). */
export function caretAt(x: number, y: number): { node: Node; offset: number } | null {
  if (typeof document.caretPositionFromPoint === "function") {
    const pos = document.caretPositionFromPoint(x, y);
    return pos ? { node: pos.offsetNode, offset: pos.offset } : null;
  }
  // WebKit before caretPositionFromPoint.
  const legacy = document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null };
  const range = legacy.caretRangeFromPoint?.(x, y);
  return range ? { node: range.startContainer, offset: range.startOffset } : null;
}

/** The DOM range of characters `start`..`end` of `root`'s text content. */
export function textRange(root: Node, start: number, end: number): Range | null {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const range = document.createRange();
  let seen = 0;
  let started = false;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const length = node.textContent?.length ?? 0;
    if (!started && start <= seen + length) {
      range.setStart(node, start - seen);
      started = true;
    }
    if (started && end <= seen + length) {
      range.setEnd(node, end - seen);
      return range;
    }
    seen += length;
  }
  return null;
}
