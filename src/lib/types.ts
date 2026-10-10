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
  /** Changes when the document is replaced (a file opened, a swap); text
   * sent to Rust carries it, and text for an earlier document is refused. */
  document: number;
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
  /** The paired file in the other language. */
  pair: PairView | null;
  /** The direction in which the editor holds the file's own language. */
  home: Direction;
  snapshot: Snapshot;
}

/**
 * A paragraph still in the language of the translations (left so by an
 * early swap), translated into the edited language: replace the segment's
 * text `old` with `new`, unless it changed meanwhile.
 */
export interface Fill {
  id: number;
  old: string;
  new: string;
}

export interface SavedView {
  path: string;
  name: string;
  suggestedMode: Mode;
  /** What happened to the paired file. */
  mirror: MirrorSaved | null;
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
  latex: LatexSettings;
  /** Look for a newer release at startup. */
  checkUpdates: boolean;
  /** Closing the window hides BiWrite in the tray. */
  closeToTray: boolean;
  /** This build's version. */
  version: string;
}

export interface LatexSettings {
  /** Build the PDF after each save of a .tex file. */
  compileOnSave: boolean;
  /** Folder with the TeX programs chosen by the user. */
  texBin?: string;
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

export type AssistAction = "polish" | "edit" | "ask" | "figure" | "mirror" | "write";
export type AssistScope = "target" | "neighbors" | "document";

/** What a job works on, in UTF-16 offsets of the text it started with. */
export interface AssistTarget {
  from: number;
  to: number;
  text: string;
  /** Exactly one paragraph's content: its approved translation can be kept. */
  wholeParagraph: boolean;
  /** The answer is inserted at `from` (figures, new text). */
  insert: boolean;
}

/** A reference text read from a file. */
export interface ReferenceView {
  name: string;
  text: string;
  chars: number;
  /** The file had more than was kept. */
  truncated: boolean;
}

/** A text whose writing to follow: pasted, or from a file. */
export interface Sample {
  /** The file it came from. */
  name: string | null;
  text: string;
  truncated: boolean;
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
  /** Packages a figure needs that the document does not load. */
  missingPackages: string[];
  /** The open file holds the preamble. */
  preambleHere: boolean;
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

// ── LaTeX ───────────────────────────────────────────────────────────

/** Which PDF: the English paper or the Chinese mirror. */
export type PdfLang = "en" | "zh";
export type TexEngine = "pdflatex" | "xelatex" | "lualatex";
export type BuildOutcome = "ok" | "errors" | "failed" | "timed_out";
export type IssueSeverity = "error" | "warning" | "box";

export interface TexStatus {
  found: boolean;
  distribution: string;
  bin: string;
  latexmk: boolean;
  synctex: boolean;
  engines: TexEngine[];
  customBin: string | null;
  compileOnSave: boolean;
}

export interface ProjectView {
  folder: string;
  /** Root file, relative to the project folder. */
  root: string;
  /** The open document, relative to the project folder. */
  current: string;
  engine: TexEngine;
  files: string[];
}

export interface IssueView {
  severity: IssueSeverity;
  /** Relative to the project folder (the original file for the Chinese PDF). */
  file: string | null;
  line: number | null;
  message: string;
  /** The line is in the open document. */
  here: boolean;
}

export interface BuildView {
  id: number;
  lang: PdfLang;
  outcome: BuildOutcome;
  hasPdf: boolean;
  /** The PDF predates the build. */
  stale: boolean;
  issues: IssueView[];
  durationMs: number;
  tool: string;
  engine: TexEngine;
  root: string;
  /** Paragraphs of the Chinese PDF still in English. */
  untranslated: number;
  output: string;
  /** The project's own latexmkrc, which ran with the build: it can run any command. */
  projectRc: string | null;
}

/** Where a source line is typeset, in PDF points from the page's top left. */
export interface PdfBox {
  page: number;
  left: number;
  top: number;
  width: number;
  height: number;
}

/** UTF-16 range in the editor's text. */
export interface Range16 {
  from: number;
  to: number;
}

export interface SyncHit {
  file: string;
  line: number;
  here: boolean;
  range: Range16 | null;
  /** The range is a whole paragraph (the PDF is in the other language). */
  paragraph: boolean;
  /** Not in the open document: the project file to open for it. */
  open: string | null;
}

export interface TemplateView {
  id: string;
  builtin: boolean;
  name: string;
  description: string;
  main: string;
  engine: TexEngine | null;
  source?: string;
  files: number;
  bytes: number;
}

/** A click on the PDF: page (1-based), point in PDF points from the top
 * left, the text-layer run under it and the click's offset in that run. */
export interface PdfPick {
  page: number;
  x: number;
  y: number;
  span: string;
  click: number;
}

/** What a click on the PDF found, shown next to it. */
export interface PdfMenu {
  page: number;
  x: number;
  y: number;
  /** Selected in the open document (sentence or paragraph). */
  here: boolean;
  paragraph: boolean;
  file: string;
  line: number;
}

// ── Updates ─────────────────────────────────────────────────────────

export type ReleaseRelation = "newer" | "current" | "older";

export interface ReleaseView {
  tag: string;
  name: string;
  version: string;
  /** Pre-releases carry branch builds. */
  prerelease: boolean;
  date: string;
  notes: string;
  relation: ReleaseRelation;
  /** The installer for this computer, if the release has one. */
  asset: { name: string; size: number; sha256: string | null } | null;
}

export interface UpdatesView {
  current: string;
  releases: ReleaseView[];
}

export interface InstalledView {
  version: string;
  /** The new version is in place and starts with the next launch. */
  relaunch: boolean;
  /** BiWrite quits so the installer can run. */
  quitting: boolean;
  file: string | null;
}

export interface UpdateProgress {
  tag: string;
  received: number;
  total: number;
}

// ── Pairs ───────────────────────────────────────────────────────────

/** The file in the other language the open document is paired with. */
export interface PairView {
  path: string;
  name: string;
  /** Paragraphs paired, of the open document's. */
  paired: number;
  units: number;
  /** The paired file has changes not written yet. */
  dirty: boolean;
}

/** What a save did with the paired file. */
export interface MirrorSaved {
  name: string;
  written: boolean;
  /** Paragraphs still being translated: the paired file waits for them. */
  pending: number;
  changed: number;
  /** New headings, captions and list items left out of the paired file (add them by hand). */
  leftOut: number;
  /** Why a paired file that was ready was not written. */
  problem: "changedOnDisk" | "structure" | null;
  /** The document was edited since it was saved: the paired file follows with the next save. */
  deferred: boolean;
  /** The paired file still lags behind the saved document. */
  behind: boolean;
}
