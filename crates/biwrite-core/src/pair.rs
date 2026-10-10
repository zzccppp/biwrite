//! A document and its hand-made mirror in the other language: which
//! paragraph of one is which paragraph of the other, and writing changed
//! paragraphs back into the mirror without touching anything else.
//!
//! Units are the translatable segments with text (paragraphs, headings,
//! captions), in order. Alignment is a dynamic program over both unit
//! lists: a pair needs the same kind (and heading level), and scores higher
//! the more language-independent anchors the two share (citation and
//! reference keys, labels, math, numbers, command names, acronyms such as
//! `TabPFN`) and the closer their lengths are. Leaving a unit unpaired costs
//! a fixed penalty, so mirrors that follow the original one to one pair up
//! in order even where anchors are scarce.

use std::collections::HashMap;
use std::ops::Range;

use crate::compose::fit;
use crate::mode::Mode;
use crate::segment::{Segment, SegmentKind, segment};

/// Penalty for leaving a unit without a partner.
const GAP: f64 = 0.35;

/// A translatable segment with text.
#[derive(Clone, Debug)]
pub struct Unit {
    /// Index in `segment(text, mode)`.
    pub segment: usize,
    pub kind: SegmentKind,
    pub content: Range<usize>,
    pub range: Range<usize>,
}

impl Unit {
    /// A paragraph of its own, which a new paragraph can follow: not a
    /// heading, a caption (inside a float) or a list item.
    pub fn is_plain_paragraph(&self, text: &str) -> bool {
        self.kind == SegmentKind::Paragraph
            && text
                .get(self.range.start..self.content.start)
                .is_some_and(|prefix| prefix.trim().is_empty())
    }
}

/// The units of `text`.
pub fn units(text: &str, mode: Mode) -> Vec<Unit> {
    segment(text, mode)
        .into_iter()
        .enumerate()
        .filter(|(_, s)| s.kind.is_translatable() && !s.content(text).trim().is_empty())
        .map(|(i, s): (usize, Segment)| Unit {
            segment: i,
            kind: s.kind,
            content: s.content,
            range: s.range,
        })
        .collect()
}

/// Paired units: `(unit of a, unit of b)`, sorted by `a`. Mostly in order on
/// both sides; a unit moved elsewhere in the mirror pairs out of order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pairing {
    pub pairs: Vec<(usize, usize)>,
    pub a_units: usize,
    pub b_units: usize,
}

impl Pairing {
    /// Share of the larger side's units that found a partner.
    pub fn coverage(&self) -> f64 {
        let n = self.a_units.max(self.b_units);
        if n == 0 {
            1.0
        } else {
            self.pairs.len() as f64 / n as f64
        }
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F)
}

/// Commands whose argument is a key shared by both languages.
const KEYED: &[&str] = &[
    "cite",
    "citep",
    "citet",
    "citeauthor",
    "citeyear",
    "ref",
    "eqref",
    "cref",
    "Cref",
    "autoref",
    "label",
    "url",
    "href",
    "input",
    "include",
];

