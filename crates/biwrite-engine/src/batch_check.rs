//! Checks that each answer of a batch request belongs to its paragraph.
//!
//! A model answering several numbered paragraphs at once sometimes merges,
//! shifts or swaps them, so an answer arrives under another paragraph's
//! number. Placeholders catch this for paragraphs with math or citations:
//! each paragraph of a batch has placeholder numbers of its own (see
//! `Protector::numbered_from`), so a foreign answer fails to restore. For
//! the rest, two signs that an answer was written for another paragraph
//! are looked for:
//!
//! - **Names and numbers.** Acronyms, names with inner capitals and numbers
//!   (`BERT`, `ResNet`, `WMT14`, `2019`) pass through translation as they
//!   are (plurals and digit groups normalised: `CNNs` is `CNN`, `1,200` is
//!   `1200`). An answer sharing at least two more of them with another
//!   paragraph than with its own is suspect, and so is one that has none of
//!   its own paragraph's three or more but some of another's.
//! - **Length.** An answer much longer or shorter than its paragraph calls
//!   for, measured against the other answers (Chinese characters against
//!   English words, words kept untranslated and URLs left out). It takes
//!   two other answers to judge one, so a single odd answer does not make
//!   the others look odd.
//!
//! A missing answer is a sign too: the model lost count, and the answers
//! around it may be merged or shifted (`[t1+t2, —, t3]`, `[t2, t3, —]`),
//! with nothing in them to tell. Then no answer of the batch is used.
//!
//! A suspect answer is not used: its paragraph is sent again on its own, so
//! a false alarm costs one request.

use std::collections::HashSet;

use biwrite_core::Direction;
use biwrite_core::protect::{OPEN, placeholder_at};

/// Paragraphs shorter than this (English words, Chinese characters) are
/// too short for their length to say anything.
const MIN_WORDS: usize = 12;
const MIN_HAN: usize = 20;
/// How far an answer's length may stray from what the other answers
/// suggest.
const LENGTH_FACTOR: f64 = 1.75;

/// Which answers seem to have been written for another paragraph.
/// `sources[k]` is paragraph k as sent (masked), `answers[k]` its answer as
/// received (`None`: no answer). Both are in `direction`.
pub(crate) fn misplaced(
    direction: Direction,
    sources: &[&str],
    answers: &[Option<&str>],
) -> Vec<bool> {
    if answers.iter().any(Option::is_none) {
        return vec![true; answers.len()];
    }
    let source_names: Vec<HashSet<String>> = sources.iter().map(|s| names(s)).collect();
    let mut out: Vec<bool> = answers
        .iter()
        .enumerate()
        .map(|(k, answer)| {
            let Some(answer) = answer else {
                return false;
            };
            let found = names(answer);
            let shared = |j: usize| found.intersection(&source_names[j]).count();
            let own = shared(k);
            let others = || (0..sources.len()).filter(move |&j| j != k);
            let fits_another = others().any(|j| shared(j) >= own + 2);
            let foreign = found.iter().any(|n| {
                !source_names[k].contains(n) && others().any(|j| source_names[j].contains(n))
            });
            let lost_its_own = source_names[k].len() >= 3 && own == 0 && foreign;
            fits_another || lost_its_own
        })
        .collect();
    for (k, odd) in odd_lengths(direction, sources, answers)
        .into_iter()
        .enumerate()
    {
        out[k] |= odd;
    }
    out
}

/// Answers whose length is far from what their paragraph's length and the
/// other answers suggest.
fn odd_lengths(direction: Direction, sources: &[&str], answers: &[Option<&str>]) -> Vec<bool> {
    let min = match direction {
        Direction::EnZh => MIN_WORDS,
        Direction::ZhEn => MIN_HAN,
    };
    // (source size, answer size) of each answered paragraph long enough to
    // judge.
    let sizes: Vec<Option<(usize, usize)>> = sources
        .iter()
        .zip(answers)
        .map(|(source, answer)| {
            let answer = (*answer)?;
            let sizes = match direction {
                Direction::EnZh => (words(source, answer), han(answer)),
                Direction::ZhEn => (han(source), words(answer, source)),
            };
            (sizes.0 >= min).then_some(sizes)
        })
        .collect();
    let ratio = |(source, answer): (usize, usize)| answer as f64 / source as f64;
    sizes
        .iter()
        .enumerate()
        .map(|(k, size)| {
            let Some(size) = *size else {
                return false;
            };
            let mut others: Vec<f64> = sizes
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != k)
                .filter_map(|(_, s)| s.map(ratio))
                .collect();
            if others.len() < 2 {
                return false;
            }
            others.sort_by(f64::total_cmp);
            let mid = others.len() / 2;
            let typical = if others.len() % 2 == 0 {
                (others[mid - 1] + others[mid]) / 2.0
            } else {
                others[mid]
            };
            let r = ratio(size);
            typical > 0.0 && (r > typical * LENGTH_FACTOR || r < typical / LENGTH_FACTOR)
        })
        .collect()
}

