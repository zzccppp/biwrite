import assert from "node:assert/strict";
import { test } from "node:test";
import { sentenceBounds, sentenceIndexAt } from "./sentences.ts";

const read = (text: string) => sentenceBounds(text).map((b) => text.slice(b.start, b.end));

test("Chinese sentences end at their marks", () => {
  assert.deepEqual(read("第一句。第二句很短！第三句"), ["第一句。", "第二句很短！", "第三句"]);
});

test("English sentences skip abbreviations, decimals and marks without a space", () => {
  assert.deepEqual(read("See Fig. 2 for x = 1.5. The model, e.g. a GNN, wins!  Next one?"), [
    "See Fig. 2 for x = 1.5.",
    "The model, e.g. a GNN, wins!",
    "Next one?",
  ]);
  assert.deepEqual(read("A value of 0.5 holds.Then"), ["A value of 0.5 holds.Then"]);
});

test("blank text is no sentence", () => {
  assert.deepEqual(read("  \n "), []);
  assert.deepEqual(read("一句。  "), ["一句。"]);
});

test("a click between sentences belongs to the one before", () => {
  const text = "One. Two. Three.";
  const bounds = sentenceBounds(text);
  assert.equal(sentenceIndexAt(bounds, text.indexOf("Two")), 1);
  assert.equal(sentenceIndexAt(bounds, text.indexOf(" Three")), 1);
  assert.equal(sentenceIndexAt(bounds, 0), 0);
  assert.equal(sentenceIndexAt(bounds, text.length), 2);
});
