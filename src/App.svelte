<script lang="ts">
  import { ChangeSet, type Text } from "@codemirror/state";
  import type { ViewUpdate } from "@codemirror/view";
  import { onMount } from "svelte";
  import { type AssistJob, AssistStore, reapplyInstruction, uniqueIndex } from "./lib/assist.svelte";
  import AssistPanel from "./lib/components/AssistPanel.svelte";
  import GlossaryPanel from "./lib/components/GlossaryPanel.svelte";
  import LogPanel from "./lib/components/LogPanel.svelte";
  import PdfPane from "./lib/components/PdfPane.svelte";
  import SettingsPanel from "./lib/components/SettingsPanel.svelte";
  import Splitter from "./lib/components/Splitter.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import TemplatePanel from "./lib/components/TemplatePanel.svelte";
  import Toolbar from "./lib/components/Toolbar.svelte";
  import TranslationPane from "./lib/components/TranslationPane.svelte";
  import { SourceEditor } from "./lib/editor/editor";
  import { count, language, t } from "./lib/i18n.svelte";
  import { assistIpc, errorMessage, ipc, latexIpc, logIpc, pairIpc, settingsIpc, subscribe, updateIpc } from "./lib/ipc";
  import { LatexStore } from "./lib/latex.svelte";
  import { RequestLogStore } from "./lib/requestLog.svelte";
  import { ScrollSync, type Side } from "./lib/scrollSync";
  import { Session } from "./lib/session.svelte";
  import { applyTheme, loadTheme, nextTheme, type ThemePref } from "./lib/theme";
  import type {
    AssistAction,
    AssistRequest,
    AssistScope,
    Direction,
    Fill,
    IssueView,
    Mode,
    PdfBox,
    PdfLang,
    MirrorSaved,
    PdfMenu,
    PdfPick,
    SavedView,
    SessionView,
    SettingsView,
    SkillInfo,
  } from "./lib/types";

  /** Idle time after the last keystroke before the engine re-segments. */
  const DEBOUNCE_MS = 800;
  const SPLIT_KEY = "biwrite.split";
  const ASSIST_KEY = "biwrite.assist.open";
  const PANE_KEY = "biwrite.right";
  const isMac = navigator.platform.toLowerCase().includes("mac");

  const session = new Session();
  const requestLog = new RequestLogStore();
  const blocks = new Map<number, HTMLElement>();

  let editor: SourceEditor | null = null;
  let editorHost: HTMLElement;
  let rightPane = $state<HTMLElement>();
  let panes = $state<HTMLElement>();
  let ratio = $state(loadRatio());
  let theme = $state<ThemePref>(loadTheme());
  let highlightTick = $state(0);
  let settings = $state<SettingsView | null>(null);
  let showSettings = $state(false);
  /** Section to show when Settings opens (e.g. "updates"). */
  let settingsFocus = $state<string | null>(null);
  /** A newer release found at startup. */
  let updateAvailable = $state<string | null>(null);
  /** Swap as soon as every paragraph is translated (the swap button was pressed early). */
  let swapWhenReady = $state(false);
  /** The paired file waits for translations before it is written. */
  let mirrorWaiting = $state(false);
  let showGlossary = $state(false);
  let showLog = $state(false);
  let showAssist = $state(loadFlag(ASSIST_KEY));
  let skill = $state<SkillInfo | null>(null);
  /** Editor selection, for the assistant's target preview. */
  let selection = $state({ from: 0, to: 0 });
  let assistPanel = $state<{ focus(): void }>();
  const assist = new AssistStore();
  const latex = new LatexStore();
  /** Right pane of a LaTeX document: the translation or the PDF. */
  let rightTab = $state<"translation" | "pdf">(loadPane());
  let showTemplates = $state(false);
  let pdfMenu = $state<PdfMenu | null>(null);
  let pdfMarks = $state<{ lang: PdfLang; boxes: PdfBox[]; tick: number }>({ lang: "en", boxes: [], tick: 0 });
  /** The last click on the PDF, to find its words again in a file opened from there. */
  let lastPick: PdfPick | null = null;
  const isLatex = $derived(session.mode === "latex");
  const showPdf = $derived(isLatex && rightTab === "pdf");
  const compilable = $derived(!!session.path && session.path.toLowerCase().endsWith(".tex"));

  // Engine round-trip bookkeeping (not reactive).
  let savedDoc: Text | null = null;
  /** Edits made since the text was last sent to the engine. */
  let unsent: ChangeSet = ChangeSet.empty(0);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let inflight = false;
  let resend = false;
  let pendingMode: Mode | null = null;
  /** Bumped when a different document is loaded; stale responses are dropped. */
  let epoch = 0;
  let saving = false;
  let compiling = false;
  let opening = false;
  let swapping = false;
  let exporting = false;
  /** Fills for paragraphs not known here yet (they came during a swap). */
  let pendingFills: Fill[] = [];
  /** A fill is being put in: not an edit that makes the file dirty. */
  let applyingFill = false;

  /** "English" or "Chinese" (in the interface language), for the source of `direction`. */
  function langName(direction: Direction): string {
    return direction === "zh-en" ? t("lang.chinese") : t("lang.english");
  }
  /** Resolves when the in-flight engine update finishes. */
  let inflightDone: Promise<void> | null = null;

  const sync = new ScrollSync({
    editor: () => editor?.view ?? null,
    right: () => rightPane ?? null,
    block: (id) => blocks.get(id),
    active: () => session.activeId,
    index: session.index,
  });

  function loadRatio(): number {
    try {
      const v = parseFloat(localStorage.getItem(SPLIT_KEY) ?? "");
      return v >= 0.22 && v <= 0.78 ? v : 0.5;
    } catch {
      return 0.5;
    }
  }

  function loadPane(): "translation" | "pdf" {
    try {
      return localStorage.getItem(PANE_KEY) === "pdf" ? "pdf" : "translation";
    } catch {
      return "translation";
    }
  }

  function setRightTab(tab: "translation" | "pdf"): void {
    rightTab = tab;
    pdfMenu = null;
    try {
      localStorage.setItem(PANE_KEY, tab);
    } catch {
      // Not persisted; harmless.
    }
    if (tab === "pdf") {
      if (compilable && latex.ready && !latex.builds[latex.lang] && !latex.building[latex.lang]) void compile();
    } else {
      requestAnimationFrame(() => sync.schedule());
    }
  }

  function loadFlag(key: string): boolean {
    try {
      return localStorage.getItem(key) === "1";
    } catch {
      return false;
    }
  }

  function setAssistOpen(open: boolean): void {
    showAssist = open;
    try {
      localStorage.setItem(ASSIST_KEY, open ? "1" : "0");
    } catch {
      // Not persisted; harmless.
    }
    requestAnimationFrame(() => sync.schedule());
  }

  function setRatio(r: number): void {
    ratio = r;
    try {
      localStorage.setItem(SPLIT_KEY, String(r));
    } catch {
      // Not persisted; harmless.
    }
  }

  function fail(err: unknown): void {
    session.error = errorMessage(err);
  }

  function dismissError(): void {
    session.error = null;
  }

  // ── Editor → engine ──────────────────────────────────────────────

  function onChange(update: ViewUpdate): void {
    session.noteEdit(update.changes);
    assist.map(update.changes);
    unsent = unsent.compose(update.changes);
    if (!session.dirty && !applyingFill) setDirty(true);
    highlightTick++;
    clearTimeout(timer);
    timer = setTimeout(flush, DEBOUNCE_MS);
  }

  /** Send the current text (and pending mode) to the engine; single-flight. */
  async function flush(): Promise<void> {
    clearTimeout(timer);
    timer = undefined;
    if (!editor) return;
    if (inflight) {
      resend = true;
      return;
    }
    inflight = true;
    let release = () => {};
    inflightDone = new Promise((resolve) => (release = resolve));
    const doc = editor.doc;
    const mode = pendingMode;
    const sentEpoch = epoch;
    pendingMode = null;
    unsent = ChangeSet.empty(doc.length);
    try {
      const text = doc.toString();
      const snap = mode ? await ipc.setMode(mode, text) : await ipc.updateDocument(text);
      if (sentEpoch !== epoch) return; // another file was opened meanwhile
      session.applySnapshot(snap, unsent);
      session.notePreamble(text);
      highlightTick++;
      sync.schedule();
    } catch (err) {
      fail(err);
    } finally {
      inflight = false;
      inflightDone = null;
      release();
    }
    refreshDirty();
    if (resend || pendingMode) {
      resend = false;
      void flush();
    }
  }

  /** Make sure the engine has the editor's current text (before save/swap). */
  async function settleEdits(): Promise<void> {
    clearTimeout(timer);
    timer = undefined;
    for (let i = 0; i < 8; i++) {
      if (inflightDone) await inflightDone;
      else if (!unsent.empty || pendingMode) await flush();
      else return;
    }
  }

  function setDirty(dirty: boolean): void {
    session.dirty = dirty;
    ipc.setDirty(dirty).catch(fail);
  }

  /** Clean again if the user undid back to the saved text. */
  function refreshDirty(): void {
    if (!editor) return;
    const dirty = !(savedDoc && editor.doc.eq(savedDoc));
    if (dirty !== session.dirty) setDirty(dirty);
  }

  function onCursor(head: number): void {
    const main = editor?.view.state.selection.main;
    if (main && (main.from !== selection.from || main.to !== selection.to)) {
      selection = { from: main.from, to: main.to };
    }
    const id = session.index.idContaining(head);
    if (id !== session.activeId) session.activeId = id;
    sync.follow();
  }

  // CodeMirror's own panels and the tray menu follow the interface language.
  $effect(() => {
    editor?.setInterfaceLanguage(language.current);
  });
  $effect(() => {
    ipc.setTrayLanguage(language.current).catch(() => {});
  });

  // Mirror the active block back into the editor (runs after CodeMirror's update).
  $effect(() => {
    const id = session.activeId;
    void highlightTick;
    editor?.highlightRange(id === null ? null : session.index.range(id));
  });

  // ── Files ────────────────────────────────────────────────────────

  function loadView(view: SessionView): void {
    if (!editor) return;
    epoch++;
    swapWhenReady = false;
    mirrorWaiting = false;
    clearTimeout(timer);
    timer = undefined;
    resend = false;
    pendingMode = null;
    editor.setDocument(view.text, view.snapshot.mode, view.snapshot.direction);
    assist.newDocument();
    selection = { from: 0, to: 0 };
    savedDoc = view.dirty ? null : editor.doc;
    unsent = ChangeSet.empty(editor.doc.length);
    session.load(view);
    // Fills that came while this view was on its way.
    const waiting = pendingFills;
    pendingFills = [];
    if (waiting.length > 0) applyFills(waiting, false);
    if (rightPane) rightPane.scrollTop = 0;
    pdfMenu = null;
    editor.focus();
    void refreshSettings(); // the document note follows the file
    void latex.loadProject().then(() => {
      if (showPdf && compilable && latex.ready) void compile();
    });
  }

  async function refreshSettings(): Promise<void> {
    try {
      settings = await settingsIpc.get();
      skill = await assistIpc.getSkill();
    } catch (err) {
      fail(err);
    }
  }

  async function open(): Promise<void> {
    if (opening) return;
    opening = true;
    // Don't let a debounced update for the old text race the new file.
    clearTimeout(timer);
    timer = undefined;
    try {
      const view = await ipc.openFile();
      if (view) loadView(view);
      else if (!unsent.empty) void flush();
    } catch (err) {
      fail(err);
      if (!unsent.empty) void flush();
    } finally {
      opening = false;
    }
  }

  async function save(as: boolean): Promise<void> {
    if (!editor || saving) return;
    saving = true;
    // While editing Chinese, Rust composes the English from the engine's state.
    await settleEdits();
    const doc = editor.doc;
    const wasUntitled = session.path === null;
    try {
      const saved: SavedView | null = as ? await ipc.saveFileAs(doc.toString()) : await ipc.saveFile(doc.toString());
      if (!saved) return;
      savedDoc = doc;
      session.path = saved.path;
      session.name = saved.name;
      reportMirror(saved.name, saved.mirror);
      // Rust marked the file clean; re-report if the user kept typing meanwhile.
      session.dirty = !editor.doc.eq(doc);
      if (session.dirty) setDirty(true);
      if (wasUntitled && saved.suggestedMode !== session.mode) changeMode(saved.suggestedMode);
      if (saved.path.toLowerCase().endsWith(".tex")) {
        if (wasUntitled) await latex.loadProject();
        if (settings?.latex.compileOnSave && latex.ready && !compiling) void compile(latex.lang, false);
      }
    } catch (err) {
      fail(err);
    } finally {
      saving = false;
    }
  }

  /** Bilingual Markdown export (English first, whichever side is edited). */
  async function exportBilingual(): Promise<void> {
    if (!editor || exporting) return;
    exporting = true;
    await settleEdits();
    try {
      const done = await ipc.exportBilingual(editor.text());
      if (!done) return;
      session.flash(
        done.missing > 0
          ? count(done.missing, "doc.exportedMissing.one", "doc.exportedMissing.many", { name: done.name })
          : t("doc.exported", { name: done.name }),
      );
    } catch (err) {
      fail(err);
    } finally {
      exporting = false;
    }
  }

  // ── Toolbar actions ──────────────────────────────────────────────

  function changeMode(mode: Mode): void {
    if (!editor || mode === session.mode) return;
    session.mode = mode;
    editor.setMode(mode);
    requestAnimationFrame(() => editor?.view.requestMeasure());
    pendingMode = mode;
    void flush();
  }

  /**
   * Paragraphs left in the language of the translations by an early swap,
   * now translated: each replaces its text on the left, unless the user
   * changed it meanwhile. Not unsaved changes, since the file's own text
   * stays the same. `keep`: hold fills for paragraphs not known yet.
   */
  function applyFills(fills: Fill[], keep = true): void {
    if (!editor || swapping) {
      if (keep) pendingFills = [...pendingFills, ...fills].slice(-500);
      return;
    }
    const clean = !session.dirty;
    let applied = false;
    for (const fill of fills) {
      const range = session.index.range(fill.id);
      if (!range) {
        if (keep) pendingFills = [...pendingFills, fill].slice(-500);
        continue;
      }
      applyingFill = true;
      try {
        applied = editor.replaceIf(range.from, range.to, fill.old, fill.new) || applied;
      } finally {
        applyingFill = false;
      }
    }
    if (applied && clean) savedDoc = editor.doc;
  }

  /** The cursor's paragraph, by position, to find it again after a reload. */
  function cursorParagraph(): number {
    return session.activeId === null ? -1 : session.index.indexOf(session.activeId);
  }

  function restoreCursor(at: number): void {
    if (editor && at >= 0 && at < session.index.size) editor.focusAt(session.index.rangeAt(at).from, 120);
  }

  /** Swap languages: edit the translation, read the original on the right. */
  async function swapLanguages(): Promise<void> {
    if (!editor || swapping) return;
    swapping = true;
    // Keystrokes during the round trip would be lost when the new text loads.
    editor.setEditable(false);
    try {
      await settleEdits();
      // Text plainly in the other language than taken (a Chinese document
      // read as English): read it as such instead of swapping, no waiting.
      if (!session.pair && !session.swapped && !swapWhenReady) {
        const at = cursorParagraph();
        const view = await ipc.retargetLanguage(editor.text(), true);
        if (view) {
          loadView(view);
          restoreCursor(at);
          session.flash(t("doc.retargetedAuto", { lang: langName(view.home), other: langName(view.home === "zh-en" ? "en-zh" : "zh-en") }));
          return;
        }
      }
      const c = session.counts;
      const waiting = c.pending + c.translating + c.error;
      let keep = false;
      if (waiting > 0) {
        // First press: swap by itself once the rest is translated. Second
        // press: swap now, untranslated paragraphs as they are.
        if (!swapWhenReady || session.pair) {
          swapWhenReady = true;
          session.flash(t(session.pair ? "doc.swapWhenReadyPair" : "doc.swapWhenReady", { n: waiting }));
          return;
        }
        keep = true;
      }
      swapWhenReady = false;
      // Keep the cursor on the same paragraph across the swap.
      const at = cursorParagraph();
      // Skipped blocks are identical on both sides: keep expanded equations open.
      const expanded = session.expandedPositions();
      const view = await ipc.swapLanguages(editor.text(), keep);
      swapping = false;
      loadView(view);
      session.restoreExpanded(expanded);
      restoreCursor(at);
      const names = { edit: langName(view.snapshot.direction), own: langName(view.home), n: waiting };
      if (keep && session.swapped) {
        session.flash(
          session.autoTranslate
            ? count(waiting, "doc.swappedFilling.one", "doc.swappedFilling.many", names)
            : count(waiting, "doc.swappedPaused.one", "doc.swappedPaused.many", names),
        );
      } else {
        session.flash(session.swapped ? t("doc.editingOther", names) : t("doc.editingOwn", names));
      }
    } catch (err) {
      fail(err);
    } finally {
      swapping = false;
      editor?.setEditable(true);
    }
  }

  function retranslateActive(): void {
    if (session.activeId !== null) ipc.retranslateSegment(session.activeId).catch(fail);
  }

  /** Translate what is left (and, swapped, what is still in the other language). */
  async function continueTranslation(): Promise<void> {
    if (!editor) return;
    await settleEdits();
    try {
      const n = await ipc.continueTranslation(editor.text());
      session.flash(n > 0 ? count(n, "doc.continued.one", "doc.continued.many") : t("doc.nothingLeft"));
    } catch (err) {
      fail(err);
    }
  }

  /** Read the document as written in the other language. */
  async function retargetLanguage(): Promise<void> {
    if (!editor || swapping) return;
    swapping = true;
    editor.setEditable(false);
    try {
      await settleEdits();
      const at = cursorParagraph();
      const view = await ipc.retargetLanguage(editor.text(), false);
      swapping = false;
      if (!view) return;
      loadView(view);
      restoreCursor(at);
      session.flash(t("doc.retargeted", { lang: langName(view.home), other: langName(view.home === "zh-en" ? "en-zh" : "zh-en") }));
    } catch (err) {
      fail(err);
    } finally {
      swapping = false;
      editor?.setEditable(true);
    }
  }

  function toggleAuto(): void {
    const on = !session.autoTranslate;
    session.autoTranslate = on;
    ipc.setAutoTranslate(on).catch(fail);
  }

  function cycleTheme(): void {
    theme = nextTheme(theme);
    applyTheme(theme);
  }

  /** Click on a right block: cursor to the segment start, keeping the block where it is. */
  function activate(id: number, offsetY: number): void {
    const range = session.index.range(id);
    if (!editor || !range) return;
    sync.driver = "right";
    session.activeId = id;
    editor.focusAt(range.from, offsetY);
  }

  function preview(from: number, to: number): string {
    if (!editor) return "";
    const doc = editor.doc;
    const slice = doc.sliceString(Math.min(from, doc.length), Math.min(to, from + 240, doc.length));
    return slice.split("\n").find((l) => l.trim()) ?? "";
  }

  function slice(from: number, to: number): string {
    if (!editor) return "";
    const doc = editor.doc;
    return doc.sliceString(Math.min(from, doc.length), Math.min(to, doc.length));
  }

  // Translations arrived: a waiting swap goes ahead, a waiting paired file is written.
  $effect(() => {
    const c = session.counts;
    const busy = c.pending + c.translating;
    if (busy > 0) return;
    if (swapWhenReady) {
      if (c.error > 0) {
        swapWhenReady = false;
        session.flash(t("doc.swapBlocked", { n: c.error }));
      } else {
        void swapLanguages();
      }
    }
    if (mirrorWaiting) void writeMirror();
  });

  function reportMirror(name: string, mirror: MirrorSaved | null): void {
    if (!mirror) {
      session.flash(t("doc.saved", { name }));
    } else if (mirror.pending > 0) {
      mirrorWaiting = true;
      // Not everything is on disk yet: closing or opening another file asks first.
      setDirty(true);
      session.flash(t("doc.savedMirrorWaiting", { name, mirror: mirror.name, n: mirror.pending }));
    } else {
      mirrorWaiting = false;
      session.flash(
        mirror.written
          ? t("doc.savedBoth", { name, mirror: mirror.name, n: mirror.changed })
          : t("doc.saved", { name }),
      );
    }
    if (session.pair) session.pair = { ...session.pair, dirty: mirror ? !mirror.written && mirror.pending > 0 : false };
  }

  async function writeMirror(): Promise<void> {
    mirrorWaiting = false;
    try {
      const done = await pairIpc.write();
      if (done?.pending) {
        mirrorWaiting = true;
        return;
      }
      if (done?.written) session.flash(t("doc.mirrorWritten", { mirror: done.name, n: done.changed }));
      refreshDirty();
    } catch (err) {
      fail(err);
    }
  }

  /** Pair the open document with its translation in a file the user picks. */
  async function importMirror(): Promise<void> {
    if (!editor) return;
    await settleEdits();
    try {
      const view = await pairIpc.importMirror(editor.text());
      if (!view) return;
      loadView(view);
      if (view.pair) session.flash(t("pair.imported", { name: view.pair.name, paired: view.pair.paired, units: view.pair.units }));
    } catch (err) {
      fail(err);
    }
  }

  async function closeMirror(): Promise<void> {
    try {
      await pairIpc.close();
      session.pair = null;
      mirrorWaiting = false;
    } catch (err) {
      fail(err);
    }
  }

  /** Rewrite a paragraph's translation in the assistant; the paragraph follows. */
  function editTranslation(id: number): void {
    const range = session.index.range(id);
    if (!editor || !range) return;
    session.activeId = id;
    editor.focusAt(range.from, 120);
    assist.setAction("mirror");
    setAssistOpen(true);
    requestAnimationFrame(() => assistPanel?.focus());
  }

  /** At startup, if allowed: is a newer release out? Quiet when offline. */
  async function checkForUpdate(): Promise<void> {
    if (!settings?.checkUpdates) return;
    try {
      const view = await updateIpc.list();
      updateAvailable = view.releases.find((r) => r.relation === "newer" && !r.prerelease && r.asset)?.version ?? null;
    } catch {
      // No network or GitHub unavailable: try again next start.
    }
  }

  function openSettings(focus: string | null = null): void {
    settingsFocus = focus;
    showSettings = true;
  }

  // ── LaTeX: building, the PDF and SyncTeX ─────────────────────────

  /** Build a PDF. Unsaved edits are saved first: the PDF shows the files. */
  async function compile(lang: PdfLang = latex.lang, saveFirst = true): Promise<void> {
    if (!editor || compiling || !compilable) return;
    compiling = true;
    try {
      if (saveFirst && session.dirty) {
        await save(false);
        if (session.dirty) return;
      }
      await settleEdits();
      await latex.compile(lang, editor.text());
    } finally {
      compiling = false;
    }
  }

  function showPdfTab(): void {
    if (rightTab !== "pdf") setRightTab("pdf");
  }

  /** A click is being followed into another file (no second hop). */
  let reopening = false;

  /** A click on the PDF: select the source behind it. */
  async function pickInPdf(pick: PdfPick): Promise<void> {
    if (!editor) return;
    lastPick = pick;
    const near = { page: pick.page, x: pick.x, y: pick.y };
    try {
      const hit = await latexIpc.inverse(latex.lang, pick.page, pick.x, pick.y, pick.span, pick.click, editor.text());
      if (!hit) {
        pdfMenu = { ...near, here: false, paragraph: false, file: "", line: 0 };
      } else if (hit.here && hit.range) {
        editor.select(hit.range.from, hit.range.to, false);
        pdfMenu = { ...near, here: true, paragraph: hit.paragraph, file: hit.file, line: hit.line };
      } else if (hit.open && !session.dirty && !reopening) {
        // Another file of the project: open it, then find the click in it.
        reopening = true;
        try {
          const view = await latexIpc.open(hit.open);
          if (view) {
            loadView(view);
            await pickInPdf(pick);
          }
        } finally {
          reopening = false;
        }
      } else {
        pdfMenu = { ...near, here: false, paragraph: false, file: hit.open ?? hit.file, line: hit.line };
      }
    } catch (err) {
      fail(err);
    }
  }

  /** The PDF menu's actions run on the selection the click made. */
  function pdfAction(action: "polish" | "edit" | "ask"): void {
    pdfMenu = null;
    if (action === "polish") {
      void runAssist({ action: "polish", instruction: "" });
      return;
    }
    assist.setAction(action);
    setAssistOpen(true);
    requestAnimationFrame(() => assistPanel?.focus());
  }

  /** Open another file of the project and select `line` (or the clicked words on it). */
  async function openProjectFile(file: string, line = 0, pick: PdfPick | null = null): Promise<void> {
    pdfMenu = null;
    try {
      const view = await latexIpc.open(file);
      if (!view || !editor) return;
      loadView(view);
      if (line > 0) {
        const r = pick
          ? await latexIpc.locate(editor.text(), line, pick.span, pick.click)
          : lineRange(line);
        editor.select(r.from, r.to);
      }
    } catch (err) {
      fail(err);
    }
  }

  /** "Open" in the PDF menu (unsaved changes are asked about first). */
  async function openFromMenu(file: string, line: number): Promise<void> {
    pdfMenu = null;
    const pick = lastPick;
    try {
      const view = await latexIpc.open(file);
      if (!view) return;
      loadView(view);
      if (pick) await pickInPdf(pick);
      else if (line > 0 && editor) {
        const r = lineRange(line);
        editor.select(r.from, r.to);
      }
    } catch (err) {
      fail(err);
    }
  }

  /** Show the other PDF: built now if it never was. */
  function switchPdfLang(lang: PdfLang): void {
    latex.setLang(lang);
    pdfMenu = null;
    if (compilable && latex.ready && !latex.builds[lang] && !latex.building[lang]) void compile(lang);
  }

  async function exportTranslatedTex(): Promise<void> {
    if (!editor) return;
    await settleEdits();
    try {
      const path = await latexIpc.exportTex(editor.text());
      if (path) session.flash(t("pdf.texExported", { path }));
    } catch (err) {
      fail(err);
    }
  }

  function lineRange(line: number): { from: number; to: number } {
    const doc = editor!.doc;
    const l = doc.line(Math.min(Math.max(1, line), doc.lines));
    return { from: l.from, to: l.to };
  }

  async function gotoIssue(issue: IssueView): Promise<void> {
    if (!editor || issue.line === null) return;
    if (issue.here) {
      const r = await latexIpc.goto(latex.lang, issue.line, editor.text()).catch(() => null);
      const range = r ?? lineRange(issue.line);
      editor.select(range.from, range.to);
    } else if (issue.file) {
      await openProjectFile(issue.file, issue.line);
    }
  }

  /** Show the cursor's place in the PDF. */
  async function locateInPdf(): Promise<void> {
    if (!editor || !isLatex) return;
    showPdfTab();
    const lang = latex.lang;
    try {
      const head = editor.view.state.selection.main.head;
      const boxes = await latexIpc.forward(lang, head, editor.text());
      if (boxes.length === 0) {
        session.flash(t("pdf.noSource"));
        return;
      }
      pdfMarks = { lang, boxes, tick: pdfMarks.tick + 1 };
    } catch (err) {
      fail(err);
    }
  }

  function onKeydown(e: KeyboardEvent): void {
    const primary = isMac ? e.metaKey : e.ctrlKey;
    if (primary && e.altKey && e.code === "KeyJ") {
      e.preventDefault();
      if (!e.repeat) void locateInPdf();
      return;
    }
    if (!primary || e.altKey) return;
    const key = e.key.toLowerCase();
    if (key === ",") {
      e.preventDefault();
      if (!showGlossary) {
        settingsFocus = null;
        showSettings = !showSettings;
      }
    } else if (key === "s") {
      e.preventDefault();
      if (!e.repeat) void save(e.shiftKey);
    } else if (key === "o" && !e.shiftKey) {
      e.preventDefault();
      if (!e.repeat) void open();
    } else if (key === "l" && e.shiftKey) {
      e.preventDefault();
      if (!e.repeat) showLog = !showLog;
    } else if (key === "j" && !e.shiftKey) {
      e.preventDefault();
      if (!e.repeat) setAssistOpen(!showAssist);
    } else if (key === "p" && e.shiftKey) {
      e.preventDefault();
      if (!e.repeat) void runAssist({ action: "polish", instruction: "" });
    } else if (key === "b" && !e.shiftKey) {
      e.preventDefault();
      if (!e.repeat && isLatex) {
        showPdfTab();
        void compile();
      }
    } else if (key === "k" && !e.shiftKey) {
      e.preventDefault();
      assist.setAction(assist.action === "polish" ? "edit" : assist.action);
      setAssistOpen(true);
      requestAnimationFrame(() => assistPanel?.focus());
    }
  }

  // ── Writing assistant ────────────────────────────────────────────

  /** What the next assistant run acts on: the selection or the paragraph. */
  const composerTarget = $derived.by(() => {
    void highlightTick;
    if (!editor) return null;
    const doc = editor.doc;
    const { from, to } = selection;
    if (to > from) {
      return {
        label: t("assist.target.selection"),
        text: doc.sliceString(from, Math.min(to, from + 400)),
        key: null,
        translation: null,
      };
    }
    const id = session.activeId;
    const seg = id === null ? undefined : session.layout.find((s) => s.id === id);
    const range = id === null ? null : session.index.range(id);
    if (!seg || !range || seg.kind.type === "skipped") return null;
    const state = session.states.get(seg.id);
    return {
      label: t("assist.target.paragraph", { n: session.index.indexOf(seg.id) + 1 }),
      text: doc.sliceString(range.from, Math.min(range.to, range.from + 400)),
      key: `${seg.id}:${session.direction}`,
      translation: state?.status === "translated" && state.text ? state.text : null,
    };
  });

  interface RunOptions {
    action?: AssistAction;
    scope?: AssistScope;
    from?: number;
    to?: number;
    instruction?: string;
    history?: [string, string][];
    reapplies?: number;
  }

  /** Start an assistant job on the selection or the paragraph at the cursor. */
  async function runAssist(o: RunOptions = {}): Promise<void> {
    if (!editor) return;
    await settleEdits();
    const sel = editor.view.state.selection.main;
    const request: AssistRequest = {
      action: o.action ?? assist.action,
      scope: o.scope ?? assist.scope,
      text: editor.text(),
      from: o.from ?? sel.from,
      to: o.to ?? sel.to,
      instruction: o.instruction ?? assist.instruction.trim(),
      references: assist.samples.map((s) => s.text),
      images: assist.attachments.map((a) => a.id),
      history: o.history ?? [],
    };
    try {
      await assist.start(request, o.reapplies ?? null);
      if (o.instruction === undefined) assist.instruction = "";
      if (!showAssist) setAssistOpen(true);
    } catch (err) {
      fail(err);
    }
  }

  /** Apply an approved revision where its target is now. If the target text
   * moved, it is found again; if it was edited, the model re-applies. */
  async function acceptJob(job: AssistJob): Promise<void> {
    const r = job.result;
    if (!editor || !r?.revision || job.epoch !== assist.epoch) return;
    await settleEdits();
    const doc = editor.doc;
    let from = Math.min(job.from, doc.length);
    let to = Math.min(job.to, doc.length);
    let insert = r.revision;
    if (job.target.insert) {
      to = from;
      // At the start of the document or of an empty line, no blank line first.
      const lead = from === 0 || doc.sliceString(Math.max(0, from - 2), from) === "\n\n" ? "" : "\n\n";
      insert = `${lead}${r.revision.trim()}`;
    } else if (doc.sliceString(from, to) !== job.target.text) {
      const at = uniqueIndex(doc.toString(), job.target.text);
      if (at < 0) {
        job.conflict = true;
        return;
      }
      from = at;
      to = at + job.target.text.length;
    }
    if (!job.target.insert && job.target.wholeParagraph && r.translation && r.translationMatches) {
      try {
        await assistIpc.offer(r.revision, r.translation);
      } catch {
        // The paragraph is translated again instead.
      }
    }
    // New paragraphs come with their translation, paragraph for paragraph.
    if (job.action === "write" && r.translation) {
      const paragraphs = (s: string) => s.trim().split(/\n\s*\n/).map((p) => p.trim());
      const [en, zh] = [paragraphs(r.revision), paragraphs(r.translation)];
      if (en.length === zh.length) {
        await Promise.all(en.map((p, i) => assistIpc.offer(p, zh[i]).catch(() => {})));
      }
    }
    const changes = [{ from, to, insert }];
    // A figure's missing packages go into the preamble, when it is here.
    let shift = 0;
    if (job.action === "figure" && r.preambleHere && r.missingPackages.length) {
      const at = doc.toString().search(/^\\begin\{document\}/m);
      if (at >= 0 && at < from) {
        const lines = r.missingPackages.map((p) => `\\usepackage{${p}}\n`).join("");
        changes.unshift({ from: at, to: at, insert: lines });
        shift = lines.length;
      }
    }
    editor.view.dispatch({
      changes,
      selection: { anchor: from + shift, head: from + shift + insert.length },
      scrollIntoView: true,
      userEvent: "input.assist",
    });
    job.state = "applied";
    editor.focus();
    void flush();
  }

  function againJob(job: AssistJob): void {
    void runAssist({
      action: job.action,
      scope: job.scope,
      from: job.from,
      to: job.target.insert ? job.from : job.to,
      instruction: job.instruction,
      history: job.history,
    });
    assist.discard(job);
  }

  function reapplyJob(job: AssistJob): void {
    if (!job.result?.revision || job.to <= job.from) return;
    void runAssist({
      action: "edit",
      scope: job.scope,
      from: job.from,
      to: job.to,
      instruction: reapplyInstruction(job.target.text, job.result.revision),
      reapplies: job.id,
    });
    assist.discard(job);
  }

  function followUp(job: AssistJob, question: string): void {
    void runAssist({
      action: "ask",
      scope: job.scope,
      from: job.from,
      to: job.to,
      instruction: question,
      history: [...job.history, [job.instruction, job.result?.answer ?? ""]],
    });
  }

  /** Show a job's target in the editor. */
  function focusJob(job: AssistJob): void {
    if (!editor || job.epoch !== assist.epoch) return;
    const doc = editor.doc;
    const from = Math.min(job.from, doc.length);
    const to = Math.min(job.to, doc.length);
    editor.view.dispatch({ selection: { anchor: from, head: to }, scrollIntoView: true });
    editor.focus();
  }

  function drive(side: Side) {
    return () => {
      sync.driver = side;
    };
  }

  /** Wheel, scrollbar or keyboard scrolling: stop pulling the active block into view. */
  function scrollManually(side: Side) {
    return () => {
      sync.driver = side;
      sync.unfollow();
    };
  }

  onMount(() => {
    editor = new SourceEditor(editorHost, {
      onChange,
      onCursor,
      onGeometry: () => sync.schedule(),
    });
    const scroller = editor.view.scrollDOM;
    const onEditorScroll = () => sync.onScroll("left");
    scroller.addEventListener("scroll", onEditorScroll, { passive: true });
    // Window resize or splitter drag changes what each pane shows.
    const resized = new ResizeObserver(() => sync.schedule());
    resized.observe(scroller);

    let unlisten: (() => void) | undefined;
    let disposed = false;
    (async () => {
      try {
        const off = await subscribe({
          onStates: (states) => {
            session.applyStates(states);
            sync.schedule();
          },
          onUsage: (usage) => {
            session.usage = usage;
          },
          onNotice: (message) => session.flash(message),
          onRequest: (record) => requestLog.apply(record),
          onAssist: (event) => assist.onEvent(event),
          onFills: (fills) => applyFills(fills),
        });
        if (disposed) off();
        else unlisten = off;
        loadView(await ipc.getSession());
        requestLog.load(await logIpc.get());
        await latex.loadStatus();
        if (showPdf && compilable && latex.ready && !latex.builds[latex.lang]) void compile();
        void checkForUpdate();
      } catch (err) {
        fail(err);
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
      clearTimeout(timer);
      scroller.removeEventListener("scroll", onEditorScroll);
      resized.disconnect();
      editor?.destroy();
      editor = null;
    };
  });
</script>

<svelte:window onkeydown={onKeydown} />

<div class="app">
  <Toolbar
    {session}
    {theme}
    project={latex.project}
    onimportmirror={importMirror}
    onclosemirror={closeMirror}
    swapWaiting={swapWhenReady}
    onnew={() => (showTemplates = true)}
    onfile={(file) => openProjectFile(file)}
    onopen={open}
    onsave={() => save(false)}
    onsaveas={() => save(true)}
    onhelp={() => ipc.openManual(language.current).catch(fail)}
    onexport={exportBilingual}
    onmode={changeMode}
    onretranslate={retranslateActive}
    onretranslateall={() => ipc.retranslateAll().catch(fail)}
    oncontinue={continueTranslation}
    onretarget={retargetLanguage}
    onswap={swapLanguages}
    onglossary={() => {
      showSettings = false;
      showGlossary = true;
    }}
    onsettings={() => openSettings()}
    onlog={() => {
      showLog = true;
    }}
    inflight={requestLog.inFlight}
    assistOpen={showAssist}
    assistBusy={assist.running}
    assistReady={assist.ready}
    onassistant={() => setAssistOpen(!showAssist)}
    ontoggleauto={toggleAuto}
    ontheme={cycleTheme}
  />

  <div class="work">
  <main class="panes" bind:this={panes} style:--split={ratio}>
    <section
      class="source"
      data-mode={session.mode}
      data-lang={session.direction === "zh-en" ? "zh" : "en"}
      aria-label={t("pane.source")}
      onpointerenter={drive("left")}
      onwheelcapture={scrollManually("left")}
      onkeydowncapture={drive("left")}
      onpointerdowncapture={scrollManually("left")}
    >
      <div class="editor-host" bind:this={editorHost}></div>
    </section>

    <Splitter {ratio} container={panes} onchange={setRatio} />

    <div class="target" class:tabbed={isLatex}>
      {#if isLatex}
        <div class="tabs" role="tablist" aria-label={t("pane.pdf")}>
          <button
            class="tab smallcaps"
            class:on={!showPdf}
            role="tab"
            aria-selected={!showPdf}
            onclick={() => setRightTab("translation")}>{t("pane.translation")}</button
          >
          <button class="tab smallcaps" class:on={showPdf} role="tab" aria-selected={showPdf} onclick={() => setRightTab("pdf")}
            >{t("pane.pdf")}{#if latex.building.en || latex.building.zh}<span class="busy" aria-hidden="true"></span>{/if}</button
          >
        </div>
      {/if}
      <div
        class="right"
        class:hidden={showPdf}
        role="presentation"
        onpointerenter={drive("right")}
        onwheelcapture={scrollManually("right")}
        onkeydowncapture={scrollManually("right")}
      >
        <TranslationPane
          {session}
          {blocks}
          {preview}
          {slice}
          bind:pane={rightPane}
          onactivate={activate}
          onretry={(id) => ipc.retranslateSegment(id).catch(fail)}
          onedit={editTranslation}
          onscroll={() => sync.onScroll("right")}
          onresize={() => sync.schedule()}
        />
      </div>
      {#if showPdf}
        <div class="right">
          <PdfPane
            store={latex}
            marks={pdfMarks}
            menu={pdfMenu}
            {compilable}
            oncompile={(lang) => compile(lang)}
            onpick={pickInPdf}
            onlocate={locateInPdf}
            onissue={gotoIssue}
            onaction={pdfAction}
            onopenfile={(file, line) => openFromMenu(file, line)}
            onlang={switchPdfLang}
            fileLang={session.home === "zh-en" ? "zh" : "en"}
            onexporttex={exportTranslatedTex}
            onclosemenu={() => (pdfMenu = null)}
          />
        </div>
      {/if}
    </div>
  </main>
  {#if showAssist}
    <div class="dock-host">
      <AssistPanel
        bind:this={assistPanel}
        store={assist}
        target={composerTarget}
        ready={settings?.assistantReady ?? false}
        model={settings?.assistantLabel ?? ""}
        skill={skill ? `${skill.name} ${skill.version}` : "research-builder"}
        onrun={() => runAssist()}
        onaccept={acceptJob}
        onagain={againJob}
        onreapply={reapplyJob}
        onfollowup={followUp}
        onfocus={focusJob}
        onclose={() => setAssistOpen(false)}
      />
    </div>
  {/if}
  </div>

  <StatusBar
    {session}
    provider={settings?.activeLabel ?? ""}
    update={updateAvailable}
    onupdate={() => openSettings("updates")}
    ondismisserror={dismissError}
  />
</div>

{#if showSettings && settings}
  <SettingsPanel
    {settings}
    {latex}
    dirty={session.dirty}
    focus={settingsFocus}
    onchange={(view) => {
      settings = view;
    }}
    onskill={(next) => {
      skill = next;
    }}
    onclose={() => {
      showSettings = false;
      editor?.focus();
    }}
    onglossary={() => {
      showSettings = false;
      showGlossary = true;
    }}
  />
{/if}

{#if showGlossary}
  <GlossaryPanel
    onclose={() => {
      showGlossary = false;
      editor?.focus();
    }}
  />
{/if}

{#if showTemplates}
  <TemplatePanel
    canExport={!!latex.project}
    onopened={(view) => {
      showTemplates = false;
      loadView(view);
      if (latex.ready) setRightTab("pdf");
    }}
    onclose={() => {
      showTemplates = false;
      editor?.focus();
    }}
  />
{/if}

{#if showLog}
  <LogPanel
    log={requestLog}
    onclose={() => {
      showLog = false;
      editor?.focus();
    }}
  />
{/if}

<style>
  .app {
    display: grid;
    grid-template-rows: auto 1fr auto;
    height: 100%;
    animation: settle 420ms var(--ease) both;
  }
  .work {
    display: flex;
    min-height: 0;
  }
  .panes {
    flex: 1;
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .dock-host {
    flex: none;
    width: clamp(320px, 30vw, 430px);
    min-height: 0;
    overflow: hidden;
  }
  .source {
    flex: none;
    width: calc(var(--split) * 100%);
    min-width: 0;
    background: var(--paper);
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
  }
  .source[data-mode="latex"] {
    --font-source: var(--font-mono);
    --source-size: 14px;
    --source-leading: 1.75;
  }
  .source[data-lang="zh"] {
    --font-source: var(--font-zh-edit);
    --source-leading: 1.85;
  }
  .source[data-lang="zh"][data-mode="latex"] {
    --font-source: var(--font-mono-zh);
  }
  .editor-host {
    height: 100%;
  }
  .target {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    grid-template-columns: minmax(0, 1fr);
    box-shadow: inset 8px 0 14px -12px rgba(0, 0, 0, 0.18);
  }
  .target.tabbed {
    grid-template-rows: auto minmax(0, 1fr);
  }
  .right {
    min-height: 0;
    min-width: 0;
  }
  .right.hidden {
    display: none;
  }
  .tabs {
    display: flex;
    gap: 2px;
    padding: 4px 10px 0;
    background: var(--chrome);
    border-bottom: 1px solid var(--rule);
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: 5px 5px 0 0;
    background: transparent;
    padding: 3px 12px 5px;
    font-size: 13.5px;
    color: var(--muted);
    cursor: pointer;
    margin-bottom: -1px;
  }
  .tab:hover {
    color: var(--ink);
  }
  .tab.on {
    background: var(--paper-2);
    border-color: var(--rule);
    color: var(--ink);
  }
  .tab .busy {
    width: 8px;
    height: 8px;
    border: 1.5px solid var(--accent-soft);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 800ms linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @keyframes settle {
    from {
      opacity: 0;
    }
  }
</style>
