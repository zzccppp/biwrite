// Segment-anchored scroll sync: the segment at the top edge of one pane is
// aligned with its counterpart at the top edge of the other, proportionally
// within the segment. Whichever pane the user is interacting with drives.
// While the user types, the active (cursor) block is also kept in view on
// the right, scrolling as little as possible.

import type { EditorView } from "@codemirror/view";
import type { SegmentIndex } from "./segmentIndex";

export type Side = "left" | "right";

export interface ScrollSyncDeps {
  editor(): EditorView | null;
  right(): HTMLElement | null;
  block(id: number): HTMLElement | undefined;
  /** Segment under the editor cursor. */
  active(): number | null;
  index: SegmentIndex;
}

const clamp01 = (x: number) => (Number.isFinite(x) ? Math.min(1, Math.max(0, x)) : 0);
/** A pane hidden behind another tab has no size: its scroll position means nothing. */
const shown = (el: HTMLElement) => el.clientHeight > 0;
/** Space kept around the active block when revealing it. */
const REVEAL_MARGIN = 24;

export class ScrollSync {
  driver: Side = "left";
  /** Keep the active block visible (typing); a user scroll turns it off. */
  private following = false;
  private deps: ScrollSyncDeps;
  private frame = 0;
  /** Scroll events caused by our own writes, per pane. */
  private echo: Record<Side, number> = { left: 0, right: 0 };

  constructor(deps: ScrollSyncDeps) {
    this.deps = deps;
  }

  /** Called from scroll events of either pane. */
  onScroll(side: Side): void {
    if (this.echo[side] > 0) {
      this.echo[side]--;
      return;
    }
    if (side === this.driver) this.schedule();
  }

  /** The cursor moved or the user typed: show the active block again. */
  follow(): void {
    this.following = true;
    this.schedule();
  }

  /** The user scrolled by hand (wheel, scrollbar): alignment alone rules. */
  unfollow(): void {
    this.following = false;
  }

  /** Re-align after content changes (streamed text, reflow). */
  schedule(): void {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      if (this.driver === "left") this.leftToRight();
      else this.rightToLeft();
    });
  }

  /** Document-relative y of the top edge of the editor viewport. */
  private editorTop(view: EditorView): number {
    const scroller = view.scrollDOM;
    return scroller.getBoundingClientRect().top - view.documentTop;
  }

  private leftToRight(): void {
    const view = this.deps.editor();
    const right = this.deps.right();
    if (!view || !right || this.deps.index.size === 0 || !shown(right)) return;
    // Reading line: the top edge plus the content padding on each side, so
    // both panes at scrollTop 0 correspond exactly.
    const y = this.editorTop(view) + this.leadLeft(view);
    const line = view.lineBlockAtHeight(Math.max(0, y));
    const i = this.deps.index.indexAtOrAfter(line.from);
    if (i < 0) return this.write(right, "right", right.scrollHeight);
    const { from, to } = this.deps.index.rangeAt(i);
    const el = this.deps.block(this.deps.index.idAt(i));
    if (!el) return;
    const top = view.lineBlockAt(from).top;
    const bottom = view.lineBlockAt(to).bottom;
    const frac = clamp01((y - top) / (bottom - top));
    const box = this.blockBox(right, el);
    this.write(right, "right", this.reveal(view, right, box.top + frac * box.height - this.lead(right)));
  }

  /**
   * Adjust an aligned scroll target so the active block is in view, if the
   * user is typing and the block's segment is visible in the editor. A block
   * taller than the pane is kept covering it.
   */
  private reveal(view: EditorView, right: HTMLElement, target: number): number {
    const id = this.deps.active();
    if (!this.following || id === null) return target;
    const range = this.deps.index.range(id);
    const el = this.deps.block(id);
    if (!range || !el) return target;
    const top = this.editorTop(view);
    const bottom = top + view.scrollDOM.clientHeight;
    if (view.lineBlockAt(range.to).bottom <= top || view.lineBlockAt(range.from).top >= bottom) return target;

    const box = this.blockBox(right, el);
    const margin = Math.min(REVEAL_MARGIN, right.clientHeight / 8);
    const showTop = box.top - margin; // highest scrollTop showing the block's top
    const showBottom = box.top + box.height + margin - right.clientHeight; // lowest showing its bottom
    return showBottom <= showTop
      ? Math.min(Math.max(target, showBottom), showTop)
      : Math.min(Math.max(target, showTop), showBottom);
  }

  /** Block position in the right pane's scroll coordinates. */
  private blockBox(right: HTMLElement, el: HTMLElement): { top: number; height: number } {
    const r = el.getBoundingClientRect();
    return { top: r.top - right.getBoundingClientRect().top + right.scrollTop, height: r.height };
  }

  private rightToLeft(): void {
    const view = this.deps.editor();
    const right = this.deps.right();
    if (!view || !right || this.deps.index.size === 0 || !shown(right)) return;
    const y = right.scrollTop + this.lead(right);
    // Binary search for the first block whose bottom is below the reading line.
    let lo = 0;
    let hi = this.deps.index.size - 1;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      const el = this.deps.block(this.deps.index.idAt(mid));
      const box = el ? this.blockBox(right, el) : null;
      if (box && box.top + box.height <= y) lo = mid + 1;
      else hi = mid;
    }
    const i = lo;
    const el = this.deps.block(this.deps.index.idAt(i));
    if (!el) return;
    const box = this.blockBox(right, el);
    const frac = clamp01((y - box.top) / box.height);
    const { from, to } = this.deps.index.rangeAt(i);
    const top = view.lineBlockAt(from).top;
    const bottom = view.lineBlockAt(to).bottom;
    const docOffset = view.documentTop - view.scrollDOM.getBoundingClientRect().top + view.scrollDOM.scrollTop;
    this.write(view.scrollDOM, "left", docOffset + top + frac * (bottom - top) - this.leadLeft(view));
  }

  /** Space above the first block in the right pane, matched to the editor's top padding. */
  private lead(right: HTMLElement): number {
    return parseFloat(getComputedStyle(right).paddingTop) || 0;
  }

  private leadLeft(view: EditorView): number {
    return parseFloat(getComputedStyle(view.contentDOM).paddingTop) || 0;
  }

  private write(el: HTMLElement, side: Side, top: number): void {
    const target = Math.max(0, Math.min(top, el.scrollHeight - el.clientHeight));
    if (Math.abs(el.scrollTop - target) < 1) return;
    this.echo[side]++;
    el.scrollTop = target;
    // If the browser coalesces or drops the event, don't swallow a real one later.
    setTimeout(() => (this.echo[side] = 0), 80);
  }
}
