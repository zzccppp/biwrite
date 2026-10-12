//! Open files changed on disk by other programs: reported, loaded again on
//! request, ignored on request, and never written over without being asked.

use std::path::{Path, PathBuf};

use crate::commands::{Overwrite, open_path, swap, write_document, write_document_with};
use crate::disk_commands::{DiskView, adopt, check, ignore, reload, reopen, repair};
use crate::error::CommandError;
use crate::pair_tests::{app_state, settle};
use crate::pairing::{self, MirrorProblem};
use crate::state::AppState;

const EN: &str = "\\section{Method}\n\
Our method prepares tables in three steps.\n\n\
First, it profiles each column and records statistics.\n";

const ZH: &str = "\\section{方法}\n\
我们的方法分三步准备表格。\n\n\
首先，它分析每一列并记录统计量。\n";

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("biwrite-disk-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sections_en")).unwrap();
    std::fs::create_dir_all(dir.join("sections_zh")).unwrap();
    dir
}

/// A document alone (`paired`: with its translation in the other folder).
async fn opened(dir: &Path, paired: bool) -> (AppState, PathBuf, PathBuf) {
    let en = dir.join("sections_en/method.tex");
    let zh = dir.join("sections_zh/method.tex");
    std::fs::write(&en, EN).unwrap();
    if paired {
        std::fs::write(&zh, ZH).unwrap();
    }
    let state = app_state(dir);
    open_path(&state, en.clone()).await.unwrap();
    assert_eq!(state.file().pair.is_some(), paired);
    settle(&state).await;
    (state, en, zh)
}

/// Which files changed (stamps aside).
fn changed(document: bool, mirror: bool) -> Option<(bool, bool)> {
    Some((document, mirror))
}

async fn checked(state: &AppState) -> Option<(bool, bool)> {
    reported(state).await.map(|v| (v.document, v.mirror))
}

async fn reported(state: &AppState) -> Option<DiskView> {
    check(state, state.engine.document()).await.unwrap()
}

/// Ignore what is reported now.
async fn ignore_reported(state: &AppState) {
    let which = reported(state).await.unwrap();
    ignore(state, state.engine.document(), which).await.unwrap();
}

/// A save never writes over a change made elsewhere unless asked to, and
/// after its own write it finds nothing changed.
#[tokio::test]
async fn saving_never_writes_over_a_change_made_elsewhere() {
    let dir = temp("save");
    let (state, en, _) = opened(&dir, false).await;
    let theirs = EN.replace("three steps", "four steps");
    std::fs::write(&en, &theirs).unwrap();
    let mine = EN.replace("records statistics", "records a few statistics");
    state.engine.update(mine.clone());
    let refused = write_document(&state, en.clone(), mine.clone(), None).await;
    assert!(
        matches!(refused, Err(CommandError::ChangedOnDisk)),
        "{:?}",
        refused.as_ref().err()
    );
    assert_eq!(std::fs::read_to_string(&en).unwrap(), theirs);
    // Asked to: written over.
    let over = Overwrite {
        document: true,
        mirror: false,
    };
    write_document_with(&state, en.clone(), mine.clone(), None, over)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&en).unwrap(), mine);
    assert_eq!(checked(&state).await, changed(false, false));
    let again = mine.replace("a few", "some");
    write_document(&state, en.clone(), again.clone(), None)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&en).unwrap(), again);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A change is reported until the author ignores it; ignored, nothing
/// changes, saving still asks, and a later change is reported again.
#[tokio::test]
async fn an_ignored_change_is_not_reported_again_but_still_not_written_over() {
    let dir = temp("ignore");
    let (state, en, _) = opened(&dir, false).await;
    assert_eq!(checked(&state).await, changed(false, false));
    let theirs = EN.replace("three steps", "four steps");
    std::fs::write(&en, &theirs).unwrap();
    assert_eq!(checked(&state).await, changed(true, false));
    ignore_reported(&state).await;
    assert_eq!(checked(&state).await, changed(false, false));
    assert_eq!(state.engine.text(), EN, "nothing loaded");
    let refused = write_document(&state, en.clone(), EN.to_owned(), None).await;
    assert!(
        matches!(refused, Err(CommandError::ChangedOnDisk)),
        "{:?}",
        refused.as_ref().err()
    );
    let later = EN.replace("three steps", "five steps");
    std::fs::write(&en, &later).unwrap();
    assert_eq!(checked(&state).await, changed(true, false));
    // Another document number: not this document's answer.
    assert_eq!(
        check(&state, state.engine.document() + 1).await.unwrap(),
        None
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Loaded again, the version on disk is what the app knows of the file:
/// nothing is reported, and saving writes without asking. Not while the
/// other language is edited (the file is opened again instead).
#[tokio::test]
async fn a_document_loaded_again_is_the_files_own() {
    let dir = temp("reload");
    let (state, en, _) = opened(&dir, false).await;
    let theirs = EN.replace("three steps", "four steps");
    std::fs::write(&en, &theirs).unwrap();
    let document = state.engine.document();
    assert_eq!(
        reload(&state, document).await.unwrap(),
        Some(theirs.clone())
    );
    assert_eq!(
        state.file().file.text(),
        EN,
        "nothing changes before the editor has it"
    );
    state.engine.update(theirs.clone());
    assert!(adopt(&state, document, &theirs).await.unwrap());
    assert_eq!(state.file().file.text(), theirs);
    assert_eq!(checked(&state).await, changed(false, false));
    let mine = theirs.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    write_document(&state, en.clone(), mine.clone(), None)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&en).unwrap(), mine);

    settle(&state).await;
    swap(&state, mine.clone(), false).unwrap();
    assert!(!state.editing_own_language());
    assert!(reload(&state, state.engine.document()).await.is_err());
    // Opened again: back in the file's own language, as on disk.
    let view = reopen(&state, state.engine.document())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.text, mine);
    assert!(state.editing_own_language());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paired file changed elsewhere is reported, paired again as it is on
