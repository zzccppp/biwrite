// Typed wrappers around Tauri commands and events.
// The webview only ever sends document text and receives segment data;
// file paths are chosen in native dialogs on the Rust side.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AssistEvent,
  AssistRequest,
  AssistStarted,
  AttachmentView,
  BuildView,
  CacheView,
  ClearedView,
  Direction,
  ExportView,
  Fill,
  GlossaryEntry,
  InstalledView,
  MirrorSaved,
  KeyEntry,
  KeyStatus,
  LogSettings,
  LogView,
  Mode,
  PdfBox,
  PdfLang,
  ProjectView,
  PromptView,
  ProviderConfig,
  Range16,
  ReferenceView,
  RequestRecord,
  SavedView,
  SegmentState,
  SessionUsage,
  SessionView,
  SettingsView,
  SkillInfo,
  Snapshot,
  SyncHit,
  TemplateView,
  TexStatus,
  UpdateProgress,
  UpdatesView,
} from "./types";

export const ipc = {
  getSession: () => invoke<SessionView>("get_session"),
  openFile: () => invoke<SessionView | null>("open_file"),
  // `document` (Snapshot.document) says which document `text` is for.
  saveFile: (text: string, document: number) => invoke<SavedView | null>("save_file", { text, document }),
  saveFileAs: (text: string, document: number) => invoke<SavedView | null>("save_file_as", { text, document }),
  /** `null`: the text was for a document replaced meanwhile, and was dropped. */
  updateDocument: (text: string, document: number) =>
    invoke<Snapshot | null>("update_document", { text, document }),
  setMode: (mode: Mode, text: string, document: number) =>
    invoke<Snapshot | null>("set_mode", { mode, text, document }),
  retranslateSegment: (id: number) => invoke<void>("retranslate_segment", { id }),
  retranslateAll: () => invoke<void>("retranslate_all"),
  setAutoTranslate: (on: boolean) => invoke<void>("set_auto_translate", { on }),
  setDirty: (dirty: boolean) => invoke<void>("set_dirty", { dirty }),
  /** `keep`: swap now, untranslated paragraphs stay as they are. */
  swapLanguages: (text: string, document: number, keep = false) =>
    invoke<SessionView>("swap_languages", { text, document, keep }),
  /**
   * Read the file as written in the other language (a Chinese file opened
   * as English). `onlyIfNeeded`: only when its text plainly is; `null`
   * when nothing changed.
   */
  retargetLanguage: (text: string, document: number, onlyIfNeeded: boolean) =>
    invoke<SessionView | null>("retarget_language", { text, document, onlyIfNeeded }),
  /** Translate what is left; returns how many paragraphs were taken up. */
  continueTranslation: (text: string, document: number) => invoke<number>("continue_translation", { text, document }),
  exportBilingual: (text: string, document: number) => invoke<ExportView | null>("export_bilingual", { text, document }),
  /** Open a known link ("skill", "repo", "releases") in the browser. */
  openLink: (name: "skill" | "repo" | "releases") => invoke<void>("open_link", { name }),
  /** The user guide on GitHub, in the browser. */
  openManual: (lang: "en" | "zh") => invoke<void>("open_manual", { lang }),
  /** The tray menu in the interface language. */
  setTrayLanguage: (lang: "en" | "zh") => invoke<void>("set_tray_language", { lang }),
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
  /** The pool with names and live state (reads the keychain). */
  listKeys: (id: string) => invoke<KeyEntry[]>("list_api_keys", { id }),
  renameKey: (id: string, fingerprint: string, name: string) =>
    invoke<SettingsView>("rename_api_key", { id, fingerprint, name }),
  testKey: (id: string, fingerprint: string) => invoke<string>("test_api_key", { id, fingerprint }),
  setBatchSize: (size: number) => invoke<SettingsView>("set_batch_size", { size }),
  setMatchPool: (on: boolean) => invoke<SettingsView>("set_match_pool", { on }),
  setCheckUpdates: (on: boolean) => invoke<SettingsView>("set_check_updates", { on }),
  setCloseToTray: (on: boolean) => invoke<SettingsView>("set_close_to_tray", { on }),
  /** Save the pool (names and keys) to a file picked in Rust. */
  exportKeys: (id: string) => invoke<string | null>("export_api_keys", { id }),
  importKeys: (id: string) => invoke<SettingsView | null>("import_api_keys", { id }),
  setAssistantProvider: (id: string) => invoke<SettingsView>("set_assistant_provider", { id }),
  setActive: (id: string) => invoke<SettingsView>("set_active_provider", { id }),
  test: (id: string) => invoke<string>("test_provider", { id }),
  listModels: (id: string) => invoke<string[]>("list_provider_models", { id }),
  setConcurrency: (concurrency: number) => invoke<SettingsView>("set_concurrency", { concurrency }),
  setDocNote: (note: string) => invoke<void>("set_doc_note", { note }),
  getPrompt: (direction: Direction) => invoke<PromptView>("get_prompt", { direction }),
  savePrompt: (direction: Direction, text: string) => invoke<PromptView>("save_prompt", { direction, text }),
  revealPrompts: () => invoke<void>("reveal_prompts"),
  getCache: () => invoke<CacheView>("get_cache"),
  clearCache: () => invoke<ClearedView>("clear_cache"),
  revealLogs: () => invoke<void>("reveal_logs"),
};

