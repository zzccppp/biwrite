// Sentences of a shown text, found by the rules the source side uses
// (biwrite_core::assist::sentence_at): a sentence ends at 。！？, or at
// . ! ? before whitespace or the end, unless the period ends a common
// abbreviation, together with the marks and closing quotes or brackets right
// after it. A sentence of the translation is matched to the source sentence
// in the same place of its paragraph.

/** Abbreviations after which a period does not end a sentence. The same as
 * biwrite_core::assist's (a test compares them). */
export const ABBREVIATIONS = [
  "e.g.", "i.e.", "et al.", "etc.", "vs.", "cf.", "approx.", "resp.", "Fig.", "Figs.", "Eq.",
  "Eqs.", "Eqn.", "Eqns.", "Sec.", "Secs.", "Sect.", "Tab.", "Thm.", "Thms.", "Lem.", "Def.",
  "Defn.", "Prop.", "Cor.", "Conj.", "Rem.", "Alg.", "App.", "Ch.", "Chap.", "Ex.", "Ref.",
  "Refs.", "No.", "Nos.", "Vol.", "pp.", "Dr.", "Prof.", "Mr.", "Mrs.", "Ms.",
];

/** Abbreviations that end a sentence after all when a capital follows:
 * "… by Smith et al. We …". */
export const ENDING = ["et al.", "etc."];

/** Closing quotes and brackets that stay with the sentence they close. */
export const CLOSERS = "\"')]}”’）」』》〉】";

const FULL_STOPS = "。！？";
const MARKS = ".!?" + FULL_STOPS;

/** How far apart (in parts of the paragraph) the middles of two sentences in
 * the same place may lie. Languages differ in length sentence by sentence,
 * and LaTeX source is longer than the text shown, so only far-off places
 * count as another sentence. */
const PLACE_SLACK = 0.25;

export interface Bounds {
  start: number;
  end: number;
}

/** Where a sentence lies in its paragraph: its start and end as parts (0–1)
 * of the paragraph's sentences put together (what lies between them, such
 * as comments, does not count). */
export interface Place {
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
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    const at = i++;
    if (!MARKS.includes(c)) continue;
    // Marks and closing quotes or brackets right after it are its own.
    while (i < text.length && (MARKS.includes(text[i]) || CLOSERS.includes(text[i]))) i++;
    // A no-break space (`~`) keeps it going.
    const breakAfter = i === text.length || (text[i] !== "\u00a0" && /\s/.test(text[i]));
    if (FULL_STOPS.includes(c) || (breakAfter && !(c === "." && goesOn(text.slice(0, at + 1), text.slice(i))))) {
      close(i);
    }
  }
  close(text.length);
  return out;
}

/** Whether the period ending `before` ends an abbreviation the sentence goes
 * on after (`rest`: what follows its closing marks). */
function goesOn(before: string, rest: string): boolean {
  // `et~al.` is `et al.` with a no-break space.
  before = before.replace(/[~\u00a0]/g, " ");
  const word = (a: string) => before.endsWith(a) && !/[A-Za-z]$/.test(before.slice(0, -a.length));
  if (ENDING.some(word)) return !/^\s*\p{Lu}/u.test(rest);
  return ABBREVIATIONS.some(word) || dotted(before);
}

/** `before` ends in two or more single letters each followed by a period:
 * i.i.d., w.r.t., s.t. */
function dotted(before: string): boolean {
  const letter = (c: string | undefined) => c !== undefined && /^[A-Za-z]$/.test(c);
  let n = before.length;
  let letters = 0;
  while (n >= 2 && before[n - 1] === "." && letter(before[n - 2])) {
    letters++;
    n -= 2;
  }
  return letters >= 2 && !letter(before[n - 1]);
}

/** The sentence at `offset`: the one containing it, else the last one before it. */
export function sentenceIndexAt(bounds: Bounds[], offset: number): number {
  let index = 0;
  for (let i = 0; i < bounds.length; i++) {
    if (bounds[i].start <= offset) index = i;
  }
  return index;
}

/** The sentence of a cursor at `offset`: as `sentenceIndexAt`, but right
 * after a closing mark with the next sentence straight after it (Chinese),
 * the sentence the mark closes. */