/// disk (the editor keeps its text), and after that written as usual.
#[tokio::test]
async fn a_paired_file_changed_elsewhere_is_paired_again() {
    let dir = temp("repair");
    let (state, en, zh) = opened(&dir, true).await;
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh, &fixed).unwrap();
    assert_eq!(checked(&state).await, changed(false, true));
    let mine = EN.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    let view = repair(&state, mine.clone(), state.engine.document())
        .await
        .unwrap()
        .unwrap();
    assert!(view.dirty, "the editor's text is not saved");
    assert_eq!(state.engine.text(), mine);
    assert_eq!(state.file().pair.as_ref().unwrap().file.text(), fixed);
    assert_eq!(checked(&state).await, changed(false, false));
    settle(&state).await;
    let saved = write_document(&state, en.clone(), mine, None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert!(mirror.written && mirror.problem.is_none(), "{mirror:?}");
    let zh_now = std::fs::read_to_string(&zh).unwrap();
    assert!(zh_now.contains("整理表格"), "their fix stays: {zh_now}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A paired file whose write was refused (changed elsewhere) is written
/// over when the author asks; a swap waits until the change is loaded or
/// ignored.
#[tokio::test]
async fn a_refused_paired_file_is_written_over_when_asked() {
    let dir = temp("mirror-over");
    let (state, en, zh) = opened(&dir, true).await;
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh, &fixed).unwrap();
    let mine = EN.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    settle(&state).await;
    let saved = write_document(&state, en.clone(), mine.clone(), None)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert_eq!(mirror.problem, Some(MirrorProblem::ChangedOnDisk));
    assert_eq!(std::fs::read_to_string(&zh).unwrap(), fixed);
    assert!(
        swap(&state, mine.clone(), false).is_err(),
        "not over their change"
    );
    let written = pairing::save_mirror_with(&state, true)
        .await
        .unwrap()
        .unwrap();
    assert!(written.written && written.problem.is_none(), "{written:?}");
    assert!(!std::fs::read_to_string(&zh).unwrap().contains("整理表格"));
    assert_eq!(checked(&state).await, changed(false, false));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Ignored, a paired file changed elsewhere no longer holds a swap back:
/// the app's version becomes the edited document, and saving it asks
/// before writing over the change.
#[tokio::test]
async fn an_ignored_paired_file_can_be_swapped_to_and_is_not_saved_over() {
    let dir = temp("mirror-swap");
    let (state, _, zh) = opened(&dir, true).await;
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh, &fixed).unwrap();
    assert!(swap(&state, EN.to_owned(), false).is_err());
    ignore_reported(&state).await;
    let view = swap(&state, EN.to_owned(), false).unwrap();
    assert_eq!(state.file().path.as_deref(), Some(zh.as_path()));
    // The ignored change stays ignored on this side too.
    assert_eq!(checked(&state).await, changed(false, false));
    let refused = write_document(&state, zh.clone(), view.text, None).await;
    assert!(
        matches!(refused, Err(CommandError::ChangedOnDisk)),
        "{:?}",
        refused.as_ref().err()
    );
    assert_eq!(std::fs::read_to_string(&zh).unwrap(), fixed);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A file gone is not a change (saving puts it back); one that is no
/// longer UTF-8 is. An untitled document has nothing on disk.
#[tokio::test]
async fn a_file_gone_or_no_longer_text() {
    let dir = temp("gone");
    let (state, en, _) = opened(&dir, false).await;
    std::fs::remove_file(&en).unwrap();
    assert_eq!(checked(&state).await, changed(false, false));
    write_document(&state, en.clone(), EN.to_owned(), None)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&en).unwrap(), EN);
    std::fs::write(&en, [0xff, 0xfe, 0x00, 0x41]).unwrap();
    assert_eq!(checked(&state).await, changed(true, false));
    let refused = write_document(&state, en.clone(), EN.to_owned(), None).await;
    assert!(
        matches!(refused, Err(CommandError::ChangedOnDisk)),
        "{:?}",
        refused.as_ref().err()
    );

    let untitled = app_state(&dir);
    assert_eq!(checked(&untitled).await, changed(false, false));
    assert_eq!(
        reload(&untitled, untitled.engine.document()).await.unwrap(),
        None
    );
    assert!(
        reopen(&untitled, untitled.engine.document())
            .await
            .unwrap()
            .is_none()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// PDF clicks map against the file TeX read: as it is on disk.
#[test]
fn a_build_maps_against_the_file_as_on_disk() {
    let dir = temp("compiled");
    let path = dir.join("paper.tex");
    std::fs::write(&path, "changed elsewhere\n").unwrap();
    assert_eq!(
        crate::latex_commands::as_on_disk(&path, "as last saved\n"),
        "changed elsewhere\n"
    );
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        crate::latex_commands::as_on_disk(&path, "as last saved\n"),
        "as last saved\n"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The editor took the text read from disk, but the file changed again
/// meanwhile: that is not taken for the file's, and is reported.
#[tokio::test]
async fn a_file_changed_again_while_loading_is_not_taken_as_known() {
    let dir = temp("adopt");
    let (state, en, _) = opened(&dir, false).await;
    let theirs = EN.replace("three steps", "four steps");
    std::fs::write(&en, &theirs).unwrap();
    let document = state.engine.document();
    let read = reload(&state, document).await.unwrap().unwrap();
    let again = EN.replace("three steps", "five steps");
    std::fs::write(&en, &again).unwrap();
    state.engine.update(read.clone());
    assert!(!adopt(&state, document, &read).await.unwrap());
    assert_eq!(state.file().file.text(), EN);
    assert_eq!(checked(&state).await, changed(true, false));
    let refused = write_document(&state, en.clone(), read, None).await;
    assert!(
        matches!(refused, Err(CommandError::ChangedOnDisk)),
        "{:?}",
        refused.as_ref().err()
    );
    assert_eq!(std::fs::read_to_string(&en).unwrap(), again);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Ignoring ignores the version reported, not a later one; loaded again,
/// nothing is ignored any more.
#[tokio::test]
async fn ignoring_never_ignores_a_version_not_reported() {
    let dir = temp("stamp");
    let (state, en, _) = opened(&dir, false).await;
    std::fs::write(&en, EN.replace("three steps", "four steps")).unwrap();
    let which = reported(&state).await.unwrap();
    assert!(which.document && !which.document_stamp.is_empty());
    std::fs::write(&en, EN.replace("three steps", "five steps")).unwrap();
    ignore(&state, state.engine.document(), which)
        .await
        .unwrap();
    assert_eq!(checked(&state).await, changed(true, false));
    ignore_reported(&state).await;
    assert_eq!(checked(&state).await, changed(false, false));
    let document = state.engine.document();
    let text = reload(&state, document).await.unwrap().unwrap();
    state.engine.update(text.clone());
    assert!(adopt(&state, document, &text).await.unwrap());
    assert!(state.file().ignored.is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A write of ours that landed though it reported a failure is no change
/// made elsewhere.
#[tokio::test]
async fn our_own_landed_write_is_no_change_made_elsewhere() {
    let dir = temp("landed");
    let (state, en, _) = opened(&dir, false).await;
    let mine = EN.replace("three steps", "four steps");
    // As if written by a save whose last step failed.
    std::fs::write(&en, &mine).unwrap();
    state.engine.update(mine.clone());
    write_document(&state, en.clone(), mine.clone(), None)
        .await
        .unwrap();
    assert_eq!(checked(&state).await, changed(false, false));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A saved edit the paired file was refused (it changed elsewhere) reaches
/// it after the paired file is loaded again: that paragraph is translated
/// afresh rather than taken from the paired file.
#[tokio::test]
async fn a_saved_edit_reaches_the_paired_file_loaded_again() {
    let dir = temp("behind");
    let (state, en, zh) = opened(&dir, true).await;
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh, &fixed).unwrap();
    let mine = EN.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    settle(&state).await;
    let saved = write_document(&state, en.clone(), mine.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        saved.mirror.unwrap().problem,
        Some(MirrorProblem::ChangedOnDisk)
    );
    let view = repair(&state, mine.clone(), state.engine.document())
        .await
        .unwrap()
        .unwrap();
    assert!(!view.dirty);
    assert!(view.pair.unwrap().dirty, "the paired file still lacks it");
    settle(&state).await;
    let written = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(written.written && written.changed == 1, "{written:?}");
    let zh_now = std::fs::read_to_string(&zh).unwrap();
    assert!(zh_now.contains("整理表格"), "their fix stays: {zh_now}");
    // The mock translator reverses the text.
    assert!(zh_now.contains(".scitsitats emos sdrocer"), "{zh_now}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Paired again with unsaved edits, every paragraph keeps its translation
/// in the view; a paired file that no longer pairs leaves all as it was.
#[tokio::test]
async fn pairing_again_shows_every_translation_and_fails_cleanly() {
    let dir = temp("repair-view");
    let (state, _, zh) = opened(&dir, true).await;
    std::fs::write(&zh, ZH.replace("准备表格", "整理表格")).unwrap();
    let mine = EN.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    let view = repair(&state, mine.clone(), state.engine.document())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.snapshot.states.len(), view.snapshot.layout.len());

    // Overwritten with English, it pairs with nothing.
    std::fs::write(&zh, EN).unwrap();
    let unsaved = mine.replace("some statistics", "a few statistics");
    state.engine.update(unsaved.clone());
    let document = state.engine.document();
    assert!(repair(&state, unsaved.clone(), document).await.is_err());
    assert_eq!(state.engine.text(), unsaved, "the editor's text stays");
    assert_eq!(state.engine.document(), document);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A new paragraph saved while the paired file was refused goes into it
/// after the paired file is loaded again (it is not left unpaired).
#[tokio::test]
async fn a_saved_new_paragraph_reaches_the_paired_file_loaded_again() {
    let dir = temp("behind-new");
    let (state, en, zh) = opened(&dir, true).await;
    std::fs::write(&zh, ZH.replace("准备表格", "整理表格")).unwrap();
    let added = format!("{EN}\nA paragraph added at the end.\n");
    state.engine.update(added.clone());
    settle(&state).await;
    let saved = write_document(&state, en.clone(), added.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        saved.mirror.unwrap().problem,
        Some(MirrorProblem::ChangedOnDisk)
    );
    repair(&state, added, state.engine.document())
        .await
        .unwrap()
        .unwrap();
    settle(&state).await;
    let written = pairing::save_mirror(&state).await.unwrap().unwrap();
    assert!(written.written, "{written:?}");
    let zh_now = std::fs::read_to_string(&zh).unwrap();
    assert!(zh_now.contains(".dne eht ta dedda hpargarap A"), "{zh_now}");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The paired file's overwrite asked for goes with the next save (its
/// write was put off until then): written over then, not refused again.
#[tokio::test]
async fn a_save_carries_the_paired_files_overwrite() {
    let dir = temp("mirror-save-over");
    let (state, en, zh) = opened(&dir, true).await;
    let fixed = ZH.replace("准备表格", "整理表格");
    std::fs::write(&zh, &fixed).unwrap();
    let mine = EN.replace("records statistics", "records some statistics");
    state.engine.update(mine.clone());
    settle(&state).await;
    let over = Overwrite {
        document: false,
        mirror: true,
    };
    let saved = write_document_with(&state, en.clone(), mine, None, over)
        .await
        .unwrap();
    let mirror = saved.mirror.unwrap();
    assert!(mirror.written && mirror.problem.is_none(), "{mirror:?}");
    assert!(!std::fs::read_to_string(&zh).unwrap().contains("整理表格"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// What changed elsewhere can be looked at, line by line, for the document
/// and its paired file; looking changes nothing.
#[tokio::test]
async fn a_change_made_elsewhere_can_be_seen_as_a_diff() {
    let dir = temp("diff");
    let (state, en, zh) = opened(&dir, true).await;
    assert!(
        crate::disk_commands::diff(&state, "document")
            .await
            .is_empty()
    );
    std::fs::write(&en, EN.replace("three steps", "four steps")).unwrap();
    std::fs::write(&zh, format!("{ZH}\n最后，它清理异常值。\n")).unwrap();
    let doc = crate::disk_commands::diff(&state, "document").await;
    assert_eq!(doc.len(), 1);
    assert_eq!(doc[0].line, 2);
    assert!(doc[0].old.contains("three steps") && doc[0].new.contains("four steps"));
    let mirror = crate::disk_commands::diff(&state, "mirror").await;
    assert_eq!(mirror.len(), 1);
    assert!(mirror[0].new.contains("最后，它清理异常值。"));
    // Looking is no answer: the save still asks.
    assert_eq!(checked(&state).await, changed(true, true));
    assert!(matches!(
        write_document(&state, en.clone(), EN.to_owned(), None).await,
        Err(CommandError::ChangedOnDisk)
    ));
    // A file gone shows nothing.
    std::fs::remove_file(&zh).unwrap();
    assert!(
        crate::disk_commands::diff(&state, "mirror")
            .await
            .is_empty()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
