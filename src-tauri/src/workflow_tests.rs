//! A paper from start to finish, through the same functions the window
//! calls: a new paper from a built-in template, writing and translating,
//! saving, both PDFs and the links between PDF and source, editing the
//! Chinese, an early swap filled in later, the glossary, pairing with the
//! Chinese version and editing either side, Save As, a section file of a
//! larger project, and the template exported and imported again. Needs a
//! TeX distribution (`cargo test -p biwrite -- --ignored workflow`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::sync::atomic::{AtomicU64, Ordering};

use biwrite_core::{Direction, GlossaryEntry, Mode, TextFile};
use biwrite_engine::{
    BoxFuture, Engine, EngineSettings, EventSink, Fill, MemoryCache, PartialFn, SegmentState,
    SessionUsage, TokenUsage, TranslateError, TranslationOutput, TranslationRequest, Translator,
};
use biwrite_latex::templates;
use tauri::Manager;

use crate::commands::{home_text, open_path, swap, write_document};
use crate::latex_commands::{Lang, LatexState, latex_compile, latex_forward, latex_inverse};
use crate::pairing;
use crate::request_log::{LogSettings, NoSink, RequestLog};
use crate::secrets::{MemoryStore, SecretStore};
use crate::settings::{AppSettings, Paths};
use crate::state::AppState;

#[derive(Default)]
struct Fills(Mutex<Vec<Fill>>);

impl EventSink for Fills {
    fn segment_states(&self, _: &[SegmentState]) {}
    fn usage(&self, _: &SessionUsage) {}
    fn notice(&self, _: &str) {}
    fn fills(&self, fills: &[Fill]) {
        self.0.lock().unwrap().extend_from_slice(fills);
    }
}

/// Lowercase letters to Han characters and back: "Chinese" that reads as
/// Chinese, keeps placeholders, digits and punctuation, and translates back
/// exactly.
const HAN: [char; 26] = [
    '阿', '波', '次', '得', '鹅', '佛', '哥', '喝', '衣', '鸡', '科', '乐', '摸', '呢', '哦', '坡',
    '期', '日', '思', '特', '乌', '维', '窝', '西', '鸭', '子',
];

fn to_zh(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => HAN[(c as u8 - b'a') as usize],
            _ => c,
        })
        .collect()
}

fn to_en(s: &str) -> String {
    s.chars()
        .map(|c| match HAN.iter().position(|&h| h == c) {
            Some(i) => (b'a' + i as u8) as char,
            None => c,
        })
        .collect()
}

/// Translates with [`to_zh`] and [`to_en`] after a moment.
#[derive(Default)]
struct Toy {
    calls: AtomicU64,
}

impl Toy {
    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Translator for Toy {
    fn provider(&self) -> &str {
        "toy"
    }

    fn model(&self) -> &str {
        "letters"
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(2)).await;
            let text = match request.direction {
                Direction::EnZh => to_zh(&request.source),
                Direction::ZhEn => to_en(&request.source),
            };
            on_partial(&text);
            Ok(TranslationOutput {
                text,
                usage: TokenUsage::default(),
            })
        })
    }
}

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/templates")
}

