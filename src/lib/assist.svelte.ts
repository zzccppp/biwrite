// Writing assistant jobs as the UI sees them. A job's target range is kept
// in step with every later edit (mapped through CodeMirror changes), and the
// job notes whether the target itself was edited, which decides how an
// approved revision is applied.

import type { ChangeSet } from "@codemirror/state";
import { assistIpc } from "./ipc";
import { openPdf } from "./pdf";

import { insertAfter, mapPlace } from "./assistText";
export {
  askedAbout,
  asParagraph,
  insertionAt,
  lineList,
  newPreambleLines,
  preamblePlace,
  notLoaded,
  reapplyInstruction,
  revertChange,
  uniqueIndex,
} from "./assistText";
import type {
  AssistAction,
  AssistEvent,
  AssistRequest,
  AssistResult,
  AssistScope,
  AssistStarted,
  AssistTarget,
  AttachmentView,
  ImageUse,
  MaterialEntry,
  CheckResult,
  FigureFormat,
  Sample,
} from "./types";

export type JobState = "running" | "done" | "failed" | "applied" | "discarded";

export const ACTION_LABEL = {
  polish: "assist.action.polish",
  // Polishing with an instruction (Re-apply runs one): shown as polishing.
  edit: "assist.action.polish",
  ask: "assist.action.ask",
  figure: "assist.action.figure",
  mirror: "assist.action.mirror",
  write: "assist.action.write",
  fix: "assist.action.fix",
} as const;

export const SCOPE_LABEL = {
  target: "assist.scope.target",
  neighbors: "assist.scope.neighbors",
  document: "assist.scope.document",
} as const;

export const PLACEHOLDER = {
  polish: "assist.placeholder.polish",
  edit: "assist.placeholder.edit",
  ask: "assist.placeholder.ask",
  figure: "assist.placeholder.figure",
  mirror: "assist.placeholder.mirror",
  write: "assist.placeholder.write",
  fix: "assist.placeholder.fix",
} as const;

const ACTION_KEY = "biwrite.assist.action";
const SCOPE_KEY = "biwrite.assist.scope";
const FIGURE_KEY = "biwrite.assist.figure";

export const FIGURE_LABEL = {
  latex: "assist.figure.latex",
  pdf: "assist.figure.pdf",
  pptx: "assist.figure.pptx",
} as const;

function load<T extends string>(key: string, allowed: readonly T[], fallback: T): T {
  try {
    const v = localStorage.getItem(key) as T | null;
    return v && allowed.includes(v) ? v : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Not persisted; harmless.
  }
}

export class AssistJob {
  readonly id: number;
  readonly action: AssistAction;
  readonly scope: AssistScope;
  readonly instruction: string;
  readonly model: string;
  readonly skill: string;
  /** The target as the job started. */
  readonly target: AssistTarget;
  /** The document the job belongs to (a newly opened file is another one). */
  readonly epoch: number;
  readonly startedAt = Date.now();
  /** Questions and answers before this one (ask follow-ups). */
  readonly history: [string, string][];
  /** Set for a job that re-applies another job's approved revision. */
  readonly reapplies: number | null;

  /** Current position of the target in the editor. */
  from = $state(0);
  to = $state(0);
  /** The target text was edited after the job started. */
  touched = $state(false);
  state = $state<JobState>("running");
  partial = $state("");
  /** The latest retry after a passing failure (a busy server), until output arrives. */
  retry = $state<{ attempt: number; of: number } | null>(null);
  result = $state<AssistResult | null>(null);
  error = $state<string | null>(null);
  /** Applying found the target changed; offer to re-apply with the model. */
  conflict = $state(false);
  /** Being applied: a second Accept meanwhile does nothing. */
  applying = $state(false);
  /** Being run again (Again, Re-apply): its buttons wait for that. */
  restarting = $state(false);
  /** Kept for later: the card folds, and can still be accepted. */
  kept = $state(false);
  /** What applying put in (the target covers it from then on), the lines
   * it added before \begin{document}, and for a fix the lines it changed
   * with what they were, for Revert (kept in step with later edits like the
   * target). Revert takes the preamble lines out only when a build shows
   * the rest of the document does without them. */
  applied = $state<{
    text: string;
    preamble?: string;
    hunks?: { from: number; to: number; text: string; old: string }[];
  } | null>(null);
  /** After Revert: the preamble lines being checked, those taken out, those
   * kept (still used), or all kept since the check could not be done. */
  tidy = $state<{ checking: boolean; removed: string[]; kept: string[]; failed: boolean } | null>(null);
  /** Revert found that text edited since: the card shows the text from before. */
  revertBlocked = $state(false);
  /** A fix building the document or asking the model to repair it (round from 1). */
  stage = $state<{ fixing: boolean; round: number } | null>(null);
  /** Accept fitting and checking the answer: the same steps. */
  checking = $state<{ fixing: boolean; round: number } | null>(null);
  /** What the last check of the answer found. */
  check = $state<CheckResult | null>(null);
  /** The last check could not finish. */
  checkError = $state<string | null>(null);
  /** Accept could not make it compile (or could not fit or check it), so
   * it was inserted as it is. */
  plainly = $state(false);

