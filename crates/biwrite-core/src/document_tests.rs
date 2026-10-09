use super::*;

fn doc(text: &str) -> DocumentModel {
    let mut d = DocumentModel::new(1);
    d.apply(text.to_owned(), Mode::Plain);
    d
}

fn ids(d: &DocumentModel) -> Vec<u64> {
    d.segments().iter().map(|s| s.id.0).collect()
}

fn paragraphs(n: usize) -> String {
    (0..n)
        .map(|i| format!("Paragraph number {i} talks about topic {i} in some detail."))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[test]
fn initial_ids_are_sequential() {
    let d = doc("a\n\nb\n\nc");
    assert_eq!(ids(&d), vec![1, 2, 3]);
}

#[test]
fn unchanged_text_keeps_everything() {
    let mut d = doc(&paragraphs(5));
    let before = ids(&d);
    let report = d.apply(paragraphs(5), Mode::Plain);
    assert_eq!(report, ApplyReport::default());
    assert_eq!(ids(&d), before);
}

#[test]
fn editing_one_paragraph_keeps_its_id_and_reports_change() {
    let mut d = doc(&paragraphs(5));
    let before = ids(&d);
    let edited = paragraphs(5).replace("topic 2 in some", "topic 2 in great");
    let report = d.apply(edited, Mode::Plain);
    assert_eq!(ids(&d), before);
    assert_eq!(report.changed, vec![SegmentId(before[2])]);
    assert!(report.added.is_empty() && report.removed.is_empty());
}

#[test]
fn rewrapping_is_not_a_change() {
    let mut d = doc("one two three\nfour five");
    let report = d.apply("one two\nthree four   five".to_owned(), Mode::Plain);
    assert_eq!(report, ApplyReport::default());
}

#[test]
fn inserting_a_paragraph_shifts_nothing() {
    let mut d = doc("a a a\n\nb b b\n\nc c c");
    let report = d.apply(
        "a a a\n\nb b b\n\nnew stuff here\n\nc c c".to_owned(),
        Mode::Plain,
    );
    assert_eq!(ids(&d), vec![1, 2, 4, 3]);
    assert_eq!(report.added, vec![SegmentId(4)]);
}

#[test]
fn deleting_a_paragraph() {
    let mut d = doc("a a a\n\nb b b\n\nc c c");
    let report = d.apply("a a a\n\nc c c".to_owned(), Mode::Plain);
    assert_eq!(ids(&d), vec![1, 3]);
    assert_eq!(report.removed, vec![SegmentId(2)]);
}

#[test]
fn moving_a_paragraph_keeps_its_id() {
    let mut d = doc(&paragraphs(6));
    let mut parts: Vec<String> = paragraphs(6).split("\n\n").map(str::to_owned).collect();
    let moved = parts.remove(1);
    parts.insert(4, moved);
    let report = d.apply(parts.join("\n\n"), Mode::Plain);
    assert_eq!(ids(&d), vec![1, 3, 4, 5, 2, 6]);
    assert_eq!(report, ApplyReport::default());
}

#[test]
fn unrelated_replacement_gets_fresh_id() {
    let mut d = doc("The model learns quickly.\n\nEnd.");
    let report = d.apply(
        "Completely different words entirely.\n\nEnd.".to_owned(),
        Mode::Plain,
    );
    assert_eq!(ids(&d), vec![3, 2]);
    assert_eq!(report.added, vec![SegmentId(3)]);
    assert_eq!(report.removed, vec![SegmentId(1)]);
}

#[test]
fn splitting_a_paragraph_reuses_id_for_best_match() {
    let mut d = doc("Intro.\n\nAlpha beta gamma delta. Epsilon zeta eta theta iota.\n\nEnd.");
    d.apply(
        "Intro.\n\nAlpha beta gamma delta.\n\nEpsilon zeta eta theta iota.\n\nEnd.".to_owned(),
        Mode::Plain,
    );
    let got = ids(&d);
    assert_eq!(got[0], 1);
    assert_eq!(got[3], 3);
    // One half keeps ID 2, the other is new.
    assert!(got[1] == 2 || got[2] == 2);
    assert!(got[1] == 4 || got[2] == 4);
}

#[test]
fn changing_mode_turns_markdown_heading_into_heading() {
    let mut d = doc("# Title\n\nBody text.");
    let report = d.apply("# Title\n\nBody text.".to_owned(), Mode::Markdown);
    // Heading content differs ("Title" vs "# Title"), but similar enough to keep the ID.
    assert_eq!(ids(&d), vec![1, 2]);
    assert_eq!(report.changed, vec![SegmentId(1)]);
}

#[test]
fn ids_stay_unique_across_documents() {
    let mut a = DocumentModel::new(1);
    a.apply("x\n\ny".to_owned(), Mode::Plain);
    let mut b = DocumentModel::new(a.next_id());
    b.apply("z".to_owned(), Mode::Plain);
    assert_eq!(ids(&b), vec![3]);
}

#[test]
fn similarity_bounds() {
    assert!((similarity("a b c", "a b c") - 1.0).abs() < f32::EPSILON);
    assert!(similarity("a b c", "x y z") < 0.5);
}

#[test]
fn large_rewrites_get_fresh_ids_instead_of_wrong_pairs() {
    let mut d = doc(&paragraphs(40));
    let mut text = String::from("```\ncode\n```\n\n");
    text.push_str(
        &(0..40)
            .map(|i| format!("Unrelated replacement text block {i} with other words."))
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    let report = d.apply(text, Mode::Markdown);
    assert_eq!(report.removed.len(), 40);
    assert_eq!(report.added.len(), 41);
    assert!(report.changed.is_empty());
}

#[test]
fn skipped_and_translatable_segments_never_pair() {
    let mut d = doc("Some text here.\n\nEnd.");
    let report = d.apply(
        "```\nSome text here.\n```\n\nEnd.".to_owned(),
        Mode::Markdown,
    );
    assert_eq!(report.removed, vec![SegmentId(1)]);
    assert_eq!(report.added.len(), 1);
}

#[test]
fn chinese_edits_keep_ids() {
    let mut d = DocumentModel::new(1);
    d.apply(
        "图神经网络是处理关系数据的标准工具。\n\n第二段。".to_owned(),
        Mode::Plain,
    );
    let report = d.apply(
        "图神经网络是处理关系型数据的标准工具。\n\n第二段。".to_owned(),
        Mode::Plain,
    );
    assert_eq!(ids(&d), vec![1, 2]);
    assert_eq!(report.changed, vec![SegmentId(1)]);
    assert!(similarity("图神经网络是标准工具。", "图神经网络是常用工具。") > 0.6);
}

#[test]
fn pasting_over_long_chinese_sections_stays_fast() {
    let para = |seed: usize| -> String {
        (0..600)
            .map(|k| {
                char::from_u32(0x4E00 + ((seed * 7919 + k * 31) % 20000) as u32).unwrap_or('字')
            })
            .collect()
    };
    let old: Vec<String> = (0..16).map(para).collect();
    let new: Vec<String> = (100..116).map(para).collect();
    let mut d = DocumentModel::new(1);
    d.apply(old.join("\n\n"), Mode::Plain);
    let start = std::time::Instant::now();
    let report = d.apply(new.join("\n\n"), Mode::Plain);
    assert!(
        start.elapsed() < std::time::Duration::from_millis(500),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(report.added.len(), 16);
}
