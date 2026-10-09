// Reactive view state of the current document. Segment states are keyed by
// stable segment ID, so the right pane never re-creates blocks for segments
// whose IDs survived an edit.

import type { ChangeDesc } from "@codemirror/state";
import { SvelteMap, SvelteSet } from "svelte/reactivity";
import { type Macros, parseMacros, preambleOf } from "./math";
import { SegmentIndex } from "./segmentIndex";
import type { Direction, Mode, SegmentLayout, SegmentState, SessionUsage, SessionView, Snapshot } from "./types";

const NO_USAGE: SessionUsage = { requests: 0, inputTokens: 0, outputTokens: 0, cacheHits: 0 };

export class Session {
  path = $state<string | null>(null);
  name = $state("Untitled");
  mode = $state<Mode>("plain");
  direction = $state<Direction>("en-zh");
  dirty = $state(false);
  autoTranslate = $state(true);
  lineEnding = $state<"lf" | "crlf" | "cr">("lf");
  usage = $state<SessionUsage>(NO_USAGE);
  /** Render order and kinds; replaced wholesale on each snapshot. */
  layout = $state.raw<SegmentLayout[]>([]);
  readonly states = new SvelteMap<number, SegmentState>();
  /** Segments edited locally but not yet re-sent to the engine (shown dimmed). */
  readonly localStale = new SvelteSet<number>();
  activeId = $state<number | null>(null);
  notice = $state<string | null>(null);
  error = $state<string | null>(null);
  /** Positions in the current editor document (not reactive). */
  readonly index = new SegmentIndex();
  /** Math macros from the LaTeX preamble, for rendering on the right. */
  macros = $state.raw<Macros>({});
  /** Collapsed math blocks the user expanded on the right. */
  readonly expanded = new SvelteSet<number>();
  private preamble = "";

  counts = $derived.by(() => {
    const c = { translated: 0, pending: 0, translating: 0, error: 0, skipped: 0 };
    // Over the live layout only: a late event may still mention a removed segment.
    for (const seg of this.layout) {
      const s = this.states.get(seg.id);
      if (!s) c.pending++;
      else if (s.status === "translated") c.translated++;
      else if (s.status === "translating") c.translating++;
      else if (s.status === "error") c.error++;
      else if (s.status === "skipped") c.skipped++;
      else c.pending++;
    }
    return c;
  });

  load(view: SessionView): void {
    this.path = view.path;
    this.name = view.name;
    this.dirty = view.dirty;
    this.autoTranslate = view.autoTranslate;
    this.lineEnding = view.lineEnding;
    this.activeId = null;
    this.localStale.clear();
    this.expanded.clear();
    this.applySnapshot(view.snapshot);
    this.notePreamble(view.text);
  }

  /** Re-read the macros if the preamble of `text` (the editor document) changed. */
  notePreamble(text: string): void {
    const preamble = this.mode === "latex" ? preambleOf(text) : "";
    if (preamble === this.preamble) return;
    this.preamble = preamble;
    this.macros = parseMacros(preamble);
  }

  toggleExpanded(id: number): void {
    if (!this.expanded.delete(id)) this.expanded.add(id);
  }

  /** Positions of the expanded blocks, to carry them across a swap. */
  expandedPositions(): number[] {
    return this.layout.flatMap((s, i) => (this.expanded.has(s.id) ? [i] : []));
  }

  restoreExpanded(positions: number[]): void {
    for (const i of positions) {
      const seg = this.layout[i];
      if (seg?.kind.type === "skipped") this.expanded.add(seg.id);
    }
  }

  /**
   * Apply an engine snapshot computed for the text that was sent; `changes`
   * are the edits made in the editor since then.
   */
  applySnapshot(snap: Snapshot, changes?: ChangeDesc): void {
    this.mode = snap.mode;
    this.direction = snap.direction;
    this.usage = snap.usage;
    this.layout = snap.layout;
    const alive = new Set(snap.layout.map((s) => s.id));
    for (const id of [...this.states.keys()]) {
      if (!alive.has(id)) this.states.delete(id);
    }
    this.applyStates(snap.states);
    this.index.reset(snap.layout, changes);

    // Edits made after sending still need to be marked as pending locally.
    this.localStale.clear();
    if (changes && !changes.empty) {
      const sent = new SegmentIndex();
      sent.reset(snap.layout);
      changes.iterChangedRanges((fromA, toA) => {
        for (const id of sent.touching(fromA, toA)) this.localStale.add(id);
      });
    }
    if (this.activeId !== null && !alive.has(this.activeId)) this.activeId = null;
    for (const id of [...this.expanded]) {
      if (!alive.has(id)) this.expanded.delete(id);
    }
  }

  /** Apply streamed states, dropping any older than what we already have. */
  applyStates(states: SegmentState[]): void {
    for (const s of states) {
      const current = this.states.get(s.id);
      if (!current || s.version > current.version) this.states.set(s.id, s);
    }
  }

  /** Map positions through a local edit and mark touched segments as pending. */
  noteEdit(changes: ChangeDesc): void {
    changes.iterChangedRanges((fromA, toA) => {
      for (const id of this.index.touching(fromA, toA)) this.localStale.add(id);
    });
    this.index.map(changes);
  }

  flash(message: string): void {
    this.notice = message;
    const shown = message;
    setTimeout(() => {
      if (this.notice === shown) this.notice = null;
    }, 6000);
  }
}
