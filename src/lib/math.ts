// Math in the right pane: split translated text into prose and TeX math,
// collect the document's macros from the LaTeX preamble, and render with
// KaTeX. Pure (no Svelte), so it runs under `node --test` as well.
//
// Safety: the result goes to `{@html}`. Prose is always HTML-escaped, and
// math goes only through KaTeX with `trust: false`, which renders \href,
// \url, \html* and \includegraphics as inert text.

import katex from "katex";
import type { Mode } from "./types";

export type MathKind = "text" | "inline" | "display";

export interface MathPart {
  kind: MathKind;
  /** Prose, or the TeX to render (without delimiters). */
  value: string;
  /** The source of a math part including its delimiters, for error fallback. */
  raw?: string;
}

/** KaTeX `macros` option: `\name` → expansion (`#1`… for arguments). */
export type Macros = Readonly<Record<string, string>>;

/** Display environments KaTeX renders as they are, mapped to their unnumbered
 * form (each block is rendered on its own, so numbers would all be "(1)"). */
const ENVS = new Map([
  ["equation", "equation*"],
  ["equation*", "equation*"],
  ["align", "align*"],
  ["align*", "align*"],
  ["flalign", "align*"],
  ["flalign*", "align*"],
  ["gather", "gather*"],
  ["gather*", "gather*"],
  ["multline", "gather*"],
  ["multline*", "gather*"],
  ["alignat", "alignat*"],
  ["alignat*", "alignat*"],
]);
/** Environments KaTeX lacks, rendered from their body. */
const BODY_ENVS = new Set(["displaymath", "math", "eqnarray", "eqnarray*"]);

/** Macros KaTeX lacks that papers commonly use inside math. */
const BASE_MACROS: Macros = { "\\ensuremath": "#1", "\\xspace": "" };

const isSpace = (c: string | undefined) => c !== undefined && /\s/.test(c);
const isDigit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";

/** Position of the next unescaped `close` at or after `from`, or -1. Math
 * may not run across a blank line (TeX would end the paragraph there). */
function findClose(text: string, from: number, close: string): number {
  for (let j = from; j < text.length; j++) {
    if (text.startsWith(close, j)) return j;
    const c = text[j];
    if (c === "\\") j++;
    else if (c === "\n" && /^\n[ \t]*\n/.test(text.slice(j, j + 64))) return -1;
  }
  return -1;
}

/** Closing `$` of Markdown inline math (Pandoc rules: no space just inside
 * the delimiters, closing `$` not followed by a digit), or -1. */
function findMarkdownDollar(text: string, from: number): number {
  if (isSpace(text[from]) || from >= text.length) return -1;
  for (let j = from; j < text.length; j++) {
    const c = text[j];
    if (c === "\\") j++;
    else if (c === "\n" && /^\n[ \t]*\n/.test(text.slice(j, j + 64))) return -1;
    else if (c === "$" && j > from && !isSpace(text[j - 1]) && !isDigit(text[j + 1])) return j;
  }
  return -1;
}

/** `\begin{env}` at `i`: the matching `\end{env}`, if env is a math one. */
function mathEnv(text: string, i: number): (MathPart & { end: number }) | null {
  const m = /^\\begin\{([a-zA-Z]+\*?)\}/.exec(text.slice(i, i + 40));
  if (!m) return null;
  const env = m[1];
  const katexEnv = ENVS.get(env);
  if (!katexEnv && !BODY_ENVS.has(env)) return null;
  const endTag = `\\end{${env}}`;
  const bodyStart = i + m[0].length;
  const close = text.indexOf(endTag, bodyStart);
  if (close < 0) return null;
  const end = close + endTag.length;
  const raw = text.slice(i, end);
  const body = text.slice(bodyStart, close);
  let value: string;
  if (katexEnv) {
    // alignat's column count stays at the start of the body.
    value = `\\begin{${katexEnv}}${body}\\end{${katexEnv}}`;
  } else if (env.startsWith("eqnarray")) {
    value = `\\begin{array}{rcl}${body}\\end{array}`;
  } else {
    value = body;
  }
  return { kind: env === "math" ? "inline" : "display", value, raw, end };
}

