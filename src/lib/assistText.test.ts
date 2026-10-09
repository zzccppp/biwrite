import assert from "node:assert/strict";
import { test } from "node:test";
import { reapplyInstruction, uniqueIndex } from "./assistText.ts";

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
