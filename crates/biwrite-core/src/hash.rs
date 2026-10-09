//! Content hashing for segments.
//!
//! The segment key is the blake3 hash of the *normalized* text, so changes that
//! only touch whitespace (re-wrapping a paragraph, trailing spaces) do not
//! invalidate a cached translation.

use std::fmt;

/// Normalize text for hashing: trim and collapse every whitespace run to a
/// single ASCII space.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// blake3 hash of normalized segment text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Hash `text` after normalizing it.
    pub fn of(text: &str) -> Self {
        Self(*blake3::hash(normalize(text).as_bytes()).as_bytes())
    }

    /// Hash raw bytes without normalization (used for non-translatable
    /// segments, where whitespace may be significant).
    pub fn of_raw(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex representation (64 chars).
    pub fn to_hex(&self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = self.to_hex();
        write!(f, "ContentHash({})", &hex[..12])
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize("  a\n\tb   c \n"), "a b c");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize(" \n "), "");
    }

    #[test]
    fn hash_ignores_rewrapping() {
        let a = ContentHash::of("The quick brown\nfox jumps.");
        let b = ContentHash::of("The quick\nbrown fox   jumps.  ");
        assert_eq!(a, b);
        assert_ne!(a, ContentHash::of("The quick brown fox jumped."));
    }

    #[test]
    fn hex_is_64_chars() {
        assert_eq!(ContentHash::of("x").to_hex().len(), 64);
    }
}