  constructor(
    started: AssistStarted,
    request: AssistRequest,
    epoch: number,
    reapplies: number | null = null,
  ) {
    this.id = started.id;
    this.action = request.action;
    this.scope = request.scope;
    this.instruction = request.instruction;
    this.model = started.model;
    this.skill = started.skill;
    this.target = started.target;
    this.epoch = epoch;
    this.history = request.history;
    this.reapplies = reapplies;
    this.from = started.target.from;
    this.to = started.target.to;
  }

  get open(): boolean {
    return this.state === "running" || this.state === "done";
  }
}

/** Map a job's target through an edit. Text typed right where new text
 * goes stays before it (the new paragraph follows what the author added).
 * An applied job covers the text it put in, whatever its target was, and
 * an inserted paragraph takes in what is typed at its ends (not a new
 * paragraph there), so Revert never leaves that text on a neighbour. */
function mapTarget(job: AssistJob, changes: ChangeSet): void {
  if (job.target.insert && job.state !== "applied") {
    job.from = job.to = changes.mapPos(job.from, 1);
    return;
  }
  if (changes.touchesRange(job.from, job.to)) job.touched = true;
  ({ from: job.from, to: job.to } =
    job.target.insert && job.applied ? insertAfter(job, job.applied.text, changes) : mapPlace(job, changes));
  for (const h of job.applied?.hunks ?? []) {
    h.from = changes.mapPos(h.from, 1);
    h.to = Math.max(h.from, changes.mapPos(h.to, -1));
  }
}

/** The first page of a PDF as a PNG in base64, at most 1600 pixels across. */
async function pdfPagePng(bytes: ArrayBuffer): Promise<string> {
  const doc = await openPdf(bytes);
  try {
    const page = await doc.getPage(1);
    const unit = page.getViewport({ scale: 1 });
    const viewport = page.getViewport({ scale: Math.min(3, 1600 / Math.max(unit.width, unit.height)) });
    const canvas = document.createElement("canvas");
    canvas.width = Math.ceil(viewport.width);
    canvas.height = Math.ceil(viewport.height);
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("no canvas");
    ctx.fillStyle = "#fff";
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    await page.render({ canvasContext: ctx, viewport, canvas }).promise;
    return canvas.toDataURL("image/png").slice("data:image/png;base64,".length);
  } finally {
    void doc.loadingTask.destroy();
  }
}

/** The longest reference text, as for a file (`MAX_REFERENCE_CHARS`). */
const MAX_REFERENCE_CHARS = 120_000;

/** The text of a PDF, page by page, at most `MAX_REFERENCE_CHARS`. */
async function pdfText(bytes: ArrayBuffer): Promise<{ text: string; truncated: boolean }> {
  const doc = await openPdf(bytes);
  try {
    const pages: string[] = [];
    let total = 0;
    for (let n = 1; n <= doc.numPages && total <= MAX_REFERENCE_CHARS; n++) {
      const content = await (await doc.getPage(n)).getTextContent();
      let page = "";
      for (const item of content.items) {
        if (!("str" in item)) continue;
        page += item.str + (item.hasEOL ? "\n" : "");
      }
      pages.push(page.trim());
      total += page.length;
    }
    const text = pages.filter(Boolean).join("\n\n");
    const truncated = text.length > MAX_REFERENCE_CHARS || pages.length < doc.numPages;
    return { text: text.slice(0, MAX_REFERENCE_CHARS), truncated };
  } finally {
    void doc.loadingTask.destroy();
  }
}

