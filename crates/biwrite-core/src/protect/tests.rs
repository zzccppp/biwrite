use super::*;
use crate::segment::segment;

fn masked(text: &str, mode: Mode) -> String {
    Protector::new(mode).mask(text)
}

fn protected(text: &str, mode: Mode) -> Vec<&str> {
    spans(text, mode)
        .into_iter()
        .map(|s| &text[s.range])
        .collect()
}

#[test]
fn latex_math_forms() {
    let t = r"Let $x$ and \(y\) and $$z$$ and \[w\] hold.";
    assert_eq!(
        masked(t, Mode::Latex),
        "Let ⟦0⟧ and ⟦1⟧ and ⟦2⟧ and ⟦3⟧ hold."
    );
    assert_eq!(
        protected(t, Mode::Latex),
        ["$x$", r"\(y\)", "$$z$$", r"\[w\]"]
    );
}

#[test]
fn escapes_are_text() {
    assert_eq!(
        masked(r"costs \$5 and \$6", Mode::Latex),
        r"costs \$5 and \$6"
    );
    assert_eq!(masked(r"100\% sure", Mode::Latex), r"100\% sure");
    // `\\` is a line break; the `$` after it opens math.
    assert_eq!(masked(r"a\\$x$", Mode::Latex), r"a\\⟦0⟧");
    // `\\[2pt]` is a line break with spacing, not display math.
    assert_eq!(masked(r"a\\[2pt] b\]", Mode::Latex), r"a\\[2pt] b\]");
    // An escaped dollar inside math doesn't close it.
    assert_eq!(protected(r"$a\$b$ c", Mode::Latex), [r"$a\$b$"]);
}

#[test]
fn unmatched_dollar_is_text() {
    assert_eq!(masked("only $5 here", Mode::Latex), "only $5 here");
    // Math never spans a paragraph break.
    assert_eq!(masked("a $x\n\ny$", Mode::Latex), "a $x\n\ny$");
    assert_eq!(masked("a $x\ny$ b", Mode::Latex), "a ⟦0⟧ b");
}

#[test]
fn citations_and_references() {
    let t = r"See \cite[p.~3]{a,b}, \citep[see][ch.~2]{c} and Section~\ref{sec:x}, Eq.~\eqref{eq:1}\label{l}.";
    assert_eq!(
        masked(t, Mode::Latex),
        "See ⟦0⟧, ⟦1⟧ and Section~⟦2⟧, Eq.~⟦3⟧⟦4⟧."
    );
    assert_eq!(
        protected(
            r"\cite*{k} \Cref{a} \crefrange{a}{b} \autoref{x} \pageref{p}",
            Mode::Latex
        ),
        [
            r"\cite*{k}",
            r"\Cref{a}",
            r"\crefrange{a}{b}",
            r"\autoref{x}",
            r"\pageref{p}"
        ]
    );
    // Nested braces in arguments.
    assert_eq!(
        protected(r"\cite[{a]b}]{k{1}} x", Mode::Latex),
        [r"\cite[{a]b}]{k{1}}"]
    );
    // Similar names and missing arguments are not protected.
    assert_eq!(
        masked(r"\citation{x} \ref \refx{y}", Mode::Latex),
        r"\citation{x} \ref \refx{y}"
    );
}

#[test]
fn urls_and_href() {
    let t = r"Code at \url{https://x.org/a%20b} and \href{https://y.org}{our site}.";
    assert_eq!(masked(t, Mode::Latex), "Code at ⟦0⟧ and ⟦1⟧{our site}.");
}

#[test]
fn comments_and_verb() {
    let t = "First line. % note: fix\nSecond \\verb|$x$| line.";
    assert_eq!(masked(t, Mode::Latex), "First line. ⟦0⟧\nSecond ⟦1⟧ line.");
    assert_eq!(protected(t, Mode::Latex)[0], "% note: fix");
}

#[test]
fn duplicates_share_a_number() {
    let mut p = Protector::new(Mode::Latex);
    assert_eq!(p.mask("$x$ then $y$ then $x$"), "⟦0⟧ then ⟦1⟧ then ⟦0⟧");
    assert_eq!(p.len(), 2);
    assert_eq!(p.restore("⟦0⟧，⟦0⟧ 和 ⟦1⟧").unwrap(), "$x$，$x$ 和 $y$");
    let err = p.restore("⟦0⟧ 和 ⟦1⟧").unwrap_err();
    assert_eq!(err.mismatched.len(), 1);
    assert_eq!(
        (err.mismatched[0].expected, err.mismatched[0].found),
        (2, 1)
    );
    assert_eq!(
        err.to_string(),
        "the model changed protected text: ⟦0⟧ `$x$` appears 1× instead of 2×"
    );
}

