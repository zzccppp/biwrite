// A stand-in backend for working on the UI in a plain browser (dev/mock.html,
// `npm run dev:mock`). It answers the IPC commands with in-memory data and
// emits events like the Rust side does. It is never part of the app bundle:
// only dev/mock.html loads it.

import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type {
  LogSettings,
  ProviderView,
  RequestRecord,
  SegmentLayout,
  SegmentState,
  SessionView,
  SettingsView,
  Snapshot,
} from "../lib/types";

type Args = Record<string, unknown>;

function clone<T>(value: T): T {
  return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T);
}

const SAMPLE = `Graph neural networks can learn new tasks from a few examples in context.

This note collects open questions about whether the same idea transfers to graph neural networks. A graph lets the model use its message-passing machinery, which seems important.

How many examples per class are needed before accuracy saturates? Early results suggest that three to five are enough on citation graphs.`;

const state = {
  text: SAMPLE,
  revision: 1,
  version: 1,
  nextRecord: 1,
  settings: {
    providers: [
      {
        id: "mock",
        name: "Mock (offline, reverses text)",
        kind: "mock",
        baseUrl: "",
        model: "reverse",
        temperature: 0,
        effort: "default",
        hasKey: false,
        keyCount: 0,
        builtin: true,
        needsKey: false,
      },
      {
        id: "p-relay",
        name: "AnyRouter (GPT-6 Astra)",
        kind: "openai_compatible",
        wireApi: "responses",
        serviceTier: "priority",
        baseUrl: "https://anyrouter.top/v1",
        model: "gpt-6-astra",
        temperature: 0,
        effort: "high",
        hasKey: true,
        keyCount: 3,
        builtin: false,
        needsKey: true,
      },
    ] as ProviderView[],
    activeProvider: "p-relay",
    activeLabel: "AnyRouter (GPT-6 Astra) · gpt-6-astra",
    concurrency: 4,
    docNote: "",
    presets: [
      {
        name: "AnyRouter (GPT-6 Astra)",
        kind: "openai_compatible",
        wireApi: "responses",
        baseUrl: "https://anyrouter.top/v1",
        model: "gpt-6-astra",
        effort: "high",
        serviceTier: "priority",
      },
      {
        name: "DeepSeek",
        kind: "openai_compatible",
        wireApi: "chat",
        baseUrl: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        effort: "low",
        serviceTier: null,
      },
    ],
    promptDir: "/Users/me/Library/Application Support/app.biwrite.desktop/prompts",
    assistantProvider: "",
    assistantLabel: "AnyRouter (GPT-6 Astra) · gpt-6-astra",
    assistantReady: true,
    requestLog: { enabled: true, persist: true } as LogSettings,
  } as SettingsView,
  records: [] as RequestRecord[],
};

/** Plain-mode segmentation: paragraphs separated by blank lines. */
function layout(text: string): SegmentLayout[] {
  const out: SegmentLayout[] = [];
  const re = /[^\n]+(?:\n[^\n]+)*/g;
  let m: RegExpExecArray | null;
  let id = 1;
  while ((m = re.exec(text))) {
    out.push({ id: id++, kind: { type: "paragraph" }, from: m.index, to: m.index + m[0].length });
  }
  return out;
}

function fakeChinese(source: string): string {
  return `（译文）${source.slice(0, 40)}……`;
}

function snapshot(full = true): Snapshot {
  const segs = layout(state.text);
  const states: SegmentState[] = segs.map((s) => ({
    id: s.id,
    version: state.version++,
    status: "translated",
    text: fakeChinese(state.text.slice(s.from, s.to)),
    partial: false,
    error: null,
  }));
  return {
    revision: state.revision++,
    mode: "plain",
    direction: "en-zh",
    full,
    layout: segs,
    states,
    usage: { requests: state.records.length, inputTokens: 1200, outputTokens: 300, cacheHits: 2 },
  };
}

function session(): SessionView {
  return {
    path: "/Users/me/paper/notes.txt",
    name: "notes.txt",
    text: state.text,
    dirty: false,
    autoTranslate: true,
    lineEnding: "lf",
    bom: false,
    snapshot: snapshot(),
  };
}

