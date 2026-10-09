// Run with `npm test` (Node's built-in runner with TypeScript type stripping).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { escapeHtml, parseMacros, preambleOf, renderToHtml, splitMath, type MathPart } from "./math.ts";

/** Tags in `html` that could run script or navigate. */
const unsafeTags = (html: string) =>
  (html.match(/<[^>]*>/g) ?? []).filter((tag) => /^<\/?(script|img|a|iframe)\b|\son\w+=|href=|src=|javascript:/i.test(tag));

const parts = (text: string, mode: "plain" | "markdown" | "latex" = "latex") =>
  splitMath(text, mode).map((p: MathPart) => [p.kind, p.value]);

test("latex: inline and display forms", () => {
  assert.deepEqual(parts("图 $\\G = (V, E)$ 上"), [
    ["text", "图 "],
    ["inline", "\\G = (V, E)"],
    ["text", " 上"],
  ]);
  assert.deepEqual(parts("a \\(x\\) b \\[y\\] c $$z$$"), [
    ["text", "a "],
    ["inline", "x"],
    ["text", " b "],
    ["display", "y"],
    ["text", " c "],
    ["display", "z"],
  ]);
  assert.deepEqual(parts("$a$$b$"), [
    ["inline", "a"],
    ["inline", "b"],
  ]);
});

test("latex: escapes, comments and unmatched delimiters are text", () => {
  assert.deepEqual(parts("costs \\$5 and \\$6"), [["text", "costs \\$5 and \\$6"]]);
  assert.deepEqual(parts("$a \\$ b$"), [["inline", "a \\$ b"]]);
  assert.deepEqual(parts("50\\% done % $not math$\nnext $x$"), [
    ["text", "50\\% done % $not math$\nnext "],
    ["inline", "x"],
  ]);
  // A streamed partial: the closing `$` hasn't arrived yet.
  assert.deepEqual(parts("其中 $\\mathcal{N}(v"), [["text", "其中 $\\mathcal{N}(v"]]);
  assert.deepEqual(parts("其中 $$x"), [["text", "其中 $$x"]]);
  assert.deepEqual(parts("open \\(x"), [["text", "open \\(x"]]);
  // Math never spans a blank line.
  assert.deepEqual(parts("$a\n\nb$"), [["text", "$a\n\nb$"]]);
  assert.deepEqual(parts("$$"), [["text", "$$"]]);
});

test("latex: CJK around math", () => {
  assert.deepEqual(parts("温度$\\tau$固定为$0.1$。"), [
    ["text", "温度"],
    ["inline", "\\tau"],
    ["text", "固定为"],
    ["inline", "0.1"],
    ["text", "。"],
  ]);
});

test("latex: math environments in text", () => {
  const [, env] = splitMath("计算\n\\begin{equation}\n  a = b \\label{eq:x}\n\\end{equation}\n其中", "latex");
  assert.equal(env.kind, "display");
  assert.equal(env.value, "\\begin{equation*}\n  a = b \\label{eq:x}\n\\end{equation*}");
  assert.equal(env.raw, "\\begin{equation}\n  a = b \\label{eq:x}\n\\end{equation}");
  assert.deepEqual(parts("\\begin{itemize}x\\end{itemize}"), [["text", "\\begin{itemize}x\\end{itemize}"]]);
  assert.deepEqual(parts("\\begin{constructor}x\\end{constructor}"), [
    ["text", "\\begin{constructor}x\\end{constructor}"],
  ]);
  assert.deepEqual(parts("\\begin{displaymath}x\\end{displaymath}"), [["display", "x"]]);
});

test("markdown: dollars, code spans and Pandoc rules", () => {
  assert.deepEqual(parts("see $x^2$ and $$\\sum_i i$$", "markdown"), [
    ["text", "see "],
    ["inline", "x^2"],
    ["text", " and "],
    ["display", "\\sum_i i"],
  ]);
  // Dollar amounts are not math.
  assert.deepEqual(parts("costs $5 and $10 total", "markdown"), [["text", "costs $5 and $10 total"]]);
  assert.deepEqual(parts("a $ b $ c", "markdown"), [["text", "a $ b $ c"]]);
  assert.deepEqual(parts("code `$x$` here", "markdown"), [["text", "code `$x$` here"]]);
  assert.deepEqual(parts("``a ` $b$`` $c$", "markdown"), [
    ["text", "``a ` $b$`` "],
    ["inline", "c"],
  ]);
  assert.deepEqual(parts("\\$x$", "markdown"), [["text", "\\$x$"]]);
  // LaTeX-only forms stay text in Markdown.
  assert.deepEqual(parts("\\(x\\)", "markdown"), [["text", "\\(x\\)"]]);
});

test("plain: no math at all", () => {
  assert.deepEqual(parts("costs $5 and $x$", "plain"), [["text", "costs $5 and $x$"]]);
  assert.deepEqual(splitMath("", "latex"), []);
});

