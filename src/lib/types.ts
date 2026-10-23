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
  /** Paragraphs still in the other language since an early swap, which a save writes into the file as they are. */
  otherLanguage: number;
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

/** Which open files changed on disk since the app last read or wrote them. */
export interface DiskView {
  /** The open document. */
  document: boolean;
  /** Its paired file. */
  mirror: boolean;
  /** Stamps of the versions reported: ignoring one never ignores a later one. */
  documentStamp: string;
  mirrorStamp: string;
}

export interface SavedView {
  path: string;
  name: string;
  suggestedMode: Mode;
  /** What happened to the paired file. */
  mirror: MirrorSaved | null;
  /** Paragraphs written in the other language as they are, their translations not there yet. */
  otherLanguage: number;
  /** A part saved under a new name: the main file that still includes the old one. */
  notInRoot?: string;
}

/** A paper saved, or its translation exported, into a new folder with its files. */
export interface ProjectCopy {
  folder: string;
  /** Files copied besides the ones written from the editor. */
  copied: number;
  /** Files too far above the paper's folder to take along. */
  leftOut: string[];
  saved: SavedView | null;
  exported: string | null;
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
export type Effort = "low" | "medium" | "high" | "xhigh" | "max" | "default";
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
  /** Claude models get the 1M-token context (null: yes, the default). */
  longContext?: boolean | null;
}

export interface ProviderView extends ProviderConfig {
  hasKey: boolean;
  /** Keys in the provider's pool. */
  keyCount: number;
  /** Key names by key fingerprint. */
  keyNames: Record<string, string>;
  builtin: boolean;
  needsKey: boolean;
  /** Getting through applies: a crowded relay, or several keys. */
  squeezable: boolean;
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
  /** Seconds an edited paragraph stays unchanged before it is translated (0 translates at once). */
  translateDelay: number;
  /** Parallel requests follow the key pool. */
  matchPool: boolean;
  /** Parallel requests in effect. */
  effectiveConcurrency: number;
  latex: LatexSettings;
  /** Look for a newer release at startup. */
  checkUpdates: boolean;
  /** Closing the window hides BiWrite in the tray. */
  closeToTray: boolean;
  /** Keys that got through to a relay that turns keys away are kept alive. */
  keepAlive: boolean;
  /** Kept alive however long the author is away (else 30 minutes). */
  keepAliveForever: boolean;
  /** Automatically queue retryable failed keep-alives, including after reaching the target. */
  keepAliveRejoin: boolean;
  /** Consecutive failed keep-alives before a key's background requests stop. */
  keepAliveFailureLimit: number;
  /** API keys in a local file only this account reads (no keychain prompt), else in the keychain. */
  keysInFile: boolean;
  /** Mean minutes between two keep-alives of a key. */
  keepAliveMinutes: number;
  /** Mean seconds between two tries of a key in a squeeze. */
  squeezeSeconds: number;
  /** A squeeze stops once this many keys got through. */
  squeezeEnough: number;
  /** The reasoning effort of the tries of a squeeze and of keep-alives. */
  squeezeEffort: Effort;
  /** Accept and Revert go through the model and a build (LaTeX). */
  assistedApply: boolean;
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
  /** When a relay that turns keys away last answered it (Unix ms); null: never. */
  lastThrough: number | null;
  /** Set aside by the author: no request uses it. */
  ignored?: boolean;
}

/** One key of a provider getting through to a crowded relay. */
export interface SqueezeKey {
  fingerprint: string;
  /** When it got through (Unix ms), while it counts as through. */
  throughSince: number | null;
  /** When it was last answered (Unix ms). */
  lastOk: number | null;
  /** When its latest keep-alive was answered (Unix ms), since it got through. */
  keptAt: number | null;
  /** Its latest keep-alive failed, and it is tried again shortly. */
  keepRetrying: boolean;
  keepAliveFailures: number;
  /** Background requests stopped after consecutive keep-alive failures. */
  backgroundStopped: boolean;
  stopReason: string | null;
  tries: number;
  trying: boolean;
  refused: boolean;
  lastError: string | null;
}

