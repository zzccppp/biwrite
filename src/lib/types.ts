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

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  temperature: number;
  effort: Effort;
}

export interface ProviderView extends ProviderConfig {
  hasKey: boolean;
  builtin: boolean;
  needsKey: boolean;
}

export interface Preset {
  name: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
}

export interface SettingsView {
  providers: ProviderView[];
  activeProvider: string;
  activeLabel: string;
  concurrency: number;
  docNote: string;
  presets: Preset[];
  promptDir: string;
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
