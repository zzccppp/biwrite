// The files a LaTeX document reads with \input, \include or \subfile, as
// links: a small arrow after each name that opens that file of the project,
// as does a click on the name with ⌘ (Ctrl elsewhere).

import { RangeSetBuilder, type Extension } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from "@codemirror/view";

export interface IncludeLinks {
  /** The project file (relative to its folder) that `name` reads, or null. */
  resolve(name: string): string | null;
  open(file: string): void;
  /** The tooltip of the link to `file`. */
  title(file: string): string;
}

const INCLUDE = /\\(?:input|include|subfile)\s*\{([^{}\n]+)\}/g;

/** `line` up to its first `%` that starts a comment. */
function uncommented(line: string): string {
  for (let i = 0; i < line.length; i++) {
    if (line[i] === "\\") i++;
    else if (line[i] === "%") return line.slice(0, i);
  }
  return line;
}

interface Found {
  /** Where the name is in the document. */
  from: number;
  to: number;
  /** Just after the closing brace. */
  end: number;
  file: string;
}

/** The includes of the lines of `from`..`to` that name a project file. */
function found(view: EditorView, links: IncludeLinks, from: number, to: number): Found[] {
  const doc = view.state.doc;
  const out: Found[] = [];
  for (let pos = from; pos <= to; ) {
    const line = doc.lineAt(pos);
    const code = uncommented(line.text);
    for (const m of code.matchAll(INCLUDE)) {
      const file = links.resolve(m[1]);
      if (!file) continue;
      const start = line.from + (m.index ?? 0) + m[0].indexOf("{") + 1;
      out.push({ from: start, to: start + m[1].length, end: line.from + (m.index ?? 0) + m[0].length, file });
    }
    pos = line.to + 1;
  }
  return out;
}

class OpenWidget extends WidgetType {
  constructor(
    readonly file: string,
    readonly links: IncludeLinks,
  ) {
    super();
  }

  override eq(other: OpenWidget): boolean {
    return other.file === this.file && other.links === this.links;
  }

  toDOM(): HTMLElement {
    const el = document.createElement("span");
    el.className = "cm-includeOpen";
    el.textContent = "↗";
    el.title = this.links.title(this.file);
    el.setAttribute("role", "link");
    el.setAttribute("aria-label", this.links.title(this.file));
    el.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      this.links.open(this.file);
    });
    return el;
  }

  override ignoreEvent(): boolean {
    return true;
  }
}

const nameMark = Decoration.mark({ class: "cm-includeName" });

function decorations(view: EditorView, links: IncludeLinks): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>();
  for (const { from, to } of view.visibleRanges) {
    for (const f of found(view, links, from, to)) {
      builder.add(f.from, f.to, nameMark);
      builder.add(f.end, f.end, Decoration.widget({ widget: new OpenWidget(f.file, links), side: 1 }));
    }
  }
  return builder.finish();
}

/** Links for the includes of a LaTeX document (see [`IncludeLinks`]). */
export function includeLinks(links: IncludeLinks): Extension {
  const plugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(view: EditorView) {
        this.decorations = decorations(view, links);
      }
      update(u: ViewUpdate): void {
        if (u.docChanged || u.viewportChanged) this.decorations = decorations(u.view, links);
      }
    },
    { decorations: (v) => v.decorations },
  );
  const click = EditorView.domEventHandlers({
    mousedown(e, view) {
      if (!(e.metaKey || e.ctrlKey) || e.button !== 0) return false;
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
      if (pos === null) return false;
      const line = view.state.doc.lineAt(pos);
      const hit = found(view, links, line.from, line.from).find((f) => f.from <= pos && pos <= f.to);
      if (!hit) return false;
      e.preventDefault();
      links.open(hit.file);
      return true;
    },
  });
  return [plugin, click];
}