export class AssistStore {
  jobs = $state<AssistJob[]>([]);
  attachments = $state<(AttachmentView & { use: ImageUse })[]>([]);
  samples = $state<Sample[]>([]);
  /** Polishing takes an instruction too, so "edit" (of older versions) loads as polishing. */
  action = $state<AssistAction>(load(ACTION_KEY, ["polish", "ask", "figure", "mirror", "write", "fix"] as const, "polish"));
  scope = $state<AssistScope>(load(SCOPE_KEY, ["target", "neighbors", "document"] as const, "neighbors"));
  /** What a figure becomes in a LaTeX document. */
  figureFormat = $state<FigureFormat>(load(FIGURE_KEY, ["latex", "pdf", "pptx"] as const, "latex"));
  /** The instruction box of each action: switching keeps each its own text
   * (via translation fills its box with the paragraph's translation). */
  drafts = $state<Record<AssistAction, string>>({ polish: "", edit: "", ask: "", figure: "", mirror: "", write: "", fix: "" });
  /** An accept or a revert is under way (its build, the model's repair,
   * the check of the preamble lines): the next waits for it, since they
   * build in the same folder and edit the same preamble. */
  get adopting(): boolean {
    return this.jobs.some((j) => !!j.checking || j.applying || !!j.tidy?.checking);
  }

  get instruction(): string {
    return this.drafts[this.action];
  }
  set instruction(text: string) {
    this.drafts[this.action] = text;
  }
  /** Bumped when another document is loaded (reactive: cards turn stale). */
  epoch = $state(0);
  /** Edits made while jobs were being started, to map their targets through. */
  #starting = new Set<{ changes: ChangeSet | null }>();
  /** Events of jobs whose start has not returned yet (they can come first). */
  #early = new Map<number, AssistEvent[]>();

  running = $derived(this.jobs.filter((j) => j.state === "running").length);
  /** Results waiting to be accepted (answers and kept results wait for nothing). */
  ready = $derived(
    this.jobs.filter((j) => j.state === "done" && j.action !== "ask" && !j.kept && (j.action !== "fix" || !!j.result?.hunks.length))
      .length,
  );

  setAction(a: AssistAction): void {
    this.action = a;
    save(ACTION_KEY, a);
  }

  setScope(s: AssistScope): void {
    this.scope = s;
    save(SCOPE_KEY, s);
  }

  setFigureFormat(f: FigureFormat): void {
    this.figureFormat = f;
    save(FIGURE_KEY, f);
  }

  async start(request: AssistRequest, reapplies: number | null = null): Promise<AssistJob> {
    // The document the request's text belongs to, before anything else loads.
    const epoch = this.epoch;
    const edits: { changes: ChangeSet | null } = { changes: null };
    this.#starting.add(edits);
    let started: AssistStarted;
    try {
      started =
        request.action === "fix"
          ? await assistIpc.fix(request.text, request.instruction)
          : await assistIpc.start(request);
    } finally {
      this.#starting.delete(edits);
    }
    const job = new AssistJob(started, request, epoch, reapplies);
    // Edits of its own document only (another one may have loaded since).
    if (edits.changes && epoch === this.epoch) mapTarget(job, edits.changes);
    this.jobs = [job, ...this.jobs];
    const early = this.#early.get(job.id) ?? [];
    this.#early.delete(job.id);
    for (const e of early) this.onEvent(e);
    return job;
  }

  find(id: number): AssistJob | undefined {
    return this.jobs.find((j) => j.id === id);
  }