/** End of `\verb|…|`, `\url{…}` or `\href{…}` at `i`, whose argument is
 * literal (a `$` in it is not math), or -1. */
function literalArgEnd(text: string, i: number): number {
  const m = /^\\(verb\*?|url|href)(?![a-zA-Z])/.exec(text.slice(i, i + 8));
  if (!m) return -1;
  const j = i + m[0].length;
  if (m[1].startsWith("verb")) {
    const delim = text[j];
    if (!delim || /[a-zA-Z\s]/.test(delim)) return -1;
    const close = text.indexOf(delim, j + 1);
    const nl = text.indexOf("\n", j + 1);
    return close < 0 || (nl >= 0 && nl < close) ? -1 : close + 1;
  }
  const arg = group(text, j);
  return arg ? arg[1] : -1;
}

function splitLatex(text: string): MathPart[] {
  const parts: MathPart[] = [];
  let textStart = 0;
  const push = (start: number, part: MathPart, end: number) => {
    if (start > textStart) parts.push({ kind: "text", value: text.slice(textStart, start) });
    parts.push(part);
    textStart = end;
  };
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "\\") {
      const next = text[i + 1];
      if (next === "(" || next === "[") {
        const end = findClose(text, i + 2, next === "(" ? "\\)" : "\\]");
        if (end >= 0) {
          const kind = next === "(" ? "inline" : "display";
          push(i, { kind, value: text.slice(i + 2, end), raw: text.slice(i, end + 2) }, end + 2);
          i = end + 2;
          continue;
        }
      } else if (next === "b") {
        const env = mathEnv(text, i);
        if (env) {
          const { end, ...part } = env;
          push(i, part, end);
          i = end;
          continue;
        }
      } else if (next === "v" || next === "u" || next === "h") {
        const end = literalArgEnd(text, i);
        if (end > 0) {
          i = end;
          continue;
        }
      }
      i += 2; // an escaped character: \$, \%, \\, \{ …
    } else if (c === "%") {
      // A comment runs to the end of the line; a `$` in it is not math.
      const nl = text.indexOf("\n", i);
      i = nl < 0 ? text.length : nl;
    } else if (c === "$") {
      const display = text[i + 1] === "$";
      const open = display ? 2 : 1;
      const end = findClose(text, i + open, display ? "$$" : "$");
      if (end > i + open) {
        const kind = display ? "display" : "inline";
        push(i, { kind, value: text.slice(i + open, end), raw: text.slice(i, end + open) }, end + open);
        i = end + open;
      } else {
        i += open; // unmatched (e.g. still streaming): plain text
      }
    } else {
      i++;
    }
  }
  if (textStart < text.length) parts.push({ kind: "text", value: text.slice(textStart) });
  return parts;
}

function splitMarkdown(text: string): MathPart[] {
  const parts: MathPart[] = [];
  let textStart = 0;
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if (c === "\\") {
      i += 2;
    } else if (c === "`") {
      // A code span is literal: skip to the closing run of the same length.
      let n = 1;
      while (text[i + n] === "`") n++;
      const fence = "`".repeat(n);
      let close = text.indexOf(fence, i + n);
      while (close >= 0 && text[close + n] === "`") close = text.indexOf(fence, close + n + 1);
      i = close < 0 ? i + n : close + n;
    } else if (c === "$") {
      const display = text[i + 1] === "$";
      const open = display ? 2 : 1;
      const end = display ? findClose(text, i + 2, "$$") : findMarkdownDollar(text, i + 1);
      if (end > i + open) {
        if (i > textStart) parts.push({ kind: "text", value: text.slice(textStart, i) });
        parts.push({
          kind: display ? "display" : "inline",
          value: text.slice(i + open, end),
          raw: text.slice(i, end + open),
        });
        i = textStart = end + open;
      } else {
        i += open;
      }
    } else {
      i++;
    }
  }
  if (textStart < text.length) parts.push({ kind: "text", value: text.slice(textStart) });
  return parts;
}