/// Language-independent tokens of a segment's source.
fn anchors(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < src.len() {
        let c = bytes[i];
        if c == b'\\' {
            let start = i + 1;
            let mut j = start;
            while j < src.len() && bytes[j].is_ascii_alphabetic() {
                j += 1;
            }
            let name = &src[start..j];
            if !name.is_empty() {
                out.push(format!("\\{name}"));
                if KEYED.contains(&name.trim_end_matches('*')) {
                    // Skip an optional [..] and take the {..} keys.
                    let mut k = j;
                    while k < src.len() && bytes[k] == b'[' {
                        k = src[k..].find(']').map_or(src.len(), |e| k + e + 1);
                    }
                    if k < src.len() && bytes[k] == b'{' {
                        let end = src[k..].find('}').map_or(src.len(), |e| k + e);
                        for key in src[k + 1..end].split(',') {
                            let key = key.trim();
                            if !key.is_empty() {
                                out.push(format!("{{{key}}}"));
                            }
                        }
                        j = end;
                    }
                }
            }
            i = j.max(i + 1);
        } else if c == b'$' {
            let end = src[i + 1..].find('$').map_or(src.len(), |e| i + 1 + e);
            let math: String = src[i + 1..end]
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if !math.is_empty() {
                out.push(format!("${math}$"));
            }
            i = end + 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < src.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            out.push(src[start..i].trim_end_matches('.').to_owned());
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < src.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                i += 1;
            }
            let word = &src[start..i];
            // Names and acronyms survive translation: TabPFN, CARVEPrep, LLM.
            if word.chars().filter(char::is_ascii_uppercase).count() >= 2 {
                out.push(word.to_owned());
            }
        } else {
            i += src[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.sort();
    out
}

/// Comparable size of a text in either language: words, or CJK characters
/// at about 1.7 per English word.
fn size(text: &str) -> f64 {
    let cjk = text.chars().filter(|c| is_cjk(*c)).count() as f64;
    let words = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .count() as f64;
    words + cjk / 1.7
}

/// Dice overlap of two sorted token lists (as multisets).
fn dice(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.5;
    }
    let (mut i, mut j, mut both) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    2.0 * both as f64 / (a.len() + b.len()) as f64
}

struct Features {
    kind: SegmentKind,
    anchors: Vec<String>,
    size: f64,
}

fn features(text: &str, units: &[Unit]) -> Vec<Features> {
    units
        .iter()
        .map(|u| Features {
            kind: u.kind,
            anchors: anchors(&text[u.range.clone()]),
            size: size(&text[u.content.clone()]),
        })
        .collect()
}

fn score(a: &Features, b: &Features) -> Option<f64> {
    if a.kind != b.kind {
        return None;
    }
    let ratio = a.size.min(b.size) / a.size.max(b.size).max(1.0);
    Some(0.4 + 1.2 * dice(&a.anchors, &b.anchors) + 0.4 * (ratio - 0.5))
}

/// Pair the units of `a` and `b` (the same document in two languages).
pub fn align(a: &str, b: &str, mode: Mode) -> Pairing {
    let (ua, ub) = (units(a, mode), units(b, mode));
    let (fa, fb) = (features(a, &ua), features(b, &ub));
    let (n, m) = (fa.len(), fb.len());
    // best[i][j]: best score aligning the first i units of a with the first j of b.
    let width = m + 1;
    let mut best = vec![0.0f64; (n + 1) * width];
    let mut step = vec![0u8; (n + 1) * width]; // 1 pair, 2 skip a, 3 skip b
    for i in 1..=n {
        best[i * width] = -(i as f64) * GAP;
        step[i * width] = 2;
    }
    for j in 1..=m {
        best[j] = -(j as f64) * GAP;
        step[j] = 3;
    }
    for i in 1..=n {
        for j in 1..=m {
            let mut top = best[(i - 1) * width + j] - GAP;
            let mut how = 2;
            let left = best[i * width + j - 1] - GAP;
            if left > top {
                top = left;
                how = 3;
            }
            if let Some(s) = score(&fa[i - 1], &fb[j - 1]) {
                let pair = best[(i - 1) * width + j - 1] + s;
                if pair >= top {
                    top = pair;
                    how = 1;
                }
            }
            best[i * width + j] = top;
            step[i * width + j] = how;
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        match step[i * width + j] {
            1 => {
                pairs.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            2 => i -= 1,
            _ => j -= 1,
        }
    }
    pairs.reverse();
    // Second pass, out of order: a float can sit at a different place in
    // the mirror. Leftover units of the same kind pair when they share most
    // of their anchors.
    let used_a: Vec<bool> = (0..n).map(|i| pairs.iter().any(|p| p.0 == i)).collect();
    let mut used_b: Vec<bool> = (0..m).map(|j| pairs.iter().any(|p| p.1 == j)).collect();
    for i in (0..n).filter(|i| !used_a[*i]) {
        let found = (0..m)
            .filter(|j| !used_b[*j] && fa[i].kind == fb[*j].kind && fa[i].anchors.len() >= 2)
            .map(|j| (j, dice(&fa[i].anchors, &fb[j].anchors)))
            .filter(|(_, d)| *d >= 0.6)
            .max_by(|x, y| x.1.total_cmp(&y.1));
        if let Some((j, _)) = found {
            used_b[j] = true;
            pairs.push((i, j));
        }
    }
    pairs.sort_unstable();
    Pairing {
        pairs,
        a_units: n,
        b_units: m,
    }
}

/// A change to the mirror, by unit index of the mirror as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    /// New content for a unit.
    Replace { unit: usize, text: String },
    /// A new paragraph after a unit (`None`: before the first unit). `tag`
    /// names it in the result's origins.
    InsertAfter {
        unit: Option<usize>,
        text: String,
        tag: u64,
    },
    /// A new paragraph before a unit.
    InsertBefore { unit: usize, text: String, tag: u64 },
    /// The unit is gone, with the blank line that separated it.
    Delete { unit: usize },
}

/// New paragraphs after one anchor unit, in order: `(tag, text)`.
type Inserts = Vec<(Option<usize>, Vec<(u64, String)>)>;

/// Where a unit of a patched mirror came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Unit of the mirror before the patch.
    Kept(usize),
    /// Inserted by the edit with this tag.
    Inserted(u64),
}

/// What an inserted paragraph needs after it so that it stays a paragraph of
/// its own, given the text that follows the insertion point: nothing before
/// a blank line or the end, else enough newlines for one blank line (a
/// paragraph on the very next line would otherwise run on from it).
/// What an inserted paragraph needs before it, given the text before the
/// insertion point (a line start): enough newlines for one blank line.
fn blank_line_before(text: &str) -> &'static str {
    let text = text.trim_end_matches([' ', '\t', '\r']);
    if text.trim().is_empty() {
        return "";
    }
    let newlines = text
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .filter(|c| *c == '\n')
        .count();
    match newlines {
        0 => "\n\n",
        1 => "\n",
        _ => "",
    }
}

