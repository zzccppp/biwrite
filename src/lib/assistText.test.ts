import assert from "node:assert/strict";
import { test } from "node:test";
import { ChangeSet, Text } from "@codemirror/state";
import {
  askedAbout,
  asParagraph,
  insertAfter,
  insertEdges,
  insertionAt,
  mapPlace,
  newPreambleLines,
  notLoaded,
  type Place,
  reapplyInstruction,
  revertChange,
  uniqueIndex,
} from "./assistText.ts";

test("assist: a target is found again only if it is unique", () => {
  assert.equal(uniqueIndex("one two one", "two"), 4);
  assert.equal(uniqueIndex("one two one", "one"), -1);
  assert.equal(uniqueIndex("one two", "three"), -1);
  assert.equal(uniqueIndex("abc", ""), -1);
});

test("assist: the re-apply instruction carries both versions", () => {
  const s = reapplyInstruction("Old text.", "New text.");
  assert.ok(s.includes("<earlier_version>\nOld text.\n</earlier_version>"));
  assert.ok(s.includes("<approved_version>\nNew text.\n</approved_version>"));
  assert.ok(!s.includes("<revision>"), "never the answer's own tag");
});

test("assist: new paragraphs never split a line or run into a neighbour", () => {
  const doc = "\\section{Intro}\nFirst line.\n\nNext.";
  // In the middle of a line: its end; at a line end or on a blank line: there.
  assert.equal(insertionAt(doc, 5), doc.indexOf("\nFirst"));
  assert.equal(insertionAt(doc, doc.indexOf("\nFirst")), doc.indexOf("\nFirst"));
  // After a heading whose paragraph starts on the next line.
  const at = doc.indexOf("\nFirst");
  const insert = asParagraph(doc.slice(0, at), doc.slice(at), "New.");
  assert.equal(doc.slice(0, at) + insert + doc.slice(at), "\\section{Intro}\n\nNew.\n\nFirst line.\n\nNext.");
  // Typed text after the paragraph: the new one goes after it, apart.
  assert.equal(asParagraph("Para. More.", "\n\nNext.", "New."), "\n\nNew.");
  // At the start and the end of the document: nothing extra.
  assert.equal(asParagraph("", "\n\nNext.", "New."), "New.");
  assert.equal(asParagraph("Last.", "", "New."), "\n\nNew.");
  assert.equal(asParagraph("A.\n\n", "\n\nB.", "New."), "New.");
});

/** `doc` after `spec`, with `place` mapped through it as the job store does. */
function edit(
  doc: Text,
  place: Place,
  spec: { from: number; to?: number; insert?: string },
  holds?: { start: boolean; end: boolean },
): [Text, Place] {
  const changes = ChangeSet.of(spec, doc.length);
  return [changes.apply(doc), mapPlace(place, changes.desc, holds)];
}

const text = (s: string) => Text.of(s.split("\n"));

/** Apply the change Revert makes, or say it was refused. */
function reverted(doc: Text, place: Place, applied: string, original: string): string | null {
  const change = revertChange(doc, place, applied, original);
  return change ? ChangeSet.of(change, doc.length).apply(doc).toString() : null;
}

test("assist: Revert puts the replaced text back, and leaves edited text alone", () => {
  let doc = text("Intro. Polished sentence. Outro.");
  let place: Place = { from: 7, to: 25 };
  // Typed right after it: the author's, kept by the revert.
  [doc, place] = edit(doc, place, { from: 25, insert: " Mine." });
  assert.equal(reverted(doc, place, "Polished sentence.", "Rough sentence."), "Intro. Rough sentence. Mine. Outro.");
  // Edited inside: nothing is overwritten.
  [doc, place] = edit(doc, place, { from: 8, insert: "x" });
  assert.equal(reverted(doc, place, "Polished sentence.", "Rough sentence."), null);
  // Gone past the end of the document.
  assert.equal(revertChange(text("Intro."), { from: 7, to: 25 }, "Polished sentence.", "Rough."), null);
});

test("assist: text typed at the end of an inserted paragraph keeps it from being reverted", () => {
  const whole = "First paragraph.\n\nSecond paragraph.";
  const at = insertionAt(whole, 5);
  const insert = asParagraph(whole.slice(0, at), whole.slice(at), "New paragraph.");
  const holds = insertEdges(insert);
  const applied = ChangeSet.of({ from: at, insert }, whole.length).apply(text(whole));
  const place = { from: at, to: at + insert.length };
  // The author goes on writing the new paragraph: it is no longer as inserted.
  const [extended, grown] = edit(applied, place, { from: place.to, insert: " Author's sentence." }, holds);
  assert.equal(extended.toString(), "First paragraph.\n\nNew paragraph. Author's sentence.\n\nSecond paragraph.");
  assert.equal(reverted(extended, grown, insert, ""), null);
  // Typed at the end of the paragraph before: that one's, and it stays there.
  const [before, kept] = edit(applied, place, { from: at, insert: " More." }, holds);
  assert.equal(reverted(before, kept, insert, ""), "First paragraph. More.\n\nSecond paragraph.");
});

