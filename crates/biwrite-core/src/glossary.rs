//! Glossary: preferred renderings of terms, picked per segment.
//!
//! Only the entries a segment actually mentions are sent with its request,
//! and the cache key holds a [`fingerprint`] of exactly those entries, so
//! editing the glossary invalidates only the paragraphs it affects.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::csv::{self, CsvError};
use crate::hash::normalize;
use crate::lang::Direction;

/// At most this many entries are sent with one request.
pub const MAX_PER_REQUEST: usize = 30;

/// Largest glossary accepted (import or save).
pub const MAX_ENTRIES: usize = 10_000;

/// One glossary entry: an English term and its Chinese rendering.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlossaryEntry {
    pub term: String,
    /// Chinese rendering; `None` means "keep in English".
    #[serde(default)]
    pub translation: Option<String>,
}

impl GlossaryEntry {
    pub fn new(term: &str, translation: Option<&str>) -> Self {
        Self {
            term: term.to_owned(),
            translation: translation.map(str::to_owned),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GlossaryError {
    #[error("the file is not UTF-8 text; in Excel use “Save As → CSV UTF-8 (Comma delimited)”")]
    NotUtf8,
    #[error(transparent)]
    Csv(#[from] CsvError),
    #[error("the glossary has {count} terms; BiWrite supports up to {MAX_ENTRIES}")]
    TooMany { count: usize },
}

/// Refuse glossaries over [`MAX_ENTRIES`].
pub fn check_size(entries: &[GlossaryEntry]) -> Result<(), GlossaryError> {
    match entries.len() {
        count if count > MAX_ENTRIES => Err(GlossaryError::TooMany { count }),
        _ => Ok(()),
    }
}

/// Clean up entries for saving: trim, collapse inner whitespace, drop empty
/// terms, treat an empty or `KEEP` translation as keep-in-English, and merge
/// duplicate terms (case-insensitive; the later entry wins, at the earlier
/// position).
pub fn normalized(entries: Vec<GlossaryEntry>) -> Vec<GlossaryEntry> {
    let mut out: Vec<GlossaryEntry> = Vec::with_capacity(entries.len());
    let mut index: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    for e in entries {
        let term = normalize(&e.term);
        if term.is_empty() {
            continue;
        }
        let translation = e
            .translation
            .map(|t| normalize(&t))
            .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("keep"));
        let entry = GlossaryEntry { term, translation };
        match index.get(&entry.term.to_lowercase()) {
            Some(&i) => out[i] = entry,
            None => {
                index.insert(entry.term.to_lowercase(), out.len());
                out.push(entry);
            }
        }
    }
    out
}

/// Read a `term,translation` CSV (header optional, UTF-8 with or without
/// BOM). Returns normalized entries, at most [`MAX_ENTRIES`].
pub fn from_csv(bytes: &[u8]) -> Result<Vec<GlossaryEntry>, GlossaryError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| GlossaryError::NotUtf8)?;
    let mut rows = csv::parse(text)?.into_iter().peekable();
    if rows
        .peek()
        .and_then(|r| r.first())
        .is_some_and(|c| c.trim().eq_ignore_ascii_case("term"))
    {
        rows.next();
    }
    let entries = rows
        .filter_map(|row| {
            let mut cells = row.into_iter();
            let term = cells.next()?;
            Some(GlossaryEntry {
                term,
                translation: cells.next(),
            })
        })
        .collect();
    let entries = normalized(entries);
    check_size(&entries)?;
    Ok(entries)
}

/// Write entries as CSV with a header and a BOM (so Excel reads it as
/// UTF-8). Keep-in-English entries have an empty translation.
pub fn to_csv(entries: &[GlossaryEntry]) -> String {
    let mut out = String::from("\u{feff}");
    csv::write_row(&mut out, &["term", "translation"]);
    for e in entries {
        csv::write_row(&mut out, &[&e.term, e.translation.as_deref().unwrap_or("")]);
    }
    out
}

/// The entries `source` mentions (see [`Glossary::relevant`]). Builds the
/// index each time; keep a [`Glossary`] for repeated lookups.
pub fn relevant(
    source: &str,
    direction: Direction,
    glossary: &[GlossaryEntry],
) -> Vec<GlossaryEntry> {
    Glossary::new(glossary.to_vec()).relevant(source, direction)
}

/// A glossary indexed for per-paragraph matching: only entries whose first
/// two words (English) or first few characters (Chinese rendering) occur in
/// a paragraph are checked, so large glossaries stay cheap.
#[derive(Clone, Debug, Default)]
pub struct Glossary {
    entries: Vec<GlossaryEntry>,
    /// Lowercased terms and renderings, parallel to `entries`.
    terms: Vec<String>,
    renderings: Vec<Option<String>>,
    /// Entries by the first one or two ASCII words of their term ("graph
    /// neural" for "graph neural network").
    by_words: HashMap<String, Vec<usize>>,
    /// Entries whose term doesn't start with an ASCII letter or digit.
    other: Vec<usize>,
    /// Entries by the first (up to) [`CHAR_KEY`] characters of their
    /// rendering.
    by_chars: HashMap<String, Vec<usize>>,
}

/// Characters of a rendering used as its index key.
const CHAR_KEY: usize = 4;

impl Glossary {
    pub fn new(entries: Vec<GlossaryEntry>) -> Self {
        let mut g = Self {
            terms: entries.iter().map(|e| e.term.to_lowercase()).collect(),
            renderings: entries
                .iter()
                .map(|e| e.translation.as_ref().map(|t| t.to_lowercase()))
                .collect(),
            entries,
            ..Self::default()
        };
        for (i, term) in g.terms.iter().enumerate() {
            let words: Vec<&str> = ascii_words(term).take(2).collect();
            match words.first() {
                Some(first) if term.starts_with(first) => {
                    g.by_words.entry(words.join(" ")).or_default().push(i)
                }
                _ => g.other.push(i),
            }
            if let Some(r) = &g.renderings[i] {
                let key: String = r.chars().take(CHAR_KEY).collect();
                if !key.is_empty() {
                    g.by_chars.entry(key).or_default().push(i);
                }
            }
        }
        g
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[GlossaryEntry] {
        &self.entries
    }

    /// The entries `source` mentions, as they should appear in a request:
    /// in order of first mention, at most [`MAX_PER_REQUEST`].
    ///
    /// - English → Chinese: the term, case-insensitively and as a whole word
    ///   (plurals `-s`, `-es`, `-y`→`-ies` included).
    /// - Chinese → English: the Chinese rendering, presented as
    ///   `中文 → term`; keep-in-English entries match their English term.
    pub fn relevant(&self, source: &str, direction: Direction) -> Vec<GlossaryEntry> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        let hay = normalize(source).to_lowercase();
        let mut found: Vec<(usize, GlossaryEntry)> = self
            .candidates(&hay, direction)
            .into_iter()
            .filter_map(|i| {
                let e = &self.entries[i];
                match (direction, &e.translation, &self.renderings[i]) {
                    (Direction::ZhEn, Some(zh), Some(lower)) => hay
                        .find(lower.as_str())
                        .map(|pos| (pos, GlossaryEntry::new(zh, Some(&e.term)))),
                    _ => find_term(&hay, &self.terms[i]).map(|pos| (pos, e.clone())),
                }
            })
            .collect();
        found.sort_by_key(|(pos, _)| *pos);
        let mut out: Vec<GlossaryEntry> = Vec::new();
        for (_, e) in found {
            if out.len() == MAX_PER_REQUEST {
                break;
            }
            if !out.iter().any(|o| o.term == e.term) {
                out.push(e);
            }
        }
        out
    }

