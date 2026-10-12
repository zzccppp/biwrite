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
  CheckRequest,
  ClearedView,
  Direction,
  DiskView,
  ExportView,
  Fill,
  GlossaryEntry,
  InstalledView,
  KeyEntry,
  KeyStatus,
  LogSettings,
  LogView,
  MirrorSaved,
  Mode,
  PdfBox,
  PdfLang,
  ProjectCopy,
  ProjectView,
  PromptView,
  ProviderConfig,
  Range16,
  ReferenceView,
  RequestRecord,
  SavedView,
  SegmentState,
  SentencesView,
  SessionUsage,
  SessionView,
  SettingsView,
  SkillInfo,
  MaterialEntry,
  MaterialsView,
  Snapshot,
  SqueezeEvent,
  SqueezeView,
  SyncHit,
  TemplateView,
  TexStatus,
  UpdateProgress,
  UpdatesView,
} from "./types";

export const ipc = {
  getSession: () => invoke<SessionView>("get_session"),
  /** Which open files changed on disk elsewhere; `null` if `document` is no longer open. */
  checkDisk: (document: number) => invoke<DiskView | null>("check_disk", { document }),
  /** Don't load those versions: not reported again (saving still asks). */
  ignoreDisk: (document: number, which: DiskView) => invoke<void>("ignore_disk", { document, which }),
  /** The document's text on disk, to put in the editor (own language edited); nothing changes until `adoptDisk`. */
  reloadDocument: (document: number) => invoke<string | null>("reload_document", { document }),
  /** The editor holds `text` from `reloadDocument`: it becomes the file's known text if the disk still reads so. */
  adoptDisk: (document: number, text: string) => invoke<boolean>("adopt_disk", { document, text }),
  /** Open the document again from disk, with its paired file. */
  reopenFile: (document: number) => invoke<SessionView | null>("reopen_file", { document }),
  /** Pair again with the paired file as on disk; the editor keeps `text`. */
  reloadMirror: (text: string, document: number) =>
    invoke<SessionView | null>("reload_mirror", { text, document }),
  openFile: () => invoke<SessionView | null>("open_file"),
  // `document` (Snapshot.document) says which document `text` is for.
  /** `overwrite`: also over a version changed on disk elsewhere (else refused, see `isChangedOnDisk`);
   * `overwriteMirror`: the paired file too (else its write is refused). */
  saveFile: (text: string, document: number, overwrite = false, overwriteMirror = false) =>
    invoke<SavedView | null>("save_file", { text, document, overwrite, overwriteMirror }),
  saveFileAs: (text: string, document: number) => invoke<SavedView | null>("save_file_as", { text, document }),
  /** Save As into a new folder with the files the paper builds with. `title` heads the folder dialog. */
  saveProjectAs: (text: string, document: number, title: string) =>
    invoke<ProjectCopy | null>("save_project_as", { text, document, title }),
  /** `null`: the text was for a document replaced meanwhile, and was dropped. */
  updateDocument: (text: string, document: number) =>
    invoke<Snapshot | null>("update_document", { text, document }),
  setMode: (mode: Mode, text: string, document: number) =>
    invoke<Snapshot | null>("set_mode", { mode, text, document }),
  retranslateSegment: (id: number) => invoke<void>("retranslate_segment", { id }),
  retranslateAll: () => invoke<void>("retranslate_all"),
  /** The paragraph at `pos` and its sentences, as a PDF click finds them. */
  sourceSentences: (text: string, pos: number) => invoke<SentencesView | null>("source_sentences", { text, pos }),
  /** Translate the edited paragraphs waiting out the translation delay now; returns how many. */
  translateHeld: () => invoke<number>("translate_held"),
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
  /** A new key in place of key `number` (its tail checked), keeping its name. */
  replaceKey: (id: string, number: number, tail: string, key: string) =>
    invoke<SettingsView>("replace_api_key", { id, number, tail, key }),
  testKey: (id: string, fingerprint: string) => invoke<string>("test_api_key", { id, fingerprint }),
  squeezeStatus: (id: string) => invoke<SqueezeView>("squeeze_status", { id }),
  squeezeStart: (id: string) => invoke<SqueezeView>("squeeze_start", { id }),
  squeezeStop: (id: string) => invoke<SqueezeView>("squeeze_stop", { id }),
  setKeepAlive: (on: boolean) => invoke<SettingsView>("set_keep_alive", { on }),
  setBatchSize: (size: number) => invoke<SettingsView>("set_batch_size", { size }),
  setTranslateDelay: (seconds: number) => invoke<SettingsView>("set_translate_delay", { seconds }),
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
  /** Make the document compile: TeX's errors go to the model, a few rounds at most. */
  fix: (text: string, instruction: string) => invoke<AssistStarted>("assist_fix", { request: { text, instruction } }),
  /** Check (and repair) an answer before it is applied; the result comes as an event. */
  check: (request: CheckRequest) => invoke<void>("assist_check", { request }),
  checkCancel: (id: number) => invoke<void>("assist_check_cancel", { id }),
  /** The PDF a figure job made. */
  figurePdf: (id: number) => invoke<ArrayBuffer>("figure_pdf", { id }),
  /** Save a copy of a figure job's file (`png`: the preview drawn as PNG). */
  figureSave: (id: number, kind: "pdf" | "pptx" | "png", png: number[] | null = null) =>
    invoke<string | null>("figure_save", { id, kind, png }),
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
  /** The author's own materials, in the folders of the skill's. */
  materials: () => invoke<MaterialsView>("materials_get"),
  /** The papers and figures of the materials, to take as references. */
  listMaterials: () => invoke<MaterialEntry[]>("materials_list"),
  materialText: (path: string) => invoke<ReferenceView>("materials_text", { path }),
  materialImage: (path: string) => invoke<AttachmentView>("materials_image", { path }),
  materialPdf: (path: string) => invoke<ArrayBuffer>("materials_pdf", { path }),
  /** A PowerPoint figure as a PDF (LibreOffice converts it). */
  materialPptx: (path: string) => invoke<ArrayBuffer>("materials_pptx", { path }),
  /** A reference image drawn here (a PDF's first page), as a PNG in base64. */
  attachImageData: (name: string, png: string) => invoke<AttachmentView>("attach_image_data", { name, png }),
  revealMaterials: () => invoke<void>("materials_reveal"),
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
  /** The same, into a new folder with the files the paper builds with. */
  exportProject: (text: string, document: number, title: string) =>
    invoke<ProjectCopy | null>("latex_export_project", { text, document, title }),
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
  /** Write the paired file once its translations arrived (`overwrite`: also over changes made to it elsewhere). */
  write: (overwrite = false) => invoke<MirrorSaved | null>("write_mirror", { overwrite }),
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
  onSqueeze(event: SqueezeEvent): void;
}

