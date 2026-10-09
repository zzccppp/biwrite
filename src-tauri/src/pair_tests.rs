//! Pairing end to end through the app state: open a document with its
//! mirror, edit a paragraph, save, swap, edit the other side, save again.
//! Each save may change only the paragraphs that were edited.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use biwrite_core::{Direction, TextFile};
use biwrite_engine::{Engine, EngineSettings, MemoryCache, MockTranslator, NullSink};

use crate::pairing;
use crate::request_log::{LogSettings, NoSink, RequestLog};
use crate::secrets::{MemoryStore, SecretStore};
use crate::settings::{AppSettings, Paths};
use crate::state::AppState;

pub(crate) fn app_state(dir: &Path) -> AppState {
    let engine = Engine::new(
        Arc::new(MockTranslator::with_delay(
            Duration::from_millis(1),
            Duration::from_millis(3),
        )),
        Arc::new(MemoryCache::default()),
        Arc::new(NullSink),
        EngineSettings::default(),
        tokio::runtime::Handle::current(),
    );
    let secrets: Arc<dyn SecretStore> = Arc::new(MemoryStore::default());
    AppState::new(
        engine,
        Arc::new(MemoryCache::default()),
        AppSettings::default(),
        Paths::in_dir(dir),
        secrets,
        Arc::new(RequestLog::new(
            LogSettings::default(),
            None,
            Arc::new(NoSink),
        )),
        crate::skills::SkillStore::new(None, None),
        crate::latex_commands::LatexState::new(dir.join("t"), dir.join("u")),
    )
}

pub(crate) async fn settle(state: &AppState) {
    for _ in 0..2000 {
        if state.engine.pending() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("translations did not finish");
}

fn read(path: &Path) -> TextFile {
    TextFile::decode(std::fs::read(path).unwrap()).unwrap()
}

/// The paragraphs (units) of `a` and `b` that differ.
fn changed_units(a: &str, b: &str) -> Vec<usize> {
    let ua = biwrite_core::pair::units(a, biwrite_core::Mode::Latex);
    let ub = biwrite_core::pair::units(b, biwrite_core::Mode::Latex);
    assert_eq!(ua.len(), ub.len(), "the number of paragraphs changed");
    ua.iter()
        .zip(&ub)
        .enumerate()
        .filter(|(_, (x, y))| a[x.content.clone()] != b[y.content.clone()])
        .map(|(i, _)| i)
        .collect()
}

const EN: &str = "\\section{Method}\\label{sec:method}\n\
Our method prepares tables for TabPFN \\cite{hollmann2025} in three steps.\n\n\
First, it profiles each column and records $k=5$ statistics per value.\n\n\
\\begin{equation}\n  s(v) = \\sum_i w_i f_i(v)\n\\end{equation}\n\n\
Second, it decides one action per record (Table~\\ref{tab:actions}).\n";

const ZH: &str = "\\section{方法}\\label{sec:method}\n\
我们的方法分三步为 TabPFN \\cite{hollmann2025} 准备表格。\n\n\
第一步，逐列画像，并为每个取值记录 $k=5$ 个统计量。\n\n\
\\begin{equation}\n  s(v) = \\sum_i w_i f_i(v)\n\\end{equation}\n\n\
第二步，为每条记录决定一个动作（表~\\ref{tab:actions}）。\n";

fn project(name: &str, en: &str, zh: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("biwrite-pairs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sections_en")).unwrap();
    std::fs::create_dir_all(dir.join("sections_zh")).unwrap();
    let (a, b) = (
        dir.join("sections_en/method.tex"),
        dir.join("sections_zh/method.tex"),
    );
    std::fs::write(&a, en).unwrap();
    std::fs::write(&b, zh).unwrap();
    (dir, a, b)
}

