// Pure helpers for the Log panel: comparing what a request asked for with
// what its response declared, summaries and number formatting.
//
// A missing value is never filled in from the other side: a field the request
// did not send is "not sent", one the response did not state is "not
// returned". A differing model name can be an alias, so "differs" reports
// the claim and is not proof of what ran.

import type { RecordState, RequestRecord } from "./types";

export type Comparison = "match" | "differs" | "not_sent" | "not_returned" | "absent";

export function compare(requested: string | null, declared: string | null): Comparison {
  if (requested === null && declared === null) return "absent";
  if (requested === null) return "not_sent";
  if (declared === null) return "not_returned";
  return requested.trim().toLowerCase() === declared.trim().toLowerCase() ? "match" : "differs";
}

export interface Summary {
  total: number;
  inFlight: number;
  errors: number;
  /** Finished requests whose response declared a different model, effort or tier. */
  differs: number;
  inputTokens: number;
  cachedTokens: number;
  outputTokens: number;
  reasoningTokens: number;
  /** Mean duration of successful requests, in ms (null: none yet). */
  meanMs: number | null;
}

export function differs(r: RequestRecord): boolean {
  return (
    compare(r.request.model, r.response.model) === "differs" ||
    compare(r.request.effort, r.response.effort) === "differs" ||
    compare(r.request.serviceTier, r.response.serviceTier) === "differs"
  );
}

export function summarize(records: Iterable<RequestRecord>): Summary {
  const s: Summary = {
    total: 0,
    inFlight: 0,
    errors: 0,
    differs: 0,
    inputTokens: 0,
    cachedTokens: 0,
    outputTokens: 0,
    reasoningTokens: 0,
    meanMs: null,
  };
  let okCount = 0;
  let okMs = 0;
  for (const r of records) {
    s.total++;
    if (r.state === "in_flight") s.inFlight++;
    if (r.state === "error") s.errors++;
    if (r.state !== "in_flight" && differs(r)) s.differs++;
    s.inputTokens += r.usage.inputTokens ?? 0;
    s.cachedTokens += r.usage.cachedTokens ?? 0;
    s.outputTokens += r.usage.outputTokens ?? 0;
    s.reasoningTokens += r.usage.reasoningTokens ?? 0;
    if (r.state === "ok" && r.durationMs !== null) {
      okCount++;
      okMs += r.durationMs;
    }
  }
  if (okCount > 0) s.meanMs = Math.round(okMs / okCount);
  return s;
}

/** 950 → "950 ms", 10267 → "10.3 s", 125000 → "2 min 5 s". */
export function formatDuration(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
  const min = Math.floor(ms / 60_000);
  const sec = Math.round((ms % 60_000) / 1000);
  return `${min} min ${sec} s`;
}

/** 999 → "999", 1234 → "1.2k", 98765 → "99k", 2500000 → "2.5M". */
export function formatCount(n: number | null): string {
  if (n === null) return "—";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** Local wall-clock time "14:05:09". */
export function formatClock(unixMs: number): string {
  const d = new Date(unixMs);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export const STATE_LABELS: Record<RecordState, string> = {
  in_flight: "in flight",
  ok: "ok",
  error: "error",
  cancelled: "cancelled",
};

/** Keep the newest `max` records (highest ids). */
export function newest(records: RequestRecord[], max: number): RequestRecord[] {
  return [...records].sort((a, b) => b.id - a.id).slice(0, max);
}