#[test]
fn cjk_around_spans() {
    let mut p = Protector::new(Mode::Latex);
    let m = p.mask(r"我们在图$\G=(V,E)$上研究\cite{kipf}。");
    assert_eq!(m, "我们在图⟦0⟧上研究⟦1⟧。");
    assert_eq!(
        p.restore("We study ⟦1⟧ on ⟦0⟧.").unwrap(),
        r"We study \cite{kipf} on $\G=(V,E)$."
    );
}

#[test]
fn restore_reports_missing_and_unknown() {
    let mut p = Protector::new(Mode::Latex);
    p.mask(r"a \cite{k} b $x$");
    let err = p.restore("甲 ⟦1⟧ 乙 ⟦7⟧").unwrap_err();
    assert_eq!(err.unknown, [7]);
    assert_eq!(
        err.to_string(),
        r"the model changed protected text: missing ⟦0⟧ `\cite{k}`; unknown ⟦7⟧"
    );
}

#[test]
fn restore_tolerates_spaces_inside_brackets() {
    let mut p = Protector::new(Mode::Latex);
    p.mask("a $x$");
    assert_eq!(p.restore("甲 ⟦ 0 ⟧").unwrap(), "甲 $x$");
    // A lone bracket is text.
    assert_eq!(p.restore("⟦ ⟦0⟧").unwrap(), "⟦ $x$");
}

#[test]
fn comment_keeps_following_text_visible() {
    let mut p = Protector::new(Mode::Latex);
    assert_eq!(p.mask("A. % todo\nB."), "A. ⟦0⟧\nB.");
    // The model joined the lines: B must not end up inside the comment.
    assert_eq!(p.restore("甲。⟦0⟧ 乙。").unwrap(), "甲。% todo\n乙。");
    assert_eq!(p.restore("甲。⟦0⟧\n乙。").unwrap(), "甲。% todo\n乙。");
    assert_eq!(p.restore("乙。甲。⟦0⟧").unwrap(), "乙。甲。% todo");
}

#[test]
fn partial_restore_hides_incomplete_placeholder() {
    let mut p = Protector::new(Mode::Latex);
    p.mask(r"see $x$ and \ref{a}");
    assert_eq!(p.restore_partial("参见 ⟦0⟧ 和 ⟦1"), "参见 $x$ 和 ");
    assert_eq!(p.restore_partial("参见 ⟦"), "参见 ");
    assert_eq!(p.restore_partial("参见 ⟦0⟧ 和 ⟦1⟧"), r"参见 $x$ 和 \ref{a}");
    assert_eq!(p.restore_partial("⟦9⟧ x"), "⟦9⟧ x");
}

#[test]
fn literal_placeholders_in_the_source_are_protected() {
    let mut p = Protector::new(Mode::Plain);
    assert_eq!(p.mask("the ⟦0⟧ token"), "the ⟦0⟧ token");
    assert_eq!(p.len(), 1);
    assert_eq!(p.restore("这个 ⟦0⟧ 记号").unwrap(), "这个 ⟦0⟧ 记号");
    let mut p = Protector::new(Mode::Latex);
    assert_eq!(p.mask("⟦3⟧ and $x$"), "⟦0⟧ and ⟦1⟧");
    assert_eq!(p.restore("⟦1⟧ 和 ⟦0⟧").unwrap(), "$x$ 和 ⟦3⟧");
}

#[test]
fn plain_mode_protects_nothing() {
    let mut p = Protector::new(Mode::Plain);
    assert_eq!(
        p.mask(r"costs $5 and $6, see \cite{x}"),
        r"costs $5 and $6, see \cite{x}"
    );
    assert!(p.is_empty());
    assert_eq!(p.restore("anything $5").unwrap(), "anything $5");
    // Placeholder-shaped text in the source would have been protected, so
    // one in the translation was made up.
    assert_eq!(p.restore("anything ⟦2⟧").unwrap_err().unknown, [2]);
}

