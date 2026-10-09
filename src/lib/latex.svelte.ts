// LaTeX state of the open document: the TeX toolchain, the project (root
// and files), and the last build of each PDF (English, Chinese mirror).

import { errorMessage, latexIpc } from "./ipc";
import type { BuildView, PdfLang, ProjectView, TexStatus } from "./types";

const LANG_KEY = "biwrite.pdf.lang";

function loadLang(): PdfLang {
  try {
    return localStorage.getItem(LANG_KEY) === "zh" ? "zh" : "en";
  } catch {
    return "en";
  }
}

export class LatexStore {
  status = $state<TexStatus | null>(null);
  project = $state<ProjectView | null>(null);
  builds = $state<Record<PdfLang, BuildView | null>>({ en: null, zh: null });
  building = $state<Record<PdfLang, boolean>>({ en: false, zh: false });
  /** Why a build could not run (no TeX, unsaved document), per language. */
  failure = $state<Record<PdfLang, string | null>>({ en: null, zh: null });
  /** The PDF on show. */
  lang = $state<PdfLang>(loadLang());
  /** Builds started per language; a finished build only reports if it is the latest. */
  #started: Record<PdfLang, number> = { en: 0, zh: 0 };

  get ready(): boolean {
    return !!this.status?.found;
  }

  setLang(lang: PdfLang): void {
    this.lang = lang;
    try {
      localStorage.setItem(LANG_KEY, lang);
    } catch {
      // Not persisted; harmless.
    }
  }

  async loadStatus(refresh = false): Promise<void> {
    try {
      this.status = await latexIpc.status(refresh);
    } catch (err) {
      this.failure.en = errorMessage(err);
    }
  }

  /** The open document changed: its project, and builds of another project dropped. */
  async loadProject(): Promise<void> {
    let next: ProjectView | null = null;
    try {
      next = await latexIpc.project();
    } catch {
      next = null;
    }
    const before = this.project;
    this.project = next;
    if (!next || !before || before.folder !== next.folder || before.root !== next.root) {
      this.builds = { en: null, zh: null };
      this.failure = { en: null, zh: null };
    }
  }

  /** Build one PDF from the editor's `text`. `null` when it was replaced by a newer build or failed to run. */
  async compile(lang: PdfLang, text: string): Promise<BuildView | null> {
    const n = ++this.#started[lang];
    this.building[lang] = true;
    this.failure[lang] = null;
    try {
      const build = await latexIpc.compile(lang, text);
      if (n === this.#started[lang]) this.builds[lang] = build;
      return build;
    } catch (err) {
      const message = errorMessage(err);
      if (message !== "cancelled" && n === this.#started[lang]) this.failure[lang] = message;
      return null;
    } finally {
      if (n === this.#started[lang]) this.building[lang] = false;
    }
  }

  cancel(lang: PdfLang): void {
    this.#started[lang]++;
    this.building[lang] = false;
    latexIpc.cancel(lang).catch(() => {});
  }
}
