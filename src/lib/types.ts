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
}

export interface ProviderView extends ProviderConfig {
  hasKey: boolean;
  /** Keys in the provider's pool. */
  keyCount: number;
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
  /** Chosen assistant provider; "" follows the translation provider. */
  assistantProvider: string;
  assistantLabel: string;
  assistantReady: boolean;
  requestLog: LogSettings;
}

/** State of one key of a provider's pool. */
export interface KeyStatus {
  number: number;
  tail: string;
  state: "ready" | "cooling" | "rejected";
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
  key: { number: number; count: number; tail: string } | null;
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
