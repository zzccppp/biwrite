use super::*;

const PAPER: &str = "\\documentclass[journal]{ieeecolor}\n\
\\usepackage{graphicx}\n\
\\newcommand{\\figdir}{../figures/}\n\
\\begin{document}\n\
\\begin{figure}[t]\n\
\\includegraphics[width=\\columnwidth]{\\figdir fig_motivation.pdf}\n\
\\end{figure}\n\
\\begin{figure*}[t]\n\
\\includegraphics[width=\\textwidth]{\\figdir fig_framework.pdf}\n\
\\end{figure*}\n\
% \\includegraphics{old/fig.pdf}\n\
\\begin{figure}[t]\n\
\\includegraphics[width=\\columnwidth]{\\figdir fig_results.pdf}\n\
\\end{figure}\n\
\\end{document}\n";

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-figures-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_new_figure_follows_the_documents_figures() {
    let p = place(PAPER, "", None);
    assert_eq!(
        p,
        Place {
            prefix: "\\figdir ".into(),
            dir: "../figures/".into(),
            stem_prefix: "fig_".into(),
            width: "\\columnwidth".into(),
        }
    );
    // Plain folders, in the open part of the document too.
    let root = "\\documentclass{article}\n\\begin{document}\n\\input{intro}\n\\end{document}";
    let part =
        "\\includegraphics[width=0.8\\linewidth]{img/plot.png}\n\\includegraphics{img/arch.pdf}";
    let p = place(root, part, None);
    assert_eq!((p.prefix.as_str(), p.dir.as_str()), ("img/", "img/"));
    assert_eq!(
        (p.stem_prefix.as_str(), p.width.as_str()),
        ("", "\\linewidth")
    );
}

