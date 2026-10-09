//! Translation direction and language-aware text helpers.

use serde::{Deserialize, Serialize};

/// Which language is being edited (source) and which is shown (target).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Write English, read Chinese (the default; the file is English).
    #[default]
    #[serde(rename = "en-zh")]
    EnZh,
    /// Write Chinese, read English (after swapping the panes).
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
}
