use crate::mode::Mode;
use crate::segment::test_util::check_invariants;
use crate::segment::{SegmentKind, SkipReason, segment};

/// Description per segment: `kind: content` (source if skipped), newlines as ⏎.
fn describe(text: &str) -> Vec<String> {
    segment(text, Mode::Latex)
        .iter()
        .map(|s| {
            let (tag, body) = match s.kind {
                SegmentKind::Paragraph => ("P".to_owned(), s.content(text)),
                SegmentKind::Heading { level } => (format!("H{level}"), s.content(text)),
                SegmentKind::Caption => ("CAP".to_owned(), s.content(text)),
                SegmentKind::Skipped { reason } => (format!("-{reason:?}"), s.source(text)),
            };
            format!("{tag}: {}", body.trim_start().replace('\n', "⏎"))
        })
        .collect()
}

/// Each description must start with the expected prefix.
#[track_caller]
fn assert_segments(text: &str, expected: &[&str]) {
    let got = describe(text);
    let ok = got.len() == expected.len() && got.iter().zip(expected).all(|(g, e)| g.starts_with(e));
    assert!(ok, "segments differ:\n got: {got:#?}\nwant: {expected:#?}");
}

fn kinds(text: &str) -> Vec<SegmentKind> {
    segment(text, Mode::Latex).iter().map(|s| s.kind).collect()
}

const P: SegmentKind = SegmentKind::Paragraph;
const fn skip(reason: SkipReason) -> SegmentKind {
    SegmentKind::Skipped { reason }
}

#[test]
fn sample_paper_segmentation() {
    let text = include_str!("../../../../samples/paper.tex");
    let expected = [
        r"-Preamble: \documentclass[11pt]{article}⏎\",
        r"-Markup: \maketitle",
        r"-Markup: \begin{abstract}",
        r"P: Large language models can lear",
        r"-Markup: \end{abstract}",
        r"H2: Introduction",
        r"P: Graph neural networks (GNNs) a",
        r"P: In contrast, large language mo",
        r"P: We answer this question affirm",
        r"H2: Related Work",
        r"P: \paragraph{Prompting for graph",
        r"P: \paragraph{Meta-learning.} Few",
        r"H2: Method",
        r"P: Let $X \in \mathbb{R}^{n \time",
        r"-Math: \begin{equation}⏎  h_v^{(\e",
        r"P: where $\mathcal{N}(v)$ is the ",
        r"P: Given $k$ labelled examples pe",
        r"-Float: \begin{figure}[t]⏎  \cente",
        r"CAP: Construction of the prompt gr",
        r"-Float: \label{fig:prompt}⏎\end{fig",
        r"H3: Training Objective",
        r"P: We pretrain the model with a c",
        r"-Math: \begin{align}⏎  \mathcal{L}",
        r"P: The temperature $\tau$ is fixe",
        r"H2: Experiments",
        r"P: We evaluate on Cora, CiteSeer,",
        r"-Float: \begin{table}[t]⏎  \center",
        r"CAP: Few-shot node classification ",
        r"-Float: \begin{tabular}{lcccc}⏎   ",
        r"P: Table~\ref{tab:main} shows tha",
        r"-Comment: % TODO: add ablation on the",
        r"H2: Conclusion",
        r"P: We showed that graph neural ne",
        r"-Markup: \bibliographystyle{plainna",
        r"-Markup: \end{document}",
    ];
    assert_segments(text, &expected);
    check_invariants(text, Mode::Latex);
    let translatable = segment(text, Mode::Latex)
        .iter()
        .filter(|s| s.kind.is_translatable())
        .count();
    assert_eq!(translatable, 22);
}

#[test]
fn sections_split_from_following_text() {
    let text = "\\section{Introduction}\\label{sec:intro}\nWe study $x$.\nMore.\n\n\\subsection*{Setup}\nBody.";
    assert_segments(
        text,
        &[
            "H2: Introduction",
            "P: We study $x$.⏎More.",
            "H3: Setup",
            "P: Body.",
        ],
    );
}

#[test]
fn nested_braces_and_short_title() {
    let text = "\\section[Short]{A {\\em nested} title}";
    assert_segments(text, &[r"H2: A {\em nested} title"]);
}

#[test]
fn run_in_paragraph_heading_stays_paragraph() {
    let text = "\\paragraph{Setup.} We train for 10 epochs.";
    assert_eq!(kinds(text), [P]);
}

#[test]
fn similar_command_names_are_not_headings() {
    let text = "\\sectionmark{x}\n\\partial f";
    assert_eq!(kinds(text), [P]);
}

#[test]
fn no_preamble_without_begin_document() {
    // A chapter file pulled in with \input has no preamble.
    let text = "\\chapter{Intro}\nText.";
    assert_segments(text, &["H1: Intro", "P: Text."]);
}

#[test]
fn comment_lines_are_their_own_segments() {
    let text = "First part of a sentence\n% a note to self\n% another\nsecond part.\n\n%% end";
    assert_segments(
        text,
        &[
            "P: First part of a sentence",
            "-Comment: % a note to self⏎% another",
            "P: second part.",
            "-Comment: %% end",
        ],
    );
}

