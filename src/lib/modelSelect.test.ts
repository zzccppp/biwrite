import assert from "node:assert/strict";
import test from "node:test";
import {
  isLoopbackBaseUrl,
  mergeModels,
  sameUrlOrigin,
  stepOptionIndex,
  visibleModels,
} from "./modelSelect.ts";

const FETCHED = ["claude-opus-4-6", "claude-sonnet-4-5", "deepseek-chat", "deepseek-reasoner", "gpt-5"];

test("modelSelect: opening the dropdown keeps every fetched model visible without deleting the input", () => {
  // Even when `draft.model` already holds a saved or preset model, `filtering = false` shows all models.
  const withSaved = visibleModels(FETCHED, "deepseek-chat", false);
  assert.deepEqual(withSaved.options, FETCHED);
  assert.equal(withSaved.canAddCustom, false);

  // Even when `draft.model` is a preset name not in the server's list, all fetched models still appear.
  const withPreset = visibleModels(FETCHED, "gpt-6-astra", false);
  assert.deepEqual(withPreset.options, FETCHED);
  assert.equal(withPreset.canAddCustom, false);
});

test("modelSelect: typing filters case-insensitively with prefix matches first and offers custom models", () => {
  const sub = visibleModels(FETCHED, "SONNET", true);
  assert.deepEqual(sub.options, ["claude-sonnet-4-5"]);
  assert.equal(sub.canAddCustom, true);
  assert.equal(sub.customValue, "SONNET");

  // Prefix matches rank ahead of mid-string matches.
  const ranked = visibleModels(["my-gpt-5", "gpt-5-mini", "gpt-5"], "gpt-5", true);
  assert.deepEqual(ranked.options, ["gpt-5-mini", "gpt-5", "my-gpt-5"]);
  // Exact match exists, so no duplicate "+ add" item is offered.
  assert.equal(ranked.canAddCustom, false);

  // Brand-new custom model name with zero server matches can still be added.
  const custom = visibleModels(FETCHED, "  my-finetune-v2  ", true);
  assert.deepEqual(custom.options, []);
  assert.equal(custom.canAddCustom, true);
  assert.equal(custom.customValue, "my-finetune-v2");

  // Clearing the input while filtering shows the full list again.
  const cleared = visibleModels(FETCHED, "   ", true);
  assert.deepEqual(cleared.options, FETCHED);
  assert.equal(cleared.canAddCustom, false);
});

test("modelSelect: added custom models merge cleanly without duplicates", () => {
  const merged = mergeModels(FETCHED, ["my-finetune-v2", "deepseek-chat", "  ", "my-finetune-v2"]);
  assert.deepEqual(merged, ["my-finetune-v2", ...FETCHED]);
});

test("modelSelect: keyboard option stepping and URL helpers", () => {
  assert.equal(stepOptionIndex(-1, 1, 3), 0);
  assert.equal(stepOptionIndex(-1, -1, 3), 2);
  assert.equal(stepOptionIndex(2, 1, 3), 0);
  assert.equal(stepOptionIndex(0, -1, 3), 2);
  assert.equal(stepOptionIndex(0, 1, 0), -1);

  assert.equal(sameUrlOrigin("https://anyrouter.top/v1", "https://anyrouter.top"), true);
  assert.equal(sameUrlOrigin("https://anyrouter.top/v1", "https://api.openai.com/v1"), false);
  assert.equal(sameUrlOrigin("file:///a", "file:///b"), false);
  assert.equal(sameUrlOrigin("not-a-url", "https://api.openai.com/v1"), false);

  assert.equal(isLoopbackBaseUrl("http://localhost:11434/v1"), true);
  assert.equal(isLoopbackBaseUrl("http://127.0.0.1:8080/v1"), true);
  assert.equal(isLoopbackBaseUrl("http://[::1]:8080/v1"), true);
  assert.equal(isLoopbackBaseUrl("https://api.openai.com/v1"), false);
  assert.equal(isLoopbackBaseUrl("ftp://localhost:21"), false);
});
