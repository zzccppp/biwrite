// Text formatting in the right pane: the common Markdown and LaTeX markup a
// translation keeps (bold, italic, code, links, lists, quotes; \textbf,
// \emph, citations, references, quotes, dashes). Deliberately small: anything
// else stays visible as escaped source. Pure, so it runs under `node --test`.
//
// Safety: the result goes to `{@html}`. All text is HTML-escaped and the only
// tags produced are the fixed ones below. Links are never clickable: an
// `<a href>` would navigate the app's own window away from the editor.

import type { Mode } from "./types";

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/** Math stand-ins (`n`, see `renderToHtml`) never go into attributes. */
function attr(s: string): string {
  return escapeHtml(s.replace(/\d+/g, ""));
}

function link(title: string, inner: string): string {
  return `<span class="fmt-link" title="${attr(title)}">${inner}</span>`;
}

/** HTML for prose in `mode` (math already replaced by stand-ins). */
export function formatProse(text: string, mode: Mode): string {
  switch (mode) {
    case "markdown":
      return markdownBlock(text);
    case "latex":
      return latexInline(text);
    default:
      return escapeHtml(text);
  }
}

// ── Markdown ───────────────────────────────────────────────────────

/** Lines: list items and quotes become block lines; the rest is inline text. */
function markdownBlock(text: string): string {
  let out = "";
  let prevBlock = true;
  for (const line of text.split("\n")) {
    const item = /^(\s*)(?:([-*+])|(\d{1,9})[.)])\s+(.*)$/.exec(line);
    const quote = item ? null : /^\s*>\s?(.*)$/.exec(line);
    let html: string;
    let block = true;
    if (item) {
      const depth = Math.min(3, Math.floor(item[1].replace(/\t/g, "  ").length / 2));
      const marker = item[3] ? `${item[3]}.` : "•";
      html = `<span class="fmt-li fmt-d${depth}"><span class="fmt-marker">${marker}</span>${markdownInline(item[4])}</span>`;
    } else if (quote) {
      html = `<span class="fmt-quote">${markdownInline(quote[1])}</span>`;
    } else {
      html = markdownInline(line);
      block = false;
    }
    // Block lines end their own line; a newline next to one would add a blank line.
    if (out && !block && !prevBlock) out += "\n";
    out += html;
    prevBlock = block;
  }
  return out;
}

