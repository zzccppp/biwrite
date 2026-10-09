// Run with `npm test` (Node's built-in runner with TypeScript type stripping).

import assert from "node:assert/strict";
import { test } from "node:test";
import { formatProse } from "./format.ts";
import { renderToHtml } from "./math.ts";

const md = (s: string) => formatProse(s, "markdown");
const tex = (s: string) => formatProse(s, "latex");

/** Every tag in `html` must be one the formatter (or KaTeX) produces. */
const tagNames = (html: string) => [...html.matchAll(/<\/?([a-z]+)/g)].map((m) => m[1]);

test("markdown: emphasis, strikethrough and code", () => {
  assert.equal(
    md("这是**加粗**、*斜体*、__粗__、_斜_、~~删除~~和`a*b*c`。"),
    "这是<strong>加粗</strong>、<em>斜体</em>、<strong>粗</strong>、<em>斜</em>、<del>删除</del>和<code>a*b*c</code>。",
  );
  assert.equal(md("**粗体里有*斜体*。**"), "<strong>粗体里有<em>斜体</em>。</strong>");
  assert.equal(md("``a ` b``"), "<code>a ` b</code>");
});

test("markdown: emphasis runs and nesting", () => {
  assert.equal(md("***both***"), "<strong><em>both</em></strong>");
  assert.equal(md("*a **b** c*"), "<em>a <strong>b</strong> c</em>");
  assert.equal(md("**a *b* c**"), "<strong>a <em>b</em> c</strong>");
});

test("markdown: what is not emphasis stays as written", () => {
  assert.equal(md("2 * 3 * 4 and a ** b"), "2 * 3 * 4 and a ** b");
  assert.equal(md("snake_case_name"), "snake_case_name");
  assert.equal(md(String.raw`\*literal\* and **open`), "*literal* and **open");
  assert.equal(md("`unclosed code"), "`unclosed code");
});

test("markdown: links and images are labelled, never clickable", () => {
  assert.equal(
    md('见[文档](https://x.org/a "标题")和<https://y.org>。'),
    '见<span class="fmt-link" title="https://x.org/a">文档</span>和<span class="fmt-link" title="https://y.org">https://y.org</span>。',
  );
  assert.equal(
    md("![示意图](fig.png)"),
    '<span class="fmt-link" title="fig.png"><span class="fmt-image">示意图</span></span>',
  );
  assert.ok(!md("[x](javascript:alert(1))").includes("<a"));
  assert.equal(
    md("[T](https://en.wikipedia.org/wiki/Transformer_(deep_learning))。"),
    '<span class="fmt-link" title="https://en.wikipedia.org/wiki/Transformer_(deep_learning)">T</span>。',
  );
});

test("markdown: list items and quotes become block lines", () => {
  assert.equal(
    md("要点：\n- 第一\n  * 第二 **重要**\n3. 第三\n> 引用\n结尾"),
    '要点：<span class="fmt-li fmt-d0"><span class="fmt-marker">•</span>第一</span>' +
      '<span class="fmt-li fmt-d1"><span class="fmt-marker">•</span>第二 <strong>重要</strong></span>' +
      '<span class="fmt-li fmt-d0"><span class="fmt-marker">3.</span>第三</span>' +
      '<span class="fmt-quote">引用</span>结尾',
  );
  assert.equal(md("一行\n两行"), "一行\n两行");
});

test("latex: style commands, nested", () => {
  assert.equal(
    tex(String.raw`\textbf{加粗}、\emph{强调 \textbf{内层}}、\texttt{a_b}、\underline{下}、\textsc{Sc}`),
    '<strong>加粗</strong>、<em>强调 <strong>内层</strong></em>、<code>a_b</code>、<u>下</u>、<span class="fmt-sc">Sc</span>',
  );
  assert.equal(tex(String.raw`正文\footnote{脚注内容}。`), '正文<span class="fmt-note">脚注内容</span>。');
});

