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
        icons: Vec::new(),
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
        images: &[ImageUse::Trace, ImageUse::Style],
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
fn a_question_gets_its_answer_only() {
    let protector = Protector::new(Mode::Plain);
    let plain = parse(Action::Ask, "The claim needs a number.", &protector).unwrap();
    assert_eq!(plain.answer.as_deref(), Some("The claim needs a number."));
    assert!(plain.revision.is_none());
    // A reply that rewrites the text anyway: the rewrite is left out.
    let both = parse(
        Action::Ask,
        "<answer>Add the number.</answer><revision>Accuracy rose by 4 points.</revision><translation>准确率提高了 4 个点。</translation><changes_zh>加了数字</changes_zh>",
        &protector,
    )
    .unwrap();
    assert_eq!(both.answer.as_deref(), Some("Add the number."));
    assert!(both.revision.is_none() && both.translation.is_none());
    assert!(both.changes_zh.is_empty());
    // The prompt asks for no rewrite.
    let ask = build(
        &Request {
            action: Action::Ask,
            target: "x",
            instruction: "How should this read?",
            ..Default::default()
        },
        &skill(),
    );
    assert!(ask.user.contains("Give no rewritten version"));
    assert!(!ask.user.contains("<revision>"));
}

#[test]
fn polishing_with_an_instruction_revises_as_it_asks() {
    let request = |instruction| Request {
        action: Action::Polish,
        target: "Results are good.",
        instruction,
        ..Default::default()
    };
    let bare = build(&request(""), &skill());
    assert!(bare.user.contains("Polish the text in <target>"));
    assert!(!bare.user.contains("<instruction>"));
    let asked = build(&request("State the dataset count."), &skill());
    assert!(asked.user.contains("as <instruction> asks"));
    assert!(
        asked
            .user
            .contains("<instruction>\nState the dataset count.")
    );
    // The same rules either way.
    assert_eq!(bare.system, asked.system);
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
fn a_paragraph_lists_its_sentences_where_sentence_at_ends_them() {
    let text = "Intro.\n\nSee Fig. 2 for $x = 1.5$. The model, e.g. a GNN, wins!  Next one?\n";
    let pos = text.find("model").unwrap();
    let (para, sentences) = sentences_at(text, Mode::Latex, pos).unwrap();
    assert!(text[para].starts_with("See Fig. 2"));
    let found: Vec<&str> = sentences.iter().map(|r| &text[r.clone()]).collect();
    assert_eq!(
        found,
        [
            "See Fig. 2 for $x = 1.5$.",
            "The model, e.g. a GNN, wins!",
            "Next one?"
        ]
    );
    // Each is the one sentence_at finds there.
    for r in &sentences {
        assert_eq!(sentence_at(text, Mode::Latex, r.start), Some(r.clone()));
    }
    let zh = "第一句。第二句很短！第三句";
    let (_, sentences) = sentences_at(zh, Mode::Plain, 0).unwrap();
    assert_eq!(sentences.len(), 3);
    assert_eq!(&zh[sentences[2].clone()], "第三句");
    assert!(sentences_at("a\n\n\n\nb", Mode::Plain, 2).is_none());
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
    assert!(
        p.user
            .contains("never colours of the document class or its style files")
    );
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
        images: &[ImageUse::Style],
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(
        p.user.contains(
            "The attached image is from another paper and is attached for its look alone."
        )
    );
    assert!(p.user.contains("colour scheme, line weights, arrow styles, the marks, line styles and legend placement of a chart"));
    assert!(
        p.user
            .contains("take no text, numbers, formulas or content from it.")
    );
    assert!(!p.user.contains("draft"));
    assert!(p.user.contains("describing the figure"));
    // Other tasks keep the general wording.
    let edit = build(
        &Request {
            action: Action::Edit,
            ..req
        },
        &skill(),
    );
    assert!(edit.user.contains("follow their layout and style"));
    assert!(!edit.user.contains("look alone"));
}

#[test]
fn a_block_without_its_closing_tag_ends_at_the_next_block() {
    let protector = Protector::new(Mode::Plain);
    let reply = "<revision>\nBetter text.\n<translation>\n更好的文本。\n</translation>\n<changes_en>\n- tighter\n</changes_en>";
    let a = parse(Action::Polish, reply, &protector).unwrap();
    assert_eq!(a.revision.as_deref(), Some("Better text."));
    assert_eq!(a.translation.as_deref(), Some("更好的文本。"));
    assert_eq!(a.changes_en, ["tighter"]);
}

#[test]
fn a_fenced_code_block_keeps_its_fence() {
    let protector = Protector::new(Mode::Markdown);
    let target = "```python\nprint( 1 )\n```";
    let reply = "<revision>\n```python\nprint(1)\n```\n</revision>";
    let a = parse_for(Action::Polish, reply, &protector, target).unwrap();
    assert_eq!(a.revision.as_deref(), Some("```python\nprint(1)\n```"));
    // Prose that came wrapped in a fence loses it.
    let reply = "<revision>\n```\nBetter prose.\n```\n</revision>";
    let a = parse_for(Action::Polish, reply, &protector, "Prose.").unwrap();
    assert_eq!(a.revision.as_deref(), Some("Better prose."));
}

#[test]
fn tags_mentioned_inside_the_revision_are_text() {
    let protector = Protector::new(Mode::Plain);
    let reply = "<revision>\nUse <translation> tags, see the <answer> element.\n</revision>\n<translation>\n译文。\n</translation>";
    let a = parse(Action::Polish, reply, &protector).unwrap();
    assert_eq!(
        a.revision.as_deref(),
        Some("Use <translation> tags, see the <answer> element.")
    );
    assert_eq!(a.translation.as_deref(), Some("译文。"));
    // Unclosed, a tag mentioned mid-line doesn't cut it short either.
    let reply = "<revision>\nSee the <answer> element.\n<translation>\n译文。";
    let a = parse(Action::Polish, reply, &protector).unwrap();
    assert_eq!(a.revision.as_deref(), Some("See the <answer> element."));
    assert_eq!(a.translation.as_deref(), Some("译文。"));
}

#[test]
fn a_repeated_opening_tag_ends_the_block() {
    let protector = Protector::new(Mode::Plain);
    let reply = "<revision>\nA.\n<revision>\nB.\n</revision>\n<translation>\n甲。\n</translation>";
    let a = parse(Action::Polish, reply, &protector).unwrap();
    assert_eq!(a.revision.as_deref(), Some("A."));
    assert_eq!(a.translation.as_deref(), Some("甲。"));
    // An answer that mentions the revision tag mid-line has no revision.
    let reply = "<answer>\nWrap it in a <revision> tag.\n</answer>";
    let a = parse(Action::Ask, reply, &protector).unwrap();
    assert_eq!(a.answer.as_deref(), Some("Wrap it in a <revision> tag."));
}

#[test]
fn a_figure_as_a_file_asks_for_its_parts_at_print_size() {
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "The pipeline as boxes.",
        figure: FigureFormat::Pptx,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(p.user.contains("as a diagram for PowerPoint"));
    assert!(p.user.contains("8.5 cm wide for one column"));
    assert!(p.user.contains("<scene>\n{\"wide\": false,"));
    assert!(p.user.contains("<name>\na short file name"));
    assert!(
        p.user
            .contains("<caption>\nthe caption in English\n</caption>")
    );
    assert!(
        p.user
            .contains("<translation>\nthe caption in Chinese (Simplified)")
    );
    assert!(!p.user.contains("<revision>"));
    let pdf = build(
        &Request {
            figure: FigureFormat::Pdf,
            ..req.clone()
        },
        &skill(),
    );
    assert!(pdf.user.contains("built with the standalone class"));
    assert!(
        pdf.user
            .contains("<revision>\nthe picture alone, from \\begin{tikzpicture} to \\end{tikzpicture}\n</revision>")
    );
    assert!(
        pdf.user
            .contains("<preamble>\nthe \\usepackage and \\usetikzlibrary lines")
    );
    // In Markdown a figure stays text.
    let md = build(
        &Request {
            mode: Mode::Markdown,
            ..req
        },
        &skill(),
    );
    assert!(md.user.contains("<revision>") && !md.user.contains("<scene>"));
}

#[test]
fn a_figure_as_a_file_is_read_with_its_name_and_caption() {
    let reply = "<name>\npaired-runs\n</name>\n<scene>\n```json\n{\"nodes\": []}\n```\n</scene>\n<caption>\nA faulty run and its paired run.\n</caption>\n<translation>\n故障运行与配对运行。\n</translation>\n<changes_zh>\n两个方框\n</changes_zh>";
    let a = parse_figure(FigureFormat::Pptx, reply).unwrap();
    assert_eq!(a.name, "paired-runs");
    assert_eq!(a.body, "{\"nodes\": []}");
    assert_eq!(a.caption, "A faulty run and its paired run.");
    assert_eq!(a.translation.as_deref(), Some("故障运行与配对运行。"));
    assert_eq!(a.changes_zh, ["两个方框"]);
    let pdf = "<revision>\n\\begin{tikzpicture}\\draw (0,0) -- (1,1);\\end{tikzpicture}\n</revision>\n<preamble>\n\\usetikzlibrary{arrows.meta}\n</preamble>\n<caption>\nA line.\n</caption>";
    let a = parse_figure(FigureFormat::Pdf, pdf).unwrap();
    assert!(a.body.starts_with("\\begin{tikzpicture}"));
    assert_eq!(a.preamble, "\\usetikzlibrary{arrows.meta}");
    assert_eq!(a.name, "");
    assert_eq!(
        parse_figure(FigureFormat::Pptx, "<caption>x</caption>"),
        Err(AssistError::NoRevision)
    );
}

#[test]
fn a_powerpoint_figure_follows_the_skill_for_system_figures() {
    let mut skill = skill();
    skill.files.extend([
        (
            "paper-figure-pptx/SKILL.md".into(),
            "ARROWS AND BOXES".into(),
        ),
        (
            "paper-figure-pptx/references/layout-and-type.md".into(),
            "TYPE SIZES".into(),
        ),
        ("knowledge/figure-archetypes.md".into(), "ARCHETYPES".into()),
    ]);
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "The pipeline.",
        figure: FigureFormat::Pptx,
        ..Default::default()
    };
    let p = build(&req, &skill);
    assert!(
        p.system
            .contains("<skill_file path=\"paper-figure-pptx/SKILL.md\">\nARROWS AND BOXES")
    );
    assert!(p.system.contains("TYPE SIZES") && p.system.contains("ARCHETYPES"));
    assert!(
        p.system
            .contains("They govern the figures and the wording you produce.")
    );
    assert!(
        !p.system.contains("FIGURE RULES"),
        "the data-figure rules are not this figure's"
    );
    assert!(
        p.user
            .contains("Follow the skill's rules for system and motivation figures")
    );
    // A figure in LaTeX code or a PDF picture keeps the data-figure rules.
    for figure in [FigureFormat::Latex, FigureFormat::Pdf] {
        let p = build(
            &Request {
                figure,
                ..req.clone()
            },
            &skill,
        );
        assert!(p.system.contains("FIGURE RULES"));
        assert!(!p.system.contains("ARROWS AND BOXES"));
    }
    // Other tasks are about wording.
    let polish = build(
        &Request {
            action: Action::Polish,
            ..req
        },
        &skill,
    );
    assert!(
        polish
            .system
            .contains("They govern the wording you produce.")
    );
}