export async function subscribe(handlers: EngineEvents): Promise<UnlistenFn> {
  const unlisten = await Promise.all([
    listen<SegmentState[]>("segment-states", (e) => handlers.onStates(e.payload)),
    listen<SessionUsage>("usage", (e) => handlers.onUsage(e.payload)),
    listen<string>("notice", (e) => handlers.onNotice(e.payload)),
    listen<RequestRecord>("request-log", (e) => handlers.onRequest(e.payload)),
    listen<AssistEvent>("assist", (e) => handlers.onAssist(e.payload)),
    listen<Fill[]>("segment-fills", (e) => handlers.onFills(e.payload)),
    listen<SqueezeEvent>("squeeze", (e) => handlers.onSqueeze(e.payload)),
  ]);
  return () => unlisten.forEach((fn) => fn());
}

/** Error from a rejected `invoke` as a display string. */
export function errorMessage(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return String(err);
}

/** The file changed on disk since it was read or saved, so the save was refused (`CommandError::ChangedOnDisk`). */
export function isChangedOnDisk(err: unknown): boolean {
  return errorMessage(err) === "changed on disk";
}

/** The PDF was built before a pairing, an unpairing or a swap without a pair (`CommandError::StalePdf`). */
export function isStalePdf(err: unknown): boolean {
  return errorMessage(err) === "stale pdf";
}