export function sentenceAtCursor(bounds: Bounds[], offset: number): number {
  const index = sentenceIndexAt(bounds, offset);
  return index > 0 && bounds[index].start === offset && bounds[index - 1].end === offset ? index - 1 : index;
}

/** Where sentence `index` of `bounds` lies in its paragraph. */
export function placeOf(bounds: Bounds[], index: number): Place {
  const lengths = bounds.map((b) => b.end - b.start);
  const total = Math.max(1, lengths.reduce((a, b) => a + b, 0));
  const before = lengths.slice(0, index).reduce((a, b) => a + b, 0);
  return { start: before / total, end: (before + (lengths[index] ?? 0)) / total };
}

/** Whether a sentence of the translation can be one of the source: the
 * numbers in it, and its Latin words when it is Chinese, all come from the
 * source sentence, as they are mostly copied over (citations, references,
 * math, names). One that is not tells two sentences apart that are as many
 * and in about the same place. */
export function anchorsAgree(translation: string, source: string): boolean {
  const chinese = /[\u3400-\u9fff]/.test(translation);
  const anchors = translation.match(chinese ? /[A-Za-z]{2,}|\d+/g : /\d+/g) ?? [];
  const words = source.toLowerCase();
  return anchors.every((a) => words.includes(a.toLowerCase()));
}

/** Whether two sentences lie in about the same place of their paragraphs. */
export function samePlace(a: Place, b: Place): boolean {
  return Math.abs((a.start + a.end) / 2 - (b.start + b.end) / 2) <= PLACE_SLACK;
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

/** How far before caret `offset` of `node` the character under point
 * `x`,`y` starts: 0 when the point is on the character after the caret, else
 * the length of the one before it (the caret goes after a character clicked
 * on its right half, and to the end of a line clicked beyond it). */
export function charBack(node: Node, offset: number, x: number, y: number): number {
  if (!(node instanceof Text) || offset === 0) return 0;
  const rects = (from: number, to: number) => {
    const range = document.createRange();
    range.setStart(node, from);
    range.setEnd(node, to);
    return [...range.getClientRects()];
  };
  // The whole of a character outside the Basic Multilingual Plane.
  const back = offset > 1 && /[\udc00-\udfff]/.test(node.data[offset - 1]) ? 2 : 1;
  const next = node.data.codePointAt(offset);
  if (next === undefined) return back;
  const after = rects(offset, offset + (next > 0xffff ? 2 : 1))[0];
  const before = rects(offset - back, offset).at(-1);
  if (!after || !before) return back;
  // On one line, by the point's x alone (also between two lines, outside
  // the characters' boxes). Where the line wraps, by the nearer line.
  if (Math.abs(after.top - before.top) < 1) return x >= after.left ? 0 : back;
  const off = (r: DOMRect) => (y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0);
  return off(after) < off(before) ? 0 : back;
}

/** KaTeX's MathML copy of a formula: hidden, so no part of the text shown. */
const HIDDEN = ".katex-mathml";

/** The text nodes of `root` that are shown, in order. */
function shownNodes(root: Node): Text[] {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT, {
    acceptNode: (node) =>
      node instanceof Element && node.matches(HIDDEN) ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT,
  });
  const out: Text[] = [];
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    if (node instanceof Text) out.push(node);
  }
  return out;
}

/** The text of `root` as shown (without KaTeX's hidden MathML). */
export function shownText(root: Node): string {
  return shownNodes(root)
    .map((n) => n.data)
    .join("");
}

/** The offset in `shownText` of position `offset` of `node` inside `root`. */
export function shownOffset(root: Node, node: Node, offset: number): number {
  const before = document.createRange();
  before.setStart(root, 0);
  before.setEnd(node, offset);
  const part = before.cloneContents();
  for (const hidden of part.querySelectorAll(HIDDEN)) hidden.remove();
  return part.textContent?.length ?? 0;
}

/** The DOM range of characters `start`..`end` of `root`'s shown text. */
export function textRange(root: Node, start: number, end: number): Range | null {
  const range = document.createRange();
  let seen = 0;
  let started = false;
  for (const node of shownNodes(root)) {
    const length = node.data.length;
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