test("latex: citations, references and labels", () => {
  assert.equal(
    tex(String.raw`见\cite[p.~3]{kipf2017, velickovic2018}和\citep{a}，第~\ref{sec:intro}节，式~\eqref{eq:mp}\label{x}。`),
    '见<span class="fmt-cite">[kipf2017, velickovic2018]</span>和<span class="fmt-cite">[a]</span>，第 ' +
      '<span class="fmt-ref">sec:intro</span>节，式 <span class="fmt-ref">(eq:mp)</span>。',
  );
  assert.equal(
    tex(String.raw`\url{https://x.org/a_b} 与 \href{https://y.org}{\emph{主页}}`),
    '<span class="fmt-link" title="https://x.org/a_b">https://x.org/a_b</span> 与 ' +
      '<span class="fmt-link" title="https://y.org"><em>主页</em></span>',
  );
});

test("latex: quotes, dashes, escapes, comments, braces", () => {
  assert.equal(tex("``引号'' 1--2 和---破折号"), "“引号” 1–2 和—破折号");
  assert.equal(tex(String.raw`5\% \& \$ \# \_ \{x\}`), "5% &amp; $ # _ {x}");
  assert.equal(tex("正文 % 注释\n下一行"), "正文 \n下一行");
  assert.equal(tex("{\\bf 旧式} 和 {分组}"), "\\bf 旧式 和 分组");
  assert.equal(tex(String.raw`未知命令 \foo{bar} 保留，\ldots{} 省略`), String.raw`未知命令 \foo{bar} 保留，… 省略`);
  // Missing arguments: shown as written.
  assert.equal(tex(String.raw`\textbf 没有参数`), String.raw`\textbf 没有参数`);
  // Still streaming: the open brace stays visible.
  assert.equal(tex(String.raw`\textbf{加粗`), String.raw`\textbf{加粗`);
  assert.equal(tex(String.raw`见\cite{a`), String.raw`见\cite{a`);
  // A line break at the end of a line is one break.
  assert.equal(tex("第一行\\\\\n第二行"), "第一行\n第二行");
});

test("plain text is only escaped", () => {
  assert.equal(formatProse("**不处理** <b>", "plain"), "**不处理** &lt;b&gt;");
});

test("formatting wraps typeset math", () => {
  const html = renderToHtml("**$x$ 很大**，`$y$` 不是公式", "markdown");
  assert.ok(html.startsWith('<strong><span class="katex">'), html);
  assert.ok(html.includes("很大</strong>"));
  assert.ok(html.includes("<code>$y$</code>"));
  const latex = renderToHtml(String.raw`\textbf{$a+b$ 成立}`, "latex");
  assert.ok(latex.startsWith('<strong><span class="katex">') && latex.endsWith(" 成立</strong>"), latex);
});

test("output is inert: only known tags, nothing clickable or scriptable", () => {
  const inputs = [
    '<script>alert(1)</script> **<img src=x onerror=alert(1)>**',
    '[<b>x</b>](https://a.org" onmouseover="alert(1))',
    "[ok](https://a.org/$x$)",
    String.raw`\href{javascript:alert(1)}{点我} \url{"><img src=x>} \textbf{<i>}`,
  ];
  const allowed = new Set(["strong", "em", "del", "code", "u", "span"]);
  for (const mode of ["markdown", "latex"] as const) {
    for (const input of inputs) {
      const html = formatProse(input, mode);
      for (const tag of tagNames(html)) assert.ok(allowed.has(tag), `${tag} in ${html}`);
      // Inside real tags, only class and (escaped) title attributes.
      for (const tag of html.match(/<[a-z][^>]*>/g) ?? []) {
        const attrs = tag.replace(/^<[a-z]+/, "").replace(/\s(class|title)="[^"<>]*"/g, "");
        assert.equal(attrs, ">", `${tag} in ${html}`);
      }
    }
  }
  // Math stand-ins never reach an attribute.
  const html = renderToHtml("[ok](https://a.org/$x$)", "markdown");
  assert.ok(!/title="[^"]*</.test(html), html);
});

test("stand-in characters in the text can't forge math", () => {
  assert.equal(renderToHtml("a0b", "plain"), "a0b");
});
