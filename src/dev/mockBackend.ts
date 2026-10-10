// A stand-in backend for working on the UI in a plain browser (dev/mock.html,
// `npm run dev:mock`). It answers the IPC commands with in-memory data and
// emits events like the Rust side does. It is never part of the app bundle:
// only dev/mock.html loads it.

import { emit } from "@tauri-apps/api/event";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import PAPER from "../../samples/paper.tex?raw";
import PAPER_ZH from "../../dev/sample-zh.json";
import type {
  BuildView,
  KeyEntry,
  LogSettings,
  ProviderView,
  RequestRecord,
  SegmentLayout,
  SegmentState,
  SessionView,
  SettingsView,
  Snapshot,
  TemplateView,
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
  if (value === undefined || value instanceof ArrayBuffer) return value;
  return JSON.parse(JSON.stringify(value)) as T;
}

/** `dev/mock.html?latex` opens the sample LaTeX paper (its PDF: dev/sample.pdf). */
const PARAMS = new URLSearchParams(window.location.search);
const LATEX = PARAMS.has("latex");
/** `pending=N`: the last N paragraphs are still being translated. */
const PENDING = Number(PARAMS.get("pending") ?? 0);
/** `zhfile`: the sample paper's Chinese version is the open file. */
const ZH_FILE = PARAMS.has("zhfile") && LATEX;
/** `wrong`: that Chinese file was taken for English. */
const WRONG = PARAMS.has("wrong");

const SAMPLE = `Graph neural networks can learn new tasks from a few examples in context.

This note collects open questions about whether the same idea transfers to graph neural networks. A graph lets the model use its message-passing machinery, which seems important.

How many examples per class are needed before accuracy saturates? Early results suggest that three to five are enough on citation graphs.`;

/** The sample paper's blocks, and the same blocks in Chinese (for the swapped view). */
const EN_BLOCKS = PAPER.match(/[^\n]+(?:\n[^\n]+)*/g) ?? [];

function chineseText(): string {
  const body = PAPER.indexOf("\\begin{document}");
  let at = 0;
  return EN_BLOCKS.map((b) => {
    const from = PAPER.indexOf(b, at);
    at = from + b.length;
    const kind = latexKind(b, body >= 0 && from < body);
    return kind.type === "paragraph" ? inPlace(b) : b;
  }).join("\n\n");
}

/** The block with each known English text replaced by its Chinese, its
 * LaTeX structure (\\section{…}, environments, labels) kept. */
function inPlace(block: string): string {
  const pairs = Object.entries(PAPER_ZH as Record<string, string>).sort((a, b) => b[0].length - a[0].length);
  let out = block;
  for (const [en, zh] of pairs) if (out.includes(en)) out = out.split(en).join(zh);
  return out;
}

/** Indices (in `EN_BLOCKS`) of the sample paper's paragraphs. */
function paragraphIndices(): number[] {
  const body = PAPER.indexOf("\\begin{document}");
  let at = 0;
  const out: number[] = [];
  EN_BLOCKS.forEach((b, i) => {
    const from = PAPER.indexOf(b, at);
    at = from + b.length;
    if (latexKind(b, body >= 0 && from < body).type === "paragraph") out.push(i);
  });
  return out;
}

const state = {
  direction: (ZH_FILE && !WRONG ? "zh-en" : "en-zh") as "en-zh" | "zh-en",
  /** The direction in which the editor holds the file's own language. */
  home: (ZH_FILE && !WRONG ? "zh-en" : "en-zh") as "en-zh" | "zh-en",
  /** Blocks still being translated (indices into the layout). */
  pending: new Set<number>(LATEX ? paragraphIndices().slice(-PENDING || paragraphIndices().length) : []),
  /** Blocks left in English by an early swap, filled in later. */
  kept: new Set<number>(),
  text: ZH_FILE ? "" : LATEX ? PAPER : SAMPLE,
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
    latex: { compileOnSave: true },
    checkUpdates: true,
    closeToTray: true,
    version: "0.1.1",
  } as SettingsView,
  records: [] as RequestRecord[],
  /** Translations the assistant handed over (for UI tests). */
  offers: [] as { source: string; translation: string }[],
  /** The last assistant request (for UI tests). */
  lastAssist: null as Record<string, unknown> | null,
};

