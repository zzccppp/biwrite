// Mirrors the serialized Rust types (biwrite-core / biwrite-engine / app).

export type Mode = "plain" | "markdown" | "latex";

/** Which language is edited (left) → which is shown (right). */
export type Direction = "en-zh" | "zh-en";

export type SkipReason =
  | "front_matter"
  | "code"
  | "rule"
  | "preamble"
  | "comment"
  | "math"
  | "table"
  | "float"
  | "markup";

export type SegmentKind =
  | { type: "paragraph" }
  | { type: "heading"; level: number }
  | { type: "caption" }
  | { type: "skipped"; reason: SkipReason };

/** Segment position in the editor, in UTF-16 code units (CodeMirror positions). */
export interface SegmentLayout {
  id: number;
  kind: SegmentKind;
  from: number;
  to: number;
}

export type SegmentStatus = "translated" | "stale" | "queued" | "translating" | "error" | "skipped";

export interface SegmentState {
  id: number;
  version: number;
  status: SegmentStatus;
  text: string | null;
  partial: boolean;
  error: string | null;
}

export interface SessionUsage {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheHits: number;
}

export interface Snapshot {
  revision: number;
  mode: Mode;
  direction: Direction;
  full: boolean;
  layout: SegmentLayout[];
  states: SegmentState[];
  usage: SessionUsage;
}

export interface SessionView {
  path: string | null;
  name: string;
  text: string;
  dirty: boolean;
  autoTranslate: boolean;
  lineEnding: "lf" | "crlf" | "cr";
  bom: boolean;
  snapshot: Snapshot;
}

export interface SavedView {
  path: string;
  name: string;
  suggestedMode: Mode;
}

/** Result of a bilingual export. */
export interface ExportView {
  name: string;
  /** Paragraphs exported without a translation. */
  missing: number;
}

/** A glossary term; `translation: null` means "keep in English". */
export interface GlossaryEntry {
  term: string;
  translation: string | null;
}

export const MODE_LABELS: Record<Mode, string> = {
  plain: "Plain",
  markdown: "Markdown",
  latex: "LaTeX",
};

export const SKIP_LABELS: Record<SkipReason, string> = {
  front_matter: "front matter",
  code: "code",
  rule: "rule",
  preamble: "preamble",
  comment: "comment",
  math: "math",
  table: "table",
  float: "figure",
  markup: "markup",
};

// ── Settings ─────────────────────────────────────────────────────────

export type ProviderKind = "openai_compatible" | "anthropic" | "mock";
export type Effort = "low" | "medium" | "high" | "default";
/** Endpoint of an OpenAI-compatible provider (absent means "chat"). */
export type WireApi = "chat" | "responses";
export type ServiceTier = "priority" | "flex" | "default";

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  temperature: number;
  effort: Effort;
  wireApi?: WireApi;
  serviceTier?: ServiceTier | null;
  /** Requests one key may carry at a time (null: no limit). */
  keyConcurrency?: number | null;
  /** Retries for transient errors (null: the default). */
  maxRetries?: number | null;
}

export interface ProviderView extends ProviderConfig {
  hasKey: boolean;
  /** Keys in the provider's pool. */
  keyCount: number;
  /** Key names by key fingerprint. */
  keyNames: Record<string, string>;
  builtin: boolean;
  needsKey: boolean;
}

export interface Preset {
  name: string;
  kind: ProviderKind;
  wireApi: WireApi;
  baseUrl: string;
  model: string;
  effort: Effort;
  serviceTier: ServiceTier | null;
  keyConcurrency: number | null;
  maxRetries: number | null;
}

export interface LogSettings {
  /** Record new requests. */
  enabled: boolean;
  /** Append finished records to the log file. */
  persist: boolean;
}

export interface SettingsView {
  providers: ProviderView[];
  activeProvider: string;
  activeLabel: string;
  concurrency: number;
  docNote: string;
  presets: Preset[];
  promptDir: string;
  /** Log folder, or null when logs only go to stderr. */
  logDir: string | null;
  /** Chosen assistant provider; "" follows the translation provider. */
  assistantProvider: string;
  assistantLabel: string;
  assistantReady: boolean;
  requestLog: LogSettings;
  /** Paragraphs per translation request. */
  batchSize: number;
  /** Parallel requests follow the key pool. */
  matchPool: boolean;
  /** Parallel requests in effect. */
  effectiveConcurrency: number;
}

