// Editor chrome and syntax colours. Everything references CSS custom
// properties from styles.css, so light/dark switching needs no reconfiguration.

import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";

export const editorTheme = EditorView.theme({
  "&": {
    height: "100%",
    color: "var(--ink)",
    backgroundColor: "transparent",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: "var(--font-source)",
    fontSize: "var(--source-size)",
    lineHeight: "var(--source-leading)",
    overflow: "auto",
  },
  ".cm-content": {
    padding: "36px 0 45vh",
    caretColor: "var(--accent)",
  },
  ".cm-line": { padding: "0 clamp(20px, 5%, 56px)" },
  ".cm-cursor, .cm-dropCursor": { borderLeft: "2px solid var(--accent)" },
  ".cm-activeLine": { backgroundColor: "var(--active-line)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection":
    { backgroundColor: "var(--selection) !important" },
  ".cm-activeSegment": {
    boxShadow: "inset 3px 0 0 var(--seal)",
  },
  ".cm-placeholder": { color: "var(--faint)", fontStyle: "italic" },
  ".cm-searchMatch": { backgroundColor: "var(--search-match)", outline: "1px solid var(--search-outline)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "var(--search-selected)" },
  ".cm-panels": {
    backgroundColor: "var(--chrome)",
    color: "var(--ink)",
    borderColor: "var(--rule)",
    fontFamily: "var(--font-ui)",
  },
  ".cm-panels input, .cm-panels button": { fontFamily: "var(--font-ui)", fontSize: "13px" },
  ".cm-matchingBracket": { backgroundColor: "var(--accent-wash)", outline: "1px solid var(--accent-soft)" },
  ".cm-specialChar": { color: "var(--seal)" },
});

const highlight = HighlightStyle.define([
  { tag: [t.heading, t.heading1, t.heading2], color: "var(--hl-heading)", fontWeight: "700" },
  { tag: [t.heading3, t.heading4, t.heading5, t.heading6], color: "var(--hl-heading)", fontWeight: "600" },
  { tag: t.strong, fontWeight: "700" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: [t.link, t.url], color: "var(--hl-link)", textDecoration: "underline" },
  { tag: [t.monospace, t.string], color: "var(--hl-code)" },
  { tag: [t.keyword, t.tagName, t.macroName], color: "var(--hl-command)" },
  { tag: [t.atom, t.bool, t.special(t.variableName), t.variableName], color: "var(--hl-arg)" },
  { tag: [t.number, t.literal], color: "var(--hl-number)" },
  { tag: [t.comment, t.lineComment, t.blockComment], color: "var(--hl-comment)", fontStyle: "italic" },
  { tag: [t.meta, t.processingInstruction, t.contentSeparator], color: "var(--hl-meta)" },
  { tag: [t.bracket, t.brace, t.squareBracket, t.paren], color: "var(--hl-bracket)" },
  { tag: t.quote, color: "var(--muted)", fontStyle: "italic" },
  { tag: t.list, color: "var(--hl-meta)" },
]);

export const editorHighlighting = syntaxHighlighting(highlight);
