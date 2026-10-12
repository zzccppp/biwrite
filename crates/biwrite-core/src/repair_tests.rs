use super::*;

const DOC: &str = "\\documentclass{article}\n\
\\usepackage{xcolor}\n\
\\usepackage{tikz}\n\
\\begin{document}\n\
Some text.\n\
\\begin{tikzpicture}\n  \\node[fill=subsectioncolor] (a) {A};\n\
\\end{tikzpicture}\n\
\\end{document}\n";

fn error(line: usize, message: &str) -> TexError {
    TexError {
        line: Some(line),
        file: Some("main.tex".into()),
        message: message.into(),
        context: format!("l.{line} \\node[fill=subsectioncolor] (a) {{A}};"),
    }
}

#[test]
fn a_short_document_goes_whole_with_line_numbers() {
    let out = excerpt(DOC, &[7]);
    assert!(out.starts_with("  1| \\documentclass{article}\n"), "{out}");
    assert!(out.contains("\n  7|   \\node[fill=subsectioncolor] (a) {A};\n"));
    assert_eq!(out.lines().count(), DOC.split('\n').count());
}

#[test]
fn a_long_document_goes_as_its_preamble_and_the_lines_around_the_errors() {
    let mut doc = String::from("\\documentclass{article}\n\\usepackage{tikz}\n\\begin{document}\n");
    for i in 0..4000 {
        doc.push_str(&format!(
            "Paragraph number {i} of a long paper, with some words.\n"
        ));
    }
    doc.push_str("\\end{document}\n");
    let out = excerpt(&doc, &[2000]);
    assert!(
        out.starts_with("   1| \\documentclass{article}"),
        "{}",
        &out[..80]
    );
    assert!(out.contains("   3| \\begin{document}\n…\n"));
    assert!(out.contains("2000| Paragraph number 1996 "));
    assert!(out.contains(&format!("{}| ", 2000 - AROUND)));
    assert!(!out.contains(&format!("{}| ", 2000 - AROUND - 1)));
    assert!(out.chars().count() < 10_000);
}

#[test]
fn the_fix_prompt_gives_the_errors_and_what_their_messages_mean() {
    let errors = [error(7, "Package xcolor Error: Undefined color model `'.")];
    let (system, user) = fix_prompt(&FixRequest {
        text: DOC,
        errors: &errors,
        instruction: "",
        packages: Some(&["xcolor".into(), "tikz".into()]),
    });
    assert!(system.contains("does not compile"));
    assert!(user.contains("<loaded_packages>\nxcolor, tikz\n</loaded_packages>"));
    assert!(
        user.contains(
            "<errors>\nline 7: Package xcolor Error: Undefined color model `'.\nl.7 \\node"
        )
    );
    assert!(user.contains("defined before xcolor was loaded"));
    assert!(user.contains("<edit>\n<find>"));
    assert!(!user.contains("<instruction>"));
    let (_, user) = fix_prompt(&FixRequest {
        text: DOC,
        errors: &errors,
        instruction: "Use blue.",
        packages: None,
    });
    assert!(user.contains("<instruction>\nUse blue.\n</instruction>"));
    assert!(!user.contains("<loaded_packages>"));
}

#[test]
fn replacements_are_read_without_line_numbers_or_fences() {
    let reply = "Here you go.\n<edit>\n<find>\n  7|   \\node[fill=subsectioncolor] (a) {A};\n</find>\n<replace>\n  \\node[fill=blue!20] (a) {A};\n</replace>\n</edit>\n<edit>\n<find>\n```latex\n\\usepackage{tikz}\n```\n</find>\n<replace>\n\\usepackage{tikz}\n\\usetikzlibrary{positioning}\n</replace>\n</edit>\n<edit><find>no replace</find></edit>\n<changes_zh>\n改了颜色\n</changes_zh>";
    let r = parse_replacements(reply);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].find, "  \\node[fill=subsectioncolor] (a) {A};");
    assert_eq!(r[0].replace, "  \\node[fill=blue!20] (a) {A};");
    assert_eq!(r[1].find, "\\usepackage{tikz}");
    assert_eq!(
        r[1].replace,
        "\\usepackage{tikz}\n\\usetikzlibrary{positioning}"
    );
    // Text that only looks numbered in part keeps its numbers.
    assert_eq!(unnumbered("3| a\nb"), "3| a\nb");
}

#[test]
fn replacements_go_where_their_lines_are() {
    let r = |find: &str, replace: &str| Replacement {
        find: find.into(),
        replace: replace.into(),
    };
    // Exactly.
    let (out, missed) = apply_replacements(
        DOC,
        &[r("\\node[fill=subsectioncolor]", "\\node[fill=blue!20]")],
        &[],
    );
    assert!(missed.is_empty());
    assert!(out.contains("  \\node[fill=blue!20] (a) {A};"));
    // Line for line, indentation aside.
    let (out, missed) = apply_replacements(
        DOC,
        &[r(
            "\\begin{tikzpicture}\n\\node[fill=subsectioncolor] (a) {A};",
            "\\begin{tikzpicture}\n  \\node (a) {A};",
        )],
        &[],
    );
    assert!(missed.is_empty(), "{out}");
    assert!(out.contains("\\begin{tikzpicture}\n  \\node (a) {A};\n\\end{tikzpicture}"));
    // Lines that are not there.
    let (out, missed) = apply_replacements(DOC, &[r("\\nothere", "x")], &[]);
    assert_eq!(missed, vec![0]);
    assert_eq!(out, DOC);
    // Of several places, the one nearest an error; without one, none.
    let doc = "x = 1\nfoo\ny\nfoo\nz\n";
    let (out, _) = apply_replacements(doc, &[r("foo", "bar")], &[4]);
    assert_eq!(out, "x = 1\nfoo\ny\nbar\nz\n");
    let (out, missed) = apply_replacements(doc, &[r("foo", "bar")], &[]);
    assert_eq!((out.as_str(), missed), (doc, vec![0]));
}

