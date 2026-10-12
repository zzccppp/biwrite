import assert from "node:assert/strict";
import { test } from "node:test";
import { type Change, differingStretch, differingStretches } from "./stretch.ts";

/** `old` with `changes` (positions in `old`, in order) made. */
const apply = (old: string, changes: Change[]) => {
  let out = "";
  let at = 0;
  for (const c of changes) {
    assert.ok(c.from >= at && c.to >= c.from, "in order and apart");
    out += old.slice(at, c.from) + c.insert;
    at = c.to;
  }
  return out + old.slice(at);
};

const one = (old: string, text: string) => {
  const c = differingStretch(old, text);
  return c ? [c] : [];
};

/** No change starts or ends inside a character outside the BMP. */
const whole = (old: string, changes: Change[]) => {
  for (const c of changes) {
    for (const s of [c.insert, old.slice(c.from, c.to)]) {
      assert.ok(!/^[\udc00-\udfff]/.test(s) && !/[\ud800-\udbff]$/.test(s), JSON.stringify(c));
    }
  }
};

test("only the stretch that differs is replaced", () => {
  const old = "One.\n\nTwo words here.\n\nThree.";
  const text = "One.\n\nTwo other words here.\n\nThree.";
  assert.deepEqual(differingStretch(old, text), { from: 10, to: 10, insert: "other " });
  assert.equal(differingStretch(old, old), null);
  for (const [a, b] of [
    ["", "new"],
    ["gone", ""],
    ["aaa", "aa"],
    ["ab", "ba"],
    ["中文段落。", "中文的段落。"],
    ["x😀y", "x😃y"],
    ["😀", "😃"],
    ["a😀", "a😃b"],
  ]) {
    assert.equal(apply(a, one(a, b)), b, `${a} → ${b}`);
    whole(a, one(a, b));
  }
});

test("edits far apart are changes of their own", () => {
  const paragraphs = Array.from({ length: 40 }, (_, i) => `Paragraph ${i} says something.`);
  const old = paragraphs.join("\n\n") + "\n";
  const edited = [...paragraphs];
  edited[2] = "Paragraph 2 says something else.";
  edited[37] = "Paragraph 37 was rewritten.";
  const text = edited.join("\n\n") + "\n";
  const changes = differingStretches(old, text);
  assert.equal(changes.length, 2, JSON.stringify(changes));
  assert.equal(apply(old, changes), text);
  // Only the words that differ: the paragraphs between are untouched.
  assert.ok(changes.every((c) => c.to - c.from < 30), JSON.stringify(changes));
});

test("lines added, removed and moved come out right", () => {
  const cases: [string, string][] = [
    ["a\nb\nc\n", "a\nc\n"],
    ["a\nc\n", "a\nb\nc\n"],
    ["a\nb\nc", "a\nb\nc\nd"],
    ["a\nb\nc\nd\n", "d\na\nb\nc\n"],
    ["", "a\nb\n"],
    ["a\nb\n", ""],
    ["no newline", "no newline at all"],
    ["😀\nb\n😀\n", "😃\nb\n😃\n"],
  ];
  for (const [a, b] of cases) {
    const changes = differingStretches(a, b);
    assert.equal(apply(a, changes), b, JSON.stringify([a, b, changes]));
    whole(a, changes);
  }
  assert.deepEqual(differingStretches("same\n", "same\n"), []);
});

test("random edits always give the new text", () => {
  // A fixed sequence, the same on every run.
  let seed = 7;
  const next = (n: number) => {
    seed = (seed * 1103515245 + 12345) % 2147483648;
    return seed % n;
  };
  const words = ["alpha", "beta", "图", "😀", "\n", "\n\n", " ", "."];
  const make = () => Array.from({ length: next(60) }, () => words[next(words.length)]).join("");
  for (let k = 0; k < 300; k++) {
    const a = make();
    const lines = a.split("\n");
    for (let e = next(4); e >= 0; e--) {
      const at = next(lines.length + 1);
      if (next(2)) lines.splice(at, 1);
      else lines.splice(at, 0, make());
    }
    const b = lines.join("\n");
    const changes = differingStretches(a, b);
    assert.equal(apply(a, changes), b, JSON.stringify([a, b]));
    whole(a, changes);
  }
});
