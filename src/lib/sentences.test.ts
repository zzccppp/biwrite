import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  ABBREVIATIONS,
  anchorsAgree,
  CLOSERS,
  ENDING,
  placeOf,
  samePlace,
  sentenceAtCursor,
  sentenceBounds,
  sentenceIndexAt,
} from "./sentences.ts";

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

// The same cases run against biwrite_core::assist::sentences_at.
const cases: { text: string; sentences: string[] }[] = JSON.parse(
  readFileSync(new URL("./sentences.cases.json", import.meta.url), "utf8"),
);

test("the sentence cases the backend shares", () => {
  for (const c of cases) assert.deepEqual(read(c.text), c.sentences, c.text);
});

test("the lists are those of the backend", () => {
  const rust = readFileSync(new URL("../../crates/biwrite-core/src/assist.rs", import.meta.url), "utf8");
  const list = (name: string) => {
    const body = new RegExp(`const ${name}: &\\[[^\\]]+\\] = &\\[([\\s\\S]*?)\\];`).exec(rust)?.[1];
    assert.ok(body, name);
    return [...body.matchAll(/"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'/g)].map((m) =>
      (m[1] ?? m[2]).replace(/\\(.)/g, "$1"),
    );
  };
  assert.deepEqual(list("ABBREVIATIONS"), ABBREVIATIONS);
  assert.deepEqual(list("ENDING"), ENDING);
  assert.deepEqual(list("CLOSERS").join(""), CLOSERS);
});

test("sentences in the same place of their paragraphs agree", () => {
  // Kept apart by the splitter, a sentence of the source and of the
  // translation sit in about the same place.
  const en = "Prior work includes Smith et al. \\cite{smith2020transformers}. We extend it. Gradients are taken w.r.t. the weights.";
  const zh = "先前的工作包括 Smith 等人 [1]。我们对其进行了扩展。梯度是对权重求的。";
  const a = sentenceBounds(en);
  const b = sentenceBounds(zh);
  assert.equal(a.length, 3);
  assert.equal(b.length, 3);
  for (let i = 0; i < 3; i++) assert.ok(samePlace(placeOf(a, i), placeOf(b, i)), `sentence ${i}`);
  // As many sentences, but one long and two short ones the other way round.
  const long = "This first sentence runs on for quite a while, through clause after clause, before it ends. Short. Brief.";
  const reversed = "短。很短。这一句很长很长，一个分句接着一个分句，说了好一会儿才说完，长度远远超过前两句。";
  const c = sentenceBounds(long);
  const d = sentenceBounds(reversed);
  assert.equal(c.length, d.length);
  assert.ok(!samePlace(placeOf(c, 0), placeOf(d, 0)));
  assert.ok(!samePlace(placeOf(c, 2), placeOf(d, 2)));
});

test("the cursor right after a closing mark is in the sentence it closes", () => {
  const zh = "第一句。第二句。";
  const bounds = sentenceBounds(zh);
  assert.equal(sentenceAtCursor(bounds, zh.indexOf("第二")), 0);
  assert.equal(sentenceAtCursor(bounds, zh.indexOf("第二") + 1), 1);
  assert.equal(sentenceAtCursor(bounds, 0), 0);
  assert.equal(sentenceAtCursor(bounds, zh.length), 1);
  // A space between them: the cursor before the next one is in it.
  const en = "One. Two.";
  const b = sentenceBounds(en);
  assert.equal(sentenceAtCursor(b, en.indexOf("Two")), 1);
  assert.equal(sentenceAtCursor(b, en.indexOf(" Two")), 0);
});

test("a place counts the sentences only, not what lies between them", () => {
  // A comment line between two sentences of the source (cut out of both).
  const source = [{ start: 0, end: 10 }, { start: 100, end: 110 }];
  const shown = [{ start: 0, end: 10 }, { start: 11, end: 21 }];
  assert.deepEqual(placeOf(source, 1), placeOf(shown, 1));
  assert.deepEqual(placeOf(source, 1), { start: 0.5, end: 1 });
});

test("a translated sentence's numbers and names come from its source sentence", () => {
  // As many sentences in about the same place, one merged and one split.
  const zh = "该队列有 500 名患者。";
  assert.ok(anchorsAgree(zh, "The cohort has 500 patients."));
  assert.ok(!anchorsAgree(zh, "Each holds a Ph.D. in science."));
  assert.ok(anchorsAgree("我们改进了 TabPFN [hollmann2025]。", "We improve TabPFN \\cite{hollmann2025}."));
  assert.ok(!anchorsAgree("我们改进了 TabPFN。", "We extend it."));
  assert.ok(anchorsAgree("我们对其进行了扩展。", "We extend it."), "nothing to tell");
  // English shown for Chinese source: only numbers count.
  assert.ok(anchorsAgree("We train 3 models.", "我们训练 3 个模型。"));
  assert.ok(!anchorsAgree("We train 3 models.", "我们训练两个模型。"));
});
