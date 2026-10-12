use super::*;

const SCENE: &str = r##"```json
{
  "name": "Paired Runs!",
  "wide": false,
  "nodes": [
    {"id": "a", "x": 1, "y": 1, "w": 2.4, "h": 0.9, "text": "Paired run", "fill": "#DCE6F2", "line": "#1F4E79"},
    {"id": "b", "x": 5, "y": 1, "w": 2.4, "h": 0.9, "text": "Faulty run\n(IDV 1)", "shape": "rounded", "bold": true},
    {"id": "c", "x": 3, "y": 3, "w": 1.6, "h": 1.0, "text": "Δ < 3σ & x", "shape": "diamond"},
    {"id": "d", "x": 5.4, "y": 3.2, "w": 2.0, "h": 0.6, "text": "note", "shape": "cloud"}
  ],
  "edges": [
    {"from": "a", "to": "b", "kind": "line", "label": "subtract"},
    {"from": "b", "to": "c", "dashed": true, "head": "both"},
    {"from": "a", "to": "c", "elbow": true},
    {"points": [[1, 4.5], [7.4, 4.5]], "head": "none", "color": "999999"}
  ],
  "groups": [
    {"x": 0.6, "y": 0.6, "w": 7.2, "h": 1.7, "label": "Simulator"}
  ]
}
```"##;

fn laid() -> Laid {
    Scene::parse(SCENE).unwrap().laid().unwrap()
}

#[test]
fn a_scene_is_read_from_a_fenced_answer_and_laid_out_tightly() {
    let l = laid();
    assert_eq!(l.name, "paired-runs");
    // The group's extent across (0.6 to 7.8), and from its top (0.6) down
    // to the line at 4.5, with a margin.
    assert!((l.width - (7.2 + 2.0 * MARGIN)).abs() < 1e-9, "{}", l.width);
    assert!(
        (l.height - (3.9 + 2.0 * MARGIN)).abs() < 1e-9,
        "{}",
        l.height
    );
    // The group comes first and the nodes follow, shifted onto the slide.
    assert_eq!(l.items[0].text, "Simulator");
    assert!(l.items[0].top_left);
    let a = &l.items[1];
    assert!((a.frame.x - (1.0 - 0.6 + MARGIN)).abs() < 1e-9);
    assert_eq!(a.fill.as_deref(), Some("DCE6F2"));
    // A shape it does not know is a rectangle.
    assert_eq!(l.items[4].shape, Shape::Other);
    // One label, after the nodes.
    assert_eq!(l.labels_from, 5);
    assert_eq!(l.items[5].text, "subtract");
}

#[test]
fn lines_leave_by_the_facing_sides_and_attach_where_powerpoint_can() {
    let l = laid();
    let ab = &l.lines[0];
    // a is left of b: from a's right side to b's left side.
    assert_eq!(ab.from, Some((1, 3)));
    assert_eq!(ab.to, Some((2, 1)));
    assert!((ab.start.1 - ab.end.1).abs() < 1e-9, "level");
    // b is above c: bottom to top; a diamond attaches too.
    let bc = &l.lines[1];
    assert_eq!(bc.from, Some((2, 2)));
    assert_eq!(bc.to, Some((3, 0)));
    assert_eq!(bc.head, Head::Both);
    // An elbow needs left and right sides; this one runs down, so it is
    // straight.
    assert!(!l.lines[2].elbow);
    // A line between two points is attached to nothing.
    assert_eq!((l.lines[3].from, l.lines[3].to), (None, None));
    assert_eq!(l.lines[3].color, "999999");
}

#[test]
fn bad_scenes_are_refused_with_a_reason() {
    let bad = |json: &str| Scene::parse(json).and_then(|s| s.laid()).unwrap_err();
    assert!(bad("{}").contains("no nodes"));
    assert!(bad("no json here").contains("JSON"));
    assert!(bad(r#"{"nodes":[{"id":"a","x":0,"y":0,"w":0,"h":1}]}"#).contains("no size"));
    assert!(
        bad(r#"{"nodes":[{"id":"a","x":0,"y":0,"w":1,"h":1},{"id":"a","x":2,"y":0,"w":1,"h":1}]}"#)
            .contains("two nodes")
    );
    assert!(
        bad(r#"{"nodes":[{"id":"a","x":0,"y":0,"w":1,"h":1}],"edges":[{"from":"a","to":"z"}]}"#)
            .contains("not both nodes")
    );
    assert!(bad(r#"{"nodes":[{"id":"a","x":0,"y":0,"w":90,"h":1}]}"#).contains("larger"));
}

#[test]
fn a_small_drawing_sits_in_the_middle_of_the_smallest_slide() {
    let l = Scene::parse(r#"{"nodes":[{"id":"a","x":5,"y":5,"w":1,"h":0.5,"text":"x"}]}"#)
        .unwrap()
        .laid()
        .unwrap();
    assert_eq!((l.width, l.height), (MIN_SIDE, MIN_SIDE));
    let f = l.items[0].frame;
    assert!((f.x - (MIN_SIDE - 1.0) / 2.0).abs() < 1e-9);
    assert!((f.y - (MIN_SIDE - 0.5) / 2.0).abs() < 1e-9);
}

/// Every part of `pptx` parses as XML, its tags balanced.
fn parts(pptx: &[u8]) -> Vec<(String, String)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(pptx)).unwrap();
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().unwrap().into_owned();
        // Pictures are bytes; every other part is XML.
        if name.ends_with(".png") {
            continue;
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut f, &mut text).unwrap();
        let mut reader = quick_xml::Reader::from_str(&text);
        reader.config_mut().check_end_names = true;
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("{name}: {e}"),
            }
        }
        out.push((name, text));
    }
    out
}

#[test]
fn the_powerpoint_file_holds_one_slide_of_native_shapes() {
    let l = laid();
    let pptx = l.pptx().unwrap();
    let parts = parts(&pptx);
    let names: Vec<&str> = parts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names[0], "[Content_Types].xml");
    for needed in [
        "ppt/presentation.xml",
        "ppt/slides/slide1.xml",
        "ppt/slideMasters/slideMaster1.xml",
        "ppt/slideLayouts/slideLayout1.xml",
        "ppt/theme/theme1.xml",
    ] {
        assert!(names.contains(&needed), "{needed}");
    }
    let text = |name: &str| parts.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let presentation = text("ppt/presentation.xml");
    assert!(presentation.contains(&format!(
        "<p:sldSz cx=\"{}\" cy=\"{}\"/>",
        emu(l.width),
        emu(l.height)
    )));
    let slide = text("ppt/slides/slide1.xml");
    assert_eq!(
        slide.matches("<p:sp>").count(),
        6,
        "a group, four nodes, a label"
    );
    assert_eq!(slide.matches("<p:cxnSp>").count(), 4);
    assert!(slide.contains("<a:prstGeom prst=\"roundRect\">"));
    assert!(slide.contains("<a:prstGeom prst=\"diamond\">"));
    // Attached connectors name their shapes and sites.
    assert!(slide.contains("<a:stCxn id=\"3\" idx=\"3\"/><a:endCxn id=\"4\" idx=\"1\"/>"));
    // Arrowheads, dashes, colours and escaped text.
    assert!(slide.contains("<a:headEnd type=\"triangle\""));
    assert!(slide.contains("<a:tailEnd type=\"triangle\""));
    assert!(slide.contains("<a:prstDash val=\"dash\"/>"));
    assert!(slide.contains("<a:srgbClr val=\"DCE6F2\"/>"));
    assert!(slide.contains("<a:t>Δ &lt; 3σ &amp; x</a:t>"));
    // Text alone grows with its text; boxes keep their size.
    assert!(slide.contains("<a:spAutoFit/>"));
    assert!(slide.contains("<a:noAutofit/>"));
    // Two lines of text are two paragraphs.
    assert!(slide.contains("<a:t>Faulty run</a:t></a:r></a:p><a:p><a:pPr algn=\"ctr\"/><a:r>"));
    assert!(slide.contains("b=\"1\""));
}

#[test]
fn the_preview_draws_the_same_shapes() {
    let svg = laid().svg();
    assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
    assert!(svg.ends_with("</svg>"));
    assert!(svg.contains("<polygon"), "the diamond");
    assert!(svg.contains("rx="), "the rounded box");
    assert!(svg.contains(">Δ &lt; 3σ &amp; x</tspan>"));
    assert_eq!(
        svg.matches("<path d=").count(),
        4 + 1,
        "four lines and the arrowhead"
    );
    let mut reader = quick_xml::Reader::from_str(&svg);
    reader.config_mut().check_end_names = true;
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn file_names_keep_letters_digits_and_dashes() {
    assert_eq!(file_stem("Paired Runs!"), "paired-runs");
    assert_eq!(file_stem("fig_framework v2"), "fig_framework-v2");
    assert_eq!(file_stem("配对运行"), "");
    assert_eq!(file_stem("--a--b--"), "a-b");
}

/// LibreOffice opens the file and turns it into a one-page PDF of the
/// slide's size.
#[test]
#[ignore = "needs LibreOffice"]
fn libreoffice_turns_the_file_into_a_pdf() {
    let soffice = [
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        "soffice",
    ]
    .into_iter()
    .find(|p| {
        std::process::Command::new(p)
            .arg("--version")
            .output()
            .is_ok()
    })
    .expect("LibreOffice");
    let dir = std::env::temp_dir().join(format!("biwrite-pptx-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let l = laid();
    std::fs::write(dir.join("fig.pptx"), l.pptx().unwrap()).unwrap();
    let profile = format!(
        "-env:UserInstallation=file://{}",
        dir.join("profile").display()
    );
    let out = std::process::Command::new(soffice)
        .args([
            profile.as_str(),
            "--headless",
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(&dir)
        .arg(dir.join("fig.pptx"))
        .output()
        .unwrap();
    let pdf = std::fs::read(dir.join("fig.pdf")).unwrap_or_else(|_| {
        panic!(
            "no PDF: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.starts_with("%PDF"));
    assert_eq!(
        text.matches("/Type /Page\n").count()
            + text.matches("/Type/Page/").count()
            + text.matches("/Type /Page/").count()
            + text.matches("/Type/Page\n").count(),
        1,
        "one page"
    );
    eprintln!("PDF at {}", dir.join("fig.pdf").display());
}

/// A sample system figure after the skill's archetype F (stage containers,
/// a red main flow of block arrows, thin side flows), written where
/// `BIWRITE_PPTX_OUT` says, to open in PowerPoint by hand.
#[test]
#[ignore = "needs BIWRITE_PPTX_OUT"]
fn write_a_sample_for_powerpoint() {
    let scene = r##"{"wide": true, "nodes": [
      {"id": "t1", "x": 0.15, "y": 0.15, "w": 5.1, "h": 0.55, "text": "Stage 1  Detection", "fill": "#1F3864", "line": "#1F3864", "color": "#FFFFFF", "bold": true},
      {"id": "t2", "x": 6.15, "y": 0.15, "w": 5.1, "h": 0.55, "text": "Stage 2  Covering", "fill": "#1F3864", "line": "#1F3864", "color": "#FFFFFF", "bold": true},
      {"id": "t3", "x": 12.15, "y": 0.15, "w": 5.1, "h": 0.55, "text": "Stage 3  Update", "fill": "#1F3864", "line": "#1F3864", "color": "#FFFFFF", "bold": true},
      {"id": "in", "x": 0.75, "y": 1.05, "w": 1.0, "h": 1.0, "icon": "sensor-wave", "text": "Sensor windows"},
      {"id": "det", "x": 2.9, "y": 1.1, "w": 2.2, "h": 0.9, "text": "Constraint\nviolations", "shape": "rounded", "fill": "#FFFFFF", "line": "#2F5597"},
      {"id": "sel", "x": 6.4, "y": 1.1, "w": 2.1, "h": 0.9, "text": "Select event\nby posterior", "shape": "rounded", "fill": "#FFFFFF", "line": "#C55A11"},
      {"id": "ded", "x": 9.0, "y": 1.1, "w": 2.1, "h": 0.9, "text": "Deduct its\nfootprint", "shape": "rounded", "fill": "#FFFFFF", "line": "#C55A11"},
      {"id": "res", "x": 9.0, "y": 2.8, "w": 2.1, "h": 0.8, "text": "Residual", "fill": "#FFFFFF", "line": "#C55A11", "dashed": true},
      {"id": "m", "x": 6.95, "y": 2.65, "w": 1.0, "h": 1.0, "icon": "network-model", "text": "Encoder"},
      {"id": "q", "x": 12.4, "y": 1.0, "w": 2.2, "h": 1.1, "text": "Unknown?", "shape": "diamond", "fill": "#FFFFFF", "line": "#548235"},
      {"id": "kb", "x": 15.3, "y": 1.05, "w": 1.0, "h": 1.0, "icon": "database", "text": "Event knowledge"}
    ], "edges": [
      {"from": "in", "to": "det", "kind": "line", "color": "#595959"},
      {"from": "det", "to": "sel", "kind": "arrow", "color": "#C00000"},
      {"from": "sel", "to": "ded", "kind": "line", "color": "#595959"},
      {"from": "ded", "to": "res", "kind": "line", "dashed": true, "color": "#595959"},
      {"from": "m", "to": "sel", "kind": "line", "color": "#595959"},
      {"from": "ded", "to": "q", "kind": "arrow", "color": "#C00000"},
      {"from": "q", "to": "kb", "kind": "line", "color": "#595959", "label": "yes"}
    ], "groups": [
      {"x": 0, "y": 0, "w": 5.4, "h": 4.2, "fill": "#EEF3FA", "line": "#2F5597", "dashed": false},
      {"x": 6.0, "y": 0, "w": 5.4, "h": 4.2, "fill": "#FDF2EA", "line": "#C55A11", "dashed": false},
      {"x": 12.0, "y": 0, "w": 5.4, "h": 4.2, "fill": "#EEF6EA", "line": "#548235", "dashed": false}
    ]}"##;
    let bundled =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/skills/research-builder");
    let icons = crate::skills::load_icons(&bundled);
    assert_eq!(icons.len(), 16);
    // Another scene from a file, when given.
    let scene = std::env::var("BIWRITE_PPTX_SCENE")
        .map(|p| std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|_| scene.to_owned());
    let laid = Scene::parse(&scene).unwrap().laid_with(&icons).unwrap();
    let out = std::env::var("BIWRITE_PPTX_OUT").unwrap();
    std::fs::write(&out, laid.pptx().unwrap()).unwrap();
    std::fs::write(format!("{out}.svg"), laid.svg()).unwrap();
}

const ARROWS: &str = r##"{"nodes": [
  {"id": "a", "x": 0, "y": 0, "w": 2, "h": 0.8, "text": "Detect"},
  {"id": "b", "x": 3, "y": 0, "w": 2, "h": 0.8, "text": "Cover"},
  {"id": "c", "x": 6, "y": 0, "w": 2, "h": 0.8, "text": "Name"},
  {"id": "d", "x": 6, "y": 2, "w": 2, "h": 0.8, "text": "Update"},
  {"id": "e", "x": 0, "y": 2, "w": 2, "h": 0.8, "text": "Knowledge"}
], "edges": [
  {"from": "a", "to": "b", "color": "#C00000"},
  {"from": "b", "to": "c", "label": "train", "color": "#C00000"},
  {"from": "c", "to": "d"},
  {"from": "d", "to": "e"},
  {"from": "e", "to": "a", "kind": "arrow", "head": "both"}
]}"##;

#[test]
fn short_connections_are_block_arrows_of_one_thickness() {
    let l = Scene::parse(ARROWS).unwrap().laid().unwrap();
    let arrows: Vec<&Item> = l.items.iter().filter(|i| i.shape.is_arrow()).collect();
    let shapes: Vec<Shape> = arrows.iter().map(|i| i.shape).collect();
    assert_eq!(
        shapes,
        [
            Shape::RightArrow,
            Shape::RightArrow,
            Shape::DownArrow,
            Shape::UpDownArrow
        ]
    );
    // One thickness throughout, and equal gaps give equal lengths.
    for a in &arrows {
        assert!(
            (a.frame.w.min(a.frame.h) - ARROW).abs() < 1e-9,
            "{:?}",
            a.frame
        );
    }
    assert!((arrows[0].frame.w - arrows[1].frame.w).abs() < 1e-9);
    assert!((arrows[0].frame.w - (1.0 - 2.0 * ARROW_GAP)).abs() < 1e-9);
    // Across the gap, on the boxes' middle line.
    let a = l.items[0].frame;
    assert!((arrows[0].frame.x - (a.x + a.w + ARROW_GAP)).abs() < 1e-9);
    assert!((arrows[0].frame.y + ARROW / 2.0 - (a.y + a.h / 2.0)).abs() < 1e-9);
    assert_eq!(arrows[0].fill.as_deref(), Some("C00000"));
    // The long way back is a line.
    assert_eq!(l.lines.len(), 1);
    // A label sits above its arrow.
    let label = l.items.iter().find(|i| i.text == "train").unwrap();
    assert!(label.frame.y + label.frame.h <= arrows[1].frame.y);
    let slide = l.slide();
    for preset in ["rightArrow", "downArrow", "upDownArrow"] {
        assert!(slide.contains(&format!("prst=\"{preset}\"")), "{preset}");
    }
    assert_eq!(slide.matches("<p:cxnSp>").count(), 1);
    assert_eq!(l.svg().matches("<polygon").count(), 4);
}

const DB_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><ellipse cx="32" cy="14" rx="20" ry="7" fill="#DCE6F2" stroke="#1F3864" stroke-width="2.4"/><path d="M12 14v34c0 4 9 7 20 7s20-3 20-7V14" fill="none" stroke="#1F3864" stroke-width="2.4"/></svg>"##;

fn icons() -> Vec<SkillIcon> {
    vec![SkillIcon {
        id: "database".into(),
        meaning: "数据集、数据源".into(),
        library: "starter".into(),
        svg: DB_SVG.into(),
        png: None,
    }]
}

#[test]
fn icons_are_svg_pictures_with_a_png_for_other_programs() {
    let scene = r##"{"nodes": [
      {"id": "db", "x": 0, "y": 0, "w": 1.2, "h": 1.2, "icon": "database", "text": "Raw data"},
      {"id": "m", "x": 3, "y": 0, "w": 2.4, "h": 1.2, "icon": "spark"},
      {"id": "x", "x": 6, "y": 0, "w": 2, "h": 1.2, "icon": "nowhere", "text": "Plain box"},
      {"id": "bad", "x": 9, "y": 0, "w": 1.2, "h": 1.2, "icon": "evil"}
    ], "edges": [{"from": "db", "to": "m"}],
    "svgs": {
      "spark": "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 32 16\"><path d=\"M2 8h28\" stroke=\"#C00000\" stroke-width=\"2\"/></svg>",
      "evil": "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\" onload=\"alert(1)\"><rect width=\"8\" height=\"8\"/></svg>"
    }}"##;
    let l = Scene::parse(scene).unwrap().laid_with(&icons()).unwrap();
    let pictures: Vec<&Item> = l.items.iter().filter(|i| i.picture.is_some()).collect();
    assert_eq!(pictures.len(), 2, "the library icon and the drawn one");
    // Fitted to the icon's own proportions inside the node's box.
    let spark = pictures[1].frame;
    assert!((spark.w / spark.h - 2.0).abs() < 1e-6, "{spark:?}");
    // The label under the icon is text of its own.
    let label = l.items.iter().find(|i| i.text == "Raw data").unwrap();
    assert!(label.picture.is_none() && label.frame.y >= pictures[0].frame.y + pictures[0].frame.h);
    // An icon not found, or one that could run code, leaves a box.
    assert!(
        l.items
            .iter()
            .any(|i| i.text == "Plain box" && i.picture.is_none())
    );
    assert!(
        !pictures
            .iter()
            .any(|p| p.picture.as_ref().unwrap().name == "evil")
    );
    // In the file: two pictures, each an SVG with its PNG.
    let pptx = l.pptx().unwrap();
    let parts = parts(&pptx);
    let text = |name: &str| {
        parts
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.clone())
    };
    let slide = text("ppt/slides/slide1.xml").unwrap();
    assert_eq!(slide.matches("<p:pic>").count(), 2);
    assert_eq!(slide.matches("<asvg:svgBlip").count(), 2);
    assert!(slide.contains("r:embed=\"rId2\"") && slide.contains("r:embed=\"rId5\""));
    let rels = text("ppt/slides/_rels/slide1.xml.rels").unwrap();
    assert!(
        rels.contains("Target=\"../media/icon1.png\"")
            && rels.contains("Target=\"../media/icon2.svg\"")
    );
    assert!(
        text("[Content_Types].xml")
            .unwrap()
            .contains("Extension=\"svg\" ContentType=\"image/svg+xml\"")
    );
    let mut zip = zip::ZipArchive::new(Cursor::new(pptx.as_slice())).unwrap();
    let mut png = Vec::new();
    std::io::Read::read_to_end(&mut zip.by_name("ppt/media/icon1.png").unwrap(), &mut png).unwrap();
    assert!(png.starts_with(b"\x89PNG"), "a PNG drawn from the SVG");
    // The preview draws them too.
    assert_eq!(l.svg().matches("<image ").count(), 2);
}

#[test]
fn only_drawings_pass_as_icons() {
    assert!(plain_svg(DB_SVG).is_some());
    for bad in [
        "<svg><script>alert(1)</script></svg>",
        "<svg><image href=\"x.png\"/></svg>",
        "<svg><foreignObject/></svg>",
        "<svg onload =\"x()\"></svg>",
        "<svg><use href=\"http://example.com/a.svg#x\"/></svg>",
        "<svg><style>@import url(http://x)</style></svg>",
        "not svg",
    ] {
        assert!(plain_svg(bad).is_none(), "{bad}");
    }
    // An XML declaration may come first.
    assert_eq!(
        plain_svg("<?xml version=\"1.0\"?>\n<svg viewBox=\"0 0 1 1\"/></svg>").map(|s| &s[..4]),
        Some("<svg")
    );
    // Words that only look like handlers are fine.
    assert!(plain_svg("<svg><title>based on data</title></svg>").is_some());
}

#[test]
fn a_label_wider_than_its_icon_stays_on_the_slide() {
    let icons = crate::skills::load_icons(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/skills/research-builder"),
    );
    let scene = r#"{"nodes": [
      {"id": "data", "x": 0, "y": 0, "w": 1, "h": 1, "icon": "database", "text": "Process data from the plant"},
      {"id": "b", "x": 3, "y": 0, "w": 2, "h": 1, "text": "Model"}
    ]}"#;
    let laid = Scene::parse(scene).unwrap().laid_with(&icons).unwrap();
    let label = laid
        .items
        .iter()
        .find(|i| i.text.starts_with("Process data"))
        .unwrap();
    let icon = laid.items.iter().find(|i| i.picture.is_some()).unwrap();
    // As wide as its text, centred under the icon, and on the slide.
    assert!(label.frame.w > 4.0, "{}", label.frame.w);
    let centre = |f: &Frame| f.x + f.w / 2.0;
    assert!((centre(&label.frame) - centre(&icon.frame)).abs() < 0.01);
    assert!(label.frame.x >= 0.0, "{}", label.frame.x);
    assert!(label.frame.x + label.frame.w <= laid.width + 1e-9);
    // Chinese is about twice as wide as Latin letters.
    assert!(text_width("数据池", 10.0) > text_width("abc", 10.0) * 1.5);
}
