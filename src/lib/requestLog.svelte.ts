// Request records for the Log panel and the toolbar badge. Kept up to date
// from "request-log" events even while the panel is closed.

import { SvelteMap } from "svelte/reactivity";
import type { LogSettings, LogView, RequestRecord } from "./types";

/** Same bound as the Rust side keeps in memory. */
const MAX_RECORDS = 500;

export class RequestLogStore {
  readonly records = new SvelteMap<number, RequestRecord>();
  settings = $state<LogSettings>({ enabled: true, persist: true });
  file = $state<string | null>(null);

  /** Newest first. */
  list = $derived([...this.records.values()].sort((a, b) => b.id - a.id));
  inFlight = $derived([...this.records.values()].filter((r) => r.state === "in_flight").length);

  load(view: LogView): void {
    this.records.clear();
    for (const r of view.records) this.records.set(r.id, r);
    this.settings = view.settings;
    this.file = view.file;
  }

  apply(record: RequestRecord): void {
    this.records.set(record.id, record);
    if (this.records.size > MAX_RECORDS) {
      const oldest = Math.min(...this.records.keys());
      this.records.delete(oldest);
    }
  }
}
