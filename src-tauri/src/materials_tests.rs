use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-materials-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn write(path: &Path, text: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const ICON: &str = "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\"><circle cx=\"32\" cy=\"32\" r=\"20\" fill=\"#DCE6F2\" stroke=\"#1F3864\" stroke-width=\"2.4\"/></svg>";

/// A paper in a folder of its own: the main file and a section it inputs.
fn paper(dir: &Path) {
    write(
        &dir.join("main.tex"),
        "\\documentclass{article}\n% my notes\n\\begin{document}\nThe opening paragraph of the paper.\n\n\\input{sections/method} % the method\n\\input{../../outside}\n\\end{document}\n",
    );
    write(
        &dir.join("sections/method.tex"),
        "The method checks each row once.\n% a remark\n",
    );
}

/// A Word file with the paragraphs `paras` (XML text, entities as they are).
fn docx(paras: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let mut out = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut out);
    let body: String = paras
        .iter()
        .map(|p| format!("<w:p><w:pPr><w:jc w:val=\"both\"/></w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">{p}</w:t></w:r><w:r><w:t/></w:r></w:p>"))
        .collect();
    zip.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(
        format!("<?xml version=\"1.0\"?><w:document xmlns:w=\"x\"><w:body>{body}<w:p/></w:body></w:document>")
            .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();
    out.into_inner()
}

#[test]
fn word_files_give_their_paragraphs() {
    let text = docx_text(&docx(&["One &lt;two&gt; &#x4E2D;&#25991;", "Three"])).unwrap();
    assert_eq!(text, "One <two> 中文\n\nThree");
    assert_eq!(unescape("a &amp;amp; b &bogus; c"), "a &amp; b &bogus; c");
}

#[test]
fn a_new_folder_has_the_skills_folders_and_says_what_goes_where() {
    let dir = temp("own");
    let m = Materials::new(dir.join("materials"));
    m.ensure_own().unwrap();
    for name in CATEGORIES {
        assert!(dir.join("materials").join(name).is_dir(), "{name}");
    }
    let readme = std::fs::read_to_string(dir.join("materials/README.md")).unwrap();
    assert!(readme.contains("writing-samples/") && readme.contains("icons/"));
    // The author's README stays.
    std::fs::write(dir.join("materials/README.md"), "mine").unwrap();
    m.ensure_own().unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("materials/README.md")).unwrap(),
        "mine"
    );
    // The skill folder's own materials are used as they are.
    assert_eq!(
        m.root(Some(Path::new("/x/skill"))),
        Path::new("/x/skill/materials")
    );
    assert_eq!(m.root(None), dir.join("materials"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn writing_samples_give_the_voice_with_their_inputs_put_in() {
    let dir = temp("voice");
    let root = dir.join("materials");
    write(&dir.join("outside.tex"), "A file outside the materials.");
    write(&root.join("writing-samples/README.md"), "What goes here.");
    write(
        &root.join("writing-samples/a-note.md"),
        "A note in my voice.",
    );
    paper(&root.join("writing-samples/tii-paper"));
    let mut long = String::new();
    for i in 0..60 {
        long.push_str(&format!(
            "Paragraph {i} of a long sample says one thing.\n\n"
        ));
    }
    write(&root.join("writing-samples/long.txt"), &long);
    write(&root.join("writing-samples/z-last.txt"), "Never read.");
    let voice = Materials::voice(&root);
    assert_eq!(voice.len(), 3, "{voice:?}");
    assert_eq!(voice[0], "A note in my voice.");
    assert!(voice[1].starts_with("Paragraph 0 of a long sample"));
    assert!(
        voice[1].chars().count() <= VOICE_CHARS,
        "cut at a paragraph"
    );
    // The paper's main file, with its section and without comments, and
    // nothing from outside the folder.
    assert!(voice[2].starts_with("The opening paragraph of the paper."));
    assert!(voice[2].contains("The method checks each row once."));
    assert!(!voice[2].contains("remark") && !voice[2].contains("my notes"));
    assert!(!voice[2].contains("outside the materials"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn icons_come_from_svg_files_slides_and_libraries() {
    use biwrite_core::assist::SkillIcon;
    let dir = temp("icons");
    let root = dir.join("materials");
    let icons = root.join("icons");
    write(&icons.join("data-pool.svg"), ICON);
    write(
        &icons.join("evil.svg"),
        "<svg><script>alert(1)</script></svg>",
    );
    // A library as the skill keeps them; its SVG files count once.
    write(
        &icons.join("lab/manifest.json"),
        r#"{"icons": [{"id": "gear", "meaning": "a step of the method", "svg": "svg/gear.svg", "png": ""}]}"#,
    );
    write(
        &icons.join("lab/svg/gear.svg"),
        ICON.replace("r=\"20\"", "r=\"9\""),
    );
    // A PowerPoint file with two SVG pictures.
    let ring = SkillIcon {
        id: "ring".into(),
        meaning: "环".into(),
        library: "starter".into(),
        svg: ICON.replace("r=\"20\"", "r=\"14\""),
        png: None,
    };
    let ring2 = SkillIcon {
        id: "ring2".into(),
        svg: ICON.replace("r=\"20\"", "r=\"12\""),
        ..ring.clone()
    };
    let scene = r##"{"nodes": [
      {"id": "a", "x": 0, "y": 0, "w": 1, "h": 1, "icon": "ring"},
      {"id": "b", "x": 2, "y": 0, "w": 1, "h": 1, "icon": "ring2"}
    ]}"##;
    let pptx = crate::figure_scene::Scene::parse(scene)
        .unwrap()
        .laid_with(&[ring, ring2])
        .unwrap()
        .pptx()
        .unwrap();
    write(&icons.join("deck.pptx"), &pptx);
    let m = Materials::new(dir.join("own"));
    let found = m.icons(&root);
    let named: Vec<(&str, &str)> = found
        .iter()
        .map(|i| (i.id.as_str(), i.meaning.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            ("materials/gear", "a step of the method"),
            ("materials/data-pool", "data pool"),
            ("materials/deck-1", "ring"),
            ("materials/deck-2", "ring2"),
        ]
    );
    assert!(found.iter().all(|i| i.library == LIBRARY));
    // A PNG for programs without SVG (a library may leave it out).
    assert!(
        found[1..]
            .iter()
            .all(|i| i.png.as_ref().is_some_and(|p| p.starts_with(b"\x89PNG")))
    );
    // Read once while the files stay as they are.
    assert!(Arc::ptr_eq(&found, &m.icons(&root)));
    write(
        &icons.join("rule-base.svg"),
        ICON.replace("r=\"20\"", "r=\"7\""),
    );
    let again = m.icons(&root);
    assert_eq!(again.len(), 5);
    assert_eq!(again[4].id, "materials/rule-base");
    std::fs::remove_dir_all(dir).unwrap();
}

/// `pptx` with `from` replaced by `to` in its first slide.
fn edit_slide(pptx: &[u8], from: &str, to: &str) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(pptx)).unwrap();
    let mut out = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut out);
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).unwrap();
        if name == "ppt/slides/slide1.xml" {
            bytes = String::from_utf8(bytes)
                .unwrap()
                .replace(from, to)
                .into_bytes();
        }
        w.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(&bytes).unwrap();
    }
    w.finish().unwrap();
    out.into_inner()
}

