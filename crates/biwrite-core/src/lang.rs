//! Translation direction and language-aware text helpers.

use serde::{Deserialize, Serialize};

use crate::mode::Mode;
use crate::pair::units;
use crate::protect::Protector;

/// Which language is being edited (source) and which is shown (target).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Write English, read Chinese (the default, and an English file).
    #[default]
    #[serde(rename = "en-zh")]
    EnZh,
    /// Write Chinese, read English (a Chinese file, or an English one after
    /// swapping the panes).
    #[serde(rename = "zh-en")]
    ZhEn,
}

impl Direction {
    pub fn flipped(self) -> Self {
        match self {
            Self::EnZh => Self::ZhEn,
            Self::ZhEn => Self::EnZh,
        }
    }

    /// Stable identifier, used in cache keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnZh => "en-zh",
            Self::ZhEn => "zh-en",
        }
    }
}

/// Han, kana, hangul, CJK punctuation and full-width forms: scripts written
/// without spaces, where each character is a token.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F
        | 0x3040..=0x30FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FFFF)
}

/// Split into comparison tokens: whitespace-separated words, except that
/// every CJK character is its own token.
pub fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut word_start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() || is_cjk(c) {
            if let Some(s) = word_start.take() {
                out.push(&text[s..i]);
            }
            if is_cjk(c) {
                out.push(&text[i..i + c.len_utf8()]);
            }
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(s) = word_start {
        out.push(&text[s..]);
    }
    out
}

/// Han characters and Latin letters in `text`: what [`chinese_share`] and
/// [`written_in`] weigh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Script {
    han: usize,
    letters: usize,
}

impl Script {
    fn count(&mut self, text: &str) {
        for c in text.chars() {
            if matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF) {
                self.han += 1;
            } else if c.is_ascii_alphabetic() {
                self.letters += 1;
            }
        }
    }

    /// Han characters against English words, five letters to a word.
    fn words(self) -> f64 {
        self.han as f64 + self.letters as f64 / 5.0
    }

    fn share(self) -> f64 {
        match self.words() {
            w if w > 0.0 => self.han as f64 / w,
            _ => 0.0,
        }
    }
}

/// Share of Chinese in `text`: Han characters against English words (five
/// Latin letters count as one word). 0 for text with neither.
pub fn chinese_share(text: &str) -> f64 {
    let mut script = Script::default();
    script.count(text);
    script.share()
}

/// `text` (any markup counted as written) reads as Chinese.
pub fn is_chinese(text: &str) -> bool {
    chinese_share(text) > 0.3
}

/// The direction in which `text` is the source: [`Direction::ZhEn`] for
/// Chinese, [`Direction::EnZh`] for English. Only its translatable
/// paragraphs count, with math, citations, references and commands masked,
/// and Chinese must outweigh English. `None` when there are too few words
/// to tell (an empty document, one of only math or markup).
pub fn written_in(text: &str, mode: Mode) -> Option<Direction> {
    judge(text, mode, 3.0)
}

/// [`written_in`] for one paragraph, where a single word is enough to tell
/// (a heading, a short caption).
pub fn paragraph_written_in(text: &str, mode: Mode) -> Option<Direction> {
    judge(text, mode, 1.0)
}

fn judge(text: &str, mode: Mode, min_words: f64) -> Option<Direction> {
    let mut script = Script::default();
    for unit in units(text, mode) {
        script.count(&Protector::new(mode).mask(&text[unit.content]));
    }
    if script.words() < min_words {
        return None;
    }
    Some(if script.share() > 0.5 {
        Direction::ZhEn
    } else {
        Direction::EnZh
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_round_trip() {
        assert_eq!(Direction::EnZh.flipped(), Direction::ZhEn);
        assert_eq!(Direction::ZhEn.flipped().flipped(), Direction::ZhEn);
        assert_eq!(
            serde_json::to_string(&Direction::ZhEn).unwrap(),
            "\"zh-en\""
        );
    }

    #[test]
    fn tokens_split_cjk_per_char() {
        assert_eq!(
            tokens("图神经网络 GNN works"),
            ["图", "神", "经", "网", "络", "GNN", "works"]
        );
        assert_eq!(
            tokens("模型（GNNs）很强。"),
            ["模", "型", "（", "GNNs", "）", "很", "强", "。"]
        );
        assert!(tokens("  ").is_empty());
    }

    #[test]
    fn chinese_share_weighs_words() {
        assert!(chinese_share("我们研究表格数据清洗。") > 0.9);
        assert!(chinese_share("We study tabular data cleaning.") < 0.1);
        assert!(chinese_share("我们使用 TabPFN 和 CARVEPrep 方法。") > 0.5);
        assert_eq!(chinese_share("1 + 2 = 3"), 0.0);
        assert!(is_chinese("这是中文。"));
        assert!(!is_chinese("This is English with one 词."));
    }

    #[test]
    fn written_in_reads_the_paragraphs_not_the_markup() {
        let zh = "\\documentclass{article}\n\\usepackage{graphicx,amsmath,hyperref}\n\
                  \\begin{document}\n\\section{引言}\n\
                  表格基础模型在小样本任务上表现突出 \\cite{hollmann2025accurate}，\
                  但数据中的错误会降低其精度 $\\epsilon$。\n\\end{document}\n";
        assert_eq!(written_in(zh, Mode::Latex), Some(Direction::ZhEn));
        let en = "\\section{Introduction}\nTabular foundation models do well on small \
                  tasks, but errors in the data (脏数据) cost accuracy.\n";
        assert_eq!(written_in(en, Mode::Latex), Some(Direction::EnZh));
        assert_eq!(written_in("$x^2$\n\n\\begin{equation}a=b\\end{equation}", Mode::Latex), None);
        assert_eq!(written_in("", Mode::Plain), None);
        assert_eq!(written_in("Introduction", Mode::Plain), None);
        assert_eq!(paragraph_written_in("Introduction", Mode::Plain), Some(Direction::EnZh));
        assert_eq!(paragraph_written_in("引言", Mode::Plain), Some(Direction::ZhEn));
        assert_eq!(paragraph_written_in("$x$", Mode::Latex), None);
    }
}