async fn round_trip(dir: &Path, en_path: &Path, zh_path: &Path) {
    let state = app_state(dir);
    let en = std::fs::read_to_string(en_path).unwrap();
    let zh = std::fs::read_to_string(zh_path).unwrap();

    // Opening finds the mirror by its folder and takes its paragraphs.
    let mirror = pairing::counterpart(en_path).expect("a counterpart");
    assert_eq!(mirror, zh_path);
    pairing::open(
        &state,
        en_path.to_path_buf(),
        read(en_path),
        Some((mirror, read(zh_path))),
    );
    settle(&state).await;
    let units = state.engine.translations();
    let paired = state.file().pair.as_ref().unwrap().links.len();
    assert!(
        paired * 10 >= units.len() * 9,
        "{paired} of {}",
        units.len()
    );

    // Nothing edited: saving leaves the mirror byte for byte.
    let saved = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(!saved.written, "an unchanged pair rewrote the mirror");
    assert_eq!(std::fs::read_to_string(zh_path).unwrap(), zh);

    // Edit the second English paragraph: only its Chinese changes.
    let ua = biwrite_core::pair::units(&en, biwrite_core::Mode::Latex);
    let second = &en[ua[2].content.clone()];
    let edited = en.replacen(second, &format!("{second} Edited."), 1);
    state.engine.update(edited.clone());
    settle(&state).await;
    let saved = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(saved.written && saved.changed == 1, "{saved:?}");
    let zh_after = std::fs::read_to_string(zh_path).unwrap();
    let links = state.file().pair.as_ref().unwrap().links.clone();
    let id = state.engine.translations()[2].0;
    let unit = links[&id];
    assert_eq!(changed_units(&zh, &zh_after), vec![unit]);

    // Swap: edit the Chinese, and only the linked English paragraph changes.
    std::fs::write(en_path, &edited).unwrap();
    {
        let mut fs = state.file();
        fs.file = read(en_path);
        fs.dirty = false;
    }
    let view = pairing::swap(&state, edited.clone()).unwrap();
    assert_eq!(view.text, zh_after);
    assert_eq!(state.engine.direction(), Direction::ZhEn);
    settle(&state).await;
    let uz = biwrite_core::pair::units(&zh_after, biwrite_core::Mode::Latex);
    let first = &zh_after[uz[1].content.clone()];
    let zh_edited = zh_after.replacen(first, &format!("{first}（改）"), 1);
    state.engine.update(zh_edited);
    settle(&state).await;
    let saved = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(saved.written && saved.changed == 1, "{saved:?}");
    let en_after = std::fs::read_to_string(en_path).unwrap();
    assert_eq!(changed_units(&edited, &en_after).len(), 1);
}

#[tokio::test]
async fn a_pair_saves_only_edited_paragraphs_both_ways() {
    let (dir, en, zh) = project("sample", EN, ZH);
    round_trip(&dir, &en, &zh).await;
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The same on a real paper: `BIWRITE_PAIR_PROJECT` names a folder with
/// `sections_en/` and `sections_zh/` (a copy: the test writes into it).
#[tokio::test]
#[ignore = "needs BIWRITE_PAIR_PROJECT"]
async fn a_real_paper_pair_saves_only_edited_paragraphs() {
    let root = PathBuf::from(std::env::var("BIWRITE_PAIR_PROJECT").unwrap());
    let mut sections: Vec<String> = std::fs::read_dir(root.join("sections_en"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tex") && root.join("sections_zh").join(n).is_file())
        .collect();
    sections.sort();
    for name in sections {
        let en = std::fs::read_to_string(root.join("sections_en").join(&name)).unwrap();
        let zh = std::fs::read_to_string(root.join("sections_zh").join(&name)).unwrap();
        if biwrite_core::pair::units(&en, biwrite_core::Mode::Latex).len() < 3 {
            continue;
        }
        eprintln!("{name}");
        let (dir, a, b) = project(&name.replace(".tex", ""), &en, &zh);
        round_trip(&dir, &a, &b).await;
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
