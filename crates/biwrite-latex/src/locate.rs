//! From a click in the PDF to a place in the source. SyncTeX gives a line;
//! the words around the click (from the PDF's text layer) pick the exact
//! spot near that line, skipping command names, comments and math that
//! the PDF shows differently. When SyncTeX gives something drawn over the
//! text instead (the line-number ruler of a submission), the same words
//! find the place anywhere in a file ([`find_words`]).

/// Lines searched on each side of the SyncTeX line.
const SPREAD: usize = 3;
/// Words compared on each side of the clicked word.
const CONTEXT: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    text: String,
    at: usize,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F
            | 0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

/// Words of PDF text: letter and digit runs, each CJK character alone.
fn pdf_words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if is_cjk(c) {
            if !cur.is_empty() {
                out.push(Word {
                    text: std::mem::take(&mut cur),
                    at: start,
                });
            }
            out.push(Word {
                text: c.to_string(),
                at: i,
            });
        } else if c.is_alphanumeric() {
            if cur.is_empty() {
                start = i;
            }
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(Word {
                text: std::mem::take(&mut cur),
                at: start,
            });
        }
    }
    if !cur.is_empty() {
        out.push(Word {
            text: cur,
            at: start,
        });
    }
    out
}

/// Words of LaTeX source in `range`, without command names and comments.
fn source_words(text: &str, from: usize, to: usize) -> Vec<Word> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < to {
        let Some(c) = text[i..].chars().next() else {
            break;
        };
        match c {
            '\\' => {
                // A command name, or an escaped character.
                i += 1;
                let name_start = i;
                while i < to && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i == name_start && i < to {
                    i += text[i..].chars().next().map_or(1, char::len_utf8);
                }
            }
            '%' => {
                while i < to && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            c if is_cjk(c) => {
                out.push(Word {
                    text: c.to_string(),
                    at: i,
                });
                i += c.len_utf8();
            }
            c if c.is_alphanumeric() => {
                let start = i;
                let mut word = String::new();
                while i < to {
                    let Some(c) = text[i..].chars().next() else {
                        break;
                    };
                    if !c.is_alphanumeric() || is_cjk(c) {
                        break;
                    }
                    word.extend(c.to_lowercase());
                    i += c.len_utf8();
                }
                out.push(Word {
                    text: word,
                    at: start,
                });
            }
            c => i += c.len_utf8(),
        }
    }
    out
}

/// Byte range of lines `line - SPREAD ..= line + SPREAD` (1-based `line`).
fn window(text: &str, line: usize) -> (usize, usize, usize) {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let n = starts.len();
    let target = line.clamp(1, n) - 1;
    let first = target.saturating_sub(SPREAD);
    let last = (target + SPREAD).min(n - 1);
    let end = starts.get(last + 1).map_or(text.len(), |s| s - 1);
    (starts[first], end.max(starts[first]), starts[target])
}

fn same(a: &str, b: &str) -> bool {
    a == b || (a.len() >= 4 && b.len() >= 4 && (a.starts_with(b) || b.starts_with(a)))
}

/// The best place of the clicked words among `source`.
struct Best {
    at: usize,
    score: usize,
    /// The clicked word itself matched, and how many of its neighbours of
    /// how many there are.
    clicked: bool,
    neighbours: usize,
    around: usize,
}

impl Best {
    /// Sure enough to override SyncTeX: the clicked word and two of its
    /// neighbours, or all of a shorter run.
    fn sure(&self) -> bool {
        self.clicked && self.neighbours >= self.around.min(2)
    }
}

fn best(words: &[Word], click: usize, source: &[Word], anchor: usize) -> Option<Best> {
    if words.is_empty() || source.is_empty() {
        return None;
    }
    // The clicked word, or the next one when the click fell between words.
    let clicked = words
        .iter()
        .position(|w| w.at + w.text.len() > click)
        .unwrap_or(words.len() - 1);
    let first = clicked.saturating_sub(CONTEXT);
    let last = (clicked + CONTEXT).min(words.len() - 1);
    let mut best: Option<(Best, usize)> = None; // (match, distance)
    for p in 0..source.len() {
        let (mut score, mut hit, mut neighbours) = (0, false, 0);
        for (k, word) in words.iter().enumerate().take(last + 1).skip(first) {
            let Some(q) = (p + k).checked_sub(clicked) else {
                continue;
            };
            if source.get(q).is_some_and(|s| same(&s.text, &word.text)) {
                if k == clicked {
                    score += 3;
                    hit = true;
                } else {
                    score += 2;
                    neighbours += 1;
                }
            }
        }
        if score == 0 {
            continue;
        }
        let at = source[p].at;
        let distance = at.abs_diff(anchor);
        if best
            .as_ref()
            .is_none_or(|(b, d)| score > b.score || score == b.score && distance < *d)
        {
            let found = Best {
                at,
                score,
                clicked: hit,
                neighbours,
                around: last - first,
            };
            best = Some((found, distance));
        }
    }
    best.map(|(b, _)| b)
}

