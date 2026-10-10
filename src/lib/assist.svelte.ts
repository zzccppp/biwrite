// Writing assistant jobs as the UI sees them. A job's target range is kept
// in step with every later edit (mapped through CodeMirror changes), and the
// job notes whether the target itself was edited, which decides how an
// approved revision is applied.

import type { ChangeDesc } from "@codemirror/state";
import { assistIpc } from "./ipc";

export { asParagraph, insertionAt, reapplyInstruction, uniqueIndex } from "./assistText";
import type {
  AssistAction,
  AssistEvent,
  AssistRequest,
  AssistResult,
  AssistScope,
  AssistStarted,
  AssistTarget,
  AttachmentView,
  Sample,
} from "./types";

export type JobState = "running" | "done" | "failed" | "applied" | "discarded";

export const ACTION_LABEL = {
  polish: "assist.action.polish",
  edit: "assist.action.edit",
  ask: "assist.action.ask",
  figure: "assist.action.figure",
  mirror: "assist.action.mirror",
  write: "assist.action.write",
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
} as const;

const ACTION_KEY = "biwrite.assist.action";
const SCOPE_KEY = "biwrite.assist.scope";

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
  result = $state<AssistResult | null>(null);
  error = $state<string | null>(null);
  /** Applying found the target changed; offer to re-apply with the model. */
  conflict = $state(false);
  /** Being applied: a second Accept meanwhile does nothing. */
  applying = $state(false);
  /** Being run again (Again, Re-apply): its buttons wait for that. */
  restarting = $state(false);

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
 * goes stays before it (the new paragraph follows what the author added). */
function mapTarget(job: AssistJob, changes: ChangeDesc): void {
  if (job.target.insert) {
    job.from = job.to = changes.mapPos(job.from, 1);
    return;
  }
  if (changes.touchesRange(job.from, job.to)) job.touched = true;
  job.from = changes.mapPos(job.from, 1);
  job.to = Math.max(job.from, changes.mapPos(job.to, -1));
}

export class AssistStore {
  jobs = $state<AssistJob[]>([]);
  attachments = $state<AttachmentView[]>([]);
  samples = $state<Sample[]>([]);
  action = $state<AssistAction>(
    load(ACTION_KEY, ["polish", "edit", "ask", "figure", "mirror", "write"] as const, "polish"),
  );
  scope = $state<AssistScope>(load(SCOPE_KEY, ["target", "neighbors", "document"] as const, "neighbors"));
  instruction = $state("");
  /** Bumped when another document is loaded (reactive: cards turn stale). */
  epoch = $state(0);
  /** Edits made while jobs were being started, to map their targets through. */
  #starting = new Set<{ changes: ChangeDesc | null }>();
  /** Events of jobs whose start has not returned yet (they can come first). */
  #early = new Map<number, AssistEvent[]>();

  running = $derived(this.jobs.filter((j) => j.state === "running").length);
  ready = $derived(this.jobs.filter((j) => j.state === "done").length);

  setAction(a: AssistAction): void {
    this.action = a;
    save(ACTION_KEY, a);
  }

  setScope(s: AssistScope): void {
    this.scope = s;
    save(SCOPE_KEY, s);
  }

  async start(request: AssistRequest, reapplies: number | null = null): Promise<AssistJob> {
    // The document the request's text belongs to, before anything else loads.
    const epoch = this.epoch;
    const edits: { changes: ChangeDesc | null } = { changes: null };
    this.#starting.add(edits);
    let started: AssistStarted;
    try {
      started = await assistIpc.start(request);
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
    if (job.state !== "running") return;
    if (e.kind === "partial") {
      job.partial = e.text;
    } else if (e.kind === "done") {
      job.result = e.result;
      job.state = "done";
    } else {
      job.error = e.message;
      job.state = "failed";
    }
  }

  /** Keep the targets of open and failed jobs (Again runs there) in step
   * with an edit, and of jobs still being started. */
  map(changes: ChangeDesc): void {
    for (const edits of this.#starting) edits.changes = edits.changes ? edits.changes.composeDesc(changes) : changes;
    for (const job of this.jobs) {
      if ((!job.open && job.state !== "failed") || job.epoch !== this.epoch) continue;
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

  discard(job: AssistJob): void {
    if (job.state === "running") this.cancel(job);
    job.state = "discarded";
  }

  /** Forget finished jobs. */
  clearFinished(): void {
    this.jobs = this.jobs.filter((j) => j.state === "running" || j.state === "done");
  }

  async attach(): Promise<void> {
    const view = await assistIpc.attachImage();
    if (view) this.attachments = [...this.attachments, view];
  }

  /** A reference paper from a file. */
  async addFile(): Promise<void> {
    const view = await assistIpc.loadReference();
    if (view) this.samples = [...this.samples, { name: view.name, text: view.text, truncated: view.truncated }];
  }

  addText(text: string): void {
    this.samples = [...this.samples, { name: null, text, truncated: false }];
  }

  detach(id: number): void {
    this.attachments = this.attachments.filter((a) => a.id !== id);
    void assistIpc.dropAttachment(id);
  }
}