const isPunct = (c: string | undefined) => c !== undefined && /^[!-/:-@[-`{-~]$/.test(c);
const isSpace = (c: string | undefined) => c === undefined || /\s/.test(c);
const isWord = (c: string | undefined) => c !== undefined && /^[\p{L}\p{N}]$/u.test(c);

const EMPHASIS: Record<string, [string, string]> = {
  "**": ["<strong>", "</strong>"],
  __: ["<strong>", "</strong>"],
  "*": ["<em>", "</em>"],
  _: ["<em>", "</em>"],
  "~~": ["<del>", "</del>"],
};

/** Closing delimiter for emphasis opened at `from`, or -1. */
function closeEmphasis(s: string, from: number, delim: string): number {
  for (let j = s.indexOf(delim, from); j >= 0; j = s.indexOf(delim, j + 1)) {
    if (j === from || isSpace(s[j - 1]) || s[j - 1] === "\\") continue;
    // A single `*` doesn't close on part of a `**` run, and `_` doesn't
    // close inside a word.
    if (delim.length === 1 && (s[j + 1] === delim || s[j - 1] === delim)) {
      while (s[j + 1] === delim) j++;
      continue;
    }
    if (delim[0] === "_" && isWord(s[j + delim.length])) continue;
    // Close on the last delimiter of a run: `***both***`.
    while (s[j + delim.length] === delim[0]) j++;
    return j;
  }
  return -1;
}

/** Inline Markdown: code spans, links, images, autolinks, emphasis, escapes. */
function markdownInline(s: string): string {
  let out = "";
  let text = 0;
  const flush = (to: number) => {
    out += escapeHtml(s.slice(text, to));
  };
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === "\\" && isPunct(s[i + 1])) {
      flush(i);
      out += escapeHtml(s[i + 1]);
      i += 2;
      text = i;
      continue;
    }
    if (c === "`") {
      let n = 1;
      while (s[i + n] === "`") n++;
      const fence = "`".repeat(n);
      let close = s.indexOf(fence, i + n);
      while (close >= 0 && s[close + n] === "`") close = s.indexOf(fence, close + n + 1);
      if (close > i + n - 1) {
        flush(i);
        out += `<code>${escapeHtml(s.slice(i + n, close))}</code>`;
        i = text = close + n;
        continue;
      }
      i += n;
      continue;
    }
    if (c === "[" || (c === "!" && s[i + 1] === "[")) {
      // The URL may contain one level of parentheses (Wikipedia links).
      const m = /^(!?)\[([^\]\n]+)\]\(((?:[^()\s]|\([^()\s]*\))+)(?:\s+"[^"\n]*")?\)/.exec(s.slice(i));
      if (m) {
        flush(i);
        const label = m[1] ? `<span class="fmt-image">${escapeHtml(m[2])}</span>` : markdownInline(m[2]);
        out += link(m[3], label);
        i = text = i + m[0].length;
        continue;
      }
    }
    if (c === "<") {
      const m = /^<([a-zA-Z][a-zA-Z0-9+.-]{1,31}:[^\s<>]+)>/.exec(s.slice(i));
      if (m) {
        flush(i);
        out += link(m[1], escapeHtml(m[1]));
        i = text = i + m[0].length;
        continue;
      }
    }
    if (c === "*" || c === "_" || (c === "~" && s[i + 1] === "~")) {
      const delim = c === "~" ? "~~" : s[i + 1] === c ? c + c : c;
      const opens = !isSpace(s[i + delim.length]) && !(c === "_" && isWord(s[i - 1]));
      const close = opens ? closeEmphasis(s, i + delim.length, delim) : -1;
      if (close > i + delim.length) {
        const [open, end] = EMPHASIS[delim];
        flush(i);
        out += open + markdownInline(s.slice(i + delim.length, close)) + end;
        i = text = close + delim.length;
        continue;
      }
      i += delim.length;
      continue;
    }
    i++;
  }
  flush(s.length);
  return out;
}

// ── LaTeX ──────────────────────────────────────────────────────────

const STYLE_COMMANDS: Record<string, [string, string]> = {
  textbf: ["<strong>", "</strong>"],
  textit: ["<em>", "</em>"],
  emph: ["<em>", "</em>"],
  textsl: ["<em>", "</em>"],
  underline: ["<u>", "</u>"],
  textsc: ['<span class="fmt-sc">', "</span>"],
  footnote: ['<span class="fmt-note">', "</span>"],
};

const CITE = /^(?:cite[pt]?|citealp|citealt|citeauthor|citeyear|citeyearpar|citenum|[Cc]ite[pt]?|[Pp]arencite|[Tt]extcite|[Aa]utocite|footcite|smartcite|supercite)$/;
const REF = /^(?:ref|autoref|Autoref|cref|Cref|pageref|cpageref|Cpageref|nameref|vref|Vref)$/;
const SYMBOLS: Record<string, string> = { ldots: "…", dots: "…", LaTeX: "LaTeX", TeX: "TeX", textendash: "–", textemdash: "—" };

/** `{…}` at `i` (after spaces): its content and the index after it. */
function braced(s: string, i: number): [string, number] | null {
  while (s[i] === " ") i++;
  if (s[i] !== "{") return null;
  let depth = 0;
  for (let j = i; j < s.length; j++) {
    if (s[j] === "\\") j++;
    else if (s[j] === "{") depth++;
    else if (s[j] === "}" && --depth === 0) return [s.slice(i + 1, j), j + 1];
  }
  return null;
}

/** Skip `*` and up to two `[…]` arguments. */
function skipOptional(s: string, i: number): number {
  if (s[i] === "*") i++;
  for (let n = 0; n < 2; n++) {
    let j = i;
    while (s[j] === " ") j++;
    if (s[j] !== "[") break;
    const close = s.indexOf("]", j);
    if (close < 0) break;
    i = close + 1;
  }
  return i;
}