#[test]
fn an_image_to_trace_becomes_a_powerpoint_scene() {
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "临摹这张系统图",
        images: &[ImageUse::Trace],
        figure: FigureFormat::Pptx,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(p.user.contains("The attached image is a draft of this figure from the author's own paper. Redraw it as the scene in native shapes"));
    assert!(
        p.user
            .contains("Unicode symbols, subscripts and superscripts")
    );
    // Corrected against the text, and the notes say what changed.
    assert!(
        p.user.contains(
            "Check every label, symbol, formula, number and step against the author's text"
        )
    );
    assert!(
        p.user
            .contains("naming each correction made to the traced draft")
    );
    assert!(!p.user.contains("describing the figure"));
    assert!(!p.user.contains("in TikZ or pgfplots"));
    // As a PDF picture it is TikZ, as before.
    let pdf = build(
        &Request {
            figure: FigureFormat::Pdf,
            ..req
        },
        &skill(),
    );
    assert!(pdf.user.contains("in TikZ or pgfplots"));
    assert!(
        pdf.user
            .contains("Redraw it in TikZ, or in pgfplots for a chart")
    );
}

#[test]
fn a_draft_to_trace_and_figures_to_take_the_look_of() {
    let req = Request {
        action: Action::Figure,
        mode: Mode::Latex,
        target: "We compare the methods.",
        instruction: "Draw the pipeline.",
        images: &[ImageUse::Style, ImageUse::Trace, ImageUse::Style],
        figure: FigureFormat::Pptx,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(
        p.user
            .contains(" Attached image 2 is a draft of this figure")
    );
    assert!(p.user.contains(" Attached images 1 and 3 are from another paper and are attached for their look alone. Take their layout"));
    assert!(
        p.user
            .contains("take no text, numbers, formulas or content from them.")
    );
    assert!(
        p.user
            .contains("The draft decides what the figure shows and where")
    );
    // Two drafts and a figure in LaTeX.
    let p = build(
        &Request {
            images: &[ImageUse::Trace, ImageUse::Trace],
            figure: FigureFormat::Latex,
            ..req
        },
        &skill(),
    );
    assert!(p.user.contains(" Attached images 1 and 2 are drafts of this figure from the author's own paper. Redraw them in LaTeX"));
    assert!(!p.user.contains("look alone"));
    // Outside LaTeX an image is a reference, as for any task.
    let md = build(
        &Request {
            mode: Mode::Markdown,
            ..req
        },
        &skill(),
    );
    assert!(md.user.contains("follow their layout and style"));
    assert!(!md.user.contains("draft"));
}

#[test]
fn the_authors_papers_give_the_voice_of_what_is_written() {
    let voice = vec![
        "We study how a table is cleaned.".to_owned(),
        "Each row is checked once.".to_owned(),
    ];
    let req = Request {
        action: Action::Polish,
        mode: Mode::Latex,
        target: "The method is good.",
        voice: &voice,
        ..Default::default()
    };
    let p = build(&req, &skill());
    assert!(
        p.user
            .contains("<voice_sample n=\"1\">\nWe study how a table is cleaned.\n</voice_sample>")
    );
    assert!(p.user.contains("<voice_sample n=\"2\">"));
    assert!(p.user.contains("write in the voice they show"));
    for action in [Action::Edit, Action::Mirror, Action::Write] {
        let p = build(
            &Request {
                action,
                instruction: "x",
                ..req.clone()
            },
            &skill(),
        );
        assert!(p.user.contains("<voice_sample n=\"1\">"), "{action:?}");
    }
    // Questions and figures are not the author's prose.
    for action in [Action::Ask, Action::Figure] {
        let p = build(
            &Request {
                action,
                instruction: "x",
                ..req.clone()
            },
            &skill(),
        );
        assert!(!p.user.contains("voice_sample"), "{action:?}");
    }
    // Without papers nothing changes.
    let plain = build(
        &Request {
            voice: &[],
            ..req.clone()
        },
        &skill(),
    );
    assert!(!plain.user.contains("voice"));
}
