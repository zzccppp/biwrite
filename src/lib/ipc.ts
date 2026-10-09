// Typed wrappers around Tauri commands and events.
// The webview only ever sends document text and receives segment data;
// file paths are chosen in native dialogs on the Rust side.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Direction,
  ExportView,
  GlossaryEntry,
  KeyStatus,
  LogSettings,
  LogView,
  Mode,
  PromptView,
  ProviderConfig,
  RequestRecord,
  SavedView,
  SegmentState,
  SessionUsage,
  SessionView,
  SettingsView,
  Snapshot,
} from "./types";

export const ipc = {
  getSession: () => invoke<SessionView>("get_session"),
  openFile: () => invoke<SessionView | null>("open_file"),
  saveFile: (text: string) => invoke<SavedView | null>("save_file", { text }),
  saveFileAs: (text: string) => invoke<SavedView | null>("save_file_as", { text }),
  updateDocument: (text: string) => invoke<Snapshot>("update_document", { text }),
  setMode: (mode: Mode, text: string) => invoke<Snapshot>("set_mode", { mode, text }),
  retranslateSegment: (id: number) => invoke<void>("retranslate_segment", { id }),
  retranslateAll: () => invoke<void>("retranslate_all"),
  setAutoTranslate: (on: boolean) => invoke<void>("set_auto_translate", { on }),
  setDirty: (dirty: boolean) => invoke<void>("set_dirty", { dirty }),
  swapLanguages: (text: string) => invoke<SessionView>("swap_languages", { text }),
  exportBilingual: (text: string) => invoke<ExportView | null>("export_bilingual", { text }),
};

/** Glossary commands. CSV files are picked in native dialogs on the Rust side. */
export const glossaryIpc = {
  get: () => invoke<GlossaryEntry[]>("get_glossary"),
  save: (entries: GlossaryEntry[]) => invoke<GlossaryEntry[]>("save_glossary", { entries }),
  importCsv: () => invoke<GlossaryEntry[] | null>("import_glossary"),
  exportCsv: () => invoke<string | null>("export_glossary"),
};

/** Settings commands. API keys only ever travel *to* Rust (write-only). */
export const settingsIpc = {
  get: () => invoke<SettingsView>("get_settings"),
  saveProvider: (provider: ProviderConfig) => invoke<SettingsView>("save_provider", { provider }),
  deleteProvider: (id: string) => invoke<SettingsView>("delete_provider", { id }),
  /** Replace the provider's keys with the pasted ones (one per line). */
  setApiKey: (id: string, key: string) => invoke<SettingsView>("set_api_key", { id, key }),
  /** Add the pasted keys to the provider's pool. */
  addApiKeys: (id: string, keys: string) => invoke<SettingsView>("add_api_keys", { id, keys }),
  removeApiKey: (id: string, number: number, tail: string) =>
    invoke<SettingsView>("remove_api_key", { id, number, tail }),
  clearApiKey: (id: string) => invoke<SettingsView>("clear_api_key", { id }),
  keyStatus: (id: string) => invoke<KeyStatus[] | null>("key_status", { id }),
  setAssistantProvider: (id: string) => invoke<SettingsView>("set_assistant_provider", { id }),
  setActive: (id: string) => invoke<SettingsView>("set_active_provider", { id }),
  test: (id: string) => invoke<string>("test_provider", { id }),
  listModels: (id: string) => invoke<string[]>("list_provider_models", { id }),
  setConcurrency: (concurrency: number) => invoke<SettingsView>("set_concurrency", { concurrency }),
  setDocNote: (note: string) => invoke<void>("set_doc_note", { note }),
  getPrompt: (direction: Direction) => invoke<PromptView>("get_prompt", { direction }),
  savePrompt: (direction: Direction, text: string) => invoke<PromptView>("save_prompt", { direction, text }),
  revealPrompts: () => invoke<void>("reveal_prompts"),
};

/** Request log (metadata of model requests, no text, no keys). */
export const logIpc = {
  get: () => invoke<LogView>("get_request_log"),
  set: (settings: LogSettings) => invoke<LogView>("set_request_log", { settings }),
  clear: () => invoke<LogView>("clear_request_log"),
  reveal: () => invoke<void>("reveal_request_log"),
};

export interface EngineEvents {
  onStates(states: SegmentState[]): void;
  onUsage(usage: SessionUsage): void;
  onNotice(message: string): void;
  onRequest(record: RequestRecord): void;
}

export async function subscribe(handlers: EngineEvents): Promise<UnlistenFn> {
  const unlisten = await Promise.all([
    listen<SegmentState[]>("segment-states", (e) => handlers.onStates(e.payload)),
    listen<SessionUsage>("usage", (e) => handlers.onUsage(e.payload)),
    listen<string>("notice", (e) => handlers.onNotice(e.payload)),
    listen<RequestRecord>("request-log", (e) => handlers.onRequest(e.payload)),
  ]);
  return () => unlisten.forEach((fn) => fn());
}

/** Error from a rejected `invoke` as a display string. */
export function errorMessage(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