/** Split `text` into prose and math. Plain text has no math (dollar
 * amounts); an unmatched delimiter is prose. */
export function splitMath(text: string, mode: Mode): MathPart[] {
  if (!text) return [];
  switch (mode) {
    case "latex":
      return splitLatex(text);
    case "markdown":
      return splitMarkdown(text);
    default:
      return [{ kind: "text", value: text }];
  }
}

// ── Macros ─────────────────────────────────────────────────────────

/** `{…}` starting at `i` (after optional spaces): its content and the index
 * after the closing brace. */
function group(s: string, i: number): [string, number] | null {
  while (isSpace(s[i])) i++;
  if (s[i] !== "{") return null;
  let depth = 0;
  for (let j = i; j < s.length; j++) {
    const c = s[j];
    if (c === "\\") j++;
    else if (c === "{") depth++;
    else if (c === "}" && --depth === 0) return [s.slice(i + 1, j), j + 1];
  }
  return null;
}

/** `[…]` starting at `i` (after optional spaces). */
function optional(s: string, i: number): [string, number] | null {
  while (isSpace(s[i])) i++;
  if (s[i] !== "[") return null;
  const close = s.indexOf("]", i);
  return close < 0 ? null : [s.slice(i + 1, close), close + 1];
}

/** A command name, braced (`{\X}`) or bare (`\X`), at `i`. */
function commandName(s: string, i: number): [string, number] | null {
  const braced = group(s, i);
  if (braced) {
    const name = braced[0].trim();
    return /^\\([a-zA-Z]+|.)$/.test(name) ? [name, braced[1]] : null;
  }
  while (isSpace(s[i])) i++;
  const m = /^\\([a-zA-Z]+|[^a-zA-Z\s])/.exec(s.slice(i, i + 64));
  return m ? [m[0], i + m[0].length] : null;
}

/** Remove `%` comments (unescaped `%` to end of line). */
function stripComments(s: string): string {
  return s.replace(/(^|[^\\])((?:\\\\)*)%.*$/gm, "$1$2");
}

/**
 * Macros defined in a LaTeX preamble: `\newcommand`, `\renewcommand`,
 * `\providecommand` (with `[n]` arguments; an optional first argument takes
 * its default), `\def\X{…}` and `\DeclareMathOperator`.
 */