fn blank_line_after(rest: &str) -> &'static str {
    let rest = rest.trim_start_matches([' ', '\t', '\r']);
    if rest.trim().is_empty() {
        return "";
    }
    let newlines = rest
        .chars()
        .take_while(|c| c.is_whitespace())
        .filter(|c| *c == '\n')
        .count();
    match newlines {
        0 => "\n\n",
        1 => "\n",
        _ => "",
    }
}

/// The mirror after `edits`, and the origin of each unit of the result in
/// order. Translations are shaped to fit their slot (no blank lines, LaTeX
/// `%` escaped); everything not edited stays byte for byte.
pub fn patch(b: &str, mode: Mode, edits: &[Edit]) -> (String, Vec<Origin>) {
    let ub = units(b, mode);
    // Byte edits: (start, end, replacement), applied from the back.
    let mut changes: Vec<(usize, usize, String)> = Vec::new();
    let mut deleted = vec![false; ub.len()];
    for e in edits {
        if let Edit::Delete { unit } = e
            && let Some(gone) = deleted.get_mut(*unit)
        {
            *gone = true;
        }
    }
    // Nothing follows unit `k` but space and units deleted too.
    let tail_gone = |k: usize| {
        let mut at = ub[k].range.end;
        for (j, u) in ub.iter().enumerate().skip(k + 1) {
            if !deleted[j] || !b[at..u.range.start].trim().is_empty() {
                return false;
            }
            at = u.range.end;
        }
        b[at..].trim().is_empty()
    };
    // Inserts per anchor, in edit order, joined into one change each.
    let mut inserts: Inserts = Vec::new();
    let mut before: Vec<(usize, Vec<(u64, String)>)> = Vec::new();
    for e in edits {
        match e {
            Edit::Replace { unit, text } => {
                if let Some(u) = ub.get(*unit) {
                    let source = &b[u.content.clone()];
                    changes.push((
                        u.content.start,
                        u.content.end,
                        fit(text, u.kind, mode, source),
                    ));
                }
            }
            // Before the first unit is before unit 0 (in edit order with
            // the other inserts there).
            Edit::InsertAfter {
                unit: None,
                text,
                tag,
            }
            | Edit::InsertBefore { unit: 0, text, tag }
                if !ub.is_empty() =>
            {
                let body = fit(text, SegmentKind::Paragraph, mode, "");
                match before.iter_mut().find(|(at, _)| *at == 0) {
                    Some((_, list)) => list.push((*tag, body)),
                    None => before.push((0, vec![(*tag, body)])),
                }
            }
            // An anchor that isn't there: nothing sensible to do.
            Edit::InsertAfter { unit: Some(u), .. } if *u >= ub.len() => {}
            Edit::InsertAfter { unit, text, tag } => {
                let unit = *unit;
                let body = fit(text, SegmentKind::Paragraph, mode, "");
                match inserts.iter_mut().find(|(at, _)| *at == unit) {
                    Some((_, list)) => list.push((*tag, body)),
                    None => inserts.push((unit, vec![(*tag, body)])),
                }
            }
            Edit::InsertBefore { unit, text, tag } => {
                if *unit < ub.len() {
                    let body = fit(text, SegmentKind::Paragraph, mode, "");
                    match before.iter_mut().find(|(at, _)| at == unit) {
                        Some((_, list)) => list.push((*tag, body)),
                        None => before.push((*unit, vec![(*tag, body)])),
                    }
                }
            }
            Edit::Delete { unit } => {
                if let Some(u) = ub.get(*unit) {
                    // With text after it: the unit and the space after it, up
                    // to the next line, so what follows keeps the separator
                    // that was before the unit (a paragraph right after a
                    // deleted heading doesn't run into the one above). At the
                    // end (later units deleted too): from the end of the unit
                    // before, so trailing deletes touch and never overlap,
                    // and never reach into a unit that stays.
                    let (start, end) = if tail_gone(*unit) {
                        match unit.checked_sub(1).and_then(|k| ub.get(k)) {
                            Some(prev) => {
                                let prev_end = b[..u.range.start].trim_end().len();
                                (prev_end.max(prev.range.end), u.range.end)
                            }
                            None if *unit + 1 == ub.len() => (u.range.start, b.len()),
                            None => (u.range.start, u.range.end),
                        }
                    } else {
                        let rest = &b[u.range.end..];
                        let space = rest.len() - rest.trim_start().len();
                        let gap = rest[..space].rfind('\n').map_or(0, |i| i + 1);
                        (u.range.start, u.range.end + gap)
                    };
                    changes.push((start, end, String::new()));
                }
            }
        }
    }
    for (unit, list) in &inserts {
        let bodies: Vec<&str> = list.iter().map(|(_, t)| t.as_str()).collect();
        match unit.and_then(|u| ub.get(u)) {
            Some(u) => changes.push((
                u.range.end,
                u.range.end,
                format!(
                    "\n\n{}{}",
                    bodies.join("\n\n"),
                    blank_line_after(&b[u.range.end..])
                ),
            )),
            None => {
                let at = ub.first().map_or(b.len(), |u| u.range.start);
                changes.push((at, at, format!("{}\n\n", bodies.join("\n\n"))));
            }
        }
    }
    for (unit, list) in &before {
        let bodies: Vec<&str> = list.iter().map(|(_, t)| t.as_str()).collect();
        let at = ub[*unit].range.start;
        changes.push((
            at,
            at,
            format!("{}{}\n\n", blank_line_before(&b[..at]), bodies.join("\n\n")),
        ));
    }
    changes.sort_by(|x, y| y.0.cmp(&x.0).then(y.1.cmp(&x.1)));
    let mut out = b.to_owned();
    let mut floor = usize::MAX;
    for (start, end, text) in changes {
        // Changes never overlap; one that would is dropped.
        if end > floor {
            continue;
        }
        out.replace_range(start..end, &text);
        floor = start;
    }
    let tags = |anchor: Option<usize>| {
        inserts
            .iter()
            .find(|(at, _)| *at == anchor)
            .map(|(_, list)| {
                list.iter()
                    .map(|(tag, _)| Origin::Inserted(*tag))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let tags_before = |unit: usize| {
        before
            .iter()
            .find(|(at, _)| *at == unit)
            .map(|(_, list)| {
                list.iter()
                    .map(|(tag, _)| Origin::Inserted(*tag))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let mut origin: Vec<Origin> = tags(None);
    for (k, gone) in deleted.iter().enumerate() {
        origin.extend(tags_before(k));
        if !gone {
            origin.push(Origin::Kept(k));
        }
        origin.extend(tags(Some(k)));
    }
    (out, origin)
}

/// [`patch`], if the result segments as the edits say: kept units keep
/// their kind and inserted ones are plain paragraphs, nothing more and
/// nothing less. `None` otherwise (a translation that adds a heading, an insert
/// that runs into a neighbour), when writing it would put paragraphs in
/// the wrong places from then on.
pub fn patch_checked(b: &str, mode: Mode, edits: &[Edit]) -> Option<(String, Vec<Origin>)> {
    let before = units(b, mode);
    let (text, origin) = patch(b, mode, edits);
    let after = units(&text, mode);
    // What each unit must read now: a replacement or an insert as shaped,
    // anything else as it was. An edit dropped along the way shows here.
    let mut replaced: HashMap<usize, String> = HashMap::new();
    let mut inserted: HashMap<u64, String> = HashMap::new();
    for e in edits {
        match e {
            Edit::Replace { unit, text } => {
                if let Some(u) = before.get(*unit) {
                    replaced.insert(*unit, fit(text, u.kind, mode, &b[u.content.clone()]));
                }
            }
            Edit::InsertAfter { text, tag, .. } | Edit::InsertBefore { text, tag, .. } => {
                inserted.insert(*tag, fit(text, SegmentKind::Paragraph, mode, ""));
            }
            Edit::Delete { .. } => {}
        }
    }
    let fits = after.len() == origin.len()
        && after.iter().zip(&origin).all(|(u, o)| {
            let now = text[u.content.clone()].trim();
            match o {
                Origin::Kept(k) => before.get(*k).is_some_and(|old| {
                    let expected = replaced
                        .get(k)
                        .map_or(&b[old.content.clone()], String::as_str);
                    old.kind == u.kind && now == expected.trim()
                }),
                Origin::Inserted(tag) => {
                    u.is_plain_paragraph(&text)
                        && inserted.get(tag).is_some_and(|t| now == t.trim())
                }
            }
        });
    fits.then_some((text, origin))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EN: &str = "\\section{Introduction}\\label{sec:intro}\n\
Tabular foundation models such as TabPFN \\cite{hollmann2025} learn in context.\n\n\
We evaluate on $n=12$ datasets and report 0.86 accuracy (Table~\\ref{tab:main}).\n\n\
\\begin{equation}\n  y = f(x)\n\\end{equation}\n\n\
A short closing paragraph.\n";

    const ZH: &str = "\\section{引言}\\label{sec:intro}\n\
TabPFN \\cite{hollmann2025} 等表格基础模型可以在上下文中学习。\n\n\
我们在 $n = 12$ 个数据集上评测，准确率为 0.86（表~\\ref{tab:main}）。\n\n\
这是中文稿多出来的一段说明，英文稿没有。\n\n\
\\begin{equation}\n  y = f(x)\n\\end{equation}\n\n\
简短的结尾段。\n";

    #[test]
    fn mirrors_pair_by_anchors_and_order() {
        let p = align(EN, ZH, Mode::Latex);
        assert_eq!(p.a_units, 4);
        assert_eq!(p.b_units, 5);
        // Heading, two anchored paragraphs, the closing one; the extra
        // Chinese paragraph stays unpaired.
        assert_eq!(p.pairs, vec![(0, 0), (1, 1), (2, 2), (3, 4)]);
        assert!(p.coverage() > 0.75);
    }

    #[test]
    fn anchors_are_language_independent() {
        let a = anchors("We use TabPFN and LLM-based cleaning \\cite{a, b} with $x_1$ in 2024.");
        assert!(a.contains(&"TabPFN".to_owned()));
        assert!(a.contains(&"LLM-based".to_owned()));
        assert!(a.contains(&"{a}".to_owned()) && a.contains(&"{b}".to_owned()));
        assert!(a.contains(&"$x_1$".to_owned()));
        assert!(a.contains(&"2024".to_owned()));
        assert!(!a.contains(&"We".to_owned()));
    }

    #[test]
    fn patching_replaces_inserts_and_deletes_in_place() {
        let edits = vec![
            Edit::Replace {
                unit: 1,
                text: "TabPFN 等模型能在上下文中学习。".into(),
            },
            Edit::Delete { unit: 3 },
            Edit::InsertAfter {
                unit: Some(4),
                text: "新增的结尾补充。\n\n多余空行被去掉。".into(),
                tag: 7,
            },
            Edit::InsertAfter {
                unit: Some(4),
                text: "再补一段。".into(),
                tag: 8,
            },
        ];
        let (out, origin) = patch(ZH, Mode::Latex, &edits);
        assert!(out.contains("TabPFN 等模型能在上下文中学习。"));
        assert!(!out.contains("英文稿没有"));
        assert!(out.contains("简短的结尾段。\n\n新增的结尾补充。\n多余空行被去掉。\n\n再补一段。"));
        // Untouched parts stay byte for byte.
        assert!(out.contains("\\begin{equation}\n  y = f(x)\n\\end{equation}"));
        assert!(out.starts_with("\\section{引言}\\label{sec:intro}\n"));
        assert_eq!(
            origin,
            vec![
                Origin::Kept(0),
                Origin::Kept(1),
                Origin::Kept(2),
                Origin::Kept(4),
                Origin::Inserted(7),
                Origin::Inserted(8)
            ]
        );
        assert_eq!(units(&out, Mode::Latex).len(), origin.len());
    }

    #[test]
    fn identical_structure_pairs_one_to_one() {
        let p = align(
            "A one.\n\nB two.\n\nC three.\n",
            "甲一。\n\n乙二。\n\n丙三。\n",
            Mode::Plain,
        );
        assert_eq!(p.pairs, vec![(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn an_insert_never_runs_into_the_next_paragraph() {
        let zh = "\\section{方法}\\label{sec:method}\n第一段。\n\n第二段。\n";
        let edits = vec![Edit::InsertAfter {
            unit: Some(0),
            text: "新的一段。".into(),
            tag: 1,
        }];
        let (out, origin) = patch_checked(zh, Mode::Latex, &edits).expect("a clean insert");
        assert_eq!(
            out,
            "\\section{方法}\\label{sec:method}\n\n新的一段。\n\n第一段。\n\n第二段。\n"
        );
        assert_eq!(
            origin,
            vec![
                Origin::Kept(0),
                Origin::Inserted(1),
                Origin::Kept(1),
                Origin::Kept(2)
            ]
        );
        // At the end of the file, nothing is added after it.
        let edits = vec![Edit::InsertAfter {
            unit: Some(2),
            text: "结尾。".into(),
            tag: 2,
        }];
        let (out, _) = patch_checked(zh, Mode::Latex, &edits).unwrap();
        assert!(out.ends_with("第二段。\n\n结尾。\n"), "{out:?}");
    }

    #[test]
    fn a_patch_that_changes_the_structure_is_refused() {
        let zh = "第一段。\n\n第二段。\n";
        let edits = vec![Edit::Replace {
            unit: 0,
            text: "第一段。\n\\section{注入}\n更多。".into(),
        }];
        let mode = Mode::Latex;
        let (unchecked, _) = patch(zh, mode, &edits);
        assert_ne!(
            units(&unchecked, mode).len(),
            2,
            "the heading took a unit of its own"
        );
        assert!(patch_checked(zh, mode, &edits).is_none());
        // Items and captions are not plain paragraphs.
        let list = "\\begin{itemize}\n\\item 一项。\n\\end{itemize}\n\n正文。\n";
        let u = units(list, mode);
        assert!(!u[0].is_plain_paragraph(list));
        assert!(u[1].is_plain_paragraph(list));
    }

    #[test]
    fn a_paragraph_before_the_first_one_goes_outside_a_leading_list() {
        let zh = "\\begin{itemize}\n\\item 一项。\n\\end{itemize}\n\n正文。\n";
        // After the list, before the first paragraph of its own.
        let edits = vec![Edit::InsertBefore {
            unit: 1,
            text: "新段。".into(),
            tag: 9,
        }];
        let (out, origin) = patch_checked(zh, Mode::Latex, &edits).expect("fits");
        assert_eq!(
            out,
            "\\begin{itemize}\n\\item 一项。\n\\end{itemize}\n\n新段。\n\n正文。\n"
        );
        assert_eq!(
            origin,
            vec![Origin::Kept(0), Origin::Inserted(9), Origin::Kept(1)]
        );
    }

    #[test]
    fn deleting_a_heading_keeps_the_paragraphs_apart() {
        let zh = "第零段。\n\n\\section{甲}\n第一段。\n\n第二段。\n";
        let edits = vec![Edit::Delete { unit: 1 }];
        let (out, origin) = patch_checked(zh, Mode::Latex, &edits).expect("fits");
        assert_eq!(out, "第零段。\n\n第一段。\n\n第二段。\n");
        assert_eq!(
            origin,
            vec![Origin::Kept(0), Origin::Kept(2), Origin::Kept(3)]
        );
        // The last unit takes the blank line before it.
        let (out, _) = patch_checked(zh, Mode::Latex, &[Edit::Delete { unit: 3 }]).unwrap();
        assert_eq!(out, "第零段。\n\n\\section{甲}\n第一段。\n");
        // An indented next unit and an insert before it both survive.
        let indented = "甲。\n\n  乙。\n";
        let edits = vec![
            Edit::Delete { unit: 0 },
            Edit::InsertBefore {
                unit: 1,
                text: "丙。".into(),
                tag: 3,
            },
        ];
        let (out, origin) = patch_checked(indented, Mode::Plain, &edits).expect("fits");
        assert_eq!(origin, vec![Origin::Inserted(3), Origin::Kept(1)]);
        assert!(
            !out.contains('甲') && out.contains("丙。") && out.contains("乙。"),
            "{out:?}"
        );
    }

    #[test]
    fn deletes_at_the_end_never_overlap_or_eat_a_neighbour() {
        // The last two units deleted together.
        let zh = "甲。\n\n乙。\n\n丙。\n";
        let edits = vec![Edit::Delete { unit: 1 }, Edit::Delete { unit: 2 }];
        let (out, origin) = patch_checked(zh, Mode::Plain, &edits).expect("fits");
        assert_eq!(out, "甲。\n");
        assert_eq!(origin, vec![Origin::Kept(0)]);
        // Trailing spaces on the line before: its replacement survives.
        let spaced = "甲。\n\n乙。  \n\n丙。\n";
        let edits = vec![
            Edit::Replace {
                unit: 1,
                text: "新乙。".into(),
            },
            Edit::Delete { unit: 2 },
        ];
        let (out, _) = patch_checked(spaced, Mode::Plain, &edits).expect("fits");
        assert_eq!(out, "甲。\n\n新乙。\n");
        // And an insert after it too.
        let edits = vec![
            Edit::InsertAfter {
                unit: Some(1),
                text: "丁。".into(),
                tag: 4,
            },
            Edit::Delete { unit: 2 },
        ];
        let (out, origin) = patch_checked(spaced, Mode::Plain, &edits).expect("fits");
        assert_eq!(out, "甲。\n\n乙。  \n\n丁。\n");
        assert_eq!(
            origin,
            vec![Origin::Kept(0), Origin::Kept(1), Origin::Inserted(4)]
        );
    }
}