/** A paragraph and its sentences (offsets in the editor's text). */
export interface SentencesView {
  paragraph: { from: number; to: number };
  sentences: { from: number; to: number }[];
}

/** A sentence by its place: sentence `index` of `count`, lying at `place`
 * of its paragraph (parts 0–1 of it, see `sentences.ts`), with its `text`. */
export interface SentencePick {
  index: number;
  count: number;
  place: { start: number; end: number };
  text: string;
}

/** The cursor's sentence (of the source) to show in the translation:
 * sentence `index` of `count` in block `id`, at `place` of its paragraph
 * (`tick` repeats it). */
export interface SentenceMark extends SentencePick {
  id: number;
  tick: number;
}

export interface SqueezeView {
  running: boolean;
  /** The relay turned keys away lately, so keys that got through go first. */
  crowded: boolean;
  started: number | null;
  keys: SqueezeKey[];
}

export type SqueezeChange =
  | { kind: "started" }
  | { kind: "tried" }
  | { kind: "through"; number: number }
  | { kind: "ended"; allThrough: boolean }
  | { kind: "keyStopped"; number: number; fingerprint: string; failures: number; reason: string; rejoined: boolean }
  /** A round of keeping keys alive went by. */
  | { kind: "kept" };

export type SqueezeEvent = SqueezeChange & {
  providerId: string;
  providerName: string;
  running: boolean;
  through: number;
  total: number;
  /** Keeping alive is on and the relay turned keys away: the keys through are kept alive. */
  keeping: boolean;
};

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
  /** What was sent and what came back (cut at 20,000 characters, keys
   * masked). */
  system: string;
  user: string;
  output: string;
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

export type AssistAction = "polish" | "edit" | "ask" | "figure" | "mirror" | "write" | "fix";
export type AssistScope = "target" | "neighbors" | "document";
/** What a figure becomes in a LaTeX document: code, a PDF of its own, or a PowerPoint file and its PDF. */
export type FigureFormat = "latex" | "pdf" | "pptx";

/** A figure that is a file, as made for a card. */
export interface FigureView {
  format: FigureFormat;
  /** Where it goes, relative to the folder TeX runs in. */
  file: string;
  /** There is a PDF to show and to save. */
  pdf: boolean;
  pptx: boolean;
  /** A drawing of the diagram, when there is no PDF. */
  svg: string | null;
  /** No LibreOffice: the PDF is to be exported from PowerPoint. */
  export: boolean;
}

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
  /** TikZ libraries a figure loads that the preamble does not (taken out of the revision). */
  libraries: string[];
  /** The open file holds the preamble. */
  preambleHere: boolean;
  /** The changes of a fix, in whole lines of the text it started from. */
  hunks: Hunk[];
  /** How a fix went. */
  fix: FixReport | null;
  /** A figure that is a file. */
  figure: FigureView | null;
}

/** One change of a fix, in whole lines. */
export interface Hunk {
  /** 1-based line where it starts in the text the fix started from. */
  line: number;
  old: string;
  new: string;
  /** Up to two lines before and after, to find its place again. */
  before: string;
  after: string;
  diff: DiffPart[];
}

/** An error TeX reported. */
export interface TexError {
  /** 1-based line in the checked text, when the error is in it. */
  line: number | null;
  file: string | null;
  /** Its line in `file`, when that is another file of the project. */
  fileLine?: number | null;
  message: string;
  context: string;
}

export interface FixReport {
  /** Errors before the fix. */
  before: number;
  /** Errors left after it. */
  errors: TexError[];
  /** Errors in other files of the project, which their own fix mends. */
  elsewhere?: TexError[];
  rounds: number;
  /** Changes whose lines were not found. */
  missed: number;
}

/** An answer checked (and repaired) before it is applied. */
/** Of the lines an inserted figure added before \begin{document}, those
 * that can go with it and those the document still uses. */
export interface Tidied {
  remove: string[];
  kept: string[];
}

