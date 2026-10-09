// Marks the lines of the segment that the right pane is highlighting.

import { StateEffect, StateField, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";

export const setActiveRange = StateEffect.define<{ from: number; to: number } | null>();

const lineMark = Decoration.line({ class: "cm-activeSegment" });

const activeSegmentField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const effect of tr.effects) {
      if (!effect.is(setActiveRange)) continue;
      const range = effect.value;
      if (!range) {
        deco = Decoration.none;
        continue;
      }
      const doc = tr.state.doc;
      const to = Math.min(range.to, doc.length);
      const marks = [];
      for (let pos = Math.min(range.from, to); pos <= to; ) {
        const line = doc.lineAt(pos);
        marks.push(lineMark.range(line.from));
        pos = line.to + 1;
      }
      deco = Decoration.set(marks);
    }
    return deco;
  },
  provide: (field) => EditorView.decorations.from(field),
});

export function activeSegment(): Extension {
  return activeSegmentField;
}