#[test]
fn slide_pictures_without_a_name_are_counted_and_left_out() {
    use biwrite_core::assist::SkillIcon;
    let dir = temp("unnamed");
    let root = dir.join("materials");
    let ring = SkillIcon {
        id: "ring".into(),
        meaning: "环".into(),
        library: "starter".into(),
        svg: ICON.to_owned(),
        png: None,
    };
    let scene = r#"{"nodes": [{"id": "a", "x": 0, "y": 0, "w": 1, "h": 1, "icon": "ring"}]}"#;
    let pptx = crate::figure_scene::Scene::parse(scene)
        .unwrap()
        .laid_with(&[ring])
        .unwrap()
        .pptx()
        .unwrap();
    // As PowerPoint names a picture nobody described.
    write(
        &root.join("icons/deck.pptx"),
        edit_slide(&pptx, "descr=\"ring\"", "descr=\"\""),
    );
    let m = Materials::new(dir.join("own"));
    let view = m.view(&root, false);
    assert_eq!(view.unnamed_icons, 1);
    assert_eq!(view.counts.last().map(|c| c.files), Some(0));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pictures_named_by_number_say_nothing() {
    for name in ["", "图片 3", "Picture 7", "Graphic 12", "图形 2", "Icon 5"] {
        assert!(generic(name), "{name}");
    }
    for name in ["ring", "Picture of a pipeline", "数据池", "Icon library 2a"] {
        assert!(!generic(name), "{name}");
    }
}

#[test]
fn the_assistant_takes_papers_and_figures_of_the_folder() {
    let dir = temp("list");
    let root = dir.join("materials");
    write(&dir.join("outside.tex"), "A file outside the materials.");
    paper(&root.join("domain/uniclean"));
    write(
        &root.join("general/proposal.md"),
        "The proposal says what we do.",
    );
    write(
        &root.join("general/grant.docx"),
        docx(&["The grant asks for &amp; plans", "A second paragraph."]),
    );
    write(&root.join("domain/old-paper.pdf"), b"%PDF-1.5");
    write(&root.join("writing-samples/voice.txt"), "My voice.");
    write(&root.join("figures/system.png"), b"\x89PNG\r\n\x1a\nrest");
    write(&root.join("figures/old/pipeline.pdf"), b"%PDF-1.5");
    write(&root.join("figures/ring.svg"), ICON);
    write(&root.join("figures/deck.pptx"), b"PK");
    write(&root.join("ppt/talk.pptx"), b"PK");
    let listed: Vec<(String, String, String)> = Materials::list(&root)
        .into_iter()
        .map(|e| (e.folder, e.path, e.kind))
        .collect();
    let want = |f: &str, p: &str, k: &str| (f.to_owned(), p.to_owned(), k.to_owned());
    assert_eq!(
        listed,
        [
            want("domain", "domain/old-paper.pdf", "pdftext"),
            want("domain", "domain/uniclean/main.tex", "text"),
            want("general", "general/grant.docx", "text"),
            want("general", "general/proposal.md", "text"),
            want("writing-samples", "writing-samples/voice.txt", "text"),
            want("figures", "figures/deck.pptx", "pptx"),
            want("figures", "figures/old/pipeline.pdf", "pdf"),
            want("figures", "figures/ring.svg", "image"),
            want("figures", "figures/system.png", "image"),
        ]
    );
    let grant = Materials::reference(&root, "general/grant.docx").unwrap();
    assert_eq!(
        grant.text,
        "The grant asks for & plans\n\nA second paragraph."
    );
    let paper = Materials::reference(&root, "domain/uniclean/main.tex").unwrap();
    assert_eq!(paper.name, "main.tex");
    assert!(paper.text.contains("The method checks each row once."));
    assert!(!paper.text.contains("outside the materials"));
    let (name, media, png) = Materials::image(&root, "figures/ring.svg").unwrap();
    assert_eq!((name.as_str(), media), ("ring.svg", "image/png"));
    assert!(png.starts_with(b"\x89PNG"));
    assert_eq!(
        Materials::image(&root, "figures/system.png").unwrap().1,
        "image/png"
    );
    // Nothing outside the folder.
    assert!(Materials::file(&root, "../outside.tex").is_none());
    assert!(Materials::file(&root, "/etc/hosts").is_none());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("outside.tex"), root.join("domain/link.tex")).unwrap();
        assert!(Materials::file(&root, "domain/link.tex").is_none());
        assert!(Materials::reference(&root, "domain/link.tex").is_err());
    }
    assert!(Materials::reference(&root, "figures/system.png").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_view_counts_what_each_folder_gives() {
    let dir = temp("view");
    let root = dir.join("materials");
    paper(&root.join("domain/uniclean"));
    write(&root.join("domain/notes.md"), "Notes.");
    write(&root.join("writing-samples/voice.txt"), "My voice.");
    write(&root.join("figures/system.png"), b"\x89PNG");
    write(&root.join("icons/data-pool.svg"), ICON);
    let m = Materials::new(dir.join("own"));
    let view = m.view(&root, true);
    assert!(view.in_skill);
    let counts: Vec<(&str, usize)> = view
        .counts
        .iter()
        .map(|c| (c.name.as_str(), c.files))
        .collect();
    assert_eq!(
        counts,
        [
            ("domain", 2),
            ("general", 0),
            ("writing-samples", 1),
            ("figures", 1),
            ("icons", 1)
        ]
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn package_paths_resolve_from_their_folder() {
    assert_eq!(
        resolve("ppt/slides", "../media/image3.svg"),
        "ppt/media/image3.svg"
    );
    assert_eq!(resolve("ppt/slides", "/ppt/media/x.png"), "ppt/media/x.png");
    assert_eq!(
        attribute("<p:cNvPr id=\"5\" name=\"A &amp; B\"", "name").as_deref(),
        Some("A & B")
    );
}

/// Copy the folder `from` to `to`.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn a_skill_folder_brings_its_own_materials_to_the_assistant_and_its_figures() {
    let dir = temp("state");
    let state = crate::pair_tests::app_state(&dir.join("cfg"));
    // Without a skill folder of the author's, the app's own folder.
    assert_eq!(root(&state), dir.join("cfg/materials"));
    let skill_dir = dir.join("skill");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/skills/research-builder"),
        &skill_dir,
    );
    state.settings().skill_folder = Some(skill_dir.display().to_string());
    assert_eq!(root(&state), skill_dir.join("materials"));
    write(
        &skill_dir.join("materials/icons/data-pool.svg"),
        ICON.replace("r=\"20\"", "r=\"18\""),
    );
    write(
        &skill_dir.join("materials/writing-samples/voice.md"),
        "My own voice.",
    );
    let (skill, info) = skill(&state).unwrap();
    assert_eq!(info.origin, "folder");
    assert!(skill.icons.iter().any(|i| i.library == "starter"));
    let mine = skill
        .icons
        .iter()
        .find(|i| i.id == "materials/data-pool")
        .expect("the materials' icon");
    assert_eq!(mine.meaning, "data pool");
    assert_eq!(Materials::voice(&root(&state)), ["My own voice."]);
    // A PowerPoint figure takes it by its id.
    let answer = biwrite_core::assist::FigureAnswer {
        body: r#"{"nodes": [{"id": "a", "x": 0, "y": 0, "w": 1.2, "h": 1.2, "icon": "materials/data-pool", "text": "Data"}]}"#.into(),
        ..Default::default()
    };
    let pptx = crate::figures::laid(&answer, &skill.icons)
        .unwrap()
        .pptx()
        .unwrap();
    let found = extract_icons(&pptx).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].svg, mine.svg);
    std::fs::remove_dir_all(dir).unwrap();
}