/// Text with placeholders blanked out (their digits are not numbers of the
/// text), URLs dropped and digit groups joined (`1,200` → `1200`).
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find(OPEN) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let skip = match placeholder_at(tail) {
            Some((len, _)) => {
                out.push(' ');
                len
            }
            None => {
                out.push(OPEN);
                OPEN.len_utf8()
            }
        };
        rest = &tail[skip..];
    }
    out.push_str(rest);
    let out = without_urls(&out);
    let chars: Vec<char> = out.chars().collect();
    let mut joined = String::with_capacity(out.len());
    for (i, &c) in chars.iter().enumerate() {
        let group = c == ','
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.len() > i + 3
            && chars[i + 1..=i + 3].iter().all(char::is_ascii_digit)
            && chars.get(i + 4).is_none_or(|c| !c.is_ascii_digit());
        if !group {
            joined.push(c);
        }
    }
    joined
}

/// `text` without URLs: from a scheme (`https://`) or `www.` to the next
/// space or non-ASCII character (Chinese text often runs right up to one).
fn without_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let scheme = rest.find("://").map(|p| {
            // Back to the start of the scheme's letters.
            rest[..p]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_ascii_alphabetic())
                .map_or(0, |(q, c)| q + c.len_utf8())
        });
        let start = match (scheme, rest.find("www.")) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => match a.or(b) {
                Some(p) => p,
                None => break,
            },
        };
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || !c.is_ascii())
            .unwrap_or(tail.len());
        out.push(' ');
        rest = &tail[end.max(1)..];
    }
    out.push_str(rest);
    out
}

/// A token that passes through translation unchanged: a number, or a name
/// with a capital after its first letter (an acronym, `ResNet`).
fn is_name(token: &str) -> bool {
    token.bytes().any(|b| b.is_ascii_digit())
        || token.bytes().skip(1).any(|b| b.is_ascii_uppercase())
}

/// A name as it may be written in the other language: without a plural
/// `s` after a capital or digit (`CNNs`, `1990s`) or an ordinal ending.
fn normal_name(token: &str) -> String {
    for ordinal in ["st", "nd", "rd", "th"] {
        if let Some(stem) = token.strip_suffix(ordinal)
            && !stem.is_empty()
            && stem.bytes().all(|b| b.is_ascii_digit())
        {
            return stem.to_owned();
        }
    }
    match token.strip_suffix('s') {
        Some(stem)
            if stem.len() >= 2
                && stem
                    .bytes()
                    .last()
                    .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) =>
        {
            stem.to_owned()
        }
        _ => token.to_owned(),
    }
}

/// Runs of ASCII letters and digits of [`plain`] text, each with whether a
/// backslash precedes it (a LaTeX command name).
fn tokens(text: &str) -> Vec<(String, bool)> {
    let text = plain(text);
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphanumeric() {
            i += 1;
        }
        let command = start > 0 && bytes[start - 1] == b'\\';
        out.push((text[start..i].to_owned(), command));
    }
    out
}

/// The names and numbers of `text` (see [`is_name`]), normalised.
fn names(text: &str) -> HashSet<String> {
    tokens(text)
        .into_iter()
        .filter(|(t, command)| !command && is_name(t))
        .map(|(t, _)| normal_name(&t))
        .collect()
}

/// English words of `text` that `other` does not have too (kept
/// untranslated), without names, numbers and LaTeX command names (names
/// and numbers stay as they are in a Chinese translation too).
fn words(text: &str, other: &str) -> usize {
    let kept: HashSet<String> = tokens(other)
        .into_iter()
        .map(|(t, _)| t.to_ascii_lowercase())
        .collect();
    tokens(text)
        .iter()
        .filter(|(t, command)| !command && !is_name(t) && !kept.contains(&t.to_ascii_lowercase()))
        .count()
}