#[test]
fn without_figures_the_graphics_path_or_a_figures_folder() {
    let root = "\\documentclass{article}\n\\usepackage{graphicx}\n\\graphicspath{{pics}{other/}}\n\\begin{document}\n\\end{document}";
    let p = place(root, "", None);
    assert_eq!((p.prefix.as_str(), p.dir.as_str()), ("", "pics/"));
    let plain = "\\documentclass{article}\n\\begin{document}\n\\end{document}";
    assert_eq!(place(plain, "", None).prefix, "figures/");
    let dir = temp("place");
    std::fs::create_dir_all(dir.join("figs")).unwrap();
    assert_eq!(place(plain, "", Some(&dir)).dir, "figs/");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_environment_includes_the_file_with_a_caption_and_a_label() {
    let p = place(PAPER, "", None);
    let env = environment(
        &p,
        "\\figdir fig_paired.pdf",
        "Paired runs.",
        "paired",
        false,
        None,
    );
    assert_eq!(
        env,
        "\\begin{figure}[t]\n\\centering\n\\includegraphics[width=\\columnwidth]{\\figdir fig_paired.pdf}\n\\caption{Paired runs.}\n\\label{fig:paired}\n\\end{figure}"
    );
    let wide = environment(&p, "x.pdf", "C.", "x", true, None);
    assert!(wide.starts_with("\\begin{figure*}[t]") && wide.contains("width=\\textwidth"));
    let export = environment(
        &p,
        "\\figdir fig_x.pdf",
        "C.",
        "x",
        false,
        Some("../figures/fig_x.pptx"),
    );
    assert!(export.contains(
        "\\IfFileExists{\\figdir fig_x.pdf}{\\includegraphics[width=\\columnwidth]{\\figdir fig_x.pdf}}{\\fbox{\\parbox{0.9\\columnwidth}{\\centering\\texttt{../figures/fig\\_x.pptx}}}}"
    ), "{export}");
}

#[test]
fn a_free_name_comes_from_the_answer_and_skips_taken_ones() {
    let dir = temp("names");
    std::fs::create_dir_all(dir.join("figures")).unwrap();
    std::fs::write(dir.join("figures/paired-runs.pdf"), b"x").unwrap();
    let store = FigureStore::default();
    let p = place(
        "\\documentclass{article}\n\\begin{document}\n\\end{document}",
        "",
        Some(&dir),
    );
    let answer = FigureAnswer {
        name: "Paired Runs".into(),
        ..FigureAnswer::default()
    };
    assert_eq!(
        free_stem(&store, &p, Some(&dir), &answer, "pdf"),
        "paired-runs-2"
    );
    store.wait(Waiting {
        reference: "figures/paired-runs-2.pdf".into(),
        files: vec![("figures/paired-runs-2.pdf".into(), b"pdf".to_vec())],
    });
    assert_eq!(
        free_stem(&store, &p, Some(&dir), &answer, "pdf"),
        "paired-runs-3"
    );
    // No name: from the caption; the document's prefix in front.
    let fig = Place {
        stem_prefix: "fig_".into(),
        ..p.clone()
    };
    let from_caption = FigureAnswer {
        caption: "The paired runs of TEP, in detail".into(),
        ..FigureAnswer::default()
    };
    assert_eq!(
        free_stem(&store, &fig, Some(&dir), &from_caption, "pdf"),
        "fig_the-paired-runs-of"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn waiting_figures_are_built_from_biwrite_and_saved_where_they_belong() {
    let top = temp("save");
    let project = top.join("en");
    std::fs::create_dir_all(&project).unwrap();
    let store = FigureStore::default();
    store.wait(Waiting {
        reference: "\\figdir fig_a.pdf".into(),
        files: vec![
            ("../figures/fig_a.pdf".into(), b"%PDF-a".to_vec()),
            ("../figures/fig_a.pptx".into(), b"PK-a".to_vec()),
        ],
    });
    store.wait(Waiting {
        reference: "\\figdir fig_b.pdf".into(),
        files: vec![("../figures/fig_b.pdf".into(), b"%PDF-b".to_vec())],
    });
    // A build of unsaved text reads the copy in .biwrite; lines stay.
    let text = "x\n\\includegraphics{\\figdir fig_a.pdf}\ny\n";
    let staged = store.staged(&project, text);
    assert_eq!(
        staged,
        "x\n\\includegraphics{.biwrite/figures/fig_a.pdf}\ny\n"
    );
    assert_eq!(
        std::fs::read(project.join(".biwrite/figures/fig_a.pdf")).unwrap(),
        b"%PDF-a"
    );
    assert!(
        !top.join("figures").exists(),
        "nothing written for the project yet"
    );
    // A save writes what the text includes (not in a comment), and not
    // over a file that is there.
    std::fs::create_dir_all(top.join("figures")).unwrap();
    std::fs::write(top.join("figures/fig_a.pptx"), b"mine").unwrap();
    let saved = "\\includegraphics{\\figdir fig_a.pdf}\n% \\includegraphics{\\figdir fig_b.pdf}\n";
    let (written, kept) = store.save(&project, saved);
    assert_eq!(written, ["../figures/fig_a.pdf"]);
    assert_eq!(kept, ["../figures/fig_a.pptx"]);
    assert_eq!(
        std::fs::read(top.join("figures/fig_a.pdf")).unwrap(),
        b"%PDF-a"
    );
    assert_eq!(
        std::fs::read(top.join("figures/fig_a.pptx")).unwrap(),
        b"mine"
    );
    assert!(!top.join("figures/fig_b.pdf").exists());
    // b still waits; a does not.
    assert!(store.taken(None, "../figures/fig_b.pdf"));
    assert!(!store.taken(None, "../figures/fig_a.pdf"));
    std::fs::remove_dir_all(&top).unwrap();
}

#[test]
fn libreoffice_profiles_are_file_urls() {
    assert_eq!(file_url(Path::new("/tmp/a b")), "file:///tmp/a%20b");
    assert_eq!(file_url(Path::new("C:\\Users\\x")), "file:///C:/Users/x");
}

#[test]
fn a_pdf_wider_than_a_column_spans_both() {
    let pdf = b"%PDF-1.5\n1 0 obj << /Type /Page /MediaBox [0 0 340.157 141.732] >>";
    let w = pdf_width(pdf).unwrap();
    assert!((w - 12.0).abs() < 0.01, "{w}");
    assert!(wide(w));
    assert!(!wide(8.5));
    assert_eq!(pdf_width(b"no box"), None);
}