test("assist: text typed at the start of an inserted paragraph keeps it from being reverted", () => {
  const whole = "Second paragraph.";
  const insert = asParagraph("", whole, "New paragraph.");
  assert.equal(insert, "New paragraph.\n\n");
  const holds = insertEdges(insert);
  const applied = text(insert + whole);
  const place = { from: 0, to: insert.length };
  const [typed, grown] = edit(applied, place, { from: 0, insert: "Lead. " }, holds);
  assert.equal(reverted(typed, grown, insert, ""), null);
  // Typed at the start of the paragraph after: it stays on that one.
  const [after, kept] = edit(applied, place, { from: insert.length, insert: "So " }, holds);
  assert.equal(reverted(after, kept, insert, ""), "So Second paragraph.");
});

test("assist: a follow-up never asks about the paragraph that took a deleted text's place", () => {
  const doc = text("Asked about.\n\nNext paragraph.");
  const place = { from: 0, to: 12 };
  assert.deepEqual(askedAbout(place), place);
  const [, gone] = edit(doc, place, { from: 0, to: 14 });
  assert.equal(gone.from, gone.to);
  assert.equal(askedAbout(gone), null);
});

test("assist: accepting a figure again adds only the packages not loaded yet", () => {
  const preamble = [
    "\\documentclass{article}",
    "\\usepackage[table]{xcolor}",
    "\\usepackage{amsmath, booktabs}",
    "% \\usepackage{multirow}",
    "\\usepackage{graphicx} % \\usepackage{tikz}",
    "",
  ].join("\n");
  assert.deepEqual(notLoaded(preamble, ["booktabs", "xcolor", "graphicx"]), []);
  assert.deepEqual(notLoaded(preamble, ["multirow", "tikz", "booktabs"]), ["multirow", "tikz"]);
});

test("assist: another card's paragraph or a new one after an inserted paragraph leaves it revertible", () => {
  const whole = "First paragraph.\n\nSecond paragraph.";
  const at = whole.indexOf("\n\nSecond");
  const insert = asParagraph(whole.slice(0, at), whole.slice(at), "Card A.");
  let doc = ChangeSet.of({ from: at, insert }, whole.length).apply(text(whole));
  let place: Place = { from: at, to: at + insert.length };
  const after = (spec: { from: number; insert: string }) => {
    const changes = ChangeSet.of(spec, doc.length);
    doc = changes.apply(doc);
    place = insertAfter(place, insert, changes);
  };
  // Card B goes in right after card A's paragraph.
  after({ from: place.to, insert: "\n\nCard B." });
  assert.equal(reverted(doc, place, insert, ""), "First paragraph.\n\nCard B.\n\nSecond paragraph.");
  // The author starts a paragraph of their own right after it.
  after({ from: place.to, insert: "\n\nMine." });
  assert.equal(
    reverted(doc, place, insert, ""),
    "First paragraph.\n\nMine.\n\nCard B.\n\nSecond paragraph.",
  );
  // Going on with its last sentence still makes it the author's.
  after({ from: place.to, insert: " More." });
  assert.equal(reverted(doc, place, insert, ""), null);
});

test("assist: a package after an escaped line break and a comment sign counts as not loaded", () => {
  assert.deepEqual(notLoaded("a\\\\% \\usepackage{tikz}\n", ["tikz"]), ["tikz"]);
  assert.deepEqual(notLoaded("100\\% \\usepackage{tikz}\n", ["tikz"]), []);
});

test("assist: lines for the preamble that it has already are not added again", () => {
  const preamble = "\\documentclass{article}\n\\usepackage{booktabs,graphicx}\n\\usetikzlibrary{arrows.meta}\n% \\usepackage{tikz}\n";
  const lines = "\\usepackage{booktabs}\n\\usepackage{tikz,graphicx}\n\\usetikzlibrary{arrows.meta}\n\\usetikzlibrary{positioning}\n";
  assert.equal(newPreambleLines(preamble, lines), "\\usepackage{tikz}\n\\usetikzlibrary{positioning}\n");
  assert.equal(newPreambleLines(preamble, "\\usepackage{booktabs}\n"), "");
  assert.equal(newPreambleLines("", "\\usepackage[table]{xcolor}\n"), "\\usepackage[table]{xcolor}\n");
});