  onEvent(e: AssistEvent): void {
    const job = this.find(e.id);
    if (!job) {
      // Its start has not returned yet: kept for it (the last partial and
      // the outcome; a few jobs at most).
      const kept = (this.#early.get(e.id) ?? []).filter((k) => k.kind !== "partial" || e.kind !== "partial");
      this.#early.set(e.id, [...kept, e]);
      if (this.#early.size > 16) this.#early.delete(this.#early.keys().next().value as number);
      return;
    }
    if (e.kind === "stage") {
      if (job.state === "running") job.stage = { fixing: e.fixing, round: e.round };
      else if (job.checking) job.checking = { fixing: e.fixing, round: e.round };
      return;
    }
    if (e.kind === "checked" || e.kind === "check_failed") {
      if (!job.checking) return;
      job.checking = null;
      if (e.kind === "checked") job.check = e.result;
      else if (e.message !== "cancelled") job.checkError = e.message;
      return;
    }
    if (job.state !== "running") return;
    if (e.kind === "partial") {
      job.partial = e.text;
      if (e.text) job.retry = null;
    } else if (e.kind === "retrying") {
      job.retry = { attempt: e.attempt, of: e.of };
      job.partial = "";
    } else if (e.kind === "done") {
      job.result = e.result;
      job.state = "done";
    } else {
      job.error = e.message;
      job.state = "failed";
    }
  }

  /** Keep the targets of the jobs of this document in step with an edit
   * (Again, a follow-up and a click on the card go there), and of jobs
   * still being started. */
  map(changes: ChangeSet): void {
    for (const edits of this.#starting) edits.changes = edits.changes ? edits.changes.compose(changes) : changes;
    for (const job of this.jobs) {
      if (job.epoch !== this.epoch) continue;
      mapTarget(job, changes);
    }
  }

  /** Another document was loaded: open jobs can no longer be applied, and
   * edits of the new one don't concern jobs still being started. */
  newDocument(): void {
    this.epoch++;
    this.#starting.clear();
  }

  cancel(job: AssistJob): void {
    void assistIpc.cancel(job.id);
  }

  /** Stop checking which lines a revert left before \begin{document} can
   * go: they stay, as a plain revert leaves them (the answer is ignored). */
  stopTidy(job: AssistJob): void {
    if (job.tidy?.checking) job.tidy = null;
  }

  /** Stop checking a job's answer. */
  cancelCheck(job: AssistJob): void {
    job.checking = null;
    void assistIpc.checkCancel(job.id);
  }

  discard(job: AssistJob): void {
    if (job.state === "running") this.cancel(job);
    if (job.checking) this.cancelCheck(job);
    job.state = "discarded";
  }

  /** Forget one job that is not running (a finished one, an answer, a kept result). */
  remove(job: AssistJob): void {
    if (job.state === "running") return;
    if (job.checking) this.cancelCheck(job);
    this.jobs = this.jobs.filter((j) => j !== job);
  }

  /** Forget finished jobs. */
  clearFinished(): void {
    this.jobs = this.jobs.filter((j) => j.state === "running" || j.state === "done");
  }

  async attach(): Promise<void> {
    const view = await assistIpc.attachImage();
    if (view) this.attachments = [...this.attachments, { ...view, use: "trace" }];
  }

  /** In a figure, trace image `id` as a draft or take its look alone. */
  setImageUse(id: number, use: ImageUse): void {
    this.attachments = this.attachments.map((a) => (a.id === id ? { ...a, use } : a));
  }

  /** A reference paper from a file. */
  async addFile(): Promise<void> {
    const view = await assistIpc.loadReference();
    if (view) this.samples = [...this.samples, { name: view.name, text: view.text, truncated: view.truncated }];
  }

  /** A paper or figure of the materials as a reference (a PDF figure is
   * drawn here, its first page). */
  async addMaterial(entry: MaterialEntry): Promise<void> {
    const name = entry.path.split("/").pop() ?? entry.path;
    if (entry.kind === "text") {
      const view = await assistIpc.materialText(entry.path);
      this.samples = [...this.samples, { name: view.name, text: view.text, truncated: view.truncated }];
      return;
    }
    if (entry.kind === "pdftext") {
      const { text, truncated } = await pdfText(await assistIpc.materialPdf(entry.path));
      if (!text.trim()) throw new Error(`${name}: no text`);
      this.samples = [...this.samples, { name, text, truncated }];
      return;
    }
    const view =
      entry.kind === "image"
        ? await assistIpc.materialImage(entry.path)
        : await assistIpc.attachImageData(
            name,
            await pdfPagePng(
              entry.kind === "pptx" ? await assistIpc.materialPptx(entry.path) : await assistIpc.materialPdf(entry.path),
            ),
          );
    this.attachments = [...this.attachments, { ...view, use: "trace" }];
  }

  addText(text: string): void {
    this.samples = [...this.samples, { name: null, text, truncated: false }];
  }

  detach(id: number): void {
    this.attachments = this.attachments.filter((a) => a.id !== id);
    void assistIpc.dropAttachment(id);
  }
}