/** Paragraphs separated by blank lines (LaTeX blocks get a rough kind). */
function layout(text: string): SegmentLayout[] {
  const out: SegmentLayout[] = [];
  const re = /[^\n]+(?:\n[^\n]+)*/g;
  const body = text.indexOf("\\begin{document}");
  let m: RegExpExecArray | null;
  let id = 1;
  while ((m = re.exec(text))) {
    const kind = LATEX ? latexKind(m[0], body >= 0 && m.index < body) : ({ type: "paragraph" } as const);
    out.push({ id: id++, kind, from: m.index, to: m.index + m[0].length });
  }
  return out;
}

function fakeChinese(source: string): string {
  return `（译文）${source.slice(0, 40)}……`;
}

/** Real translations of the sample paper (dev/sample-zh.json), in the order they occur. */
function sampleChinese(source: string): string | null {
  const found = Object.entries(PAPER_ZH as Record<string, string>)
    .map(([en, zh]) => ({ at: source.indexOf(en), zh }))
    .filter((x) => x.at >= 0)
    .sort((a, b) => a.at - b.at);
  return found.length ? found.map((x) => x.zh).join("\n") : null;
}

/** What a LaTeX block is, roughly like the real segmenter. */
function latexKind(block: string, inPreamble: boolean): SegmentLayout["kind"] {
  if (inPreamble || /^\\(documentclass|usepackage|newcommand|title|author)/.test(block)) return { type: "skipped", reason: "preamble" };
  if (/^\\begin\{(equation|align)/.test(block)) return { type: "skipped", reason: "math" };
  if (/^\\begin\{table/.test(block)) return { type: "skipped", reason: "table" };
  if (/^\\(maketitle|bibliography|end\{document)/.test(block)) return { type: "skipped", reason: "markup" };
  return { type: "paragraph" };
}

function snapshot(full = true): Snapshot {
  const segs = layout(state.text);
  const states: SegmentState[] = segs.map((s) => {
    const source = state.text.slice(s.from, s.to);
    const skipped = s.kind.type === "skipped";
    const index = segs.indexOf(s);
    // The Chinese file's own language is Chinese: its translation is English.
    const chineseSource = state.direction === "zh-en";
    // A translation the assistant handed over wins.
    const offered = state.offers.find((o) => o.source.trim() === source.trim())?.translation;
    const shown =
      offered ??
      (chineseSource ? (EN_BLOCKS[index] ?? source) : ((LATEX ? sampleChinese(source) : null) ?? fakeChinese(source)));
    const waiting = !chineseSource && state.pending.has(index);
    const filling = chineseSource && state.kept.has(index);
    return {
      id: s.id,
      version: state.version++,
      status: skipped ? "skipped" : waiting || filling ? "translating" : "translated",
      text: skipped || waiting ? null : shown,
      partial: false,
      error: null,
    };
  });
  return {
    document: 1,
    revision: state.revision++,
    mode: LATEX ? "latex" : "plain",
    direction: state.direction,
    full,
    layout: segs,
    states,
    usage: { requests: state.records.length, inputTokens: 1200, outputTokens: 300, cacheHits: 2 },
  };
}

function session(): SessionView {
  const name = ZH_FILE ? "paper_zh.tex" : LATEX ? "paper.tex" : "notes.txt";
  return {
    path: LATEX ? `/Users/me/papers/gnn-icl/${name}` : "/Users/me/paper/notes.txt",
    name,
    text: state.text,
    dirty: false,
    autoTranslate: true,
    lineEnding: "lf",
    bom: false,
    pair: null,
    home: state.home,
    snapshot: snapshot(),
  };
}

/** The sample paper with Chinese paragraphs, except the blocks in `keep`. */
function chineseKeeping(keep: Set<number>): string {
  const zh = chineseText().split("\n\n");
  return EN_BLOCKS.map((b, i) => (keep.has(i) ? b : (zh[i] ?? b))).join("\n\n");
}

/** Put the kept blocks in Chinese one after another, as their translations arrive. */
function fillKept(): number {
  const zh = chineseText().split("\n\n");
  const kept = [...state.kept];
  kept.forEach((index, k) => {
    setTimeout(() => {
      if (!state.kept.has(index)) return;
      const segs = layout(state.text);
      const seg = segs[index];
      if (!seg) return;
      if (zh[index] === EN_BLOCKS[index]) {
        // Nothing to put in (the engine sends no fill then): just done.
        state.kept.delete(index);
        void emit("segment-states", snapshot().states);
        return;
      }
      void emit("segment-fills", [{ id: seg.id, old: EN_BLOCKS[index], new: zh[index] }]);
    }, 1400 + 700 * k);
  });
  return kept.length;
}

/** Translations of the pending blocks arrive. */
function finishPending(delay: number): number {
  const n = state.pending.size;
  setTimeout(() => {
    state.pending.clear();
    if (state.direction === state.home) void emit("segment-states", snapshot().states);
  }, delay);
  return n;
}

/** Share of Han characters against English words, as the Rust side judges. */
function chineseShare(text: string): number {
  const han = (text.match(/[\u3400-\u9fff]/g) ?? []).length;
  const letters = (text.match(/[A-Za-z]/g) ?? []).length;
  return han + letters / 5 > 0 ? han / (han + letters / 5) : 0;
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

let nextJob = 1;

/** Paragraph (blank-line separated) around `pos`, as [from, to). */
function paragraphAround(text: string, pos: number): [number, number] {
  const start = text.lastIndexOf("\n\n", Math.max(0, pos - 1));
  const from = start < 0 ? 0 : start + 2;
  const end = text.indexOf("\n\n", pos);
  return [from, end < 0 ? text.length : end];
}

/** Word diff by common prefix and suffix (enough for the mock). */
function roughDiff(a: string, b: string): { kind: string; text: string }[] {
  const wa = a.split(/(\s+)/);
  const wb = b.split(/(\s+)/);
  let p = 0;
  while (p < wa.length && p < wb.length && wa[p] === wb[p]) p++;
  let q = 0;
  while (q < wa.length - p && q < wb.length - p && wa[wa.length - 1 - q] === wb[wb.length - 1 - q]) q++;
  const parts = [
    { kind: "equal", text: wa.slice(0, p).join("") },
    { kind: "delete", text: wa.slice(p, wa.length - q).join("") },
    { kind: "insert", text: wb.slice(p, wb.length - q).join("") },
    { kind: "equal", text: wa.slice(wa.length - q).join("") },
  ];
  return parts.filter((x) => x.text);
}

const POLISHED: Record<string, { revision: string; translation: string; zh: string[]; en: string[] }> = {
  default: {
    revision: "This note asks whether the same idea transfers to graph neural networks. A graph lets the model use message passing, which the results below show to matter.",
    translation: "本文探讨同样的思路能否迁移到图神经网络。图结构使模型能够利用消息传递，下文结果表明这一点很重要。",
    zh: ["首句直接陈述研究问题，删去“收集开放问题”的铺垫。", "将“seems important”改为可核验的说法，指向后文结果。"],
    en: ["The first sentence states the question instead of announcing a collection of questions.", "Replaced “seems important” with a checkable statement that points to the results."],
  },
};

/** What the stand-in writes for "Write": two paragraphs and their Chinese. */
const WRITTEN = {
  revision:
    "Graph learning tasks change faster than labelled data can be collected. A citation graph gains new topics every month, and a molecule graph gains new properties with every assay, yet a trained graph neural network answers only the label set it was trained on.\n\n" +
    "We close this gap with prompt graphs. Instead of retraining, the model reads a handful of labelled example nodes attached to the query and predicts the new labels from them. Section~\\ref{sec:method} builds the prompt graph, and Section~\\ref{sec:experiments} shows that four benchmarks need no gradient update.",
  translation:
    "图学习任务的变化快于标注数据的积累。引文图每个月都有新主题，分子图每做一次实验就多出新的性质，而训练好的图神经网络只能回答训练时的标签集合。\n\n" +
    "我们用提示图弥补这一差距。模型不再重新训练，而是读取附在查询节点上的少量带标签示例节点，并据此预测新的标签。第~\\ref{sec:method}~节给出提示图的构建方法，第~\\ref{sec:experiments}~节表明四个基准都无需梯度更新。",
  zh: ["仿照参照论文引言的写法，先用两个具体场景说明问题，再给出方法。", "结果与引用都指向论文已有的小节，没有新增数字。"],
  en: ["Follows the reference introduction: two concrete settings state the problem, then the method answers it.", "Results and references point to the paper's own sections; no new numbers."],
};

/** A reference paper as `load_reference` returns it. */
const UNICLEAN = `\\section{Introduction}
Data cleaning is a crucial step in the data analysis pipeline. Real-world tables contain missing values, typos and violated constraints, and every downstream model inherits them.

We present UniClean, a cleaning system that composes operators into a pipeline and decides which operator to apply to each record.`;

const TABLE =
  "\\begin{table}[t]\n  \\centering\n  \\caption{Accuracy on four benchmarks (\\%).}\n  \\label{tab:accuracy}\n  \\begin{tabular}{lrrrr}\n    \\toprule\n    Method & Cora & CiteSeer & PubMed & arXiv \\\\\n    \\midrule\n    Fine-tuned GNN & 81.5 & 70.3 & 79.0 & 71.7 \\\\\n    Prompt graph (ours) & 80.9 & 69.8 & 78.6 & 70.9 \\\\\n    \\bottomrule\n  \\end{tabular}\n\\end{table}";

const TABLE_NOTES = {
  zh: ["三线表，两行方法对比四个基准，数值取自第 5 节。"],
  en: ["A booktabs table: two methods on four benchmarks, numbers from Section 5."],
};

function emitLater(delay: number, payload: unknown): void {
  setTimeout(() => void emit("assist", clone(payload)), delay);
}

let nextBuild = 1;

function wait(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms));
}

/** UTF-16 range of the sentence of the sample text that holds `words`. */
function sentenceOf(words: string): { from: number; to: number } {
  const text = state.text;
  const probe = words.trim().split(/\s+/).slice(0, 4).join(" ");
  const at = probe ? text.indexOf(probe) : -1;
  const pos = at >= 0 ? at : text.indexOf("We answer");
  const from = Math.max(text.lastIndexOf(". ", pos) + 2, text.lastIndexOf("\n", pos) + 1);
  const end = text.indexOf(". ", pos);
  const nl = text.indexOf("\n", pos);
  const to = Math.min(end < 0 ? text.length : end + 1, nl < 0 ? text.length : nl);
  return { from, to };
}

const TEMPLATES: TemplateView[] = [
  { id: "iclr2027", builtin: true, name: "ICLR 2027", description: "Official ICLR 2027 conference template with natbib and the ICLR math commands.", main: "iclr2027_conference.tex", engine: "pdflatex", source: "https://github.com/ICLR/Master-Template", files: 7, bytes: 134_000 },
  { id: "ieee-tii", builtin: true, name: "IEEE TII (Transactions on Industrial Informatics)", description: "IEEE TII article class (ieeecolor with the TII header) and IEEEtran references, with a short skeleton paper.", main: "main.tex", engine: "pdflatex", source: "https://www.ieee-ies.org/pubs/transactions-on-industrial-informatics", files: 6, bytes: 921_000 },
  { id: "pvldb", builtin: true, name: "VLDB (PVLDB Vol. 20, 2027)", description: "Official PVLDB template: acmart v2.19 (sigconf) with pvldb.sty, ACM reference format.", main: "main.tex", engine: "pdflatex", source: "https://github.com/vldbproceedings/VLDB-Template", files: 6, bytes: 469_000 },
  { id: "my-lab-thesis", builtin: false, name: "My lab thesis", description: "", main: "thesis.tex", engine: "xelatex", files: 23, bytes: 2_400_000 },
];

const RELEASES = [
  { tag: "v0.2.0", name: "BiWrite 0.2.0", version: "0.2.0", prerelease: false, date: "2026-10-10", notes: "LaTeX PDF view with SyncTeX, paper templates, writing assistant, key pools, request log, updates.", relation: "newer", asset: { name: "BiWrite_0.2.0_universal.dmg", size: 24_800_000, sha256: "9f2c" } },
  { tag: "v0.2.0-beta.1", name: "BiWrite 0.2.0 beta 1 (feat/v0.2)", version: "0.2.0-beta.1", prerelease: true, date: "2026-10-10", notes: "Branch build of feat/v0.2.", relation: "newer", asset: { name: "BiWrite_0.2.0-beta.1_universal.dmg", size: 24_700_000, sha256: "1a2b" } },
  { tag: "v0.1.1", name: "BiWrite 0.1.1", version: "0.1.1", prerelease: false, date: "2026-10-09", notes: "Formatted translation pane, logs, cache management.", relation: "current", asset: { name: "BiWrite_0.1.1_universal.dmg", size: 9_600_000, sha256: null } },
  { tag: "v0.1.0", name: "BiWrite 0.1.0", version: "0.1.0", prerelease: false, date: "2026-10-09", notes: "First release.", relation: "older", asset: { name: "BiWrite_0.1.0_universal.dmg", size: 9_500_000, sha256: null } },
];

const handlers: Record<string, (args: Args) => unknown> = {
  list_releases: async () => {
    await wait(500);
    return { current: "0.1.1", releases: RELEASES };
  },
  install_release: async (a) => {
    const release = RELEASES.find((r) => r.tag === a.tag)!;
    const total = release.asset.size;
    for (let i = 1; i <= 10; i++) {
      await wait(120);
      void emit("update-progress", { tag: release.tag, received: (total * i) / 10, total });
    }
    return { version: release.version, relaunch: true, quitting: false, file: null };
  },
  relaunch: () => null,
  set_check_updates: (a) => {
    state.settings.checkUpdates = Boolean(a.on);
    return state.settings;
  },
  latex_status: () => ({
    found: true,
    distribution: "TeX Live 2023",
    bin: "/Library/TeX/texbin",
    latexmk: true,
    synctex: true,
    engines: ["pdflatex", "xelatex", "lualatex"],
    customBin: null,
    compileOnSave: state.settings.latex.compileOnSave,
  }),
  latex_set_compile_on_save: (a) => {
    state.settings.latex.compileOnSave = Boolean(a.on);
    return null;
  },
  latex_choose_bin: () => handlers.latex_status({}),
  latex_reset_bin: () => handlers.latex_status({}),
  latex_project: () => {
    if (!LATEX) return null;
    const main = ZH_FILE ? "paper_zh.tex" : "paper.tex";
    return { folder: "gnn-icl", root: main, current: main, engine: ZH_FILE ? "xelatex" : "pdflatex", files: [main, "sections/appendix.tex"] };
  },
  latex_open: () => session(),
  latex_open_folder: () => session(),
  latex_compile: async (a): Promise<BuildView> => {
    await wait(1200);
    const zh = a.lang === "zh";
    return {
      id: nextBuild++,
      lang: zh ? "zh" : "en",
      outcome: "errors",
      hasPdf: true,
      stale: false,
      issues: [
        { severity: "error", file: "paper.tex", line: 45, message: "Package pdftex.def Error: File `figures/prompt_graph.pdf' not found: using draft setting.", here: true },
        { severity: "warning", file: null, line: 23, message: "Citation `kipf2017gcn' on page 1 undefined on input line 23.", here: true },
        { severity: "warning", file: null, line: 26, message: "Reference `sec:experiments' on page 1 undefined on input line 26.", here: true },
        { severity: "box", file: null, line: 61, message: "Overfull \\hbox (3.97pt too wide) in paragraph at lines 61--62", here: false },
      ],
      durationMs: zh ? 3400 : 1830,
      tool: zh ? "latexmk -xelatex" : "latexmk -pdf",
      engine: zh ? "xelatex" : "pdflatex",
      root: "paper.tex",
      untranslated: zh ? 2 : 0,
      output: "",
    };
  },
  latex_cancel: () => null,
  latex_export_tex: () => "/Users/me/papers/gnn-icl/paper_zh.tex",
  import_mirror: () => ({ ...session(), pair: { path: "/Users/me/papers/gnn-icl/paper_zh.tex", name: "paper_zh.tex", paired: 21, units: 21, dirty: false } }),
  close_mirror: () => null,
  write_mirror: () => ({ name: "paper_zh.tex", written: true, pending: 0, changed: 1 }),
  export_api_keys: () => "/Users/me/Desktop/AnyRouter-keys.txt",
  import_api_keys: () => state.settings,
  swap_languages: (a) => {
    state.direction = state.direction === "en-zh" ? "zh-en" : "en-zh";
    if (state.direction === "zh-en" && LATEX) {
      // Swapping early keeps the untranslated paragraphs in English for now.
      state.kept = a.keep ? new Set(state.pending) : new Set();
      state.pending.clear();
      state.text = chineseKeeping(state.kept);
      if (state.kept.size > 0) fillKept();
    } else {
      state.kept.clear();
      state.text = LATEX ? PAPER : SAMPLE;
    }
    return session();
  },
  retarget_language: (a) => {
    const text = String(a.text);
    if (a.onlyIfNeeded && (state.direction !== state.home || (state.home === "en-zh") === chineseShare(text) <= 0.5)) {
      return null;
    }
    state.home = state.home === "en-zh" ? "zh-en" : "en-zh";
    state.direction = state.home;
    state.pending.clear();
    state.kept.clear();
    state.text = text;
    return session();
  },
  continue_translation: (a) => {
    state.text = String(a.text);
    if (state.direction !== state.home) return fillKept();
    return finishPending(1500);
  },
  set_close_to_tray: (a) => {
    state.settings.closeToTray = Boolean(a.on);
    return state.settings;
  },
  set_tray_language: () => null,
  latex_pdf: async (a) => (await fetch(a.lang === "zh" ? "/dev/sample-zh.pdf" : "/dev/sample.pdf")).arrayBuffer(),
  latex_reveal_pdf: () => null,
  latex_save_pdf: () => "/Users/me/papers/gnn-icl/paper.pdf",
  latex_inverse: (a) => {
    const range = sentenceOf(String(a.span ?? ""));
    return { file: "paper.tex", line: 26, here: true, range, paragraph: false };
  },
  latex_forward: () => [{ page: 1, left: 133, top: 470, width: 345, height: 12 }],
  latex_locate: (a) => sentenceOf(String(a.span ?? "")),
  latex_goto: (a) => {
    const lines = state.text.split("\n");
    const n = Math.min(Number(a.line), lines.length) - 1;
    const from = lines.slice(0, n).reduce((sum, l) => sum + l.length + 1, 0);
    return { from, to: from + lines[n].length };
  },
  latex_templates: () => TEMPLATES,
  latex_new_paper: () => session(),
  latex_import_template: () => TEMPLATES[3],
  latex_export_template: () => "/Users/me/papers/gnn-icl.zip",
  latex_delete_template: () => null,
  latex_reveal_templates: () => null,
  assist_start: (a) => {
    const req = a.request as { action: string; text: string; from: number; to: number; instruction: string };
    state.lastAssist = clone(a.request) as Record<string, unknown>;
    const id = nextJob++;
    let [from, to] = req.from === req.to ? paragraphAround(req.text, req.from) : [req.from, req.to];
    const insert = req.action === "figure" || req.action === "write";
    if (insert) from = to;
    const original = insert ? req.text.slice(...paragraphAround(req.text, req.from)) : req.text.slice(from, to);
    const p = POLISHED.default;
    const revision =
      req.action === "figure"
        ? TABLE
        : req.action === "write"
          ? WRITTEN.revision
          : req.action === "ask"
            ? null
            : p.revision;
    const chunks = ["<revision>\nThis note asks", "<revision>\nThis note asks whether the same idea transfers", "<revision>\nThis note asks whether the same idea transfers to graph neural networks."];
    chunks.forEach((c, i) => emitLater(300 + i * 350, { kind: "partial", id, text: c.replace("<revision>\n", "") }));
    emitLater(1600, {
      kind: "done",
      id,
      result: {
        revision,
        translation:
          req.action === "figure"
            ? "四个基准上的准确率（%）。"
            : req.action === "write"
              ? WRITTEN.translation
              : req.action === "ask"
                ? null
                : p.translation,
        changesZh:
          req.action === "ask" ? [] : req.action === "write" ? WRITTEN.zh : req.action === "figure" ? TABLE_NOTES.zh : p.zh,
        changesEn:
          req.action === "ask" ? [] : req.action === "write" ? WRITTEN.en : req.action === "figure" ? TABLE_NOTES.en : p.en,
        answer:
          req.action === "ask"
            ? "第二句的“seems important”没有给出依据。建议改为指向结果的陈述，例如写明消息传递带来的准确率提升，并引用对应的表。"
            : null,
        removed: [],
        repeated: [],
        translationMatches: true,
        diff: revision && !insert ? roughDiff(original, revision) : [],
        usage: { inputTokens: 5210, outputTokens: 412 },
        durationMs: 9400,
        // The sample paper loads amsmath, graphicx, hyperref, natbib: the table needs booktabs.
        missingPackages: req.action === "figure" && LATEX ? ["booktabs"] : [],
        preambleHere: req.action === "figure" && LATEX,
      },
    });
    return {
      id,
      target: { from, to, text: original, wholeParagraph: req.from === req.to, insert },
      model: "AnyRouter (GPT-6 Astra) · gpt-6-astra",
      skill: "research-builder d227e92",
    };
  },
  assist_cancel: () => null,
  load_reference: () => ({ name: "uniclean.tex", text: UNICLEAN, chars: UNICLEAN.length, truncated: false }),
  assist_offer: (a) => {
    state.offers.push({ source: String(a.source), translation: String(a.translation) });
    return null;
  },
  attach_image: () => ({
    id: Date.now() % 100000,
    name: "reference-figure.png",
    dataUrl:
      "data:image/svg+xml;utf8," +
      encodeURIComponent("<svg xmlns='http://www.w3.org/2000/svg' width='40' height='40'><rect width='40' height='40' fill='%23b8432f'/><rect x='8' y='14' width='6' height='20' fill='white'/><rect x='17' y='8' width='6' height='26' fill='white'/><rect x='26' y='18' width='6' height='16' fill='white'/></svg>"),
  }),
  drop_attachment: () => null,
  get_skill: () => ({
    name: "research-builder",
    source: "https://github.com/qzkinhit/research-builder",
    version: "d227e92",
    date: "2026-10-09",
    origin: "builtin",
    folder: null,
    files: 16,
    missing: [],
  }),
  open_link: () => null,
  open_manual: () => null,
  get_session: () => session(),
  update_document: (a) => {
    state.text = String(a.text);
    // A filled block (now Chinese) is no longer waiting.
    const segs = layout(state.text);
    for (const index of [...state.kept]) {
      const seg = segs[index];
      if (!seg || state.text.slice(seg.from, seg.to) !== EN_BLOCKS[index]) state.kept.delete(index);
    }
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
  get_glossary: () => [
    { term: "in-context learning", translation: "上下文学习" },
    { term: "prompt graph", translation: "提示图" },
    { term: "message passing", translation: "消息传递" },
    { term: "GNN", translation: null },
  ],
  save_glossary: (a) => a.entries,
};

/** Install the stand-in backend. Unknown commands fail like a missing Rust command. */
export function installMockBackend(): void {
  if (ZH_FILE) state.text = chineseText();
  if (PENDING > 0 && state.direction === state.home) finishPending(60_000);
  const names = ["main", "lab", "backup-1", "backup-2", "team", "spare"];
  POOL.forEach((k, i) => (state.settings.providers[1].keyNames[k.fingerprint] = names[i]));
  seedRecords();
  mockWindows("main");
  mockIPC(
    (cmd, payload) => {
      const handler = handlers[cmd];
      if (!handler) throw new Error(`mock backend: no handler for ${cmd}`);
      // Real IPC serializes: never hand out the mock's own objects.
      const result = handler((payload ?? {}) as Args);
      return result instanceof Promise ? result.then(clone) : clone(result);
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