fn workflow_state(dir: &Path) -> (AppState, Arc<Toy>, Arc<Fills>) {
    let translator = Arc::new(Toy::default());
    let fills = Arc::new(Fills::default());
    let engine = Engine::new(
        translator.clone(),
        Arc::new(MemoryCache::default()),
        fills.clone(),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    let state = AppState::new(
        engine,
        Arc::new(MemoryCache::default()),
        AppSettings::default(),
        Paths::in_dir(&dir.join("config")),
        secrets,
        Arc::new(RequestLog::new(
            LogSettings::default(),
            None,
            Arc::new(NoSink),
        )),
        crate::skills::SkillStore::new(None, None),
        LatexState::new(templates_dir(), dir.join("user-templates")),
    );
    (state, translator, fills)
}

async fn settle(state: &AppState) {
    crate::pair_tests::settle(state).await;
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-flow-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn read(path: &Path) -> String {
    TextFile::decode(std::fs::read(path).unwrap())
        .unwrap()
        .text()
        .to_owned()
}

/// UTF-16 offset of byte `at` (the editor's positions).
fn utf16(text: &str, at: usize) -> usize {
    text[..at].encode_utf16().count()
}

/// Indices of the paragraphs (pairing units) of `a` and `b` that differ.
fn changed_units(a: &str, b: &str) -> Vec<usize> {
    let ua = biwrite_core::pair::units(a, Mode::Latex);
    let ub = biwrite_core::pair::units(b, Mode::Latex);
    assert_eq!(ua.len(), ub.len(), "the number of paragraphs changed");
    ua.iter()
        .zip(&ub)
        .enumerate()
        .filter(|(_, (x, y))| a[x.content.clone()] != b[y.content.clone()])
        .map(|(i, _)| i)
        .collect()
}

fn unit_of(text: &str, needle: &str) -> usize {
    let at = text.find(needle).unwrap();
    biwrite_core::pair::units(text, Mode::Latex)
        .iter()
        .position(|u| u.content.contains(&at))
        .unwrap()
}

/// The only paragraph of `b` that differs from `a`.
fn one_changed<'a>(a: &str, b: &'a str) -> &'a str {
    let changed = changed_units(a, b);
    assert_eq!(changed.len(), 1, "exactly one paragraph changed: {changed:?}");
    let unit = &biwrite_core::pair::units(b, Mode::Latex)[changed[0]];
    &b[unit.content.clone()]
}

const OURS: &str = "\\section{Our study}\n\
Tabular foundation models do well on small tasks, but errors in the data cost accuracy \
\\citep{goodfellow2016deep} on $k=5$ benchmarks.\n\n\
We assign one cleaning action per row and test it on twelve datasets.\n\n";

/// New paper from `id`, our section written after the title. Returns the
/// project folder, the main file and the written text.
async fn new_paper(state: &AppState, dir: &Path, id: &str) -> (PathBuf, PathBuf, String) {
    let (template, t) = templates::find(&templates_dir(), &dir.join("user-templates"), id).unwrap();
    let project = dir.join(format!("{id}-paper"));
    let main = templates::instantiate(&template, &project).unwrap();
    assert_eq!(main.file_name().unwrap().to_string_lossy(), t.manifest.main);
    let view = open_path(state, main.clone()).await.unwrap();
    assert_eq!(view.home, Direction::EnZh, "{id}: a template is English");
    assert!(view.pair.is_none());
    let text = view
        .text
        .replacen("\\maketitle\n", &format!("\\maketitle\n\n{OURS}"), 1);
    assert_ne!(text, view.text, "{id}: no \\maketitle line to write after");
    state.engine.update(text.clone());
    settle(state).await;
    assert!(
        state.engine.translations().iter().all(|(_, t)| t.is_some()),
        "{id}: every paragraph translated"
    );
    let own = home_text(state, text.clone()).unwrap();
    write_document(state, main.clone(), own, None).await.unwrap();
    assert_eq!(read(&main), text, "{id}: saved as written");
    assert!(!state.file().dirty);
    (project, main, text)
}