/// The byte offset in `text` of the word clicked in the PDF: `span` is the
/// text-layer run under the click, `click` the byte offset of the click in
/// it, and `line` the SyncTeX line (1-based). Falls back to the start of
/// the line when no word matches.
pub fn locate(text: &str, line: u32, span: &str, click: usize) -> usize {
    let (from, to, line_start) = window(text, line as usize);
    let fallback = line_start
        + text[line_start..]
            .find(|c: char| !c.is_whitespace())
            .unwrap_or(0)
            .min(to.saturating_sub(line_start));
    let source = source_words(text, from, to);
    best(&pdf_words(span), click, &source, line_start).map_or(fallback, |b| b.at)
}

/// Where the clicked words surely are in `text`: near `line` (1-based)
/// when given, else anywhere. `None` when they are not there with enough
/// of their neighbours to be sure.
pub fn find_words(text: &str, line: Option<u32>, span: &str, click: usize) -> Option<usize> {
    let (from, to, anchor) = match line {
        Some(line) => window(text, line as usize),
        None => (0, text.len(), 0),
    };
    let source = source_words(text, from, to);
    best(&pdf_words(span), click, &source, anchor)
        .filter(Best::sure)
        .map(|b| b.at)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\\section{Intro}\n\
Data cleaning is costly. We study \\emph{row-level} repair with a budget~\\cite{x}.\n\
% a comment that mentions repair\n\
The repair step runs after detection.\n";

    #[test]
    fn the_clicked_word_is_found_near_the_line() {
        // Click on "repair" in "row-level repair with a budget".
        let span = "We study row-level repair with a budget [1].";
        let click = span.find("repair").unwrap() + 2;
        let at = locate(SOURCE, 2, span, click);
        assert_eq!(&SOURCE[at..at + 6], "repair");
        assert!(at < SOURCE.find("% a comment").unwrap());
        // The same word on line 4 when the PDF context says so.
        let span = "The repair step runs";
        let at = locate(SOURCE, 4, span, 5);
        assert_eq!(at, SOURCE.find("repair step").unwrap());
    }

    #[test]
    fn no_match_falls_back_to_the_line() {
        let at = locate(SOURCE, 4, "∑ ∫", 0);
        assert_eq!(at, SOURCE.find("The repair").unwrap());
        assert_eq!(locate("", 9, "x", 0), 0);
    }

    #[test]
    fn chinese_characters_match_one_by_one() {
        let source = "\\section{引言}\n数据清洗代价高昂。我们研究行级修复。\n";
        let span = "我们研究行级修复。";
        let click = span.find("行").unwrap();
        let at = locate(source, 2, span, click);
        assert_eq!(&source[at..at + 3], "行");
    }

    #[test]
    fn words_are_found_anywhere_only_when_sure() {
        let span = "The repair step runs after detection.";
        let click = span.find("step").unwrap();
        // Not near line 1 of a longer text, but surely further down.
        let long = format!("{}{SOURCE}", "Other words on a line.\n".repeat(10));
        assert_eq!(find_words(&long, Some(1), span, click), None);
        assert_eq!(find_words(&long, None, span, click), long.find("step runs"));
        assert_eq!(find_words(&long, Some(14), span, click), long.find("step runs"));
        // One common word alone is not enough to be sure.
        assert_eq!(find_words(SOURCE, None, "the budget of others", 5), None);
        // A short run counts when all of it matches.
        assert_eq!(find_words(SOURCE, None, "row-level", 1), SOURCE.find("row-level"));
    }

    #[test]
    fn commands_are_not_words() {
        let words: Vec<String> = source_words("\\emph{row} 50\\% x", 0, 17)
            .into_iter()
            .map(|w| w.text)
            .collect();
        assert_eq!(words, ["row", "50", "x"]);
    }
}