/** A finished request as the Rust side would report it. */
export function fakeRecord(over: Partial<RequestRecord> = {}): RequestRecord {
  const id = state.nextRecord++;
  return {
    id,
    startedAt: Date.now() - 1000 * (10 - id),
    purpose: "translate",
    provider: "AnyRouter (GPT-6 Astra)",
    wire: "responses",
    endpoint: "anyrouter.top/v1/responses",
    key: { number: ((id - 1) % 3) + 1, count: 3, tail: ["H5aK", "q9Zt", "77Lm"][(id - 1) % 3] },
    request: { model: "gpt-6-astra", effort: "high", serviceTier: "priority" },
    response: { model: "gpt-6-astra", effort: "high", serviceTier: "default" },
    httpStatus: 200,
    state: "ok",
    error: null,
    durationMs: 6000 + id * 731,
    firstTokenMs: 4200,
    usage: { inputTokens: 91, cachedTokens: 0, outputTokens: 136, reasoningTokens: 118 },
    promptChars: 420,
    outputChars: 18,
    notes: [],
    ...over,
  };
}

function seedRecords(): void {
  state.records = [
    fakeRecord(),
    fakeRecord({
      state: "error",
      error: "rate limited by the provider",
      response: { model: "gpt-6-astra", effort: "high", serviceTier: "auto" },
      usage: { inputTokens: null, cachedTokens: null, outputTokens: null, reasoningTokens: null },
      notes: ["stream error rate_limit_exceeded: Your requests to gpt-6-astra have exceeded token rate limit.", "key 2 of 3 set aside: retrying with another key"],
      durationMs: 1900,
      firstTokenMs: null,
    }),
    fakeRecord(),
    fakeRecord({ purpose: "polish", response: { model: "gpt-6-astra-2026-09", effort: "medium", serviceTier: null } }),
    fakeRecord({
      provider: "DeepSeek",
      wire: "chat",
      endpoint: "api.deepseek.com/v1/chat/completions",
      key: { number: 1, count: 1, tail: "9f3c" },
      request: { model: "deepseek-chat", effort: null, serviceTier: null },
      response: { model: "deepseek-chat", effort: null, serviceTier: null },
      usage: { inputTokens: 210, cachedTokens: 128, outputTokens: 64, reasoningTokens: null },
    }),
    fakeRecord({ state: "in_flight", durationMs: null, firstTokenMs: null, httpStatus: null, response: { model: null, effort: null, serviceTier: null }, usage: { inputTokens: null, cachedTokens: null, outputTokens: null, reasoningTokens: null } }),
  ];
}

const handlers: Record<string, (args: Args) => unknown> = {
  get_session: () => session(),
  update_document: (a) => {
    state.text = String(a.text);
    return snapshot(false);
  },
  set_mode: (a) => {
    state.text = String(a.text);
    return snapshot(false);
  },
  set_dirty: () => null,
  set_auto_translate: () => null,
  retranslate_segment: () => null,
  retranslate_all: () => null,
  get_settings: () => state.settings,
  set_assistant_provider: (a) => {
    state.settings.assistantProvider = String(a.id);
    return state.settings;
  },
  key_status: () => [
    { number: 1, tail: "H5aK", state: "ready", detail: null },
    { number: 2, tail: "q9Zt", state: "rejected", detail: "request rejected (HTTP 401): invalid api key — check the API key" },
    { number: 3, tail: "77Lm", state: "cooling", detail: null },
  ],
  get_prompt: (a) => ({
    text: a.direction === "en-zh" ? "You translate academic English into Chinese…" : "You translate the author's Chinese…",
    isDefault: true,
    path: "/prompts/en-zh.txt",
  }),
  get_request_log: () => ({ settings: state.settings.requestLog, records: [...state.records].reverse(), file: "/Users/me/Library/Application Support/app.biwrite.desktop/requests.jsonl" }),
  set_request_log: (a) => {
    state.settings.requestLog = a.settings as LogSettings;
    return handlers.get_request_log(a);
  },
  clear_request_log: (a) => {
    state.records = [];
    return handlers.get_request_log(a);
  },
  reveal_request_log: () => null,
  get_glossary: () => [],
};

/** Install the stand-in backend. Unknown commands fail like a missing Rust command. */
export function installMockBackend(): void {
  seedRecords();
  mockWindows("main");
  mockIPC(
    (cmd, payload) => {
      const handler = handlers[cmd];
      if (!handler) throw new Error(`mock backend: no handler for ${cmd}`);
      // Real IPC serializes: never hand out the mock's own objects.
      return clone(handler((payload ?? {}) as Args));
    },
    { shouldMockEvents: true },
  );
  // A request finishing a moment after start-up, as an event.
  setTimeout(() => {
    const done = state.records.find((r) => r.state === "in_flight");
    if (!done) return;
    Object.assign(done, {
      state: "ok",
      httpStatus: 200,
      durationMs: 8123,
      firstTokenMs: 5000,
      response: { model: "gpt-6-astra", effort: "high", serviceTier: "default" },
      usage: { inputTokens: 95, cachedTokens: 64, outputTokens: 120, reasoningTokens: 99 },
    });
    void emit("request-log", clone(done));
  }, 1500);
}

export const mockState = state;