/// On a real materials folder, read only (`BIWRITE_MATERIALS`): what each
/// folder gives, the voice, the icons, and a PowerPoint figure as a PDF.
#[tokio::test]
#[ignore = "needs BIWRITE_MATERIALS"]
async fn a_real_materials_folder_is_read_as_it_is() {
    let root = PathBuf::from(std::env::var("BIWRITE_MATERIALS").unwrap());
    let m = Materials::new(temp("real-own"));
    let view = m.view(&root, true);
    println!(
        "counts {:?}, unnamed icons {}",
        view.counts, view.unnamed_icons
    );
    let listed = Materials::list(&root);
    for kind in ["text", "pdftext", "image", "pdf", "pptx"] {
        println!(
            "{kind}: {}",
            listed.iter().filter(|e| e.kind == kind).count()
        );
    }
    for e in listed.iter().filter(|e| e.kind == "text") {
        let r = Materials::reference(&root, &e.path);
        println!(
            "{} -> {}",
            e.path.rsplit('/').next().unwrap_or(""),
            r.map_or_else(|e| format!("error {e}"), |r| format!("{} chars", r.chars))
        );
    }
    let voice = Materials::voice(&root);
    println!(
        "voice: {:?}",
        voice.iter().map(|v| v.chars().count()).collect::<Vec<_>>()
    );
    let icons = m.icons(&root);
    println!("icons: {}", icons.len());
    if let (Some(e), Some(soffice)) = (
        listed.iter().find(|e| e.kind == "pptx"),
        crate::figures::soffice(),
    ) {
        let bytes = std::fs::read(root.join(&e.path)).unwrap();
        let work = temp("real-pptx");
        let pdf = crate::figures::convert(&soffice, &work, "figure", &bytes)
            .await
            .unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        println!("pptx figure -> PDF of {} bytes", pdf.len());
    }
}
