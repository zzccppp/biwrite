import assert from "node:assert/strict";
import { test } from "node:test";
import { asParagraph, insertionAt, reapplyInstruction, uniqueIndex } from "./assistText.ts";

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