#[test]
fn markdown_constructs() {
    let t = "Run `cargo test` or ``a ` b``, see <https://x.org/a> and [docs](https://y.org/p (Title)) with $E=mc^2$.";
    assert_eq!(
        masked(t, Mode::Markdown),
        "Run ⟦0⟧ or ⟦1⟧, see ⟦2⟧ and [docs]⟦3⟧ with ⟦4⟧."
    );
    // Dollar amounts and escapes are text; LaTeX commands aren't special.
    assert_eq!(
        masked(r"From $5 to $10, \$x and \cite{k}", Mode::Markdown),
        r"From $5 to $10, \$x and \cite{k}"
    );
    assert_eq!(masked("$$a+b$$ and `x", Mode::Markdown), "⟦0⟧ and `x");
    assert_eq!(masked("a <b> c <http:x>", Mode::Markdown), "a <b> c ⟦0⟧");
}

#[test]
fn mask_known_masks_previous_translation_consistently() {
    let mut p = Protector::new(Mode::Latex);
    let source = p.mask(r"New: $x$, \cite{a} and $xy$.");
    let old_source = p.mask_context(r"Old: $x$ and \ref{s}.");
    let old_translation = p.mask_known(r"旧：$x$ 和 \ref{s}，$xy$ 也是。");
    assert_eq!(source, "New: ⟦0⟧, ⟦1⟧ and ⟦2⟧.");
    assert_eq!(old_source, "Old: ⟦0⟧ and ⟦3⟧.");
    // Spans are scanned, not searched: `$xy$` is not read as `$x` + `y$`.
    assert_eq!(old_translation, "旧：⟦0⟧ 和 ⟦3⟧，⟦2⟧ 也是。");
    // Spans only in the previous source are not expected back.
    assert!(p.restore("新：⟦0⟧，⟦1⟧ 和 ⟦2⟧。").is_ok());
    let err = p.restore("新：⟦0⟧，⟦1⟧ 和 ⟦2⟧ ⟦3⟧。").unwrap_err();
    assert_eq!(
        err.to_string(),
        r"the model changed protected text: unexpected ⟦3⟧ `\ref{s}`"
    );
}

#[test]
fn mask_known_respects_escapes() {
    // A bare line-end `%` comment must not match the `%` of `\%`.
    let mut p = Protector::new(Mode::Latex);
    let source = p.mask("Accuracy improves by 5\\% here.%\nMore text.");
    assert_eq!(source, "Accuracy improves by 5\\% here.⟦0⟧\nMore text.");
    let old = p.mask_known("准确率提高了 5\\%。%\n更多。");
    assert_eq!(old, "准确率提高了 5\\%。⟦0⟧\n更多。");
    // Unknown spans stay raw.
    assert_eq!(p.mask_known("新 $y$ 和 %"), "新 $y$ 和 ⟦0⟧");
}

#[test]
fn model_written_percent_is_escaped_in_latex() {
    let mut p = Protector::new(Mode::Latex);
    p.mask("Up 5\\% here.%\nNext.");
    // The model's own `%` (even at a line end) is a percent sign; the source's
    // comment comes back only through its placeholder.
    assert_eq!(
        p.restore("提高了 5%\n⟦0⟧下一句 50\\%。").unwrap(),
        "提高了 5\\%\n%\n下一句 50\\%。"
    );
    assert_eq!(p.restore_partial("提高了 5%"), "提高了 5\\%");
    // Without anything protected too, and never in Markdown or plain text.
    assert_eq!(Protector::new(Mode::Latex).restore("95%").unwrap(), "95\\%");
    assert_eq!(
        Protector::new(Mode::Markdown).restore("95%").unwrap(),
        "95%"
    );
}

#[test]
fn long_protected_text_is_shortened_in_errors() {
    let mut p = Protector::new(Mode::Latex);
    p.mask(&format!("${}$", "a".repeat(80)));
    let msg = p.restore("nothing").unwrap_err().to_string();
    assert!(msg.ends_with("aaa…`"), "{msg}");
    assert!(msg.chars().count() < 120);
}

