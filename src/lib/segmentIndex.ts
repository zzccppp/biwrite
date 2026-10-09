// Non-reactive index of segment positions in the *current* editor document.
// Positions arrive from Rust for the text that was sent; they are mapped
// through every later CodeMirror change so lookups stay exact between updates.

import type { ChangeDesc } from "@codemirror/state";
import type { SegmentLayout } from "./types";

export class SegmentIndex {
  private ids: number[] = [];
  private from: number[] = [];
  private to: number[] = [];

  get size(): number {
    return this.ids.length;
  }

  /** Replace with a new layout, mapped through `changes` made since it was computed. */
  reset(layout: SegmentLayout[], changes?: ChangeDesc): void {
    this.ids = layout.map((s) => s.id);
    this.from = layout.map((s) => s.from);
    this.to = layout.map((s) => s.to);
    if (changes && !changes.empty) this.map(changes);
  }

  /**
   * Map through an edit. Typing at either edge of a segment extends it:
   * starts stick left (-1), ends stick right (+1).
   */
  map(changes: ChangeDesc): void {
    for (let i = 0; i < this.ids.length; i++) {
      this.from[i] = changes.mapPos(this.from[i], -1);
      this.to[i] = Math.max(this.from[i], changes.mapPos(this.to[i], 1));
    }
  }

  idAt(i: number): number {
    return this.ids[i];
  }

  rangeAt(i: number): { from: number; to: number } {
    return { from: this.from[i], to: this.to[i] };
  }

  indexOf(id: number): number {
    return this.ids.indexOf(id);
  }

  range(id: number): { from: number; to: number } | null {
    const i = this.ids.indexOf(id);
    return i < 0 ? null : this.rangeAt(i);
  }

  /** Last segment starting at or before `pos`, or -1. */
  private lastStartingAtOrBefore(pos: number): number {
    let lo = 0;
    let hi = this.ids.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (this.from[mid] <= pos) {
        found = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }
    return found;
  }

  /** Segment containing `pos` (edges inclusive), or null if `pos` is in a gap. */
  idContaining(pos: number): number | null {
    const i = this.lastStartingAtOrBefore(pos);
    return i >= 0 && pos <= this.to[i] ? this.ids[i] : null;
  }

  /** Index of the segment containing `pos`, else the next one after it; -1 past the end. */
  indexAtOrAfter(pos: number): number {
    const i = this.lastStartingAtOrBefore(pos);
    if (i >= 0 && pos <= this.to[i]) return i;
    return i + 1 < this.ids.length ? i + 1 : -1;
  }

  /** IDs of segments touching `[from, to]` (old coordinates of an edit). */
  touching(from: number, to: number): number[] {
    const out: number[] = [];
    for (let i = Math.max(0, this.lastStartingAtOrBefore(from)); i < this.ids.length; i++) {
      if (this.from[i] > to) break;
      if (this.to[i] >= from) out.push(this.ids[i]);
    }
    return out;
  }
}