#[test]
fn escaped_percent_is_text() {
    let text = "\\% of nodes";
    assert_eq!(kinds(text), [P]);
}

#[test]
fn every_math_environment_is_skipped() {
    for env in [
        "equation",
        "equation*",
        "align",
        "align*",
        "gather",
        "gather*",
        "multline",
        "eqnarray",
        "displaymath",
        "flalign",
    ] {
        let text = format!("Before.\n\\begin{{{env}}}\n  a = b\n\n  c = d\n\\end{{{env}}}\nAfter.");
        assert_eq!(
            kinds(&text),
            [P, skip(SkipReason::Math), P],
            "environment {env}"
        );
    }
}

#[test]
fn code_and_table_environments_are_skipped() {
    for (env, reason) in [
        ("verbatim", SkipReason::Code),
        ("lstlisting", SkipReason::Code),
        ("algorithm", SkipReason::Code),
        ("tabular", SkipReason::Table),
    ] {
        let text = format!(
            "\\begin{{{env}}}\n\\section{{not a heading}}\n% not a comment\n\\end{{{env}}}"
        );
        assert_eq!(kinds(&text), [skip(reason)], "environment {env}");
    }
}

#[test]
fn nested_environment_of_same_name() {
    let text = "\\begin{tabular}{c}\n\\begin{tabular}{c} x \\end{tabular}\n\\end{tabular}\nAfter.";
    assert_eq!(kinds(text), [skip(SkipReason::Table), P]);
}

#[test]
fn display_math_delimiters() {
    let text = "We have\n\\[\n  x^2\n\\]\nand\n$$ y $$\nfinally.";
    assert_eq!(
        kinds(text),
        [P, skip(SkipReason::Math), P, skip(SkipReason::Math), P]
    );
}

#[test]
fn multi_line_caption_with_short_title() {
    let text = "\\begin{figure*}\n\\includegraphics{a.pdf}\n\\caption[Short]{A long caption\nthat spans {two} lines.}\\label{f}\n\\end{figure*}";
    assert_segments(
        text,
        &[
            r"-Float: \begin{figure*}⏎\includegraphics{a.pdf}",
            "CAP: A long caption⏎that spans {two} lines.",
            r"-Float: \end{figure*}",
        ],
    );
}

#[test]
fn caption_lookalikes_are_not_captions() {
    let text = "\\begin{figure}\n\\captionsetup{font=small}\n\\captionof{figure}{x}\n\\end{figure}";
    assert_eq!(kinds(text), [skip(SkipReason::Float)]);
}

#[test]
fn items_start_new_paragraphs() {
    let text = "\\begin{itemize}\n  \\item First point\n  continues here.\n  \\item[(b)] Second.\n  \\item\n  Third on next line.\n\\end{itemize}";
    assert_segments(
        text,
        &[
            r"-Markup: \begin{itemize}",
            "P: First point⏎  continues here.",
            "P: Second.",
            "P: Third on next line.",
            r"-Markup: \end{itemize}",
        ],
    );
}

#[test]
fn markup_lines_vs_prose_with_commands() {
    let text = "\\noindent We show that\n\\textbf{bold} works.\n\n\\vspace{2mm}\\centering % spacing\n\\label{x}";
    assert_eq!(kinds(text), [P, skip(SkipReason::Markup)]);
}

#[test]
fn content_after_end_document_is_skipped() {
    let text = "\\begin{document}\nHello.\n\\end{document}\n\nNotes after the end.\n";
    assert_segments(
        text,
        &[
            r"-Preamble: \begin{document}",
            "P: Hello.",
            r"-Markup: \end{document}⏎⏎Notes after the end.",
        ],
    );
    check_invariants(text, Mode::Latex);
}

#[test]
fn unclosed_environment_runs_to_end() {
    let text = "Text.\n\\begin{equation}\nx\n\nstill math";
    assert_eq!(kinds(text), [P, skip(SkipReason::Math)]);
    check_invariants(text, Mode::Latex);
}

#[test]
fn declarations_wrapping_prose_are_prose() {
    for text in [
        "\\noindent{\\em Remark: the bound is tight.}",
        "\\small{All numbers are averaged over five seeds.}",
        "\\centering {Centered prose.}",
    ] {
        assert_eq!(kinds(text), [P], "{text}");
    }
    assert_eq!(
        kinds("\\clearpage\n\\pagebreak[4]"),
        [skip(SkipReason::Markup)]
    );
}

#[test]
fn subcaptions_are_translated_and_commented_captions_are_not() {
    let text = "\\begin{figure}\n\\subcaption{Left panel.}\n% \\caption{Old caption.}\n\\caption{Both panels.}\n\\end{figure}";
    assert_segments(
        text,
        &[
            r"-Float: \begin{figure}",
            "CAP: Left panel.",
            r"-Float: % \caption{Old caption.}",
            "CAP: Both panels.",
            r"-Float: \end{figure}",
        ],
    );
    check_invariants(text, Mode::Latex);
}

#[test]
fn bibliography_entries_are_skipped() {
    let text = "Text.\n\n\\begin{thebibliography}{9}\n\\bibitem{a} A. Author. A title. 2020.\n\\end{thebibliography}";
    assert_eq!(kinds(text), [P, skip(SkipReason::Markup)]);
}