test("macros from the sample paper", () => {
  const paper = readFileSync(new URL("../../samples/paper.tex", import.meta.url), "utf8");
  const macros = parseMacros(preambleOf(paper));
  assert.equal(macros["\\G"], "\\mathcal{G}");
  assert.equal(macros["\\E"], "\\mathbb{E}");
  assert.equal(preambleOf("no document here"), "");
});

test("macro forms", () => {
  const macros = parseMacros(
    [
      "\\newcommand{\\R}{\\mathbb{R}}",
      "\\renewcommand\\vec[1]{\\mathbf{#1}}",
      "\\newcommand*{\\norm}[1]{\\lVert #1 \\rVert}",
      "\\providecommand{\\R}{\\mathrm{R}}",
      "\\newcommand{\\opt}[2][x]{#1^{#2}}",
      "\\def\\half{\\frac{1}{2}}",
      "\\def\\pair#1#2{(#1, #2)}",
      "\\definecolor{red}{rgb}{1,0,0}",
      "\\DeclareMathOperator{\\Tr}{Tr}",
      "\\DeclareMathOperator*{\\argmax}{arg\\,max}",
      "% \\newcommand{\\gone}{x}",
      "\\newcommand{\\nested}{\\left\\{ {a} \\right\\}}",
    ].join("\n"),
  );
  assert.deepEqual(macros, {
    "\\R": "\\mathbb{R}",
    "\\vec": "\\mathbf{#1}",
    "\\norm": "\\lVert #1 \\rVert",
    "\\opt": "x^{#1}",
    "\\half": "\\frac{1}{2}",
    "\\pair": "(#1, #2)",
    "\\Tr": "\\operatorname{Tr}",
    "\\argmax": "\\operatorname*{arg\\,max}",
    "\\nested": "\\left\\{ {a} \\right\\}",
  });
});

test("rendering: prose is escaped and inert", () => {
  const html = renderToHtml("<script>alert(1)</script> & $x$ <img src=x onerror=alert(1)>", "latex");
  assert.ok(html.startsWith("&lt;script&gt;alert(1)&lt;/script&gt; &amp; "));
  assert.deepEqual(unsafeTags(html), []);
  assert.ok(html.includes('class="katex"'));
  assert.equal(escapeHtml(`a"b'c`), "a&quot;b&#39;c");
  assert.equal(renderToHtml("<b>$5</b>", "plain"), "&lt;b&gt;$5&lt;/b&gt;");
});

test("rendering: untrusted commands stay inert", () => {
  const html = renderToHtml("$\\href{javascript:alert(1)}{x} \\url{javascript:y} \\htmlData{onclick=z}{w}$", "latex");
  assert.deepEqual(unsafeTags(html), []);
  assert.ok(html.includes("\\href"));
});

test("rendering: macros, labels and display math", () => {
  const html = renderToHtml("在 $\\G$ 上\n\\begin{equation}\n  \\E[x] \\label{eq:a}\n\\end{equation}\n其中", "latex", {
    "\\G": "\\mathcal{G}",
    "\\E": "\\mathbb{E}",
  });
  assert.ok(html.includes("katex-display"));
  assert.ok(!html.includes("math-error"), html);
  assert.ok(!html.includes("eq:a"));
  // Line breaks next to the display block are dropped.
  assert.ok(html.endsWith("其中") && !html.endsWith("\n其中"));
});

test("rendering: KaTeX errors fall back to the escaped source", () => {
  const html = renderToHtml("bad $\\notacommand{<b>}$ end", "latex");
  assert.match(html, /<code class="math-error" title="[^"<>]*">\$\\notacommand\{&lt;b&gt;\}\$<\/code>/);
  assert.ok(html.endsWith(" end"));
});

test("rendering: \\gdef in one expression does not leak into the macros", () => {
  const macros = {};
  renderToHtml("$\\gdef\\leak{1}\\leak$", "latex", macros);
  assert.deepEqual(macros, {});
  assert.ok(renderToHtml("$\\leak$", "latex", macros).includes("math-error"));
});

test("latex: verb, url and href arguments are not math", () => {
  const kinds = (text: string) => splitMath(text, "latex").map((p: MathPart) => p.kind);
  assert.deepEqual(kinds(String.raw`\verb|$x$| and $y$`), ["text", "inline"]);
  assert.deepEqual(kinds(String.raw`\url{a$b} $y$`), ["text", "inline"]);
  assert.deepEqual(kinds(String.raw`\href{x$y}{link $z$}`), ["text", "inline", "text"]);
  // An unclosed \verb is ordinary text.
  assert.deepEqual(kinds(String.raw`\verb|open $q$`), ["text", "inline"]);
});

test("rendering: LaTeX prose escapes show as characters", () => {
  assert.equal(renderToHtml(String.raw`提高了 5\% \& 更多 \\ 行`, "latex"), String.raw`提高了 5% &amp; 更多 \\ 行`);
  assert.equal(renderToHtml(String.raw`5\%`, "markdown"), String.raw`5\%`);
});
