<script lang="ts">
  import { ChangeSet, type Text } from "@codemirror/state";
  import type { ViewUpdate } from "@codemirror/view";
  import { onMount } from "svelte";
  import { type AssistJob, AssistStore, reapplyInstruction, uniqueIndex } from "./lib/assist.svelte";
  import AssistPanel from "./lib/components/AssistPanel.svelte";
  import GlossaryPanel from "./lib/components/GlossaryPanel.svelte";
  import LogPanel from "./lib/components/LogPanel.svelte";
  import SettingsPanel from "./lib/components/SettingsPanel.svelte";
  import Splitter from "./lib/components/Splitter.svelte";
  import StatusBar from "./lib/components/StatusBar.svelte";
  import Toolbar from "./lib/components/Toolbar.svelte";
  import TranslationPane from "./lib/components/TranslationPane.svelte";
  import { SourceEditor } from "./lib/editor/editor";
  import { t } from "./lib/i18n.svelte";
  import { assistIpc, errorMessage, ipc, logIpc, settingsIpc, subscribe } from "./lib/ipc";
  import { RequestLogStore } from "./lib/requestLog.svelte";
  import { ScrollSync, type Side } from "./lib/scrollSync";
  import { Session } from "./lib/session.svelte";
  import { applyTheme, loadTheme, nextTheme, type ThemePref } from "./lib/theme";
  import type {
    AssistAction,
    AssistRequest,
    AssistScope,
    Mode,
    SavedView,
    SessionView,
    SettingsView,
    SkillInfo,
  } from "./lib/types";

  /** Idle time after the last keystroke before the engine re-segments. */
  const DEBOUNCE_MS = 800;
  const SPLIT_KEY = "biwrite.split";
  const ASSIST_KEY = "biwrite.assist.open";
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
  let showGlossary = $state(false);
  let showLog = $state(false);
  let showAssist = $state(loadFlag(ASSIST_KEY));
  let skill = $state<SkillInfo | null>(null);
  /** Editor selection, for the assistant's target preview. */
  let selection = $state({ from: 0, to: 0 });
  let assistPanel = $state<{ focus(): void }>();
  const assist = new AssistStore();

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
  let opening = false;
  let swapping = false;
  let exporting = false;
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
    if (!session.dirty) setDirty(true);
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
    if (rightPane) rightPane.scrollTop = 0;
    editor.focus();
    void refreshSettings(); // the document note follows the file
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
      session.flash(`Saved ${saved.name}`);
      // Rust marked the file clean; re-report if the user kept typing meanwhile.
      session.dirty = !editor.doc.eq(doc);
      if (session.dirty) setDirty(true);
      if (wasUntitled && saved.suggestedMode !== session.mode) changeMode(saved.suggestedMode);
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
          ? `Exported ${done.name} — ${done.missing} paragraph${done.missing === 1 ? " is" : "s are"} not translated yet.`
          : `Exported ${done.name}`,
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

  /** Swap languages: edit the translation, read the original on the right. */
  async function swapLanguages(): Promise<void> {
    if (!editor || swapping) return;
    swapping = true;
    // Keystrokes during the round trip would be lost when the new text loads.
    editor.setEditable(false);
    try {
      await settleEdits();
      const c = session.counts;
      const waiting = c.pending + c.translating + c.error;
      if (waiting > 0) {
        session.flash(`Swapping needs every paragraph translated — ${waiting} not ready yet.`);
        return;
      }
      // Keep the cursor on the same paragraph across the swap.
      const at = session.activeId === null ? -1 : session.index.indexOf(session.activeId);
      // Skipped blocks are identical on both sides: keep expanded equations open.
      const expanded = session.expandedPositions();
      const view = await ipc.swapLanguages(editor.text());
      loadView(view);
      session.restoreExpanded(expanded);
      if (at >= 0 && at < session.index.size) editor.focusAt(session.index.rangeAt(at).from, 120);
      session.flash(
        view.snapshot.direction === "zh-en"
          ? "Editing Chinese — the English on the right is what gets saved."
          : "Editing English again.",
      );
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

  function onKeydown(e: KeyboardEvent): void {
    if (!(isMac ? e.metaKey : e.ctrlKey) || e.altKey) return;
    const key = e.key.toLowerCase();
    if (key === ",") {
      e.preventDefault();
      if (!showGlossary) showSettings = !showSettings;
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
      return { label: t("assist.target.selection"), text: doc.sliceString(from, Math.min(to, from + 400)) };
    }
    const id = session.activeId;
    const seg = id === null ? undefined : session.layout.find((s) => s.id === id);
    const range = id === null ? null : session.index.range(id);
    if (!seg || !range || seg.kind.type === "skipped") return null;
    return {
      label: t("assist.target.paragraph", { n: session.index.indexOf(seg.id) + 1 }),
      text: doc.sliceString(range.from, Math.min(range.to, range.from + 400)),
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
      references: [...assist.samples],
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
      insert = `\n\n${r.revision.trim()}`;
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
    editor.view.dispatch({
      changes: { from, to, insert },
      selection: { anchor: from, head: from + insert.length },
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
        });
        if (disposed) off();
        else unlisten = off;
        loadView(await ipc.getSession());
        requestLog.load(await logIpc.get());
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
    onopen={open}
    onsave={() => save(false)}
    onexport={exportBilingual}
    onmode={changeMode}
    onretranslate={retranslateActive}
    onretranslateall={() => ipc.retranslateAll().catch(fail)}
    onswap={swapLanguages}
    onglossary={() => {
      showSettings = false;
      showGlossary = true;
    }}
    onsettings={() => {
      showSettings = true;
    }}
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
      aria-label="English source"
      onpointerenter={drive("left")}
      onwheelcapture={scrollManually("left")}
      onkeydowncapture={drive("left")}
      onpointerdowncapture={scrollManually("left")}
    >
      <div class="editor-host" bind:this={editorHost}></div>
    </section>

    <Splitter {ratio} container={panes} onchange={setRatio} />

    <div
      class="target"
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
        onscroll={() => sync.onScroll("right")}
        onresize={() => sync.schedule()}
      />
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

  <StatusBar {session} provider={settings?.activeLabel ?? ""} ondismisserror={dismissError} />
</div>

{#if showSettings && settings}
  <SettingsPanel
    {settings}
    onchange={(view) => {
      settings = view;
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
    box-shadow: inset 8px 0 14px -12px rgba(0, 0, 0, 0.18);
  }
  @keyframes settle {
    from {
      opacity: 0;
    }
  }
</style>
