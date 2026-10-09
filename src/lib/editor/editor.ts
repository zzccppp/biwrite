// CodeMirror 6 setup for the English source pane.

import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { markdown } from "@codemirror/lang-markdown";
import { bracketMatching, StreamLanguage } from "@codemirror/language";
import { stex } from "@codemirror/legacy-modes/mode/stex";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { Compartment, EditorState, type Extension, type Text, Transaction } from "@codemirror/state";
import {
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightSpecialChars,
  keymap,
  placeholder,
  type ViewUpdate,
} from "@codemirror/view";
import type { Direction, Mode } from "../types";
import { activeSegment, setActiveRange } from "./activeSegment";
import { editorHighlighting, editorTheme } from "./theme";

export interface EditorHandlers {
  /** Document changed by the user (not by `setDocument`). */
  onChange(update: ViewUpdate): void;
  /** Main cursor moved to `head`. */
  onCursor(head: number): void;
  /** Content height changed (e.g. text reflowed). */
  onGeometry(): void;
}

const latex = StreamLanguage.define(stex);

function languageFor(mode: Mode): Extension {
  switch (mode) {
    case "markdown":
      return markdown();
    case "latex":
      return latex;
    default:
      return [];
  }
}

/** Chinese for CodeMirror's own words (search panel, go to line). */
const ZH_PHRASES: Record<string, string> = {
  Find: "查找",
  Replace: "替换",
  next: "下一个",
  previous: "上一个",
  all: "全部",
  "match case": "区分大小写",
  regexp: "正则表达式",
  "by word": "全词匹配",
  replace: "替换",
  "replace all": "全部替换",
  close: "关闭",
  "current match": "当前匹配",
  "replaced $ matches": "已替换 $ 处",
  "replaced match on line $": "已替换第 $ 行的匹配",
  "on line": "所在行",
  "Go to line": "跳转到行",
  go: "跳转",
  "Control character": "控制字符",
};

export class SourceEditor {
  readonly view: EditorView;
  private readonly language = new Compartment();
  private readonly editable = new Compartment();
  private readonly phrases = new Compartment();
  private uiLanguage: "en" | "zh" = "en";
  private readonly handlers: EditorHandlers;

  constructor(parent: HTMLElement, handlers: EditorHandlers) {
    this.handlers = handlers;
    this.view = new EditorView({ parent, state: this.createState("", "plain", "en-zh") });
  }

  private createState(text: string, mode: Mode, direction: Direction): EditorState {
    const zh = direction === "zh-en";
    return EditorState.create({
      doc: text,
      extensions: [
        history(),
        drawSelection(),
        dropCursor(),
        highlightSpecialChars(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        bracketMatching(),
        EditorView.lineWrapping,
        EditorState.allowMultipleSelections.of(true),
        keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
        placeholder(zh ? "用中文写作，右侧实时显示英文…" : "Write in English…"),
        this.language.of(languageFor(mode)),
        this.editable.of(EditorView.editable.of(true)),
        this.phrases.of(this.phrasesFor(this.uiLanguage)),
        editorHighlighting,
        editorTheme,
        activeSegment(),
        EditorView.contentAttributes.of({
          spellcheck: zh ? "false" : "true",
          autocorrect: "off",
          lang: zh ? "zh-CN" : "en",
        }),
        EditorView.updateListener.of((u) => this.onUpdate(u)),
      ],
    });
  }

  private onUpdate(update: ViewUpdate): void {
    if (update.docChanged) this.handlers.onChange(update);
    if (update.selectionSet || update.docChanged) {
      this.handlers.onCursor(update.state.selection.main.head);
    }
    if (update.geometryChanged) this.handlers.onGeometry();
  }

  /** Replace the whole document (open file, swap). Clears undo history. */
  setDocument(text: string, mode: Mode, direction: Direction): void {
    this.view.setState(this.createState(text, mode, direction));
  }

  private phrasesFor(lang: "en" | "zh"): Extension {
    return lang === "zh" ? EditorState.phrases.of(ZH_PHRASES) : [];
  }

  /** The interface language, for CodeMirror's own panels. */
  setInterfaceLanguage(lang: "en" | "zh"): void {
    if (lang === this.uiLanguage) return;
    this.uiLanguage = lang;
    this.view.dispatch({ effects: this.phrases.reconfigure(this.phrasesFor(lang)) });
  }

  /** Lock the editor (e.g. while the panes are being swapped). */
  setEditable(on: boolean): void {
    this.view.dispatch({ effects: this.editable.reconfigure(EditorView.editable.of(on)) });
  }

  setMode(mode: Mode): void {
    this.view.dispatch({ effects: this.language.reconfigure(languageFor(mode)) });
  }

  get doc(): Text {
    return this.view.state.doc;
  }

  text(): string {
    return this.view.state.doc.toString();
  }

  highlightRange(range: { from: number; to: number } | null): void {
    this.view.dispatch({ effects: setActiveRange.of(range) });
  }

  /** Put the cursor at `pos`, scrolled so it sits `offsetY` px below the top. */
  focusAt(pos: number, offsetY: number): void {
    const pos2 = Math.min(pos, this.view.state.doc.length);
    this.view.dispatch({
      selection: { anchor: pos2 },
      effects: EditorView.scrollIntoView(pos2, { y: "start", yMargin: Math.max(0, offsetY) }),
    });
    this.view.focus();
  }

  /**
   * Replace `from`..`to` with `text` if it still reads `old` (a paragraph
   * translated in the background). Not an undo step: undo passes over it.
   * Returns whether it was replaced.
   */
  replaceIf(from: number, to: number, old: string, text: string): boolean {
    const doc = this.view.state.doc;
    if (from < 0 || to > doc.length || from > to || doc.sliceString(from, to) !== old) return false;
    this.view.dispatch({
      changes: { from, to, insert: text },
      annotations: [Transaction.addToHistory.of(false), Transaction.userEvent.of("input.fill")],
    });
    return true;
  }

  /** Select `from`..`to` and scroll it to the middle of the view. */
  select(from: number, to: number, focus = true): void {
    const length = this.view.state.doc.length;
    const a = Math.min(Math.max(0, from), length);
    const b = Math.min(Math.max(a, to), length);
    this.view.dispatch({
      selection: { anchor: a, head: b },
      effects: EditorView.scrollIntoView(a, { y: "center" }),
    });
    if (focus) this.view.focus();
  }

  focus(): void {
    this.view.focus();
  }

  destroy(): void {
    this.view.destroy();
  }
}
