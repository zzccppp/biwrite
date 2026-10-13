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

use crate::commands::{continue_left, home_text, open_path, swap, write_document};
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
    assert_eq!(
        changed.len(),
        1,
        "exactly one paragraph changed: {changed:?}"
    );
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
    write_document(state, main.clone(), own, None)
        .await
        .unwrap();
    assert_eq!(read(&main), text, "{id}: saved as written");
    assert!(!state.file().dirty);
    (project, main, text)
}

/// Both PDFs of the open document, and the links between PDF and source.
async fn both_pdfs(state: &tauri::State<'_, AppState>, text: &str, sentence: &str, id: &str) {
    let en = latex_compile(
        state.clone(),
        Lang::En,
        text.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(en.has_pdf, "{id}: English PDF\n{}", en.output);
    let at = text.find(sentence).unwrap();
    let boxes = latex_forward(
        state.clone(),
        Lang::En,
        utf16(text, at) + 3,
        text.to_owned(),
        state.engine.document(),
    )
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
        state.engine.document(),
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
    assert!(
        range.to - range.from < 400,
        "{id}: a sentence, not the document"
    );

    let zh = latex_compile(
        state.clone(),
        Lang::Zh,
        text.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(zh.has_pdf, "{id}: Chinese PDF\n{}", zh.output);
    assert_eq!(zh.untranslated, 0, "{id}: every paragraph in Chinese");
    let boxes = latex_forward(
        state.clone(),
        Lang::Zh,
        utf16(text, at) + 3,
        text.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(
        !boxes.is_empty(),
        "{id}: the paragraph is found in the Chinese PDF"
    );
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
    assert!(
        project
            .join(".biwrite/zh")
            .join(main.file_name().unwrap())
            .with_extension("pdf")
            .is_file()
    );

    // 3. Swap, edit the Chinese of one paragraph, save: only that paragraph
    // of the English file changes; swapping back shows it.
    let swapped = swap(&state, text.clone(), false).unwrap();
    assert_eq!(swapped.snapshot.direction, Direction::ZhEn);
    let ours_zh = to_zh("We assign one cleaning action per row and test it on twelve datasets.");
    assert!(
        swapped.text.contains(&ours_zh),
        "the Chinese of our paragraph"
    );
    let edited = swapped
        .text
        .replacen(&ours_zh, &format!("{ours_zh} 补充"), 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    let own = home_text(&state, edited.clone()).unwrap();
    assert_eq!(
        changed_units(&text, &own),
        vec![unit_of(&text, "We assign one")],
        "only the edited paragraph of the English file changes"
    );
    write_document(&state, main.clone(), own.clone(), None)
        .await
        .unwrap();
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
    assert!(
        early
            .text
            .contains("A paragraph written just before swapping.")
    );
    settle(&state).await;
    assert!(
        fills.0.lock().unwrap().is_empty(),
        "paused: nothing filled yet"
    );
    assert_eq!(
        continue_left(&state, early.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1);
    assert!(
        filled[0]
            .old
            .contains("A paragraph written just before swapping.")
    );
    let now = early.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    settle(&state).await;
    assert_eq!(
        home_text(&state, now.clone()).unwrap(),
        added,
        "the English file is as written"
    );
    let back = swap(&state, now, false).unwrap();
    assert_eq!(back.text, added);
    state.engine.set_auto_translate(true);
    let text = added;
    write_document(&state, main.clone(), text.clone(), None)
        .await
        .unwrap();

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
    assert_eq!(
        translator.calls(),
        calls + 1,
        "one paragraph mentions the term"
    );
    state.engine.set_glossary(Vec::new());
    settle(&state).await;
    write_document(&state, main.clone(), text.clone(), None)
        .await
        .unwrap();

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
    assert_eq!(
        translator.calls(),
        calls,
        "the Chinese file is the translation, so no requests"
    );
    let edited = text.replacen("do well on small tasks", "do very well on small tasks", 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    let saved = write_document(&state, main.clone(), edited.clone(), None)
        .await
        .unwrap();
    assert!(
        saved
            .mirror
            .as_ref()
            .is_some_and(|m| m.written && m.changed == 1)
    );
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
    write_document(
        &state,
        renamed.clone(),
        back.text.clone(),
        Some(mirror_to.clone()),
    )
    .await
    .unwrap();
    assert_eq!(read(&renamed), back.text);
    assert_eq!(read(&mirror_to), zh_edited);
    assert_eq!(read(&main), en_now, "the old English file is untouched");
    assert_eq!(
        read(&zh_path),
        zh_edited,
        "the old Chinese file is untouched"
    );

    // 8. A section in its own file: opened, built through the main file, and
    // a click in the PDF lands in the section.
    let section = project.join("sections").join("background.tex");
    std::fs::create_dir_all(section.parent().unwrap()).unwrap();
    let section_text =
        "\\section{Background}\nPrior work cleans tables before training the model.\n";
    std::fs::write(&section, section_text).unwrap();
    let with_input = en_now.replacen(
        "\\section{Our study}\n",
        "\\input{sections/background}\n\n\\section{Our study}\n",
        1,
    );
    open_path(&state, main.clone()).await.unwrap();
    state.engine.update(with_input.clone());
    settle(&state).await;
    write_document(&state, main.clone(), with_input, None)
        .await
        .unwrap();
    let view = open_path(&state, section.clone()).await.unwrap();
    assert!(view.pair.is_none());
    settle(&state).await;
    both_pdfs(&state, section_text, "Prior work cleans tables", "section").await;
    assert!(
        project
            .join(".biwrite/zh/sections/background.tex")
            .is_file()
    );

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
    assert_eq!(
        sorted,
        ["booktabs", "pgfplots"],
        "the ICLR template loads neither"
    );
    let after = main_text.find("We assign one").unwrap();
    let end = after + main_text[after..].find("\n\n").unwrap();
    let mut with_figures = main_text.clone();
    with_figures.insert_str(end, &format!("\n\n{figures}"));
    let at = with_figures.find("\\begin{document}").unwrap();
    let lines: String = missing
        .iter()
        .map(|p| format!("\\usepackage{{{p}}}\n"))
        .collect();
    with_figures.insert_str(at, &lines);
    state.engine.update(with_figures.clone());
    settle(&state).await;
    write_document(&state, main.clone(), with_figures.clone(), None)
        .await
        .unwrap();
    let built = latex_compile(
        state.clone(),
        Lang::En,
        with_figures.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
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
    assert!(
        bad.is_empty(),
        "the inserted table and plot compile: {bad:?}"
    );
    // Once loaded, nothing is missing any more.
    let (loaded, _) = crate::latex_commands::document_packages(&state, &with_figures).unwrap();
    assert!(biwrite_latex::packages::missing(figures, &loaded).is_empty());
    let zh = latex_compile(
        state.clone(),
        Lang::Zh,
        with_figures,
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(
        zh.has_pdf,
        "the Chinese PDF with the figures: {}",
        zh.output
    );

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
    let built = latex_compile(
        state.clone(),
        Lang::En,
        view.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let mut checked = 0;
    for name in [
        "01_intro.tex",
        "03_problem.tex",
        "04_method.tex",
        "05_experiments.tex",
    ] {
        let section = root.join("sections_en").join(name);
        if !section.is_file() {
            continue;
        }
        let view = open_path(&state, section.clone()).await.unwrap();
        let text = view.text.clone();
        // A plain sentence of prose: a long line without commands.
        let Some(sentence) = text
            .lines()
            .filter(|l| {
                l.len() > 80 && !l.contains('\\') && !l.contains('$') && !l.starts_with('%')
            })
            .find_map(|l| l.split(". ").find(|s| s.len() > 50 && s.is_ascii()))
        else {
            continue;
        };
        let at = text.find(sentence).unwrap();
        let boxes = latex_forward(
            state.clone(),
            Lang::En,
            utf16(&text, at) + 3,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(
            !boxes.is_empty(),
            "{name}: the sentence is found in the PDF"
        );
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
            state.engine.document(),
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
        let before = latex_compile(
            state.clone(),
            Lang::En,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(before.has_pdf, "{id}: {}", before.output);
        let before_zh = latex_compile(
            state.clone(),
            Lang::Zh,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();

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
        let lines: String = missing
            .iter()
            .map(|p| format!("\\usepackage{{{p}}}\n"))
            .collect();
        with.insert_str(at, &lines);
        state.engine.update(with.clone());
        settle(&state).await;
        write_document(&state, main.clone(), with.clone(), None)
            .await
            .unwrap();

        let after = latex_compile(
            state.clone(),
            Lang::En,
            with.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(after.has_pdf, "{id}: {}", after.output);
        let new: Vec<String> = errors(&after.issues)
            .into_iter()
            .filter(|e| !errors(&before.issues).contains(e))
            .collect();
        assert!(
            new.is_empty(),
            "{id}: the figures add errors {new:?} (packages added {missing:?})"
        );
        let (loaded, _) = crate::latex_commands::document_packages(&state, &with).unwrap();
        assert!(
            biwrite_latex::packages::missing(FIGURES, &loaded).is_empty(),
            "{id}"
        );

        // The Chinese PDF carries them too.
        let zh = latex_compile(state.clone(), Lang::Zh, with, state.engine.document())
            .await
            .unwrap();
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

/// The app state with the offline provider's translator.
fn offline_state(dir: &Path, delay: Duration) -> AppState {
    let translator = Arc::new(
        biwrite_engine::MockTranslator::with_delay(delay, delay + Duration::from_millis(2))
            .keeping_markup(),
    );
    let engine = Engine::new(
        translator,
        Arc::new(MemoryCache::default()),
        Arc::new(biwrite_engine::NullSink),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    AppState::new(
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
    )
}

/// LaTeX errors of the Chinese PDF of a new paper from template `id`, its
/// paragraphs translated by the offline provider, or none translated yet.
async fn offline_chinese_errors(id: &str, translated: bool) -> Vec<String> {
    let dir = temp(&format!("offline-{id}-{translated}"));
    let delay = if translated {
        Duration::from_millis(1)
    } else {
        Duration::from_secs(600)
    };
    let app = tauri::test::mock_app();
    app.manage(offline_state(&dir, delay));
    let state = app.state::<AppState>();
    let (template, _) = templates::find(&templates_dir(), &dir.join("user-templates"), id).unwrap();
    let main = templates::instantiate(&template, &dir.join("paper")).unwrap();
    open_path(&state, main).await.unwrap();
    if translated {
        settle(&state).await;
        assert!(
            state.engine.translations().iter().all(|(_, t)| t.is_some()),
            "{id}: every paragraph translated"
        );
    }
    let view = latex_compile(
        state.clone(),
        Lang::Zh,
        state.engine.text(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(view.has_pdf, "{id}: {}", view.output);
    std::fs::remove_dir_all(&dir).unwrap();
    view.issues
        .into_iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .map(|i| i.message)
        .collect()
}

/// The offline provider reverses only the letters of words, so a paper it
/// translates keeps its LaTeX. In every template, the Chinese PDF has no
/// error the untranslated document does not have. (The whole-text reversal
/// of earlier versions turned `\textit{Deep Learning}` into
/// `}gninraeL peeD{titxet\`.)
#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_offline_translations_of_every_template_compile() {
    for id in ["iclr2027", "pvldb", "ieee-tii"] {
        let before = offline_chinese_errors(id, false).await;
        let after = offline_chinese_errors(id, true).await;
        eprintln!(
            "{id}: {} errors untranslated, {} translated offline",
            before.len(),
            after.len()
        );
        assert!(
            after.iter().all(|e| before.contains(e)),
            "{id}: errors the translation brought: {after:?} (untranslated: {before:?})"
        );
    }
}

const SHORT: &str = "\\documentclass{article}\n\\begin{document}\n\\section{Method}\nwe assign one action per row.\n\nwe test it on twelve datasets.\n\\end{document}\n";

/// A pair swapped at once, while an edited English paragraph is still on
/// its way. The paragraph goes over in English and is put in Chinese once
/// translated. Saving writes the Chinese file as edited and the English
/// file with the paragraph exactly as typed.
#[tokio::test]
async fn a_pair_swaps_now_and_fills_in_the_paragraph_on_its_way() {
    let dir = temp("pair-now");
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    let zh_path = dir.join("paper_zh.tex");
    std::fs::write(&main, SHORT).unwrap();
    open_path(&state, main.clone()).await.unwrap();
    settle(&state).await;
    let zh = state.engine.compose_mirror().text;
    std::fs::write(&zh_path, &zh).unwrap();
    let view = open_path(&state, main.clone()).await.unwrap();
    assert!(view.pair.is_some(), "paired");
    settle(&state).await;

    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    assert!(
        swap(&state, edited.clone(), false).is_err(),
        "without keep the swap waits"
    );
    let early = swap(&state, edited.clone(), true).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(zh_path.as_path()));
    assert!(early.text.contains("twelve datasets and three more."));
    assert!(early.text.contains(&to_zh("we assign one action per row.")));
    assert_eq!(read(&main), SHORT, "nothing written yet");
    assert_eq!(read(&zh_path), zh);

    assert_eq!(
        continue_left(&state, early.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1, "{filled:?}");
    assert!(filled[0].old.contains("three more."));
    assert!(filled[0].new.contains(&to_zh("three more.")));
    let now = early.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    settle(&state).await;
    state.engine.set_auto_translate(true);

    write_document(&state, zh_path.clone(), now.clone(), None)
        .await
        .unwrap();
    assert_eq!(read(&zh_path), now);
    assert_eq!(read(&main), edited, "the English as typed");
    let back = swap(&state, now, false).unwrap();
    assert_eq!(back.text, edited);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Back to the file's own language at once, while an edited Chinese
/// paragraph is on its way. It comes back in Chinese and is put in English
/// once translated, and the Chinese typed stays its exact translation.
#[tokio::test]
async fn swapping_back_home_now_fills_in_the_paragraph_on_its_way() {
    let dir = temp("home-now");
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    std::fs::write(&main, SHORT).unwrap();
    open_path(&state, main.clone()).await.unwrap();
    settle(&state).await;
    let swapped = swap(&state, SHORT.to_owned(), false).unwrap();

    state.engine.set_auto_translate(false);
    let target = to_zh("we test it on twelve datasets.");
    assert!(swapped.text.contains(&target));
    let zh_edited = swapped
        .text
        .replacen(&target, &format!("{target}{}", to_zh(" and more")), 1);
    state.engine.update(zh_edited.clone());
    let back = swap(&state, zh_edited.clone(), true).unwrap();
    assert_eq!(state.engine.direction(), Direction::EnZh);
    assert!(back.text.contains(&to_zh(" and more")), "kept in Chinese");
    assert_eq!(read(&main), SHORT, "nothing written yet");

    assert_eq!(
        continue_left(&state, back.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1, "{filled:?}");
    let english = to_en(&format!("{target}{}", to_zh(" and more")));
    assert!(filled[0].new.contains(&english), "{filled:?}");
    let now = back.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    settle(&state).await;
    state.engine.set_auto_translate(true);

    write_document(&state, main.clone(), now.clone(), None)
        .await
        .unwrap();
    assert_eq!(read(&main), now);
    let again = swap(&state, now, false).unwrap();
    assert_eq!(again.text, zh_edited, "the Chinese as typed");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paired paper with its Chinese file next to it, opened and translated.
async fn paired(
    name: &str,
    english: &str,
) -> (PathBuf, AppState, Arc<Fills>, PathBuf, PathBuf, String) {
    let dir = temp(name);
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    let zh_path = dir.join("paper_zh.tex");
    std::fs::write(&main, english).unwrap();
    open_path(&state, main.clone()).await.unwrap();
    settle(&state).await;
    let zh = state.engine.compose_mirror().text;
    std::fs::write(&zh_path, &zh).unwrap();
    let view = open_path(&state, main.clone()).await.unwrap();
    assert!(view.pair.is_some(), "paired");
    settle(&state).await;
    (dir, state, fills, main, zh_path, zh)
}

/// A pair swapped at once and straight back, translation paused, before
/// the paragraph that went over in English is put in Chinese: that English
/// is no translation of the English paragraph. The paragraph waits for one,
/// and so does the Chinese file, which then gets the Chinese.
#[tokio::test]
async fn a_pair_swapped_back_before_the_fill_waits_for_the_translation() {
    let (dir, state, _fills, main, zh_path, zh) = paired("pair-back", SHORT).await;
    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited.clone(), true).unwrap();
    assert_eq!(early.other_language, 1, "saving would write it in English");
    // Nothing reads as waiting in the Chinese file: one press swaps back.
    let back = swap(&state, early.text.clone(), false).unwrap();
    assert_eq!(back.text, edited);
    assert_eq!(back.other_language, 0);

    let saved = write_document(&state, main.clone(), edited.clone(), None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert_eq!(mirror.pending, 1, "{mirror:?}");
    assert!(!mirror.written);
    assert_eq!(read(&zh_path), zh, "the Chinese file waits");
    assert_eq!(read(&main), edited);

    state.engine.set_auto_translate(true);
    settle(&state).await;
    let done = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(done.written, "{done:?}");
    let zh_now = read(&zh_path);
    assert!(!zh_now.contains("twelve datasets"), "{zh_now}");
    assert!(
        zh_now.contains(&to_zh("we test it on twelve datasets and three more.")),
        "{zh_now}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// As above with translation on: swapped back at once while the fill is
/// still queued, the paragraph is translated afresh and the Chinese file
/// gets the Chinese.
#[tokio::test]
async fn a_pair_swapped_back_while_the_fill_is_queued_gets_the_chinese() {
    let (dir, state, _fills, main, zh_path, _zh) = paired("pair-back-on", SHORT).await;
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    // No settling: the edit's translation is on its way.
    let early = swap(&state, edited.clone(), true).unwrap();
    let back = swap(&state, early.text.clone(), true).unwrap();
    assert_eq!(back.text, edited);
    settle(&state).await;
    let saved = write_document(&state, main.clone(), edited.clone(), None)
        .await
        .unwrap();
    assert!(
        saved.mirror.as_ref().is_some_and(|m| m.written),
        "{:?}",
        saved.mirror
    );
    let zh_now = read(&zh_path);
    assert!(!zh_now.contains("twelve datasets"), "{zh_now}");
    assert!(
        zh_now.contains(&to_zh("we test it on twelve datasets and three more.")),
        "{zh_now}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

const WITH_EQUATION: &str = "\\documentclass{article}\n\\begin{document}\n\\section{Method}\nwe assign one action per row.\n\\begin{equation}\nx = y\n\\end{equation}\nwe test it on twelve datasets.\n\\end{document}\n";

/// A paragraph put in Chinese by its fill after an early pair swap, then
/// edited before the save, stays where it is in the English file with its
/// new translation. It sits after an equation, so it has no paired
/// neighbour to go next to: it is the fill's paragraph, never a new one.
#[tokio::test]
async fn a_filled_paragraph_edited_before_the_save_stays_in_the_pair() {
    let (dir, state, fills, main, zh_path, _zh) = paired("pair-fill-edit", WITH_EQUATION).await;
    state.engine.set_auto_translate(false);
    let edited = WITH_EQUATION.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited.clone(), true).unwrap();
    assert_eq!(
        continue_left(&state, early.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1, "{filled:?}");
    let now = early.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    settle(&state).await;
    // The author goes on editing the paragraph, now in Chinese.
    let rewritten = now.replacen(&to_zh("more."), &to_zh("morex."), 1);
    assert_ne!(now, rewritten);
    state.engine.update(rewritten.clone());
    state.engine.set_auto_translate(true);
    settle(&state).await;

    let saved = write_document(&state, zh_path.clone(), rewritten.clone(), None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert!(mirror.written, "{mirror:?}");
    assert_eq!(mirror.left_out, 0, "{mirror:?}");
    assert_eq!(saved.other_language, 0);
    assert_eq!(
        read(&main),
        WITH_EQUATION.replace("twelve datasets.", "twelve datasets and three morex.")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Back to the file's own language at once with a Chinese paragraph on its
/// way, a save writes it in Chinese as it is and says so, as the swap did.
/// Once it is put in English, the next save writes English and says
/// nothing.
#[tokio::test]
async fn saving_paragraphs_left_in_chinese_says_so() {
    let dir = temp("home-now-save");
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    std::fs::write(&main, SHORT).unwrap();
    open_path(&state, main.clone()).await.unwrap();
    settle(&state).await;
    let swapped = swap(&state, SHORT.to_owned(), false).unwrap();
    assert_eq!(
        swapped.other_language, 0,
        "the Chinese is composed, not saved"
    );

    state.engine.set_auto_translate(false);
    let target = to_zh("we test it on twelve datasets.");
    let zh_edited = swapped
        .text
        .replacen(&target, &format!("{target}{}", to_zh(" and more")), 1);
    state.engine.update(zh_edited.clone());
    let back = swap(&state, zh_edited, true).unwrap();
    assert_eq!(back.other_language, 1);
    let own = home_text(&state, back.text.clone()).unwrap();
    let saved = write_document(&state, main.clone(), own, None)
        .await
        .unwrap();
    assert_eq!(saved.other_language, 1);
    assert!(
        read(&main).contains(&to_zh(" and more")),
        "written as it is"
    );

    assert_eq!(
        continue_left(&state, back.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1, "{filled:?}");
    let now = back.text.replacen(&filled[0].old, &filled[0].new, 1);
    state.engine.update(now.clone());
    let saved = write_document(&state, main.clone(), now, None)
        .await
        .unwrap();
    assert_eq!(saved.other_language, 0);
    assert_eq!(
        read(&main),
        SHORT.replace("twelve datasets.", "twelve datasets. and more")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paragraph kept in English by an early pair swap and then edited in the
/// Chinese editor, still English, stays a paragraph left in English: saving
/// says so and the English file gets the edit as typed. Swapped back, it is
/// translated afresh, and the Chinese file ends up in Chinese.
#[tokio::test]
async fn an_edited_paragraph_left_in_english_is_still_one() {
    let (dir, state, _fills, main, zh_path, _zh) = paired("pair-edited-standin", SHORT).await;
    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited.clone(), true).unwrap();
    let typo = early.text.replacen("three more.", "three more ones.", 1);
    state.engine.update(typo.clone());
    assert_eq!(state.left_in_other_language(), 1);
    state.engine.set_auto_translate(true);
    settle(&state).await;
    let saved = write_document(&state, zh_path.clone(), typo.clone(), None)
        .await
        .unwrap();
    assert_eq!(saved.other_language, 1);
    assert!(read(&main).contains("twelve datasets and three more ones."));

    let back = swap(&state, typo.clone(), false).unwrap();
    assert!(back.text.contains("twelve datasets and three more ones."));
    settle(&state).await;
    let saved = write_document(&state, main.clone(), back.text.clone(), None)
        .await
        .unwrap();
    assert!(
        saved.mirror.as_ref().is_some_and(|m| m.written),
        "{:?}",
        saved.mirror
    );
    let zh_now = read(&zh_path);
    assert!(!zh_now.contains("three more ones."), "{zh_now}");
    assert!(zh_now.contains(&to_zh("three more ones.")), "{zh_now}");
    let again = swap(&state, back.text, false).unwrap();
    assert!(!again.text.contains("three more ones."));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Continue as the window asks for it, with a pair and back home, takes up
/// the paragraphs an early swap left in the other language.
#[tokio::test]
async fn continue_takes_up_what_an_early_swap_left_with_a_pair_and_at_home() {
    let (dir, state, fills, _main, _zh_path, _zh) = paired("pair-continue", SHORT).await;
    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited, true).unwrap();
    assert_eq!(
        continue_left(&state, early.text.clone(), state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    assert_eq!(fills.0.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();

    let dir = temp("home-continue");
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    std::fs::write(&main, SHORT).unwrap();
    open_path(&state, main).await.unwrap();
    settle(&state).await;
    let swapped = swap(&state, SHORT.to_owned(), false).unwrap();
    state.engine.set_auto_translate(false);
    let target = to_zh("we test it on twelve datasets.");
    let zh_edited = swapped
        .text
        .replacen(&target, &format!("{target}{}", to_zh(" and more")), 1);
    state.engine.update(zh_edited.clone());
    let back = swap(&state, zh_edited, true).unwrap();
    assert_eq!(
        continue_left(&state, back.text, state.engine.document()).unwrap(),
        1
    );
    settle(&state).await;
    assert_eq!(fills.0.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The fill and an edit of it reach the engine in one update (typed right
/// after the fill landed): the paragraph keeps its place in the pair.
#[tokio::test]
async fn a_fill_edited_in_the_same_update_stays_in_the_pair() {
    let (dir, state, fills, main, zh_path, _zh) =
        paired("pair-fill-edit-once", WITH_EQUATION).await;
    state.engine.set_auto_translate(false);
    let edited = WITH_EQUATION.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited.clone(), true).unwrap();
    continue_left(&state, early.text.clone(), state.engine.document()).unwrap();
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 1, "{filled:?}");
    let now = early.text.replacen(&filled[0].old, &filled[0].new, 1);
    let rewritten = now.replacen(&to_zh("more."), &to_zh("morex."), 1);
    state.engine.update(rewritten.clone());
    state.engine.set_auto_translate(true);
    settle(&state).await;
    let saved = write_document(&state, zh_path.clone(), rewritten, None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert!(mirror.written, "{mirror:?}");
    assert_eq!(mirror.left_out, 0, "{mirror:?}");
    assert_eq!(
        read(&main),
        WITH_EQUATION.replace("twelve datasets.", "twelve datasets and three morex.")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paragraph kept by an early pair swap goes over exactly as typed, its
/// new comment included (not escaped as translations are), so it stays a
/// paragraph left in English until its fill lands.
#[tokio::test]
async fn a_kept_paragraph_with_a_comment_goes_over_as_typed() {
    let (dir, state, fills, main, zh_path, _zh) = paired("pair-kept-comment", SHORT).await;
    let edited = SHORT.replace("twelve datasets.", "twelve datasets. % add three more");
    state.engine.update(edited.clone());
    // Swapped at once, while its translation is on its way.
    let early = swap(&state, edited.clone(), true).unwrap();
    assert!(
        early.text.contains("twelve datasets. % add three more"),
        "{}",
        early.text
    );
    settle(&state).await;
    assert_eq!(fills.0.lock().unwrap().len(), 1, "filled, not refused");
    let saved = write_document(&state, zh_path.clone(), early.text.clone(), None)
        .await
        .unwrap();
    assert_eq!(saved.other_language, 1);
    let back = swap(&state, early.text, false).unwrap();
    assert_eq!(back.text, edited);
    settle(&state).await;
    write_document(&state, main.clone(), back.text, None)
        .await
        .unwrap();
    let zh_now = read(&zh_path);
    assert!(!zh_now.contains("twelve datasets"), "{zh_now}");
    assert!(!zh_now.contains("\\%"), "{zh_now}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Saved with a paragraph left in English, then swapped back: the Chinese
/// file is behind (it holds the English until the paragraph is translated
/// afresh), so the pair counts as unsaved.
#[tokio::test]
async fn a_pair_swapped_back_from_a_paragraph_left_in_english_is_unsaved() {
    let (dir, state, _fills, _main, zh_path, _zh) = paired("pair-back-unsaved", SHORT).await;
    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited.clone(), true).unwrap();
    write_document(&state, zh_path.clone(), early.text.clone(), None)
        .await
        .unwrap();
    let back = swap(&state, early.text, false).unwrap();
    assert!(back.pair.as_ref().is_some_and(|p| p.dirty));
    assert!(state.is_dirty());
    state.engine.set_auto_translate(true);
    settle(&state).await;
    assert!(state.is_dirty(), "the Chinese file still holds English");
    std::fs::remove_dir_all(&dir).unwrap();
}

const KINDS: &str = "\\documentclass{article}\n\\begin{document}\n\\section{our method}\\label{sec:m}\nwe assign one action per row. % note\n\n\\begin{itemize}\n  \\item first point here\n  \\item[b)] second point here\n\\end{itemize}\n\\begin{figure}\n\\centering\n\\caption[short]{a figure of the results}\\label{fig:r}\n\\end{figure}\n\\subsection*{related work}\nmore text here.\n\\end{document}\n";

/// Headings, list items and captions put into English by their fills keep
/// their IDs as paragraphs do.
#[tokio::test]
async fn fills_of_headings_items_and_captions_keep_their_ids() {
    let dir = temp("fill-kinds");
    let (state, _translator, fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    std::fs::write(&main, KINDS).unwrap();
    open_path(&state, main).await.unwrap();
    settle(&state).await;
    let swapped = swap(&state, KINDS.to_owned(), false).unwrap();
    state.engine.set_auto_translate(false);
    let mut zh = swapped.text.clone();
    for en in [
        "our method",
        "first point here",
        "second point here",
        "a figure of the results",
        "related work",
        "we assign one action per row.",
    ] {
        let z = to_zh(en);
        assert!(zh.contains(&z), "{en}: {zh}");
        zh = zh.replacen(&z, &format!("{z}{}", to_zh("x")), 1);
    }
    state.engine.update(zh.clone());
    let back = swap(&state, zh, true).unwrap();
    assert_eq!(back.other_language, 6);
    let ids = |state: &AppState| -> Vec<_> {
        state
            .engine
            .snapshot()
            .layout
            .iter()
            .map(|s| (s.id, s.kind))
            .collect()
    };
    let before = ids(&state);
    continue_left(&state, back.text.clone(), state.engine.document()).unwrap();
    settle(&state).await;
    let filled: Vec<Fill> = std::mem::take(&mut *fills.0.lock().unwrap());
    assert_eq!(filled.len(), 6, "{filled:?}");
    let mut now = back.text;
    for f in &filled {
        now = now.replacen(&f.old, &f.new, 1);
    }
    state.engine.update(now);
    assert_eq!(ids(&state), before);
    assert_eq!(state.left_in_other_language(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// With a pair, Continue sends the fill, then the author edits the
/// paragraph, still English, before it lands: saving still says so, and
/// swapped back the Chinese file gets Chinese, never the English.
#[tokio::test]
async fn a_paragraph_left_in_english_edited_after_continue_is_still_one() {
    let (dir, state, _fills, main, zh_path, _zh) = paired("pair-continue-edit", SHORT).await;
    state.engine.set_auto_translate(false);
    let edited = SHORT.replace("twelve datasets.", "twelve datasets and three more.");
    state.engine.update(edited.clone());
    let early = swap(&state, edited, true).unwrap();
    continue_left(&state, early.text.clone(), state.engine.document()).unwrap();
    let typo = early.text.replacen("three more.", "three more ones.", 1);
    state.engine.update(typo.clone());
    settle(&state).await;
    let saved = write_document(&state, zh_path.clone(), typo.clone(), None)
        .await
        .unwrap();
    assert_eq!(saved.other_language, 1);
    let back = swap(&state, typo, false).unwrap();
    state.engine.set_auto_translate(true);
    settle(&state).await;
    write_document(&state, main.clone(), back.text, None)
        .await
        .unwrap();
    let zh_now = read(&zh_path);
    assert!(!zh_now.contains("three more ones."), "{zh_now}");
    assert!(zh_now.contains(&to_zh("three more ones.")), "{zh_now}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Back home with Chinese paragraphs an early swap left, enough to tip the
/// text towards Chinese: the swap button's check for a file read as the
/// wrong language leaves the file alone.
#[tokio::test]
async fn paragraphs_left_by_an_early_swap_never_make_the_file_chinese() {
    let dir = temp("home-left-retarget");
    let (state, _translator, _fills) = workflow_state(&dir);
    let main = dir.join("paper.tex");
    std::fs::write(&main, SHORT).unwrap();
    open_path(&state, main).await.unwrap();
    settle(&state).await;
    let swapped = swap(&state, SHORT.to_owned(), false).unwrap();
    state.engine.set_auto_translate(false);
    let more: String = (0..6)
        .map(|i| {
            format!(
                "\n\n{}",
                to_zh(&format!("a new chinese paragraph number {i} with words"))
            )
        })
        .collect();
    let zh_more = swapped.text.replacen(
        "\\end{document}",
        &format!("{more}\n\n\\end{{document}}"),
        1,
    );
    state.engine.update(zh_more.clone());
    let back = swap(&state, zh_more, true).unwrap();
    assert!(back.other_language >= 6, "{}", back.other_language);
    assert_eq!(
        biwrite_core::lang::written_in(&back.text, Mode::Latex),
        Some(Direction::ZhEn),
        "Chinese prevails"
    );
    let retargeted = crate::commands::retarget(&state, back.text, true).unwrap();
    assert!(retargeted.is_none());
    assert_eq!(state.file().home, Direction::EnZh);
    assert!(state.left_in_other_language() >= 6);
    std::fs::remove_dir_all(&dir).unwrap();
}

// A project of a main file and its sections, in the Chinese PDF whichever
// file is open.

const PROJECT_ROOT: &str = "\\documentclass{article}\n\
\\begin{document}\n\
\\begin{abstract}\n\
we study how to clean tables before a model reads them.\n\
\\end{abstract}\n\
\\input{sections/intro}\n\
\\include{sections/method}\n\
\\end{document}\n";

const PROJECT_INTRO: &str = "\\section{Introduction}\n\
we clean tables row by row before a model reads them.\n\n\
the second paragraph of the introduction states the question.\n";

const PROJECT_METHOD: &str = "\\section{Method}\n\
our method assigns one action to each row.\n\n\
it then checks the result on rows held out from the search.\n";

/// A project folder with the main file and both sections, as the builds
/// name it (the temporary folder may be behind a link).
fn sectioned(name: &str) -> PathBuf {
    let dir = std::fs::canonicalize(temp(name)).unwrap();
    std::fs::create_dir_all(dir.join("sections")).unwrap();
    std::fs::write(dir.join("main.tex"), PROJECT_ROOT).unwrap();
    std::fs::write(dir.join("sections/intro.tex"), PROJECT_INTRO).unwrap();
    std::fs::write(dir.join("sections/method.tex"), PROJECT_METHOD).unwrap();
    dir
}

/// 1-based line of `needle` in `text`.
fn line_with(text: &str, needle: &str) -> u32 {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle} in {text}"));
    text[..at].matches('\n').count() as u32 + 1
}

/// Where `line` of `input` is in the Chinese PDF.
async fn box_of(state: &AppState, input: &Path, line: u32) -> biwrite_latex::synctex::PdfBox {
    let tc = crate::latex_commands::require_toolchain(state)
        .await
        .unwrap();
    let pdf = crate::latex_commands::pdf_of(state, Lang::Zh).unwrap();
    biwrite_latex::synctex::forward(&tc, &pdf, input, line, 0)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no box for {}:{line}", input.display()))
}

/// A click in the middle of `b` of the Chinese PDF, on the words `span`
/// (as the PDF's text layer gives them).
async fn click(
    state: &tauri::State<'_, AppState>,
    b: &biwrite_latex::synctex::PdfBox,
    span: &str,
    text: &str,
) -> crate::latex_commands::SyncHit {
    latex_inverse(
        state.clone(),
        Lang::Zh,
        b.page,
        b.left + b.width / 2.0,
        b.top + b.height / 2.0,
        span.to_owned(),
        span.encode_utf16().count() / 2,
        text.to_owned(),
        state.engine.document(),
    )
    .await
    .unwrap()
    .expect("a hit")
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_sections_not_open_are_built_in_chinese_from_their_translations() {
    let dir = sectioned("parts");
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    // Each section translated once, as when the author read it.
    for part in ["sections/method.tex", "sections/intro.tex"] {
        open_path(&state, dir.join(part)).await.unwrap();
        settle(&state).await;
    }
    let main = open_path(&state, dir.join("main.tex")).await.unwrap();
    settle(&state).await;
    let built = latex_compile(
        state.clone(),
        Lang::Zh,
        main.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);

    // Both sections are read from Chinese copies, and the project's files
    // stay as they were.
    let copy = |part: &str| read(&dir.join(".biwrite/zh").join(part));
    assert!(
        copy("sections/intro.tex").contains(&to_zh("clean tables row by row")),
        "{}",
        copy("sections/intro.tex")
    );
    let method = copy("sections/method.tex");
    assert!(
        method.contains(&to_zh("it then checks the result")),
        "{method}"
    );
    let root = read(&dir.join(".biwrite/zh/main.tex"));
    assert!(
        root.contains("\\input{.biwrite/zh/sections/intro}"),
        "{root}"
    );
    assert!(
        root.contains("\\include{.biwrite/zh/sections/method}"),
        "{root}"
    );
    assert_eq!(read(&dir.join("main.tex")), PROJECT_ROOT);
    assert_eq!(read(&dir.join("sections/intro.tex")), PROJECT_INTRO);
    assert_eq!(read(&dir.join("sections/method.tex")), PROJECT_METHOD);

    // A click on the method's second paragraph opens the method there.
    let copy_line = line_with(&method, &to_zh("it then checks"));
    let b = box_of(
        &state,
        &dir.join(".biwrite/zh/sections/method.tex"),
        copy_line,
    )
    .await;
    let span = to_zh("then checks the result");
    let hit = click(&state, &b, &span, &main.text).await;
    assert!(!hit.here);
    assert_eq!(hit.open.as_deref(), Some("sections/method.tex"));
    assert_eq!(hit.line, line_with(PROJECT_METHOD, "it then checks"));
    // Opened, as the window does, the same click selects the paragraph.
    let view = open_path(&state, dir.join("sections/method.tex"))
        .await
        .unwrap();
    let hit = click(&state, &b, &span, &view.text).await;
    assert!(hit.here, "{hit:?}");
    let units: Vec<u16> = view.text.encode_utf16().collect();
    let r = hit.range.expect("a range");
    let picked = String::from_utf16_lossy(&units[r.from..r.to]);
    assert!(picked.starts_with("it then checks"), "{picked}");
    // And the paragraph is found from the editor in the same PDF.
    let at = view.text.find("our method").unwrap();
    let boxes = latex_forward(
        state.clone(),
        Lang::Zh,
        utf16(&view.text, at),
        view.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    let first = boxes.first().expect("a box for the method's paragraph");
    let back = click(&state, first, &to_zh("method assigns one"), &view.text).await;
    let r = back.range.expect("a range");
    let picked = String::from_utf16_lossy(&units[r.from..r.to]);
    assert!(picked.starts_with("our method"), "{picked}");

    // Built with the method open, the introduction comes from its copy.
    let built = latex_compile(
        state.clone(),
        Lang::Zh,
        view.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let root = read(&dir.join(".biwrite/zh/main.tex"));
    assert!(
        root.contains("\\input{.biwrite/zh/sections/intro}"),
        "{root}"
    );
    assert!(
        root.contains("\\include{.biwrite/zh/sections/method}"),
        "{root}"
    );
    assert!(copy("sections/intro.tex").contains(&to_zh("clean tables row by row")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_a_section_paired_by_import_is_built_from_its_chinese_file() {
    let dir = sectioned("imported-part");
    let chinese = dir.join("sections/method_translated.tex");
    std::fs::write(
        &chinese,
        "\\section{方法}\n我们的方法为每一行分配一个动作。\n\n随后在留出的行上检验结果。\n",
    )
    .unwrap();
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    let method = dir.join("sections/method.tex");
    let view = open_path(&state, method.clone()).await.unwrap();
    pairing::pair_with(
        &state,
        view.text.clone(),
        state.engine.document(),
        method.clone(),
        chinese.clone(),
    )
    .await
    .unwrap();
    settle(&state).await;
    // Opened again, it pairs again.
    let view = open_path(&state, method.clone()).await.unwrap();
    assert!(view.pair.is_some(), "the pair is remembered");

    let main = open_path(&state, dir.join("main.tex")).await.unwrap();
    settle(&state).await;
    assert!(main.pair.is_none());
    let built = latex_compile(
        state.clone(),
        Lang::Zh,
        main.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let root = read(&dir.join(".biwrite/zh/main.tex"));
    assert!(
        root.contains("\\include{sections/method_translated}"),
        "{root}"
    );
    // The introduction was never translated: it stays as it is.
    assert!(root.contains("\\input{sections/intro}"), "{root}");
    assert!(!dir.join(".biwrite/zh/sections/intro.tex").exists());

    // A click in the Chinese method opens the English one, paired.
    let b = box_of(&state, &chinese, 4).await;
    let hit = click(&state, &b, "随后在留出的行上检验结果", &main.text).await;
    assert_eq!(hit.open.as_deref(), Some("sections/method.tex"), "{hit:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The Chinese version of the whole project: its own main file (with the
/// abstract, or only its `\\input`s), the introduction in `zh_parts`, and
/// the method as it is (not translated yet, the English file).
fn chinese_version(dir: &Path, root: &str, abstract_too: bool) {
    std::fs::create_dir_all(dir.join("zh_parts")).unwrap();
    let abstract_ = if abstract_too {
        "\\begin{abstract}\n我们研究在模型读取表格之前如何清洗表格。\n\\end{abstract}\n"
    } else {
        ""
    };
    std::fs::write(
        dir.join(root),
        format!(
            "\\documentclass{{article}}\n\\usepackage[UTF8]{{ctex}}\n\\begin{{document}}\n\
{abstract_}\\input{{zh_parts/intro}}\n\\include{{sections/method}}\n\\end{{document}}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("zh_parts/intro.tex"),
        "\\section{引言}\n我们在模型读取表格之前逐行清洗。\n\n第二段说明研究问题。\n",
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_a_chinese_main_file_paired_by_import_is_built_from_any_section() {
    let dir = sectioned("imported-root");
    chinese_version(&dir, "chinese_version.tex", true);
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    let main = dir.join("main.tex");
    let view = open_path(&state, main.clone()).await.unwrap();
    pairing::pair_with(
        &state,
        view.text.clone(),
        state.engine.document(),
        main.clone(),
        dir.join("chinese_version.tex"),
    )
    .await
    .unwrap();
    settle(&state).await;
    let intro = dir.join("sections/intro.tex");
    pairing::pair_with(
        &state,
        PROJECT_INTRO.to_owned(),
        open_path(&state, intro.clone())
            .await
            .unwrap()
            .snapshot
            .document,
        intro.clone(),
        dir.join("zh_parts/intro.tex"),
    )
    .await
    .unwrap();
    settle(&state).await;

    // The method has no Chinese file of its own, and the Chinese version
    // reads it as it is: open, it is not paired, and the Chinese PDF is
    // still the Chinese version's.
    let view = open_path(&state, dir.join("sections/method.tex"))
        .await
        .unwrap();
    assert!(view.pair.is_none());
    let built = latex_compile(
        state.clone(),
        Lang::Zh,
        view.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    assert_eq!(built.root, "chinese_version.tex");

    // A click in the Chinese introduction opens the English one.
    let span = "我们在模型读取表格之前逐行清洗";
    let b = box_of(&state, &dir.join("zh_parts/intro.tex"), 2).await;
    let hit = click(&state, &b, span, &view.text).await;
    assert_eq!(hit.open.as_deref(), Some("sections/intro.tex"), "{hit:?}");
    // Opened, it pairs with the Chinese introduction: the Chinese PDF is
    // now the paired file's (built anew, as the window does on its stale
    // mark), and a click selects the paragraph.
    let view = open_path(&state, intro.clone()).await.unwrap();
    assert!(view.pair.is_some());
    settle(&state).await;
    let built = latex_compile(
        state.clone(),
        Lang::Zh,
        view.text.clone(),
        state.engine.document(),
    )
    .await
    .unwrap();
    assert!(built.has_pdf, "{}", built.output);
    let b = box_of(&state, &dir.join("zh_parts/intro.tex"), 2).await;
    let hit = click(&state, &b, span, &view.text).await;
    assert!(hit.here, "{hit:?}");
    let units: Vec<u16> = view.text.encode_utf16().collect();
    let r = hit.range.expect("a range");
    let picked = String::from_utf16_lossy(&units[r.from..r.to]);
    assert!(picked.starts_with("we clean tables"), "{picked}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
#[ignore = "needs a TeX distribution"]
async fn workflow_a_main_zh_file_is_the_chinese_pdf_of_every_section() {
    let dir = sectioned("named-root");
    chinese_version(&dir, "main_zh.tex", false);
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    // The method, which the Chinese main file reads as it is: that file
    // makes the Chinese PDF. The introduction, whose Chinese the main file
    // has under a name nothing pairs with it: its translation does.
    for (part, root) in [
        ("sections/method.tex", "main_zh.tex"),
        ("sections/intro.tex", "main.tex"),
    ] {
        let view = open_path(&state, dir.join(part)).await.unwrap();
        assert!(view.pair.is_none(), "{part} has no Chinese file by name");
        settle(&state).await;
        let built = latex_compile(
            state.clone(),
            Lang::Zh,
            view.text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(built.has_pdf, "{}", built.output);
        assert_eq!(built.root, root, "{part}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_whole_paper_for_the_assistant_puts_each_part_in_its_place() {
    let dir = sectioned("whole");
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    open_path(&state, dir.join("sections/method.tex"))
        .await
        .unwrap();
    // Edited, not saved: the paper has the editor's text.
    let edited = PROJECT_METHOD.replace("held out", "kept apart");
    let target = edited.find("it then checks").unwrap();
    let (paper, at) = crate::latex_commands::whole_paper(&state, &edited, target).unwrap();
    assert!(paper.starts_with("\\documentclass{article}"), "{paper}");
    assert!(paper.contains("clean tables row by row"), "{paper}");
    assert!(
        paper.contains("kept apart") && !paper.contains("held out"),
        "{paper}"
    );
    assert!(
        paper.contains("%% ===== sections/method.tex (the file open in the editor) ====="),
        "{paper}"
    );
    assert!(
        paper.contains("%% ===== sections/intro.tex ====="),
        "{paper}"
    );
    assert!(!paper.contains("\\input{sections/intro}"), "{paper}");
    assert!(paper.trim_end().ends_with("\\end{document}"), "{paper}");
    assert!(
        paper[at..].starts_with("it then checks"),
        "{}",
        &paper[at..]
    );

    // The root open: its own text, the parts as saved.
    let view = open_path(&state, dir.join("main.tex")).await.unwrap();
    let target = view.text.find("we study").unwrap();
    let (paper, at) = crate::latex_commands::whole_paper(&state, &view.text, target).unwrap();
    assert!(paper.contains("held out"), "{paper}");
    assert!(paper[at..].starts_with("we study"), "{}", &paper[at..]);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn a_document_of_one_file_has_no_whole_paper_of_its_own() {
    let dir = std::fs::canonicalize(temp("single")).unwrap();
    std::fs::write(
        dir.join("main.tex"),
        "\\documentclass{article}\n\\begin{document}\nOne file.\n\\end{document}\n",
    )
    .unwrap();
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    let view = open_path(&state, dir.join("main.tex")).await.unwrap();
    assert!(crate::latex_commands::whole_paper(&state, &view.text, 0).is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_paper_as_one_file_has_every_section_in_place() {
    let dir = sectioned("flat");
    // `\includeonly` would leave parts out of the copy: one file has all.
    std::fs::write(
        dir.join("main.tex"),
        PROJECT_ROOT.replace(
            "\\begin{document}",
            "\\includeonly{sections/method}\n\\begin{document}",
        ),
    )
    .unwrap();
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    open_path(&state, dir.join("sections/intro.tex"))
        .await
        .unwrap();
    settle(&state).await;
    let edited = PROJECT_INTRO.replace("row by row", "one row at a time");
    let flat = crate::latex_commands::flat_paper(&state, &edited, None).unwrap();
    assert!(flat.contains("one row at a time"), "{flat}");
    assert!(flat.contains("our method assigns"), "{flat}");
    assert!(
        !flat.contains("\\input{") && !flat.contains("\\include{"),
        "{flat}"
    );
    assert!(flat.contains("% \\includeonly{sections/method}"), "{flat}");
    assert!(
        flat.contains("\\clearpage\n\\section{Method}"),
        "an \\include starts a page: {flat}"
    );
    assert!(!flat.contains("====="), "no marks: {flat}");

    // The Chinese of the whole paper: the open section's translation, and
    // the other's from the cache (the method was opened once).
    open_path(&state, dir.join("sections/method.tex"))
        .await
        .unwrap();
    settle(&state).await;
    let view = open_path(&state, dir.join("sections/intro.tex"))
        .await
        .unwrap();
    settle(&state).await;
    let (name, body, root_dir) =
        crate::latex_commands::translation_tex(&state, view.text.clone(), state.engine.document())
            .unwrap();
    assert_eq!(name, "main-merged_zh.tex", "no version of main.tex");
    assert_eq!(root_dir.as_deref(), Some(dir.as_path()));
    assert!(body.contains(&to_zh("clean tables row by row")), "{body}");
    assert!(body.contains(&to_zh("our method assigns")), "{body}");
    assert!(body.contains("ctex"), "{body}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Every `.tex` file of `dir` outside `.biwrite`, with its text.
fn tex_sources(dir: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    biwrite_latex::tex_files(dir)
        .into_iter()
        .map(|f| {
            let text = std::fs::read_to_string(&f).unwrap();
            (f, text)
        })
        .collect()
}

/// A plain sentence of `text`: a long line without commands, cut at its
/// first full stop.
fn plain_sentence(text: &str) -> Option<&str> {
    text.lines()
        .filter(|l| l.len() > 80 && !l.contains('\\') && !l.contains('$') && !l.starts_with('%'))
        .find_map(|l| l.split(". ").find(|s| s.len() > 50 && s.is_ascii()))
}

/// The author's own paper of several files (a copy; `BIWRITE_SECTIONED_PAPER`
/// is its English main file, with `sections_en/` and `sections_zh/` beside
/// it and a Chinese main file named after it): from a section, the Chinese
/// PDF is the paper's and clicks go both ways, the assistant's whole
/// document is the paper, the paper as one file builds, and the project
/// saved from a section builds in its new folder. No source file changes.
#[tokio::test]
#[ignore = "needs a TeX distribution; uses BIWRITE_SECTIONED_PAPER"]
async fn workflow_a_real_paper_of_sections_works_from_its_sections() {
    let Ok(main) = std::env::var("BIWRITE_SECTIONED_PAPER") else {
        eprintln!("skipped: BIWRITE_SECTIONED_PAPER names no paper (a copy of its main file)");
        return;
    };
    let main = std::fs::canonicalize(main).unwrap();
    let root = main.parent().unwrap().to_owned();
    let before = tex_sources(&root);
    let dir = temp("real-sections");
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();

    for name in ["01_intro.tex", "04_method.tex"] {
        let section = root.join("sections_en").join(name);
        let view = open_path(&state, section.clone()).await.unwrap();
        assert!(view.pair.is_some(), "{name} pairs with its Chinese file");
        settle(&state).await;
        let text = view.text.clone();
        let zh = latex_compile(
            state.clone(),
            Lang::Zh,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        assert!(zh.has_pdf, "{name}: Chinese PDF\n{}", zh.output);
        assert!(zh.root.ends_with("_zh.tex"), "{name}: {}", zh.root);
        let sentence = plain_sentence(&text).expect("a plain sentence");
        let at = text.find(sentence).unwrap();
        let boxes = latex_forward(
            state.clone(),
            Lang::Zh,
            utf16(&text, at) + 3,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap();
        let b = boxes
            .first()
            .unwrap_or_else(|| panic!("{name}: found in the Chinese PDF"));
        let hit = latex_inverse(
            state.clone(),
            Lang::Zh,
            b.page,
            b.left + b.width / 2.0,
            b.top + b.height / 2.0,
            String::new(),
            0,
            text.clone(),
            state.engine.document(),
        )
        .await
        .unwrap()
        .expect("a hit");
        assert!(hit.here, "{name}: back in the section, not {hit:?}");
        let r = hit.range.expect("a range");
        assert!(
            r.from <= utf16(&text, at) + 3 && utf16(&text, at) <= r.to,
            "{name}: the paragraph of the sentence"
        );
        eprintln!("{name}: Chinese PDF {} and clicks both ways", zh.root);
    }

    // The assistant's whole document from the method: every section, the
    // method as edited.
    let method = root.join("sections_en/04_method.tex");
    let view = open_path(&state, method.clone()).await.unwrap();
    settle(&state).await;
    let edited = format!("{}\n% edited in the editor\n", view.text);
    let (paper, at) = crate::latex_commands::whole_paper(&state, &edited, view.text.len()).unwrap();
    let intro = std::fs::read_to_string(root.join("sections_en/01_intro.tex")).unwrap();
    let intro_sentence = plain_sentence(&intro).unwrap();
    assert!(paper.contains(intro_sentence), "the introduction is in it");
    assert!(paper.contains("% edited in the editor"));
    assert!(paper[at..].starts_with("\n% edited in the editor"));
    for part in ["01_intro", "05_experiments", "07_appendix"] {
        assert!(
            paper.contains(&format!("%% ===== sections_en/{part}.tex")),
            "{part} marked"
        );
    }
    eprintln!("whole document: {} characters", paper.chars().count());

    // The pane's whole paper from the method: the introduction before it,
    // in the Chinese of its paired file.
    let view_paper = crate::paper_view::paper_view(&state, &view.text).unwrap();
    let intro_part = view_paper
        .before
        .iter()
        .find(|p| p.file == "sections_en/01_intro.tex")
        .expect("the introduction before the method");
    let known = intro_part
        .blocks
        .iter()
        .filter(|b| {
            b.translation
                .as_deref()
                .is_some_and(crate::pairing::is_chinese)
        })
        .count();
    assert!(
        known * 10 >= intro_part.blocks.len() * 8,
        "{known} of {} paragraphs in Chinese",
        intro_part.blocks.len()
    );
    assert!(
        view_paper
            .after
            .iter()
            .any(|p| p.file == "sections_en/05_experiments.tex"),
        "the experiments after it"
    );
    eprintln!(
        "pane's whole paper: {} parts before, {} after, introduction {known} of {} in Chinese",
        view_paper.before.len(),
        view_paper.after.len(),
        intro_part.blocks.len()
    );

    // The paper as one file builds as the project does.
    let flat = crate::latex_commands::flat_paper(&state, &view.text, None).unwrap();
    assert!(
        !flat.contains("\\input{sections_en/"),
        "every section in place"
    );
    let flat_path = root.join("merged-check.tex");
    std::fs::write(&flat_path, &flat).unwrap();
    let tc = crate::latex_commands::require_toolchain(&state)
        .await
        .unwrap();
    let job = biwrite_latex::Job {
        dir: root.clone(),
        root: PathBuf::from("merged-check.tex"),
        engine: biwrite_latex::engine_for(&flat),
        out_dir: Some(PathBuf::from(".biwrite/merged")),
        timeout: Duration::from_secs(600),
    };
    let built = biwrite_latex::compile(&tc, &job).await.unwrap();
    let errors: Vec<_> = built
        .issues
        .iter()
        .filter(|i| i.severity == biwrite_latex::Severity::Error)
        .map(|i| format!("{:?}:{:?} {}", i.file, i.line, i.message))
        .collect();
    assert!(built.pdf.is_some(), "{}", built.output);
    assert!(errors.is_empty(), "{errors:?}");
    std::fs::remove_file(&flat_path).unwrap();
    eprintln!("one file: built in {} ms", built.duration_ms);

    // Saved as a project from the method, it builds in the new folder.
    let bundle = crate::project_copy::paper_bundle(&state).await.unwrap();
    let dest = dir.join("saved-project");
    let _lock = state.save_lock.lock().await;
    crate::project_copy::save_into(&state, edited.clone(), &bundle, dest.clone())
        .await
        .unwrap();
    drop(_lock);
    let saved_main = dest.join(main.file_name().unwrap());
    assert!(saved_main.is_file());
    assert_eq!(
        std::fs::read_to_string(dest.join("sections_en/04_method.tex")).unwrap(),
        edited,
        "the method as edited"
    );
    assert!(dest.join("sections_zh/04_method.tex").is_file());
    let job = biwrite_latex::Job {
        dir: dest.clone(),
        root: PathBuf::from(main.file_name().unwrap()),
        engine: biwrite_latex::engine_for(&std::fs::read_to_string(&saved_main).unwrap()),
        out_dir: Some(PathBuf::from(".biwrite/en")),
        timeout: Duration::from_secs(600),
    };
    let built = biwrite_latex::compile(&tc, &job).await.unwrap();
    assert!(built.pdf.is_some(), "{}", built.output);
    eprintln!("saved project: built in {} ms", built.duration_ms);

    assert_eq!(
        tex_sources(&root),
        before,
        "the paper's files are as they were"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn the_paper_view_puts_every_part_around_the_open_one() {
    let dir = sectioned("paper-view");
    let app = tauri::test::mock_app();
    let (state, _, _) = workflow_state(&dir);
    app.manage(state);
    let state = app.state::<AppState>();
    // The introduction translated once, the main file too.
    for part in ["sections/intro.tex", "main.tex"] {
        open_path(&state, dir.join(part)).await.unwrap();
        settle(&state).await;
    }
    let view = open_path(&state, dir.join("sections/method.tex"))
        .await
        .unwrap();
    let paper = crate::paper_view::paper_view(&state, &view.text).unwrap();
    let files: Vec<&str> = paper.before.iter().map(|p| p.file.as_str()).collect();
    assert_eq!(files, ["main.tex", "sections/intro.tex"], "in order");
    assert!(paper.after.is_empty());
    let intro = &paper.before[1].blocks;
    assert!(intro[0].heading);
    let second = &intro[1];
    assert!(
        second.source.starts_with("we clean tables"),
        "{}",
        second.source
    );
    assert_eq!(
        second.translation.as_deref(),
        Some(to_zh(&second.source).as_str()),
        "the cached translation"
    );
    // Where it is in its file, to select it there.
    let units: Vec<u16> = PROJECT_INTRO.encode_utf16().collect();
    assert_eq!(
        String::from_utf16_lossy(&units[second.from..second.to]),
        second.source
    );

    // The main file open: what it includes, by name.
    let view = open_path(&state, dir.join("main.tex")).await.unwrap();
    let paper = crate::paper_view::paper_view(&state, &view.text).unwrap();
    let names: Vec<&str> = paper.inside.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["sections/intro", "sections/method"]);
    let method = &paper.inside[1].parts[0].blocks;
    assert!(method[1].translation.is_none(), "never translated: none");
    std::fs::remove_dir_all(&dir).unwrap();
}
