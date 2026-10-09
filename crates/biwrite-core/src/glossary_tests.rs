use super::*;

fn g(entries: &[(&str, Option<&str>)]) -> Vec<GlossaryEntry> {
    entries
        .iter()
        .map(|(t, tr)| GlossaryEntry::new(t, *tr))
        .collect()
}

fn terms(v: &[GlossaryEntry]) -> Vec<&str> {
    v.iter().map(|e| e.term.as_str()).collect()
}

#[test]
fn english_terms_match_whole_words_and_plurals() {
    let glossary = g(&[
        ("GNN", None),
        ("graph", Some("图")),
        ("in-context learning", Some("上下文学习")),
        ("ontology", Some("本体")),
        ("class", Some("类")),
        ("art", Some("艺术")),
    ]);
    let found = relevant(
        "In-context\nlearning for GNNs on graphs: ontologies and classes.",
        Direction::EnZh,
        &glossary,
    );
    assert_eq!(
        terms(&found),
        ["in-context learning", "GNN", "graph", "ontology", "class"]
    );
    // Substrings of other words don't match ("art" in "start", "graph" in
    // "paragraph"/"graphical").
    assert!(relevant("We start a paragraph.", Direction::EnZh, &glossary).is_empty());
    assert!(relevant("graphical models", Direction::EnZh, &glossary).is_empty());
}

#[test]
fn chinese_source_matches_renderings() {
    let glossary = g(&[
        ("graph neural network", Some("图神经网络")),
        ("GNN", None),
        ("prompt", Some("提示")),
    ]);
    let found = relevant("我们用GNN和图神经网络做实验。", Direction::ZhEn, &glossary);
    assert_eq!(
        found,
        g(&[("GNN", None), ("图神经网络", Some("graph neural network"))])
    );
    assert!(relevant("没有术语。", Direction::ZhEn, &glossary).is_empty());
}

#[test]
fn request_entries_are_capped_and_unique() {
    let many: Vec<GlossaryEntry> = (0..40)
        .map(|i| GlossaryEntry::new(&format!("term{i}"), Some("译")))
        .collect();
    let source: String = (0..40).map(|i| format!("term{i} ")).collect();
    let found = relevant(&source, Direction::EnZh, &many);
    assert_eq!(found.len(), MAX_PER_REQUEST);
    assert_eq!(found[0].term, "term0");
    // Two terms with one rendering are sent once in zh→en.
    let dup = g(&[("model", Some("模型")), ("models", Some("模型"))]);
    assert_eq!(relevant("模型", Direction::ZhEn, &dup).len(), 1);
}

#[test]
fn normalizing_trims_merges_and_reads_keep() {
    let raw = g(&[
        ("  GNN ", Some("KEEP")),
        ("", Some("空")),
        ("graph\n neural  network", Some(" 图神经网络 ")),
        ("gnn", Some("")),
        ("Prompt", Some("提示")),
        ("prompt", Some("提示词")),
    ]);
    assert_eq!(
        normalized(raw),
        g(&[
            ("gnn", None),
            ("graph neural network", Some("图神经网络")),
            ("prompt", Some("提示词")),
        ])
    );
}

#[test]
fn csv_import_handles_bom_header_and_keep() {
    let file = "\u{feff}term,translation\r\nGNN,KEEP\r\n\"graph, neural\",图\r\nprompt,\r\n,ignored\r\n\r\nlabel\r\n";
    assert_eq!(
        from_csv(file.as_bytes()).unwrap(),
        g(&[
            ("GNN", None),
            ("graph, neural", Some("图")),
            ("prompt", None),
            ("label", None),
        ])
    );
    // No header: the first row is data.
    assert_eq!(from_csv(b"a,b\n").unwrap(), g(&[("a", Some("b"))]));
}

#[test]
fn csv_import_rejects_non_utf8() {
    // "图神" in GBK.
    assert_eq!(
        from_csv(b"term,translation\ngraph,\xcd\xbc\xc9\xf1\n").unwrap_err(),
        GlossaryError::NotUtf8
    );
    assert!(matches!(
        from_csv(b"\"open\n").unwrap_err(),
        GlossaryError::Csv(_)
    ));
}

#[test]
fn csv_round_trip() {
    let entries = g(&[
        ("GNN", None),
        ("graph, neural", Some("图\"神经\"网络")),
        ("in-context learning", Some("上下文学习")),
    ]);
    let csv = to_csv(&entries);
    assert!(csv.starts_with("\u{feff}term,translation\r\n"));
    assert_eq!(from_csv(csv.as_bytes()).unwrap(), entries);
}

#[test]
fn index_finds_terms_by_any_word_form() {
    let glossary = Glossary::new(g(&[
        ("GPT-4", None),
        ("C++", Some("C++语言")),
        ("(x)", Some("括号")),
        ("ontology", Some("本体")),
        ("graph neural network", Some("图神经网络")),
    ]));
    let found = glossary.relevant(
        "Ontologies, graph neural networks, GPT-4 and C++ (x).",
        Direction::EnZh,
    );
    assert_eq!(
        terms(&found),
        ["ontology", "graph neural network", "GPT-4", "C++", "(x)"]
    );
    assert_eq!(glossary.entries().len(), 5);
}

#[test]
fn large_glossaries_are_supported_up_to_the_cap() {
    let entries: Vec<GlossaryEntry> = (0..MAX_ENTRIES)
        .map(|i| GlossaryEntry::new(&format!("term{i}"), Some(&format!("术语{i}"))))
        .collect();
    let csv = to_csv(&entries);
    let imported = from_csv(csv.as_bytes()).unwrap();
    assert_eq!(imported.len(), MAX_ENTRIES);
    let glossary = Glossary::new(imported);
    let paragraph = "We use term42 and term9999 here, plus 术语7。";
    assert_eq!(
        terms(&glossary.relevant(paragraph, Direction::EnZh)),
        ["term42", "term9999"]
    );
    assert_eq!(
        terms(&glossary.relevant(paragraph, Direction::ZhEn)),
        ["术语7"]
    );
    // One more is refused.
    let mut too_many = entries;
    too_many.push(GlossaryEntry::new("extra", None));
    assert_eq!(
        from_csv(to_csv(&too_many).as_bytes()).unwrap_err(),
        GlossaryError::TooMany {
            count: MAX_ENTRIES + 1
        }
    );
}

#[test]
fn fingerprints_identify_entry_sets() {
    let a = g(&[("GNN", None)]);
    let b = g(&[("GNN", Some("图神经网络"))]);
    assert_eq!(fingerprint(&[]), 0);
    assert_ne!(fingerprint(&a), 0);
    assert_ne!(fingerprint(&a), fingerprint(&b));
    assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
    assert!(i64::try_from(fingerprint(&b)).is_ok());
    // Field boundaries are unambiguous.
    assert_ne!(
        fingerprint(&g(&[("ab", Some("c"))])),
        fingerprint(&g(&[("a", Some("bc"))]))
    );
}