#[test]
fn hunks_cover_the_changed_lines_and_find_their_place_again() {
    let new = DOC
        .replace(
            "\\usepackage{tikz}\n",
            "\\usepackage{tikz}\n\\definecolor{figA}{RGB}{31,78,121}\n",
        )
        .replace("subsectioncolor", "figA");
    let hs = hunks(DOC, &new);
    assert_eq!(hs.len(), 2, "{hs:?}");
    assert_eq!(hs[0].line, 4);
    assert_eq!(hs[0].old, "");
    assert_eq!(hs[0].new, "\\definecolor{figA}{RGB}{31,78,121}\n");
    assert_eq!(hs[0].before, "\\usepackage{xcolor}\n\\usepackage{tikz}\n");
    assert_eq!(hs[1].line, 7);
    assert_eq!(hs[1].old, "  \\node[fill=subsectioncolor] (a) {A};\n");
    assert!(
        hs[1]
            .diff
            .iter()
            .any(|p| p.kind == "insert" && p.text.contains("figA"))
    );
    // Put in place by their context, they give the new text.
    let mut text = DOC.to_owned();
    for h in &hs {
        let probe = format!("{}{}{}", h.before, h.old, h.after);
        let at = text.find(&probe).expect("its place") + h.before.len();
        text.replace_range(at..at + h.old.len(), &h.new);
    }
    assert_eq!(text, new);
    // Changes two lines apart or closer make one hunk.
    let near = DOC
        .replace("Some text.", "More text.")
        .replace("(a) {A}", "(a) {B}");
    assert_eq!(hunks(DOC, &near).len(), 1);
    assert!(hunks(DOC, DOC).is_empty());
}

#[test]
fn the_answer_prompt_shows_what_was_put_in_and_what_may_change() {
    let text = DOC.replace(
        "\\begin{document}",
        "\\usetikzlibrary{arrows.meta}\n\\begin{document}",
    );
    let start = text.find("\\begin{tikzpicture}").unwrap();
    let end = text.find("\\end{document}").unwrap();
    let pre = text.find("\\usetikzlibrary").unwrap();
    let pre_end = pre + "\\usetikzlibrary{arrows.meta}\n".len();
    let errors = [error(8, "Package xcolor Error: Undefined color model `'.")];
    let (_, user) = answer_fix_prompt(&AnswerFixRequest {
        text: &text,
        answer: start..end,
        preamble: Some(pre..pre_end),
        errors: &errors,
        packages: None,
    });
    assert!(
        user.contains("<inserted>\n  7| \\begin{tikzpicture}\n  8|   \\node"),
        "{user}"
    );
    assert!(
        user.contains("  9| \\end{tikzpicture}\n</inserted>"),
        "{user}"
    );
    assert!(user.contains("lines 4 to 4 of <preamble> were added"));
    assert!(user.contains("<preamble>\n  1| \\documentclass{article}"));
    assert!(user.contains("<preamble>\nall the lines to add"));
    let (_, user) = answer_fix_prompt(&AnswerFixRequest {
        text: &text,
        answer: start..end,
        preamble: None,
        errors: &errors,
        packages: None,
    });
    assert!(user.contains("cannot be changed"));
    assert!(!user.contains("<preamble>\nall the lines"));
}

#[test]
fn a_corrected_answer_is_read_with_its_preamble_lines() {
    let reply = "<revision>\n  8| \\begin{tikzpicture}\n  9| \\node[fill=figA] {A};\n 10| \\end{tikzpicture}\n</revision>\n<preamble>\n\\definecolor{figA}{RGB}{31,78,121}\n</preamble>\n<changes_zh>\n- 换成自定义颜色\n</changes_zh>\n<changes_en>\nOwn color.\n</changes_en>";
    let fix = parse_answer_fix(reply);
    assert_eq!(
        fix.revision.as_deref(),
        Some("\\begin{tikzpicture}\n\\node[fill=figA] {A};\n\\end{tikzpicture}")
    );
    assert_eq!(
        fix.preamble.as_deref(),
        Some("\\definecolor{figA}{RGB}{31,78,121}\n")
    );
    assert_eq!(fix.changes_zh, vec!["换成自定义颜色"]);
    // An empty preamble block means no lines; none at all, as they were.
    let fix = parse_answer_fix("<revision>\nx\n</revision>\n<preamble>\n</preamble>");
    assert_eq!(fix.preamble.as_deref(), Some(""));
    assert_eq!(
        parse_answer_fix("<revision>\nx\n</revision>").preamble,
        None
    );
}

#[test]
fn lines_of_a_range_end_before_its_last_line_break() {
    let text = "a\nb\nc\n";
    assert_eq!(lines_of(text, &(2..4)), 2..3);
    assert_eq!(lines_of(text, &(2..5)), 2..4);
    assert_eq!(lines_of(text, &(2..2)), 2..3);
    assert_eq!(line_at(text, 0), 1);
    assert_eq!(line_at(text, 6), 4);
}