export interface CheckResult {
  revision: string;
  /** Lines to add before \begin{document} (empty for none). */
  preamble: string;
  passed: boolean;
  /** Errors the answer brings that are left. */
  errors: TexError[];
  /** Errors the document had before. */
  before: number;
  rounds: number;
  /** The model fitted the answer to the document before the build, and changed it. */
  fitted: boolean;
  changesZh: string[];
  changesEn: string[];
  usage: { inputTokens: number; outputTokens: number };
}

/** An answer about to be applied, as the editor would put it in. */
export interface CheckRequest {
  id: number;
  /** The document without the answer. */
  text: string;
  from: number;
  to: number;
  lead: string;
  trail: string;
  revision: string;
  /** Where lines go before \begin{document}, when the preamble is in the open file. */
  preambleAt: number | null;
  preamble: string;
  /** A part's answer: the lines the main file's preamble needs, put in the check's copy of it. */
  rootLines?: string;
  /** The model fits the answer to the document before the first build. */
  fit: boolean;
}

export type AssistEvent =
  | { kind: "partial"; id: number; text: string }
  /** The request failed for a passing reason and is sent again. */
  | { kind: "retrying"; id: number; attempt: number; of: number; message: string }
  | { kind: "done"; id: number; result: AssistResult }
  | { kind: "failed"; id: number; message: string }
  /** A fix or a check moved on: building, or the model repairing (round from 1). */
  | { kind: "stage"; id: number; fixing: boolean; round: number }
  | { kind: "checked"; id: number; result: CheckResult }
  | { kind: "check_failed"; id: number; message: string };

export interface AssistRequest {
  action: AssistAction;
  scope: AssistScope;
  text: string;
  from: number;
  to: number;
  instruction: string;
  references: string[];
  images: number[];
  /** What each image is for in a figure, in the order of `images`. */
  imageUses: ImageUse[];
  history: [string, string][];
  /** What a figure becomes (LaTeX documents). */
  figure: FigureFormat;
}

/** What an image is for in a figure: a draft of this figure to redraw and
 * correct against the text, or a figure of another paper whose look to take. */
export type ImageUse = "trace" | "style";

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

/** The author's own materials: the folders of the skill's `materials/`. */
export interface MaterialsView {
  folder: string;
  /** The folder is the `materials/` of the author's own skill folder. */
  inSkill: boolean;
  /** What each folder holds that BiWrite uses (icons for `icons`). */
  counts: { name: string; files: number }[];
  /** SVG pictures of PowerPoint files in `icons` without alternative text,
   * which are not used. */
  unnamedIcons: number;
}

/** A paper or figure of the materials, to take as a reference. */
export interface MaterialEntry {
  folder: string;
  /** From the materials folder, with `/`. */
  path: string;
  /** `pdftext`: a paper as a PDF. `pdf`, `pptx`: a figure. */
  kind: "text" | "pdftext" | "image" | "pdf" | "pptx";
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
  /** Edits not saved yet are not in the PDF (built from the files as saved). */
  unsavedLeftOut: boolean;
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

/** A paragraph of another file of the paper, for the whole-paper view. */
export interface PaperBlock {
  /** UTF-16 range in its file as saved. */
  from: number;
  to: number;
  heading: boolean;
  source: string;
  /** In the pane's language; null when not known. */
  translation: string | null;
}

/** Paragraphs of one file in a row. */
export interface PaperPart {
  file: string;
  blocks: PaperBlock[];
}

/** The whole paper around the open document, for the translation pane. */
export interface PaperView {
  before: PaperPart[];
  after: PaperPart[];
  /** What the open document includes, by the name its include gives (no ./ nor .tex). */
  inside: { name: string; parts: PaperPart[] }[];
}

/** A picture the document names that is not on disk, and where the document says it comes from. */
export interface MissingImage {
  file: string;
  url: string;
}

export interface DownloadedImage {
  file: string;
  error: string | null;
}

/** A test of a provider or a key: a small question and the model's answer. */
export interface TestAnswer {
  question: string;
  answer: string;
}