/** Writing assistant: background jobs, reference images, the skill. */
export const assistIpc = {
  start: (request: AssistRequest) => invoke<AssistStarted>("assist_start", { request }),
  cancel: (id: number) => invoke<void>("assist_cancel", { id }),
  /** Hand the engine an approved translation before the edit is applied. */
  offer: (source: string, translation: string) => invoke<void>("assist_offer", { source, translation }),
  attachImage: () => invoke<AttachmentView | null>("attach_image"),
  /** A reference paper or text from a file (.tex, .md, .txt). */
  loadReference: () => invoke<ReferenceView | null>("load_reference"),
  dropAttachment: (id: number) => invoke<void>("drop_attachment", { id }),
  getSkill: () => invoke<SkillInfo>("get_skill"),
  updateSkill: () => invoke<SkillInfo>("update_skill"),
  chooseSkillFolder: () => invoke<SkillInfo | null>("choose_skill_folder"),
  resetSkillFolder: () => invoke<SkillInfo>("reset_skill_folder"),
  revealSkill: () => invoke<void>("reveal_skill"),
};

/** LaTeX: builds, the PDF, SyncTeX, project files and templates. Folders
 * and files are chosen in native dialogs on the Rust side. */
export const latexIpc = {
  status: (refresh = false) => invoke<TexStatus>("latex_status", { refresh }),
  setCompileOnSave: (on: boolean) => invoke<void>("latex_set_compile_on_save", { on }),
  chooseBin: () => invoke<TexStatus>("latex_choose_bin"),
  resetBin: () => invoke<TexStatus>("latex_reset_bin"),
  project: () => invoke<ProjectView | null>("latex_project"),
  /** Open another .tex file of the project (relative to its folder). */
  open: (file: string) => invoke<SessionView | null>("latex_open", { file }),
  openFolder: () => invoke<SessionView | null>("latex_open_folder"),
  compile: (lang: PdfLang, text: string, document: number) =>
    invoke<BuildView>("latex_compile", { lang, text, document }),
  cancel: (lang: PdfLang) => invoke<void>("latex_cancel", { lang }),
  pdf: (lang: PdfLang) => invoke<ArrayBuffer>("latex_pdf", { lang }),
  revealPdf: (lang: PdfLang) => invoke<void>("latex_reveal_pdf", { lang }),
  savePdf: (lang: PdfLang) => invoke<string | null>("latex_save_pdf", { lang }),
  /** Save the Chinese version as a .tex that compiles on its own. */
  exportTex: (text: string, document: number) => invoke<string | null>("latex_export_tex", { text, document }),
  inverse: (
    lang: PdfLang,
    page: number,
    x: number,
    y: number,
    span: string,
    click: number,
    text: string,
    document: number,
  ) => invoke<SyncHit | null>("latex_inverse", { lang, page, x, y, span, click, text, document }),
  forward: (lang: PdfLang, offset: number, text: string, document: number) =>
    invoke<PdfBox[]>("latex_forward", { lang, offset, text, document }),
  locate: (text: string, line: number, span: string, click: number) =>
    invoke<Range16>("latex_locate", { text, line, span, click }),
  goto: (lang: PdfLang, line: number, text: string) => invoke<Range16 | null>("latex_goto", { lang, line, text }),
  templates: () => invoke<TemplateView[]>("latex_templates"),
  newPaper: (id: string) => invoke<SessionView | null>("latex_new_paper", { id }),
  importTemplate: (zip: boolean) => invoke<TemplateView | null>("latex_import_template", { zip }),
  exportTemplate: () => invoke<string | null>("latex_export_template"),
  deleteTemplate: (id: string) => invoke<void>("latex_delete_template", { id }),
  revealTemplates: () => invoke<void>("latex_reveal_templates"),
};

/** The open document's pair: a file in the other language kept in step. */
export const pairIpc = {
  /** Pair with a file the user picks (its paragraphs become the translations). */
  importMirror: (text: string, document: number) => invoke<SessionView | null>("import_mirror", { text, document }),
  close: () => invoke<void>("close_mirror"),
  /** Write the paired file once its translations arrived. */
  write: () => invoke<MirrorSaved | null>("write_mirror"),
};

/** Updates from BiWrite's GitHub releases (pre-releases are branch builds). */
export const updateIpc = {
  list: () => invoke<UpdatesView>("list_releases"),
  install: (tag: string) => invoke<InstalledView>("install_release", { tag }),
  relaunch: () => invoke<void>("relaunch"),
  onProgress: (handler: (p: UpdateProgress) => void) =>
    listen<UpdateProgress>("update-progress", (e) => handler(e.payload)),
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
  onAssist(event: AssistEvent): void;
  onFills(fills: Fill[]): void;
}

export async function subscribe(handlers: EngineEvents): Promise<UnlistenFn> {
  const unlisten = await Promise.all([
    listen<SegmentState[]>("segment-states", (e) => handlers.onStates(e.payload)),
    listen<SessionUsage>("usage", (e) => handlers.onUsage(e.payload)),
    listen<string>("notice", (e) => handlers.onNotice(e.payload)),
    listen<RequestRecord>("request-log", (e) => handlers.onRequest(e.payload)),
    listen<AssistEvent>("assist", (e) => handlers.onAssist(e.payload)),
    listen<Fill[]>("segment-fills", (e) => handlers.onFills(e.payload)),
  ]);
  return () => unlisten.forEach((fn) => fn());
}

/** Error from a rejected `invoke` as a display string. */
export function errorMessage(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}