/// Both PDFs of the open document, and the links between PDF and source.
async fn both_pdfs(state: &tauri::State<'_, AppState>, text: &str, sentence: &str, id: &str) {
    let en = latex_compile(state.clone(), Lang::En, text.to_owned())
        .await
        .unwrap();
    assert!(en.has_pdf, "{id}: English PDF\n{}", en.output);
    let at = text.find(sentence).unwrap();
    let boxes = latex_forward(state.clone(), Lang::En, utf16(text, at) + 3, text.to_owned())
        .await
        .unwrap();
    assert!(!boxes.is_empty(), "{id}: the cursor is found in the PDF");
    let b = &boxes[0];
    let hit = latex_inverse(
        state.clone(),
        Lang::En,
        b.page,
        b.left + 4.0,
        b.top + b.height / 2.0,
        sentence.to_owned(),
        2,
        text.to_owned(),
    )
    .await
    .unwrap()
    .expect("a click in the PDF finds the source");
    assert!(
        hit.here,
        "{id}: in the open file, not {} line {} (open {:?})",
        hit.file, hit.line, hit.open
    );
    let range = hit.range.expect("a range to select");
    let (from, to) = (utf16(text, at), utf16(text, at + sentence.len()));
    assert!(
        range.from <= from + 2 && range.to >= to,
        "{id}: the click selects the clicked sentence ({:?} vs {from}..{to})",
        (range.from, range.to)
    );
    assert!(range.to - range.from < 400, "{id}: a sentence, not the document");

    let zh = latex_compile(state.clone(), Lang::Zh, text.to_owned())
        .await
        .unwrap();
    assert!(zh.has_pdf, "{id}: Chinese PDF\n{}", zh.output);
    assert_eq!(zh.untranslated, 0, "{id}: every paragraph in Chinese");
    let boxes = latex_forward(state.clone(), Lang::Zh, utf16(text, at) + 3, text.to_owned())
        .await
        .unwrap();
    assert!(!boxes.is_empty(), "{id}: the paragraph is found in the Chinese PDF");
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_every_template_writes_and_builds_both_pdfs() {
    for id in ["pvldb", "ieee-tii"] {
        let dir = temp(id);
        let app = tauri::test::mock_app();
        let (state, _, _) = workflow_state(&dir);
        app.manage(state);
        let state = app.state::<AppState>();
        let (_, _, text) = new_paper(&state, &dir, id).await;
        both_pdfs(&state, &text, "We assign one cleaning action", id).await;
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_a_paper_from_the_iclr_template_through_every_step() {
    let dir = temp("iclr");
    let app = tauri::test::mock_app();
    let (state, translator, fills) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();

    // 1. New paper, written, translated, saved.
    let (project, main, text) = new_paper(&state, &dir, "iclr2027").await;

    // 2. Both PDFs, and the links between PDF and source.
    both_pdfs(&state, &text, "We assign one cleaning action", "iclr2027").await;
    assert!(project.join(".biwrite/zh").join(main.file_name().unwrap()).with_extension("pdf").is_file());

    // 3. Swap, edit the Chinese of one paragraph, save: only that paragraph
    // of the English file changes; swapping back shows it.
    let swapped = swap(&state, text.clone(), false).unwrap();
    assert_eq!(swapped.snapshot.direction, Direction::ZhEn);
    let ours_zh = to_zh("We assign one cleaning action per row and test it on twelve datasets.");
    assert!(swapped.text.contains(&ours_zh), "the Chinese of our paragraph");
    let edited = swapped.text.replacen(&ours_zh, &format!("{ours_zh} 补充"), 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    let own = home_text(&state, edited.clone()).unwrap();
    assert_eq!(
        changed_units(&text, &own),
        vec![unit_of(&text, "We assign one")],
        "only the edited paragraph of the English file changes"
    );
    write_document(&state, main.clone(), own.clone(), None).await.unwrap();
    assert_eq!(read(&main), own);
    let back = swap(&state, edited, false).unwrap();
    assert_eq!(back.text, own, "swapping back shows the saved English");
    let text = own;

    // 4. Paused, a new paragraph waits; swapping at once keeps it in English;
    // "continue" translates it and it is put in place; nothing else moves.
    state.engine.set_auto_translate(false);
    let added = text.replacen(
        "\\section{Our study}\n",
        "\\section{Our study}\nA paragraph written just before swapping.\n\n",
        1,
    );
    state.engine.update(added.clone());
    let early = swap(&state, added.clone(), true).unwrap();
    assert!(early.text.contains("A paragraph written just before swapping."));
    settle(&state).await;
    assert!(fills.0.lock().unwrap().is_empty(), "paused: nothing filled yet");
    assert_eq!(state.engine.continue_translation(true), 1);
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1);
    assert!(filled[0].old.contains("A paragraph written just before swapping."));
    let now = early.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    settle(&state).await;
    assert_eq!(home_text(&state, now.clone()).unwrap(), added, "the English file is as written");
    let back = swap(&state, now, false).unwrap();
    assert_eq!(back.text, added);
    state.engine.set_auto_translate(true);
    let text = added;
    write_document(&state, main.clone(), text.clone(), None).await.unwrap();

    // 5. The glossary: a machine-translated paragraph with the term is
    // translated again; the rest, and Chinese the author wrote while
    // swapped, stay as they are.
    let text = text.replacen(
        "\\section{Our study}\n",
        "\\section{Our study}\nThe twelve benchmarks come from public repositories.\n\n",
        1,
    );
    state.engine.update(text.clone());
    settle(&state).await;
    let calls = translator.calls();
    state.engine.set_glossary(vec![GlossaryEntry {
        term: "twelve benchmarks".into(),
        translation: Some("十二个基准".into()),
    }]);
    settle(&state).await;
    assert_eq!(translator.calls(), calls + 1, "one paragraph mentions the term");
    state.engine.set_glossary(Vec::new());
    settle(&state).await;
    write_document(&state, main.clone(), text.clone(), None).await.unwrap();

    // 6. The Chinese version as a file next to the paper: reopening pairs
    // them, and an edit on either side changes one paragraph of the other.
    let zh_path = main.with_file_name(format!(
        "{}_zh.tex",
        main.file_stem().unwrap().to_string_lossy()
    ));
    let chinese = biwrite_latex::with_chinese(&state.engine.compose_mirror().text);
    std::fs::write(&zh_path, &chinese).unwrap();
    let view = open_path(&state, main.clone()).await.unwrap();
    let pair = view.pair.expect("paired with the Chinese file");
    assert_eq!(pair.paired, pair.units, "every paragraph pairs up");
    let calls = translator.calls();
    settle(&state).await;
    assert_eq!(translator.calls(), calls, "the Chinese file is the translation, so no requests");
    let edited = text.replacen("do well on small tasks", "do very well on small tasks", 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    let saved = write_document(&state, main.clone(), edited.clone(), None)
        .await
        .unwrap();
    assert!(saved.mirror.as_ref().is_some_and(|m| m.written && m.changed == 1));
    let zh_now = read(&zh_path);
    assert!(
        one_changed(&chinese, &zh_now).contains(&to_zh("do very well")),
        "the Chinese of the edited paragraph changed, and nothing else"
    );
    // The other side: edit the Chinese file, the English follows.
    let swapped = swap(&state, edited.clone(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(zh_path.as_path()));
    assert_eq!(swapped.text, zh_now);
    let target = to_zh("We assign one cleaning action per row and test it on twelve datasets.");
    assert!(zh_now.contains(&target));
    let zh_edited = zh_now.replacen(&target, &format!("{target} 再补"), 1);
    state.engine.update(zh_edited.clone());
    settle(&state).await;
    write_document(&state, zh_path.clone(), zh_edited.clone(), None)
        .await
        .unwrap();
    let en_now = read(&main);
    assert!(
        one_changed(&edited, &en_now).contains("We assign one"),
        "the English of the edited paragraph changed, and nothing else"
    );
    assert_eq!(read(&zh_path), zh_edited);

    // 7. Save As: both files under new names, the old ones left as they were.
    let back = swap(&state, zh_edited.clone(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(main.as_path()));
    let renamed = main.with_file_name("draft.tex");
    let mirror_to = pairing::mirror_path_for(&main, &renamed, &zh_path);
    write_document(&state, renamed.clone(), back.text.clone(), Some(mirror_to.clone()))
        .await
        .unwrap();
    assert_eq!(read(&renamed), back.text);
    assert_eq!(read(&mirror_to), zh_edited);
    assert_eq!(read(&main), en_now, "the old English file is untouched");
    assert_eq!(read(&zh_path), zh_edited, "the old Chinese file is untouched");

    // 8. A section in its own file: opened, built through the main file, and
    // a click in the PDF lands in the section.
    let section = project.join("sections").join("background.tex");
    std::fs::create_dir_all(section.parent().unwrap()).unwrap();
    let section_text = "\\section{Background}\nPrior work cleans tables before training the model.\n";
    std::fs::write(&section, section_text).unwrap();
    let with_input = en_now.replacen(
        "\\section{Our study}\n",
        "\\input{sections/background}\n\n\\section{Our study}\n",
        1,
    );
    open_path(&state, main.clone()).await.unwrap();
    state.engine.update(with_input.clone());
    settle(&state).await;
    write_document(&state, main.clone(), with_input, None).await.unwrap();
    let view = open_path(&state, section.clone()).await.unwrap();
    assert!(view.pair.is_none());
    settle(&state).await;
    both_pdfs(&state, section_text, "Prior work cleans tables", "section").await;
    assert!(project.join(".biwrite/zh/sections/background.tex").is_file());

    // 9. A table and a plot from the assistant, inserted the way the window
    // inserts them: after a paragraph, with the packages they need added to
    // the preamble. The paper still compiles, without errors from them.
    let main_text = open_path(&state, main.clone()).await.unwrap().text;
    settle(&state).await;
    let figures = "\\begin{table}[t]\n\\centering\n\\caption{Accuracy (\\%).}\\label{tab:acc}\n\
                   \\begin{tabular}{lrr}\\toprule Method & Cora & PubMed \\\\ \\midrule\n\
                   Ours & 80.9 & 78.6 \\\\ \\bottomrule\\end{tabular}\n\\end{table}\n\n\
                   \\begin{figure}[t]\n\\centering\n\\begin{tikzpicture}\\begin{axis}[ybar, width=6cm]\n\
                   \\addplot coordinates {(1,80.9) (2,78.6)};\\end{axis}\\end{tikzpicture}\n\
                   \\caption{Accuracy per dataset.}\\label{fig:acc}\n\\end{figure}";
    let (loaded, here) = crate::latex_commands::document_packages(&state, &main_text).unwrap();
    assert!(here, "the main file holds the preamble");
    let missing = biwrite_latex::packages::missing(figures, &loaded);
    let mut sorted = missing.clone();
    sorted.sort();
    assert_eq!(sorted, ["booktabs", "pgfplots"], "the ICLR template loads neither");
    let after = main_text.find("We assign one").unwrap();
    let end = after + main_text[after..].find("\n\n").unwrap();
    let mut with_figures = main_text.clone();
    with_figures.insert_str(end, &format!("\n\n{figures}"));
    let at = with_figures.find("\\begin{document}").unwrap();
    let lines: String = missing.iter().map(|p| format!("\\usepackage{{{p}}}\n")).collect();
    with_figures.insert_str(at, &lines);
    state.engine.update(with_figures.clone());
    settle(&state).await;
    write_document(&state, main.clone(), with_figures.clone(), None).await.unwrap();
    let built = latex_compile(state.clone(), Lang::En, with_figures.clone()).await.unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let bad: Vec<_> = built
        .issues
        .iter()
        .filter(|i| {
            i.message.contains("Undefined control sequence")
                || i.message.contains("undefined")
                    && (i.message.contains("Environment") || i.message.contains("\\toprule"))
        })
        .map(|i| i.message.clone())
        .collect();
    assert!(bad.is_empty(), "the inserted table and plot compile: {bad:?}");
    // Once loaded, nothing is missing any more.
    let (loaded, _) = crate::latex_commands::document_packages(&state, &with_figures).unwrap();
    assert!(biwrite_latex::packages::missing(figures, &loaded).is_empty());
    let zh = latex_compile(state.clone(), Lang::Zh, with_figures).await.unwrap();
    assert!(zh.has_pdf, "the Chinese PDF with the figures: {}", zh.output);

    // 10. The project as a template, imported again and used.
    let zip = dir.join("lab.zip");
    let files = templates::export_zip(&project, &main, &zip).unwrap();
    assert!(files >= 3);
    let user = dir.join("user-templates");
    let imported = templates::import_zip(&zip, &user).unwrap();
    let (tdir, _) = templates::find(&templates_dir(), &user, &imported.id).unwrap();
    let again = templates::instantiate(&tdir, &dir.join("second-paper")).unwrap();
    assert!(read(&again).contains("Our study"));

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The author's own paper (a copy; `BIWRITE_REAL_PAPER` is its main file):
/// a sentence of each section file is found in the PDF, and a click on it
/// there selects it in the section.
#[tokio::test]
#[ignore = "needs a TeX distribution; uses BIWRITE_REAL_PAPER"]
async fn workflow_clicks_in_a_real_paper_land_in_its_sections() {
    let Ok(main) = std::env::var("BIWRITE_REAL_PAPER") else {
        eprintln!("skipped: BIWRITE_REAL_PAPER names no paper (a copy of its main file)");
        return;
    };
    let main = PathBuf::from(main);
    let root = main.parent().unwrap().to_owned();
    let dir = temp("real");
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    let view = open_path(&state, main.clone()).await.unwrap();
    settle(&state).await;
    let built = latex_compile(state.clone(), Lang::En, view.text.clone())
        .await
        .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let mut checked = 0;
    for name in ["01_intro.tex", "03_problem.tex", "04_method.tex", "05_experiments.tex"] {
        let section = root.join("sections_en").join(name);
        if !section.is_file() {
            continue;
        }
        let view = open_path(&state, section.clone()).await.unwrap();
        let text = view.text.clone();
        // A plain sentence of prose: a long line without commands.
        let Some(sentence) = text
            .lines()
            .filter(|l| l.len() > 80 && !l.contains('\\') && !l.contains('$') && !l.starts_with('%'))
            .find_map(|l| l.split(". ").find(|s| s.len() > 50 && s.is_ascii()))
        else {
            continue;
        };
        let at = text.find(sentence).unwrap();
        let boxes = latex_forward(state.clone(), Lang::En, utf16(&text, at) + 3, text.clone())
            .await
            .unwrap();
        assert!(!boxes.is_empty(), "{name}: the sentence is found in the PDF");
        let b = &boxes[0];
        let hit = latex_inverse(
            state.clone(),
            Lang::En,
            b.page,
            b.left + b.width / 3.0,
            b.top + b.height / 2.0,
            sentence.to_owned(),
            sentence.len() / 3,
            text.clone(),
        )
        .await
        .unwrap()
        .expect("a click finds the source");
        assert!(hit.here, "{name}: not {} line {}", hit.file, hit.line);
        let range = hit.range.unwrap();
        let (from, to) = (utf16(&text, at), utf16(&text, at + sentence.len()));
        assert!(
            range.from < to && range.to > from,
            "{name}: the selection {:?} misses the sentence {from}..{to}",
            (range.from, range.to)
        );
        checked += 1;
    }
    assert!(checked >= 3, "only {checked} sections checked");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What models write for "Figure": a grouped bar chart, a pipeline diagram
/// with its own TikZ libraries, a table with merged and coloured cells, and
/// two subfigures of an image in the project.
const FIGURES: &str = r"\begin{figure}[t]
  \centering
  \begin{tikzpicture}
    \begin{axis}[ybar, bar width=8pt, width=0.9\linewidth, height=4.5cm,
        symbolic x coords={Cora,CiteSeer,PubMed,arXiv}, xtick=data,
        ylabel={Accuracy (\%)}, ymin=60, ymax=90,
        legend style={at={(0.5,1.03)}, anchor=south, legend columns=-1},
        nodes near coords, every node near coord/.append style={font=\tiny}]
      \addplot coordinates {(Cora,81.5) (CiteSeer,70.3) (PubMed,79.0) (arXiv,71.7)};
      \addplot coordinates {(Cora,80.9) (CiteSeer,69.8) (PubMed,78.6) (arXiv,70.9)};
      \legend{Fine-tuned GNN, Prompt graph}
    \end{axis}
  \end{tikzpicture}
  \caption{Accuracy on four benchmarks.}
  \label{fig:bars}
\end{figure}

\begin{figure}[t]
  \centering
  \usetikzlibrary{arrows.meta,positioning}
  \begin{tikzpicture}[node distance=6mm, box/.style={draw, rounded corners, inner sep=3pt}]
    \node[box] (q) {Query node};
    \node[box, right=of q] (p) {Prompt graph};
    \node[box, right=of p] (g) {Frozen GNN};
    \draw[-{Stealth}] (q) -- (p);
    \draw[-{Stealth}] (p) -- (g);
  \end{tikzpicture}
  \caption{The prompt graph pipeline.}
  \label{fig:pipeline}
\end{figure}

\begin{table}[t]
  \centering
  \caption{Accuracy (\%) by setting.}
  \label{tab:settings}
  \begin{tabular}{llrr}
    \toprule
    Setting & Method & Cora & PubMed \\
    \midrule
    \multirow{2}{*}{5-shot} & Fine-tuned & 81.5 & 79.0 \\
     & \cellcolor{gray!15}Ours & \cellcolor{gray!15}80.9 & \cellcolor{gray!15}78.6 \\
    \bottomrule
  \end{tabular}
\end{table}

\begin{figure}[t]
  \centering
  \begin{subfigure}{0.45\linewidth}
    \includegraphics[width=\linewidth]{figures/sample.png}
    \caption{Before.}
  \end{subfigure}
  \hfill
  \begin{subfigure}{0.45\linewidth}
    \includegraphics[width=\linewidth]{figures/sample.png}
    \caption{After.}
  \end{subfigure}
  \caption{Two subfigures.}
  \label{fig:sub}
\end{figure}";

/// A 1 x 1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn errors(issues: &[crate::latex_commands::IssueView]) -> Vec<String> {
    issues
        .iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .map(|i| i.message.clone())
        .collect()
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_figures_the_assistant_writes_compile_in_every_template() {
    for id in ["iclr2027", "pvldb", "ieee-tii"] {
        let dir = temp(&format!("figures-{id}"));
        let app = tauri::test::mock_app();
        let (state, _, _) = workflow_state(&dir);
        app.manage(state);
        let state = app.state::<AppState>();
        let (project, main, text) = new_paper(&state, &dir, id).await;
        std::fs::create_dir_all(project.join("figures")).unwrap();
        std::fs::write(project.join("figures/sample.png"), PNG).unwrap();
        let before = latex_compile(state.clone(), Lang::En, text.clone()).await.unwrap();
        assert!(before.has_pdf, "{id}: {}", before.output);
        let before_zh = latex_compile(state.clone(), Lang::Zh, text.clone()).await.unwrap();

        // Inserted as the window does: after a paragraph, with the missing
        // packages before \begin{document}.
        let (loaded, here) = crate::latex_commands::document_packages(&state, &text).unwrap();
        assert!(here, "{id}: the main file holds the preamble");
        let missing = biwrite_latex::packages::missing(FIGURES, &loaded);
        let after_para = text.find("We assign one").unwrap();
        let end = after_para + text[after_para..].find("\n\n").unwrap();
        let mut with = text.clone();
        with.insert_str(end, &format!("\n\n{FIGURES}"));
        let at = with.find("\\begin{document}").unwrap();
        let lines: String = missing.iter().map(|p| format!("\\usepackage{{{p}}}\n")).collect();
        with.insert_str(at, &lines);
        state.engine.update(with.clone());
        settle(&state).await;
        write_document(&state, main.clone(), with.clone(), None).await.unwrap();

        let after = latex_compile(state.clone(), Lang::En, with.clone()).await.unwrap();
        assert!(after.has_pdf, "{id}: {}", after.output);
        let new: Vec<String> = errors(&after.issues)
            .into_iter()
            .filter(|e| !errors(&before.issues).contains(e))
            .collect();
        assert!(new.is_empty(), "{id}: the figures add errors {new:?} (packages added {missing:?})");
        let (loaded, _) = crate::latex_commands::document_packages(&state, &with).unwrap();
        assert!(biwrite_latex::packages::missing(FIGURES, &loaded).is_empty(), "{id}");

        // The Chinese PDF carries them too.
        let zh = latex_compile(state.clone(), Lang::Zh, with).await.unwrap();
        assert!(zh.has_pdf, "{id} Chinese: {}", zh.output);
        let zh_new: Vec<String> = errors(&zh.issues)
            .into_iter()
            .filter(|e| !errors(&before_zh.issues).contains(e))
            .collect();
        assert!(
            errors(&zh.issues).len() <= errors(&before_zh.issues).len(),
            "{id} Chinese: more errors with the figures"
        );
        assert!(zh_new.is_empty(), "{id} Chinese: {zh_new:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