/// Reverse the word order and translate the words, keeping placeholders
/// whole: a model that rewrites everything around the placeholders.
fn shuffle(masked: &str) -> String {
    let words: Vec<String> = masked
        .split(' ')
        .rev()
        .map(|w| match w.find(OPEN) {
            Some(i) => format!("译{}", &w[i..]),
            None => "译".to_owned(),
        })
        .collect();
    words.join(" ")
}

fn kinds_count(text: &str, mode: Mode) -> Vec<String> {
    let mut v: Vec<String> = protected(text, mode)
        .into_iter()
        .map(str::to_owned)
        .collect();
    v.sort();
    v
}

/// Every distinct protected span in the sample paper's translatable text.
/// (`\label`s sit on heading lines and in skipped blocks, outside it.)
const PAPER_SPANS: &[&str] = &[
    "$0.1$",
    "$A$",
    r"$X \in \mathbb{R}^{n \times d}$",
    r"$\G = (V, E)$",
    r"$\mathcal{N}(v)$",
    r"$\sigma$",
    r"$\tau$",
    "$k$",
    "$v$",
    r"\cite{brown2020gpt3}",
    r"\cite{kipf2017gcn, velickovic2018gat}",
    r"\eqref{eq:mp}",
    r"\ref{sec:experiments}",
    r"\ref{tab:main}",
];

#[test]
fn paper_round_trip_never_alters_protected_text() {
    let paper = include_str!("../../../../samples/paper.tex");
    let mut seen = Vec::new();
    for seg in segment(paper, Mode::Latex) {
        if !seg.kind.is_translatable() {
            continue;
        }
        let source = seg.content(paper);
        let mut p = Protector::new(Mode::Latex);
        let m = p.mask(source);
        assert!(
            !m.contains('$') && !m.contains(r"\cite") && !m.contains(r"\ref"),
            "{m}"
        );
        let back = p.restore(&shuffle(&m)).unwrap();
        // Every protected construct survives byte for byte, as often as before.
        assert_eq!(
            kinds_count(&back, Mode::Latex),
            kinds_count(source, Mode::Latex)
        );
        seen.extend(protected(source, Mode::Latex));
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, PAPER_SPANS);
}

#[test]
fn sample_notes_round_trip() {
    let notes = include_str!("../../../../samples/notes.md");
    for seg in segment(notes, Mode::Markdown) {
        let source = seg.content(notes);
        let mut p = Protector::new(Mode::Markdown);
        let m = p.mask(source);
        let back = p.restore(&shuffle(&m)).unwrap();
        assert_eq!(
            kinds_count(&back, Mode::Markdown),
            kinds_count(source, Mode::Markdown)
        );
    }
}

#[test]
fn numbering_can_start_later() {
    let mut p = Protector::numbered_from(Mode::Latex, 3);
    assert_eq!(p.mask(r"$x$ and \cite{k} and $x$"), "⟦3⟧ and ⟦4⟧ and ⟦3⟧");
    assert_eq!(p.next_number(), 5);
    assert_eq!(p.mask_context(r"old $y$ and $x$"), "old ⟦5⟧ and ⟦3⟧");
    assert_eq!(p.mask_known(r"旧 $x$ 和 $z$"), "旧 ⟦3⟧ 和 $z$");
    assert_eq!(
        p.restore("⟦3⟧ 和 ⟦4⟧ 和 ⟦3⟧").unwrap(),
        r"$x$ 和 \cite{k} 和 $x$"
    );
    assert_eq!(p.restore_partial("⟦3⟧ 和 ⟦4"), "$x$ 和 ");
    // Another paragraph's numbers are unknown here.
    let err = p.restore("⟦0⟧ 和 ⟦4⟧ 和 ⟦3⟧ ⟦3⟧").unwrap_err();
    assert_eq!(err.unknown, [0]);
    assert_eq!(
        p.restore("⟦4⟧ ⟦3⟧").unwrap_err().to_string(),
        "the model changed protected text: ⟦3⟧ `$x$` appears 1× instead of 2×"
    );
}

#[test]
fn placeholders_in_a_translation_of_text_without_any_are_unknown() {
    let p = Protector::numbered_from(Mode::Plain, 2);
    assert_eq!(p.restore("没有占位符。").unwrap(), "没有占位符。");
    assert_eq!(p.restore("⟦ 不是占位符").unwrap(), "⟦ 不是占位符");
    assert_eq!(p.restore("带 ⟦0⟧ 的译文").unwrap_err().unknown, [0]);
}
