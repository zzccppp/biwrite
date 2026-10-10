use super::*;

fn skill() -> Skill {
    Skill {
        name: "research-builder".into(),
        source: "https://github.com/qzkinhit/research-builder".into(),
        version: "d227e92".into(),
        files: vec![
            ("writing-deai.md".into(), "DEAI RULES".into()),
            ("writing-playbook.md".into(), "PLAYBOOK RULES".into()),
            ("knowledge/paper-anatomy.md".into(), "ANATOMY".into()),
            ("figure-style/README.md".into(), "FIGURE RULES".into()),
        ],
    }
}

const LATEX: &str = "We follow \\citet{smith2020} and set $k=3$. Results improve.";

#[test]
fn polish_prompt_masks_the_target_and_carries_the_skill() {
    let req = Request {
        mode: Mode::Latex,
        target: LATEX,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(p.system.contains("research-builder skill"));
    assert!(
        p.system
            .contains("<skill_file path=\"writing-deai.md\">\nDEAI RULES")
    );
    assert!(p.system.contains("PLAYBOOK RULES"));
    assert!(
        !p.system.contains("ANATOMY"),
        "polish uses the writing files only"
    );
    assert!(
        p.user
            .contains("<target>\nWe follow ⟦0⟧ and set ⟦1⟧. Results improve.\n</target>")
    );
    assert!(p.user.contains("keep every one in <revision>"));
    assert!(p.user.contains("LaTeX source"));
    assert!(p.user.contains("<changes_zh>"));
    assert_eq!(p.protector.len(), 2);
}

#[test]
fn scope_and_references_shape_the_message() {
    let refs = vec!["A sample paragraph in the author's voice.".to_owned()];
    let doc = Request {
        action: Action::Edit,
        target: "Short target.",
        instruction: "Make it more precise.",
        document: Some("Whole paper."),
        references: &refs,
        images: 2,
        ..Default::default()
    };
    let p = build(&doc, &skill());
    assert!(p.user.contains("<document>\nWhole paper.\n</document>"));
    assert!(!p.user.contains("<context_before>"));
    assert!(p.user.contains("<reference_text n=\"1\">"));
    assert!(p.user.contains("attached images"));
    assert!(
        p.user
            .contains("<instruction>\nMake it more precise.\n</instruction>")
    );

    let near = Request {
        target: "Short target.",
        before: Some("Before."),
        after: Some("After."),
        ..Default::default()
    };
    let p = build(&near, &skill());
    assert!(p.user.contains("<context_before>\nBefore."));
    assert!(p.user.contains("<context_after>\nAfter."));
    assert!(!p.user.contains("<document>"));
}

#[test]
fn ask_and_figure_use_their_own_files() {
    let ask = build(
        &Request {
            action: Action::Ask,
            target: "x",
            instruction: "Is the claim supported?",
            ..Default::default()
        },
        &skill(),
    );
    assert!(ask.system.contains("ANATOMY"));
    assert!(ask.user.contains("<answer>"));
    let fig = build(
        &Request {
            action: Action::Figure,
            mode: Mode::Latex,
            target: "Results improve with $k$.",
            instruction: "A bar chart of accuracy per dataset.",
            ..Default::default()
        },
        &skill(),
    );
    assert!(fig.system.contains("FIGURE RULES"));
    assert!(
        fig.user.contains("<target>\nResults improve with $k$."),
        "figures do not mask"
    );
    assert!(fig.user.contains("\\label"));
}

#[test]
fn polish_answers_are_restored() {
    let mut protector = Protector::new(Mode::Latex);
    protector.mask(LATEX);
    let reply = "<revision>\nWe follow ⟦0⟧ with ⟦1⟧, and the results improve.\n</revision>\n<translation>\n我们沿用 ⟦0⟧ 并设 ⟦1⟧，结果随之提升。\n</translation>\n<changes_zh>\n- 合并两句\n</changes_zh>\n<changes_en>\n- Joined two sentences\n</changes_en>";
    let a = parse(Action::Polish, reply, &protector).unwrap();
    assert_eq!(
        a.revision.as_deref(),
        Some("We follow \\citet{smith2020} with $k=3$, and the results improve.")
    );
    assert_eq!(
        a.translation.as_deref(),
        Some("我们沿用 \\citet{smith2020} 并设 $k=3$，结果随之提升。")
    );
    assert!(a.translation_matches);
    assert_eq!(a.changes_zh, ["合并两句"]);
    assert_eq!(a.changes_en, ["Joined two sentences"]);
    assert!(a.removed.is_empty() && a.repeated.is_empty());
}

#[test]
fn dropped_citations_are_reported_not_fatal() {
    let mut protector = Protector::new(Mode::Latex);
    protector.mask(LATEX);
    let reply = "<revision>We set ⟦1⟧. Results improve.</revision><translation>我们设 ⟦1⟧ 和 ⟦0⟧。</translation>";
    let a = parse(Action::Edit, reply, &protector).unwrap();
    assert_eq!(
        a.revision.as_deref(),
        Some("We set $k=3$. Results improve.")
    );
    assert_eq!(a.removed, ["\\citet{smith2020}"]);
    assert!(
        !a.translation_matches,
        "the translation kept a citation the revision dropped"
    );
}

#[test]
fn unusable_answers_are_errors() {
    let mut protector = Protector::new(Mode::Latex);
    protector.mask(LATEX);
    assert_eq!(
        parse(Action::Polish, "Here you go: better text.", &protector),
        Err(AssistError::NoRevision)
    );
    assert_eq!(
        parse(Action::Polish, "<revision>See ⟦7⟧.</revision>", &protector),
        Err(AssistError::UnknownPlaceholder(vec![7]))
    );
}

#[test]
fn ask_answers_with_or_without_a_rewrite() {
    let protector = Protector::new(Mode::Plain);
    let plain = parse(Action::Ask, "The claim needs a number.", &protector).unwrap();
    assert_eq!(plain.answer.as_deref(), Some("The claim needs a number."));
    assert!(plain.revision.is_none());
    let both = parse(
        Action::Ask,
        "<answer>Add the number.</answer><revision>Accuracy rose by 4 points.</revision><translation>准确率提高了 4 个点。</translation>",
        &protector,
    )
    .unwrap();
    assert_eq!(both.answer.as_deref(), Some("Add the number."));
    assert_eq!(both.revision.as_deref(), Some("Accuracy rose by 4 points."));
}

#[test]
fn fences_and_figures() {
    let protector = Protector::new(Mode::Latex);
    let reply = "<revision>\n```latex\n\\begin{figure}\n% comment stays\n\\end{figure}\n```\n</revision><translation>图 1：结果</translation>";
    let a = parse(Action::Figure, reply, &protector).unwrap();
    assert_eq!(
        a.revision.as_deref(),
        Some("\\begin{figure}\n% comment stays\n\\end{figure}")
    );
    assert!(!a.translation_matches);
}

#[test]
fn streaming_preview_hides_tags() {
    assert_eq!(
        streaming_preview("<revision>\nWe follow the plan"),
        "We follow the plan"
    );
    assert_eq!(
        streaming_preview("<revision>A</revision>\n<translation>甲</transl"),
        "A\n\n\n甲"
    );
    assert_eq!(streaming_preview("a < b and c > d"), "a < b and c > d");
}

#[test]
fn diffs_read_by_word_and_by_character() {
    let d = diff("We use the method.", "We adopt the method.");
    let kinds: Vec<(&str, &str)> = d.iter().map(|p| (p.kind, p.text.as_str())).collect();
    assert_eq!(
        kinds,
        [
            ("equal", "We "),
            ("delete", "use"),
            ("insert", "adopt"),
            ("equal", " the method.")
        ]
    );
    let zh = diff("我们使用该方法。", "我们采用该方法。");
    assert!(zh.iter().any(|p| p.kind == "delete" && p.text == "使"));
    assert!(zh.iter().any(|p| p.kind == "insert" && p.text == "采"));
    let rebuilt: String = zh
        .iter()
        .filter(|p| p.kind != "delete")
        .map(|p| p.text.as_str())
        .collect();
    assert_eq!(rebuilt, "我们采用该方法。");
}

#[test]
fn paragraphs_and_neighbors() {
    let text = "First paragraph.\n\nSecond paragraph here.\n\nThird.";
    let pos = text.find("paragraph here").unwrap();
    let range = paragraph_at(text, Mode::Plain, pos).unwrap();
    assert_eq!(&text[range.clone()], "Second paragraph here.");
    let (before, after) = neighbors(text, Mode::Plain, &range, 2);
    assert_eq!(before.as_deref(), Some("First paragraph."));
    assert_eq!(after.as_deref(), Some("Third."));
    assert_eq!(paragraph_at("a\n\n\n\nb", Mode::Plain, 2), None);
}

#[test]
fn sentences_skip_abbreviations_and_math() {
    let text = "See Fig. 2 for $x = 1.5$. The model, e.g. a GNN, wins! Next one?";
    let at = |needle: &str| {
        let pos = text.find(needle).unwrap();
        let r = sentence_at(text, Mode::Latex, pos).unwrap();
        text[r].to_owned()
    };
    assert_eq!(at("Fig."), "See Fig. 2 for $x = 1.5$.");
    assert_eq!(at("GNN"), "The model, e.g. a GNN, wins!");
    assert_eq!(at("Next"), "Next one?");
    let zh = "第一句。第二句很短！第三句";
    let pos = zh.find("第二").unwrap();
    let r = sentence_at(zh, Mode::Plain, pos).unwrap();
    assert_eq!(&zh[r], "第二句很短！");
}

#[test]
fn mirror_requests_carry_the_new_translation_and_ask_for_a_revision_only() {
    let skill = Skill::default();
    let req = Request {
        action: Action::Mirror,
        mode: Mode::Latex,
        target: "We evaluate on four datasets \\cite{a}.",
        instruction: "我们在五个数据集上评测 \\cite{a}。",
        ..Request::default()
    };
    let p = build(&req, &skill);
    assert!(p.user.contains("<instruction>\n我们在五个数据集上评测"));
    assert!(p.user.contains("rewrote the Chinese (Simplified) version"));
    assert!(!p.user.contains("<translation>"));
    // The model answers with the masked target, revised.
    let target = tag(&p.user, "target").unwrap().replace("four", "five");
    let answer = parse(
        Action::Mirror,
        &format!(
            "<revision>\n{target}\n</revision>\n<changes_zh>\n数据集个数改为五个。\n</changes_zh>"
        ),
        &p.protector,
    )
    .unwrap();
    assert_eq!(
        answer.revision.as_deref(),
        Some("We evaluate on five datasets \\cite{a}.")
    );
    assert!(parse(Action::Mirror, "no tags", &p.protector).is_err());
}

#[test]
fn write_requests_imitate_the_references_and_insert_new_text() {
    let uniclean = vec![
        "\\section{Introduction}\nData cleaning is a crucial step. We present UniClean, a cleaning \
         system that composes operators.\n"
            .to_owned(),
    ];
    let req = Request {
        action: Action::Write,
        mode: Mode::Latex,
        target: "Errors cost accuracy \\cite{x} for $k=5$ models.",
        instruction: "Two paragraphs motivating row-level cleaning, like the UniClean introduction.",
        references: &uniclean,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(
        p.system.contains("ANATOMY"),
        "writing new text uses the paper anatomy"
    );
    assert!(p.system.contains("DEAI RULES"));
    // The paragraph before is context, sent as written (nothing to restore).
    assert!(
        p.user
            .contains("<target>\nErrors cost accuracy \\cite{x} for $k=5$ models.\n</target>")
    );
    assert_eq!(p.protector.len(), 0);
    assert!(
        p.user
            .contains("<reference_text n=\"1\">\n\\section{Introduction}")
    );
    assert!(p.user.contains("imitate closely"));
    assert!(
        p.user
            .contains("never copy their sentences, claims, numbers or citations")
    );
    assert!(p.user.contains("never invent results"));
    assert!(p.user.contains("paragraphs separated by a blank line"));

    let reply = "<revision>\nFirst paragraph \\cite{x}.\n\nSecond paragraph.\n</revision>\n\
                 <translation>\n第一段 \\cite{x}。\n\n第二段。\n</translation>\n\
                 <changes_zh>\n- 两段动机\n</changes_zh>\n<changes_en>\n- two paragraphs\n</changes_en>";
    let a = parse(Action::Write, reply, &p.protector).unwrap();
    assert_eq!(
        a.revision.as_deref(),
        Some("First paragraph \\cite{x}.\n\nSecond paragraph.")
    );
    assert_eq!(
        a.translation.as_deref(),
        Some("第一段 \\cite{x}。\n\n第二段。")
    );
    assert_eq!(a.changes_zh, ["两段动机"]);
    assert!(Action::Write.inserts() && Action::Figure.inserts() && !Action::Edit.inserts());
    assert_eq!(
        parse(Action::Write, "no tags", &p.protector),
        Err(AssistError::NoRevision)
    );
}

#[test]
fn figures_are_told_which_packages_the_document_loads() {
    let loaded = vec!["booktabs".to_owned(), "graphicx".to_owned()];
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "A table of accuracy per dataset.",
        packages: Some(&loaded),
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(
        p.user
            .contains("<loaded_packages>\nbooktabs, graphicx\n</loaded_packages>")
    );
    assert!(p.user.contains("never \\usepackage"));
    // Unknown packages: the old wording.
    let p = build(
        &Request {
            packages: None,
            ..req
        },
        &skill(),
    );
    assert!(!p.user.contains("<loaded_packages>"));
    assert!(p.user.contains("compile on its own"));
}

#[test]
fn a_figure_with_a_reference_image_imitates_its_style() {
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "Accuracy on four benchmarks, like the attached chart.",
        images: 1,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(p.user.contains("show the figure to imitate"));
    assert!(
        p.user
            .contains("layout, colour scheme, marks, line styles, fonts and legend placement")
    );
    assert!(p.user.contains("in TikZ or pgfplots"));
    assert!(
        p.user
            .contains("Never take numbers or labels from the image")
    );
    // Other tasks keep the general wording.
    let edit = build(
        &Request {
            action: Action::Edit,
            ..req
        },
        &skill(),
    );
    assert!(edit.user.contains("follow their layout and style"));
    assert!(!edit.user.contains("figure to imitate"));
}