    /// Entries that might occur in `hay`, in index order.
    fn candidates(&self, hay: &str, direction: Direction) -> Vec<usize> {
        let mut set: HashSet<usize> = HashSet::new();
        // Terms: matched in English, and keep-in-English ones in Chinese too.
        let by_term = |i: &usize| direction == Direction::EnZh || self.renderings[*i].is_none();
        let add = |key: &str, set: &mut HashSet<usize>| {
            if let Some(ids) = self.by_words.get(key) {
                set.extend(ids.iter().filter(|i| by_term(i)));
            }
        };
        // Only the last word of a term can be a plural: one-word keys and
        // the second word of two-word keys are looked up by their stems.
        let words: Vec<&str> = ascii_words(hay).collect();
        for (i, word) in words.iter().enumerate() {
            for stem in stems(word) {
                add(&stem, &mut set);
            }
            if let Some(next) = words.get(i + 1) {
                for stem in stems(next) {
                    add(&format!("{word} {stem}"), &mut set);
                }
            }
        }
        set.extend(self.other.iter().filter(|i| by_term(i)));
        if direction == Direction::ZhEn {
            let chars: Vec<char> = hay.chars().collect();
            let mut key = String::new();
            for i in 0..chars.len() {
                for len in 1..=CHAR_KEY {
                    let Some(window) = chars.get(i..i + len) else {
                        continue;
                    };
                    key.clear();
                    key.extend(window);
                    if let Some(ids) = self.by_chars.get(&key) {
                        set.extend(ids);
                    }
                }
            }
        }
        let mut ids: Vec<usize> = set.into_iter().collect();
        ids.sort_unstable();
        ids
    }
}

/// Maximal runs of ASCII letters and digits.
fn ascii_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// A word and the singular forms it may be a plural of.
fn stems(word: &str) -> Vec<Cow<'_, str>> {
    let mut out = vec![Cow::Borrowed(word)];
    out.extend(word.strip_suffix('s').map(Cow::Borrowed));
    out.extend(word.strip_suffix("es").map(Cow::Borrowed));
    if let Some(stem) = word.strip_suffix("ies") {
        out.push(Cow::Owned(format!("{stem}y")));
    }
    out
}