/// Chinese characters.
fn han(text: &str) -> usize {
    text.chars()
        .filter(|&c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(direction: Direction, pairs: &[(&str, Option<&str>)]) -> Vec<bool> {
        let sources: Vec<&str> = pairs.iter().map(|(s, _)| *s).collect();
        let answers: Vec<Option<&str>> = pairs.iter().map(|(_, a)| *a).collect();
        misplaced(direction, &sources, &answers)
    }

    const LONG_EN: &str = "We fine-tune the encoder on the training split and report the average of five runs with different random seeds for every configuration we consider in this section.";
    const LONG_ZH: &str =
        "我们在训练集上微调编码器，并对本节考虑的每种配置报告五次不同随机种子运行的平均结果。";
    const SHORT_EN: &str = "Results are shown below in the next table for all of the settings.";
    const SHORT_ZH: &str = "结果见下表。";

    #[test]
    fn names_and_numbers_are_what_passes_through() {
        let found = names(
            r"BERT and ResNet-50 beat GPTs on WMT14 in the 1990s; The \LaTeX model ⟦3⟧ works on 1,200 images, 2nd run.",
        );
        let mut found: Vec<String> = found.into_iter().collect();
        found.sort_unstable();
        assert_eq!(
            found,
            ["1200", "1990", "2", "50", "BERT", "GPT", "ResNet", "WMT14"]
        );
        assert!(names("Table 3 ⟦3⟧").contains("3"));
        assert!(!names("Table ⟦3⟧").contains("3"));
        assert!(names("see https://example.org/v2/BERT2 and www.x.org/A1").is_empty());
    }

    #[test]
    fn words_skip_names_commands_placeholders_and_kept_words() {
        assert_eq!(
            words(r"We \emph{study} ⟦0⟧ graphs with GNNs in 2024.", ""),
            5
        );
        assert_eq!(
            words(
                "We train a Transformer with Adam.",
                "我们用 Adam 训练 Transformer。"
            ),
            4
        );
        assert_eq!(han("我们研究 ⟦0⟧ 图。"), 5);
    }

    #[test]
    fn answers_in_place_pass() {
        let ok = check(
            Direction::EnZh,
            &[
                ("BERT is evaluated on GLUE.", Some("BERT 在 GLUE 上评估。")),
                (LONG_EN, Some(LONG_ZH)),
                ("We use the model.", Some("我们使用该模型。")),
                (SHORT_EN, Some("所有设置的结果见下一张表。")),
                (LONG_EN, Some(LONG_ZH)),
            ],
        );
        assert_eq!(ok, [false; 5]);
    }

    #[test]
    fn ordinary_translations_of_names_pass() {
        let en_zh = check(
            Direction::EnZh,
            &[
                (
                    "We train CNNs and RNNs on eight GPUs.",
                    Some("我们在八块 GPU 上训练 CNN 和 RNN。"),
                ),
                (
                    "We collect 1,200 images and 3,400 captions.",
                    Some("我们收集了 1200 张图像和 3400 条描述。"),
                ),
                (
                    "AI and ML help the US and EU in COVID-19 ICU care.",
                    Some("人工智能和机器学习帮助美国和欧盟的新冠重症监护。"),
                ),
                (
                    "Section II and IV discuss the 3rd setting.",
                    Some("第二节和第四节讨论第 3 种设置。"),
                ),
            ],
        );
        assert_eq!(en_zh, [false; 4]);
        let zh_en = check(
            Direction::ZhEn,
            &[
                (
                    "我们比较 CNN、RNN 和 GNN。",
                    Some("We compare CNNs, RNNs, and GNNs."),
                ),
                ("如第三节所述。", Some("As described in Section 3.")),
                ("结果见表3。", Some("Results are in Table 3.")),
                ("GNNs 在图上很强。", Some("GNNs are strong on graphs.")),
            ],
        );
        assert_eq!(zh_en, [false; 4]);
    }

    #[test]
    fn swapped_answers_with_names_are_caught() {
        let swapped = check(
            Direction::EnZh,
            &[
                (
                    "BERT is evaluated on GLUE.",
                    Some("ResNet 在 ImageNet 上训练。"),
                ),
                (
                    "ResNet is trained on ImageNet.",
                    Some("BERT 在 GLUE 上评估。"),
                ),
            ],
        );
        assert_eq!(swapped, [true, true]);
        // From Chinese: the English answer lost every name of its source
        // and has one of another paragraph.
        let lost = check(
            Direction::ZhEn,
            &[
                (
                    "我们在 WMT14 上用 BERT 和 GPT 评估。",
                    Some("ResNet is trained."),
                ),
                (
                    "ResNet 在 ImageNet 上训练。",
                    Some("ResNet is trained on ImageNet."),
                ),
            ],
        );
        assert_eq!(lost, [true, false]);
    }

    #[test]
    fn answers_of_the_wrong_length_are_caught() {
        // A long and a short paragraph whose answers were swapped.
        let swapped = check(
            Direction::EnZh,
            &[
                (LONG_EN, Some(SHORT_ZH)),
                (SHORT_EN, Some(LONG_ZH)),
                (LONG_EN, Some(LONG_ZH)),
            ],
        );
        assert_eq!(swapped, [true, true, false]);
        // Two paragraphs merged into the first answer, the second missing.
        let merged = format!("{LONG_ZH}{LONG_ZH}");
        let merged = check(
            Direction::EnZh,
            &[
                (LONG_EN, Some(&merged)),
                (LONG_EN, None),
                (LONG_EN, Some(LONG_ZH)),
                (LONG_EN, Some(LONG_ZH)),
            ],
        );
        // The whole batch goes again: the answers around a gap may be
        // merged or shifted.
        assert_eq!(merged, [true; 4]);
        // The other way round.
        let swapped = check(
            Direction::ZhEn,
            &[
                (LONG_ZH, Some(SHORT_EN)),
                (SHORT_ZH, Some(LONG_EN)),
                (LONG_ZH, Some(LONG_EN)),
                (LONG_ZH, Some(LONG_EN)),
            ],
        );
        assert_eq!(swapped, [true, false, false, false]);
    }

    #[test]
    fn a_gap_makes_every_answer_suspect() {
        let t = |s: &'static str| Some(s);
        let merged = format!("{LONG_ZH}{SHORT_ZH}");
        for answers in [
            vec![Some(merged.as_str()), None, t(LONG_ZH)],
            vec![Some(merged.as_str()), None],
            vec![t(SHORT_ZH), t(LONG_ZH), None],
        ] {
            let pairs: Vec<(&str, Option<&str>)> = [LONG_EN, SHORT_EN, LONG_EN]
                .iter()
                .copied()
                .zip(answers.iter().copied())
                .collect();
            assert!(
                check(Direction::EnZh, &pairs).iter().all(|&s| s),
                "{answers:?}"
            );
        }
    }

    #[test]
    fn urls_are_left_out_also_when_glued_to_chinese() {
        assert_eq!(
            without_urls("见https://x.org/BERT2，和 www.y.org/A1 以及 GPT4"),
            "见 ，和   以及 GPT4"
        );
        let mut found: Vec<String> = names("我们在WMT14上评估，代码见https://github.com/a/b2c3。")
            .into_iter()
            .collect();
        found.sort_unstable();
        assert_eq!(found, ["WMT14"]);
    }

    #[test]
    fn urls_do_not_make_an_answer_look_short() {
        // A URL left as it is does not make an answer look short.
        let with_url = format!(
            "{LONG_EN} See https://github.com/zzccppp/biwrite/blob/main/docs/setup/guide.md for setup."
        );
        let answer = format!(
            "{LONG_ZH}安装见 https://github.com/zzccppp/biwrite/blob/main/docs/setup/guide.md。"
        );
        let ok = check(
            Direction::EnZh,
            &[
                (&with_url, Some(&answer)),
                (LONG_EN, Some(LONG_ZH)),
                (LONG_EN, Some(LONG_ZH)),
            ],
        );
        assert_eq!(ok, [false; 3]);
    }

    #[test]
    fn short_batches_and_paragraphs_are_not_judged_by_length() {
        let short = check(
            Direction::EnZh,
            &[
                ("Introduction", Some("引言")),
                ("Related Work", Some("相关工作与背景介绍")),
            ],
        );
        assert_eq!(short, [false, false]);
    }
}