export function parseMacros(preamble: string): Macros {
  const s = stripComments(preamble);
  const macros: Record<string, string> = {};
  const re = /\\(newcommand|renewcommand|providecommand|def|DeclareMathOperator)(\*?)/g;
  for (let m = re.exec(s); m; m = re.exec(s)) {
    const [, command, star] = m;
    // `\define` is not `\def`.
    if (/[a-zA-Z]/.test(s[re.lastIndex] ?? "")) continue;
    const name = commandName(s, re.lastIndex);
    if (!name) continue;
    let i = name[1];
    if (command === "def") {
      const params = /^(?:#\d)*/.exec(s.slice(i))?.[0] ?? "";
      const body = group(s, i + params.length);
      if (!body) continue;
      macros[name[0]] = body[0];
      re.lastIndex = body[1];
      continue;
    }
    if (command === "DeclareMathOperator") {
      const body = group(s, i);
      if (!body) continue;
      macros[name[0]] = `\\operatorname${star}{${body[0]}}`;
      re.lastIndex = body[1];
      continue;
    }
    const count = optional(s, i);
    if (count) i = count[1];
    const fallback = count ? optional(s, i) : null;
    if (fallback) i = fallback[1];
    const body = group(s, i);
    if (!body) continue;
    if (command === "providecommand" && name[0] in macros) continue;
    let value = body[0];
    if (fallback) {
      // KaTeX has no optional arguments: use the default, shift the rest.
      value = value.replace(/#(\d)/g, (_, d: string) => (d === "1" ? fallback[0] : `#${Number(d) - 1}`));
    }
    macros[name[0]] = value;
    re.lastIndex = body[1];
  }
  return macros;
}

/** The preamble of a LaTeX document: everything before `\begin{document}`. */
export function preambleOf(text: string): string {
  const end = text.indexOf("\\begin{document}");
  return end < 0 ? "" : text.slice(0, end);
}

// ── Rendering ──────────────────────────────────────────────────────

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/** Drop `\label{…}` (KaTeX has no cross-references). */
function stripLabels(tex: string): string {
  let out = "";
  let i = 0;
  for (let at = tex.indexOf("\\label", i); at >= 0; at = tex.indexOf("\\label", i)) {
    const arg = group(tex, at + 6);
    if (!arg) {
      out += tex.slice(i, at + 6);
      i = at + 6;
      continue;
    }
    out += tex.slice(i, at);
    i = arg[1];
  }
  return out + tex.slice(i);
}

const CACHE_LIMIT = 2000;
const cache = new Map<string, string>();
const macroIds = new WeakMap<Macros, number>();
let nextMacroId = 1;

function macrosKey(macros: Macros): number {
  let id = macroIds.get(macros);
  if (id === undefined) {
    id = nextMacroId++;
    macroIds.set(macros, id);
  }
  return id;
}

function renderMath(part: MathPart, macros: Macros): string {
  const display = part.kind === "display";
  const key = `${display ? "D" : "I"}${macrosKey(macros)}\u0000${part.value}`;
  const hit = cache.get(key);
  if (hit !== undefined) {
    // Refresh recency.
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }
  let html: string;
  try {
    html = katex.renderToString(stripLabels(part.value), {
      displayMode: display,
      // A copy: \gdef inside an expression would otherwise leak into it.
      macros: { ...BASE_MACROS, ...macros },
      throwOnError: true,
      trust: false,
      strict: "ignore",
      maxSize: 50,
      maxExpand: 1000,
      output: "htmlAndMathml",
    });
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    const tag = display ? "div" : "code";
    html = `<${tag} class="math-error" title="${escapeHtml(message)}">${escapeHtml(part.raw ?? part.value)}</${tag}>`;
  }
  cache.set(key, html);
  if (cache.size > CACHE_LIMIT) cache.delete(cache.keys().next().value as string);
  return html;
}

/** LaTeX prose escapes shown as their character (`\%` → `%`); `\\` stays. */
function unescapeLatexText(s: string): string {
  return s.replace(/\\(\\|[%&#_$])/g, (m, c: string) => (c === "\\" ? m : c));
}

/**
 * HTML for `text` with its math typeset. Prose is escaped; line breaks next
 * to display math are dropped (the display block breaks the line itself).
 * In LaTeX, escaped characters in prose (`\%`, `\&`, `\$`, …) are shown
 * plainly.
 */
export function renderToHtml(text: string, mode: Mode, macros: Macros = {}): string {
  const parts = splitMath(text, mode);
  let out = "";
  parts.forEach((part, i) => {
    if (part.kind !== "text") {
      out += renderMath(part, macros);
      return;
    }
    let value = part.value;
    if (parts[i - 1]?.kind === "display") value = value.replace(/^[ \t]*\n/, "");
    if (parts[i + 1]?.kind === "display") value = value.replace(/\n[ \t]*$/, "");
    if (mode === "latex") value = unescapeLatexText(value);
    out += escapeHtml(value);
  });
  return out;
}

/** Clear the render cache (tests). */
export function clearMathCache(): void {
  cache.clear();
}