/// First position of `term` in `hay` (both lowercase). Terms containing
/// ASCII letters or digits match whole words only.
fn find_term(hay: &str, term: &str) -> Option<usize> {
    if term.is_empty() {
        return None;
    }
    if !term.chars().any(|c| c.is_ascii_alphanumeric()) {
        return hay.find(term);
    }
    let mut stems = vec![(term, &["", "s", "es"][..])];
    if let Some(stem) = term.strip_suffix('y') {
        stems.push((stem, &["ies"][..]));
    }
    stems
        .into_iter()
        .filter_map(|(stem, suffixes)| {
            hay.match_indices(stem).find_map(|(pos, _)| {
                let before = hay[..pos].chars().next_back();
                if before.is_some_and(|c| c.is_ascii_alphanumeric()) {
                    return None;
                }
                let rest = &hay[pos + stem.len()..];
                suffixes
                    .iter()
                    .filter_map(|s| rest.strip_prefix(s))
                    .any(|after| !after.starts_with(|c: char| c.is_ascii_alphanumeric()))
                    .then_some(pos)
            })
        })
        .min()
}

/// Identifies a set of request entries for the cache key: 0 for none,
/// otherwise a 63-bit hash (it is stored as a signed SQLite integer).
pub fn fingerprint(entries: &[GlossaryEntry]) -> u64 {
    if entries.is_empty() {
        return 0;
    }
    let mut h = blake3::Hasher::new();
    for e in entries {
        h.update(e.term.as_bytes());
        h.update(b"\x1f");
        match &e.translation {
            Some(t) => h.update(t.as_bytes()),
            None => h.update(b"\x1e"),
        };
        h.update(b"\x1d");
    }
    let mut first = [0u8; 8];
    first.copy_from_slice(&h.finalize().as_bytes()[..8]);
    (u64::from_le_bytes(first) & (u64::MAX >> 1)).max(1)
}

#[cfg(test)]
#[path = "glossary_tests.rs"]
mod tests;
