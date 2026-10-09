// A stand-in backend for working on the UI in a plain browser (dev/mock.html,
// `npm run dev:mock`). It answers the IPC commands with in-memory data and
// emits events like the Rust side does. It is never part of the app bundle:
// only dev/mock.html loads it.

import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type {
  KeyEntry,
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

/** Six relay accounts with made-up key tails (never real key material). */
const POOL: (Omit<KeyEntry, "name">)[] = [
  { number: 1, fingerprint: "a1b2c3d4e5f6", tail: "Xa7Q", state: "ready", inFlight: 2, detail: null },
  { number: 2, fingerprint: "b2c3d4e5f6a1", tail: "M3kd", state: "ready", inFlight: 1, detail: null },
  { number: 3, fingerprint: "c3d4e5f6a1b2", tail: "p0Rz", state: "cooling", inFlight: 0, detail: null },
  { number: 4, fingerprint: "d4e5f6a1b2c3", tail: "Ve2t", state: "ready", inFlight: 2, detail: null },
  { number: 5, fingerprint: "e5f6a1b2c3d4", tail: "q8Lw", state: "ready", inFlight: 1, detail: null },
  { number: 6, fingerprint: "f6a1b2c3d4e5", tail: "Hn5s", state: "ready", inFlight: 0, detail: null },
];

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
        keyNames: {},
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
        keyConcurrency: 2,
        maxRetries: 10,
        hasKey: true,
        keyCount: 6,
        keyNames: {},
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
        keyConcurrency: 2,
        maxRetries: 10,
      },
      {
        name: "DeepSeek",
        kind: "openai_compatible",
        wireApi: "chat",
        baseUrl: "https://api.deepseek.com/v1",
        model: "deepseek-chat",
        effort: "low",
        serviceTier: null,
        keyConcurrency: null,
        maxRetries: null,
      },
    ],
    promptDir: "/Users/me/Library/Application Support/app.biwrite.desktop/prompts",
    assistantProvider: "",
    assistantLabel: "AnyRouter (GPT-6 Astra) · gpt-6-astra",
    assistantReady: true,
    logDir: "/Users/me/Library/Logs/app.biwrite.desktop",
    requestLog: { enabled: true, persist: true } as LogSettings,
    batchSize: 3,
    matchPool: true,
    effectiveConcurrency: 12,
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
    key: { number: ((id - 1) % 6) + 1, count: 6, tail: POOL[(id - 1) % 6].tail, fingerprint: POOL[(id - 1) % 6].fingerprint },
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
      key: { number: 1, count: 1, tail: "9f3c", fingerprint: "d41d8cd98f00" },
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
  key_status: () => POOL.map((k) => ({ ...k, detail: k.detail })),
  list_api_keys: () => POOL.map((k) => ({ ...k, name: state.settings.providers[1].keyNames[k.fingerprint] ?? "" })),
  rename_api_key: (a) => {
    state.settings.providers[1].keyNames[String(a.fingerprint)] = String(a.name);
    return state.settings;
  },
  test_api_key: () => "图神经网络可以从上下文中的少量示例学习新任务。",
  set_batch_size: (a) => {
    state.settings.batchSize = Number(a.size);
    return state.settings;
  },
  set_match_pool: (a) => {
    state.settings.matchPool = Boolean(a.on);
    state.settings.effectiveConcurrency = state.settings.matchPool ? 12 : state.settings.concurrency;
    return state.settings;
  },
  save_provider: (a) => {
    const p = a.provider as ProviderView;
    const i = state.settings.providers.findIndex((x) => x.id === p.id);
    if (i >= 0) state.settings.providers[i] = { ...state.settings.providers[i], ...p };
    return state.settings;
  },
  get_cache: () => ({ entries: 1284, bytes: 2_310_000, location: "/Users/me/Library/Application Support/app.biwrite.desktop/cache.sqlite3", groups: [] }),
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
  const names = ["qzkinharbin", "qzkinhit", "qzkinjsj", "qzkinlss", "qzkinmdc", "qzkinxj"];
  POOL.forEach((k, i) => (state.settings.providers[1].keyNames[k.fingerprint] = names[i]));
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
