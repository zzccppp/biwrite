import assert from "node:assert/strict";
import { test } from "node:test";
import { compare, formatCount, formatDuration, newest, summarize } from "./logFormat.ts";
import type { RequestRecord } from "./types.ts";

function record(id: number, over: Partial<RequestRecord> = {}): RequestRecord {
  return {
    id,
    startedAt: 0,
    purpose: "translate",
    provider: "Relay",
    wire: "responses",
    endpoint: "anyrouter.top/v1/responses",
    key: null,
    request: { model: "gpt-6-astra", effort: "high", serviceTier: "priority" },
    response: { model: "gpt-6-astra", effort: "high", serviceTier: "default" },
    httpStatus: 200,
    state: "ok",
    error: null,
    durationMs: 1000,
    firstTokenMs: 400,
    usage: { inputTokens: 100, cachedTokens: 40, outputTokens: 20, reasoningTokens: 10 },
    promptChars: 300,
    outputChars: 30,
    notes: [],
    ...over,
  };
}

test("log: comparisons never fill a missing side from the other", () => {
  assert.equal(compare("high", "high"), "match");
  assert.equal(compare("High", "high "), "match");
  assert.equal(compare("gpt-6-astra", "gpt-6-astra-2026"), "differs");
  assert.equal(compare(null, "default"), "not_sent");
  assert.equal(compare("priority", null), "not_returned");
  assert.equal(compare(null, null), "absent");
});

test("log: summary counts differences only for finished requests", () => {
  const s = summarize([
    record(1),
    record(2, { state: "in_flight", durationMs: null }),
    record(3, { state: "error", durationMs: 50, response: { model: null, effort: null, serviceTier: null } }),
    record(4, { response: { model: "gpt-6-astra", effort: "low", serviceTier: "priority" }, durationMs: 3000 }),
  ]);
  assert.equal(s.total, 4);
  assert.equal(s.inFlight, 1);
  assert.equal(s.errors, 1);
  // 1 and 4 differ (tier, effort); 2 is in flight; 3 returned nothing.
  assert.equal(s.differs, 2);
  assert.equal(s.meanMs, 2000);
  assert.equal(s.inputTokens, 400);
  assert.equal(s.reasoningTokens, 40);
});

test("log: number formatting", () => {
  assert.equal(formatDuration(950), "950 ms");
  assert.equal(formatDuration(10267), "10.3 s");
  assert.equal(formatDuration(125000), "2 min 5 s");
  assert.equal(formatDuration(null), "—");
  assert.equal(formatCount(999), "999");
  assert.equal(formatCount(1234), "1.2k");
  assert.equal(formatCount(98765), "99k");
  assert.equal(formatCount(2_500_000), "2.5M");
});

test("log: newest keeps the highest ids", () => {
  assert.deepEqual(
    newest([record(3), record(9), record(5)], 2).map((r) => r.id),
    [9, 5],
  );
});