/** What the translation cache holds. */
export interface CacheView {
  entries: number;
  bytes: number;
  /** Database file; null for the temporary in-memory cache. */
  location: string | null;
  groups: CacheGroupView[];
}

export interface ClearedView {
  removed: number;
  cache: CacheView;
}

export interface CacheGroupView {
  /** Provider identity in the cache (unique together with model and direction). */
  id: string;
  /** Display name: the configured provider, or the API host. */
  provider: string;
  model: string;
  direction: Direction;
  entries: number;
}

/** State of one key of a provider's pool. */
export interface KeyStatus {
  number: number;
  fingerprint: string;
  tail: string;
  state: "ready" | "cooling" | "rejected";
  inFlight: number;
  detail: string | null;
}

/** One key in the key manager (never the key itself). */
export interface KeyEntry {
  number: number;
  fingerprint: string;
  tail: string;
  name: string;
  state: "ready" | "cooling" | "rejected" | "idle";
  inFlight: number;
  detail: string | null;
}

// ── Request log ──────────────────────────────────────────────────────

/** What a request asked for, or what its response declared (null: not sent / not returned). */
export interface Declared {
  model: string | null;
  effort: string | null;
  serviceTier: string | null;
}

export interface UsageDetail {
  inputTokens: number | null;
  cachedTokens: number | null;
  outputTokens: number | null;
  reasoningTokens: number | null;
}

export type RecordState = "in_flight" | "ok" | "error" | "cancelled";

/** One HTTP request to a model (metadata only). */
export interface RequestRecord {
  id: number;
  /** Unix time in milliseconds. */
  startedAt: number;
  purpose: string;
  provider: string;
  wire: string;
  endpoint: string;
  key: { number: number; count: number; tail: string; fingerprint: string } | null;
  request: Declared;
  response: Declared;
  httpStatus: number | null;
  state: RecordState;
  error: string | null;
  durationMs: number | null;
  firstTokenMs: number | null;
  usage: UsageDetail;
  promptChars: number;
  outputChars: number;
  notes: string[];
}

export interface LogView {
  settings: LogSettings;
  /** Newest first. */
  records: RequestRecord[];
  file: string | null;
}

export interface PromptView {
  text: string;
  isDefault: boolean;
  path: string;
}

export const KIND_LABELS: Record<ProviderKind, string> = {
  openai_compatible: "OpenAI-compatible",
  anthropic: "Anthropic",
  mock: "Mock",
};

// ── Writing assistant ────────────────────────────────────────────────

export type AssistAction = "polish" | "edit" | "ask" | "figure";
export type AssistScope = "target" | "neighbors" | "document";

/** What a job works on, in UTF-16 offsets of the text it started with. */
export interface AssistTarget {
  from: number;
  to: number;
  text: string;
  /** Exactly one paragraph's content: its approved translation can be kept. */
  wholeParagraph: boolean;
  /** The answer is inserted at `from` (figures). */
  insert: boolean;
}

export interface AssistStarted {
  id: number;
  target: AssistTarget;
  model: string;
  skill: string;
}

export interface DiffPart {
  kind: "equal" | "insert" | "delete";
  text: string;
}

export interface AssistResult {
  revision: string | null;
  translation: string | null;
  changesZh: string[];
  changesEn: string[];
  answer: string | null;
  /** Protected texts (math, citations) the revision dropped. */
  removed: string[];
  repeated: string[];
  translationMatches: boolean;
  diff: DiffPart[];
  usage: { inputTokens: number; outputTokens: number };
  durationMs: number;
}

export type AssistEvent =
  | { kind: "partial"; id: number; text: string }
  | { kind: "done"; id: number; result: AssistResult }
  | { kind: "failed"; id: number; message: string };

export interface AssistRequest {
  action: AssistAction;
  scope: AssistScope;
  text: string;
  from: number;
  to: number;
  instruction: string;
  references: string[];
  images: number[];
  history: [string, string][];
}

/** A reference image picked in Rust (the webview keeps only a preview). */
export interface AttachmentView {
  id: number;
  name: string;
  dataUrl: string;
}

export interface SkillInfo {
  name: string;
  source: string;
  version: string;
  date: string;
  origin: "folder" | "downloaded" | "builtin";
  folder: string | null;
  files: number;
  missing: string[];
}
