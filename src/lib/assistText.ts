// Pure helpers of the writing assistant (tested with node --test).

import type { ChangeDesc, ChangeSet } from "@codemirror/state";

/** A job's place in the document. */
export interface Place {
  from: number;
  to: number;
}

/** Index of `needle` in `hay` if it occurs exactly once, else -1. */
export function uniqueIndex(hay: string, needle: string): number {
  if (!needle) return -1;
  const first = hay.indexOf(needle);
  if (first < 0) return -1;
  return hay.indexOf(needle, first + 1) < 0 ? first : -1;
}

/** The instruction that asks the model to bring an approved revision onto
 * text the author has edited since. */
export function reapplyInstruction(earlier: string, approved: string): string {
  return [
    "The author approved a revision of an earlier version of this text.",
    `<earlier_version>\n${earlier}\n</earlier_version>`,
    `<approved_version>\n${approved}\n</approved_version>`,
    "The text in <target> is the current version: the author has edited it since. Apply the changes the approved version made to the current text, and keep every later edit of the author.",
  ].join("\n");
}

/** Where new paragraphs go near `pos`: the end of its line when text
 * follows on it, so a line is never split. */
export function insertionAt(text: string, pos: number): number {
  const nl = text.indexOf("\n", pos);
  const end = nl < 0 ? text.length : nl;
  return text.slice(pos, end).trim() ? end : pos;
}

/** `body` with the line breaks it needs to be a paragraph of its own
 * between `before` and `after` (the text on either side): a blank line on
 * each side, none at the start or the end of the document. */
export function asParagraph(before: string, after: string, body: string): string {
  const b = before.replace(/[ \t]+$/, "");
  const lead = b.trim() === "" || b.endsWith("\n\n") ? "" : b.endsWith("\n") ? "\n" : "\n\n";
  const a = after.replace(/^[ \t]+/, "");
  const trail = a.trim() === "" || a.startsWith("\n\n") ? "" : a.startsWith("\n") ? "\n" : "\n\n";
  return `${lead}${body}${trail}`;
}

/** Where `place` is after `changes`. Text typed at an edge stays outside,
 * unless `holds` says the place's own words reach that edge: it then joins
 * the place (typing at the end of an inserted paragraph extends it). */
export function mapPlace(place: Place, changes: ChangeDesc, holds = { start: false, end: false }): Place {
  const from = changes.mapPos(place.from, holds.start ? -1 : 1);
  return { from, to: Math.max(from, changes.mapPos(place.to, holds.end ? 1 : -1)) };
}

/** The edges inserted text reaches with its own words: not those where it
 * starts or ends with the line breaks that set it apart. */
export function insertEdges(text: string): { start: boolean; end: boolean } {
  return { start: /^\S/.test(text), end: /\S$/.test(text) };
}

/** The edges where an applied insert takes in text `changes` put there:
 * those its own words reach (see {@link insertEdges}), unless that text is
 * set apart by a line break (a paragraph the author starts, or another
 * card's insert), which then stays outside. */
export function heldEdges(place: Place, applied: string, changes: ChangeSet): { start: boolean; end: boolean } {
  const held = insertEdges(applied);
  changes.iterChanges((fromA, toA, _fromB, _toB, inserted) => {
    const text = inserted.toString();
    if (fromA === place.to && text.startsWith("\n")) held.end = false;
    if (toA === place.from && text.endsWith("\n")) held.start = false;
  });
  return held;
}

/** Where an applied insert's text is after `changes`. */
export function insertAfter(place: Place, applied: string, changes: ChangeSet): Place {
  return mapPlace(place, changes, heldEdges(place, applied, changes));
}

/** The change Revert makes: the text an applied job replaced (nothing, for
 * an insert) back over what it put in. Null when that was edited since:
 * nothing is then overwritten. */
export function revertChange(
  doc: { length: number; sliceString(from: number, to?: number): string },
  place: Place,
  applied: string,
  original: string,
): { from: number; to: number; insert: string } | null {
  if (place.to > doc.length || doc.sliceString(place.from, place.to) !== applied) return null;
  return { from: place.from, to: place.to, insert: original };
}

/** What a follow-up or a rerun asks about: the job's text, or null when it
 * was deleted (its place shrank to a point, where the paragraph there
 * would be taken instead). */
export function askedAbout(place: Place): Place | null {
  return place.to > place.from ? { from: place.from, to: place.to } : null;
}

/** The packages of `wanted` that no \usepackage line of `preamble` loads
 * (a figure accepted again after Revert, which leaves its lines in). */
export function notLoaded(preamble: string, wanted: string[]): string[] {
  const loaded = new Set<string>();
  for (const line of preamble.split("\n")) {
    // A comment starts at a % no backslash escapes (\\% is a line break and one).
    const code = line.replace(/(?<!\\)(?:\\\\)*%.*$/, "");
    for (const m of code.matchAll(/\\(?:usepackage|RequirePackage)\s*(?:\[[^\]]*\])?\s*\{([^}]*)\}/g)) {
      for (const name of m[1].split(",")) loaded.add(name.trim());
    }
  }
  return wanted.filter((p) => !loaded.has(p));
}