/** One known command at `i` (a backslash): its HTML and end, or null. */
function latexCommand(s: string, i: number): [string, number] | null {
  const name = /^[a-zA-Z]+/.exec(s.slice(i + 1))?.[0];
  if (!name) return null;
  let j = i + 1 + name.length;
  if (name in SYMBOLS) {
    // `\ldots{}` is the same as `\ldots`.
    return [SYMBOLS[name], s[j] === "{" && s[j + 1] === "}" ? j + 2 : j];
  }
  const style = STYLE_COMMANDS[name];
  if (style) {
    const arg = braced(s, j);
    return arg ? [style[0] + latexInline(arg[0]) + style[1], arg[1]] : null;
  }
  if (name === "texttt") {
    const arg = braced(s, j);
    return arg ? [`<code>${unescapeLatex(arg[0])}</code>`, arg[1]] : null;
  }
  if (CITE.test(name) || name === "nocite") {
    const arg = braced(s, skipOptional(s, j));
    if (!arg) return null;
    const keys = arg[0]
      .split(",")
      .map((k) => k.trim())
      .filter(Boolean);
    return [name === "nocite" ? "" : `<span class="fmt-cite">[${escapeHtml(keys.join(", "))}]</span>`, arg[1]];
  }
  if (REF.test(name) || name === "eqref") {
    const arg = braced(s, skipOptional(s, j));
    if (!arg) return null;
    const label = escapeHtml(arg[0].trim());
    return [`<span class="fmt-ref">${name === "eqref" ? `(${label})` : label}</span>`, arg[1]];
  }
  if (name === "label") {
    const arg = braced(s, j);
    return arg ? ["", arg[1]] : null;
  }
  if (name === "url") {
    const arg = braced(s, j);
    return arg ? [link(arg[0], escapeHtml(arg[0])), arg[1]] : null;
  }
  if (name === "href") {
    const url = braced(s, j);
    const label = url && braced(s, url[1]);
    return url && label ? [link(url[0], latexInline(label[0])), label[1]] : null;
  }
  return null;
}

/** Escaped characters (`\%`, `\&`, …) as themselves, everything HTML-escaped. */
function unescapeLatex(s: string): string {
  return escapeHtml(s.replace(/\\([%&#_${}])/g, "$1"));
}

/** Inline LaTeX text: style commands, citations, references, links, quotes,
 * dashes, `~`, escapes. Comments are hidden and bare braces dropped, as in
 * the PDF; unknown commands stay visible. */
function latexInline(s: string): string {
  let out = "";
  let text = 0;
  const flush = (to: number) => {
    out += escapeHtml(s.slice(text, to));
  };
  const put = (html: string, end: number, at: number) => {
    flush(at);
    out += html;
    text = end;
    return end;
  };
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === "\\") {
      const next = s[i + 1];
      if (next !== undefined && "%&#_${}".includes(next)) {
        i = put(escapeHtml(next), i + 2, i);
      } else if (next === "\\") {
        // `\\` at the end of a line: one line break, not two.
        const eol = /^[ \t]*\n/.exec(s.slice(i + 2, i + 40));
        i = put("\n", i + 2 + (eol ? eol[0].length : 0), i);
      } else if (next === " ") {
        i = put(" ", i + 2, i);
      } else {
        const command = latexCommand(s, i);
        if (command) {
          i = put(command[0], command[1], i);
        } else {
          // Unknown: shown as written, with its first argument. A brace
          // that isn't closed yet (streaming) stays visible too.
          const name = /^[a-zA-Z]*/.exec(s.slice(i + 1))?.[0] ?? "";
          const end = i + 1 + Math.max(name.length, 1);
          const arg = name ? braced(s, end) : null;
          i = arg ? arg[1] : name && s[end] === "{" ? end + 1 : end;
        }
      }
    } else if (c === "%") {
      const nl = s.indexOf("\n", i);
      i = put("", nl < 0 ? s.length : nl, i);
    } else if (c === "{" || c === "}") {
      i = put("", i + 1, i);
    } else if (c === "~") {
      i = put(" ", i + 1, i);
    } else if (c === "`" && s[i + 1] === "`") {
      i = put("“", i + 2, i);
    } else if (c === "'" && s[i + 1] === "'") {
      i = put("”", i + 2, i);
    } else if (c === "-" && s[i + 1] === "-") {
      const em = s[i + 2] === "-";
      i = put(em ? "—" : "–", i + (em ? 3 : 2), i);
    } else {
      i++;
    }
  }
  flush(s.length);
  return out;
}
