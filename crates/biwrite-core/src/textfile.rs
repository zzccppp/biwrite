//! Byte-exact round-tripping of text files.
//!
//! The editor works on normalized text (UTF-8, `\n` line endings, no BOM).
//! A [`TextFile`] remembers the original bytes so that saving an unedited
//! document writes exactly the bytes that were read, and saving an edited one
//! restores the original BOM and dominant line ending.

use serde::Serialize;

const BOM: &str = "\u{feff}";

/// Line terminator style used when writing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
    Cr,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
            Self::Cr => "\r",
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("the file is not valid UTF-8 (first invalid byte at offset {offset})")]
    NotUtf8 { offset: usize },
}

/// A text file as loaded from disk plus its normalized editor text.
#[derive(Clone, Debug, Default)]
pub struct TextFile {
    original: Vec<u8>,
    text: String,
    bom: bool,
    eol: LineEnding,
    mixed_eol: bool,
}

impl TextFile {
    /// Decode file bytes. Only UTF-8 (with or without BOM) is accepted.
    pub fn decode(bytes: Vec<u8>) -> Result<Self, DecodeError> {
        let s = std::str::from_utf8(&bytes).map_err(|e| DecodeError::NotUtf8 {
            offset: e.valid_up_to(),
        })?;
        let (bom, body) = match s.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let (crlf, cr, lf) = count_line_endings(body);
        let eol = if crlf >= lf && crlf >= cr && crlf > 0 {
            LineEnding::Crlf
        } else if cr > lf {
            LineEnding::Cr
        } else {
            LineEnding::Lf
        };
        let kinds_present = [crlf, cr, lf].iter().filter(|n| **n > 0).count();
        let text = if crlf + cr == 0 {
            body.to_owned()
        } else {
            body.replace("\r\n", "\n").replace('\r', "\n")
        };
        Ok(Self {
            text,
            bom,
            eol,
            mixed_eol: kinds_present > 1,
            original: bytes,
        })
    }

    /// An unsaved document with the given text, LF endings and no BOM.
    pub fn untitled(text: String) -> Self {
        Self {
            original: text.clone().into_bytes(),
            text,
            ..Self::default()
        }
    }

    /// Normalized text for the editor.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn line_ending(&self) -> LineEnding {
        self.eol
    }

    pub fn has_bom(&self) -> bool {
        self.bom
    }

    /// Whether the original file mixed several line-ending styles.
    pub fn has_mixed_line_endings(&self) -> bool {
        self.mixed_eol
    }

    /// Bytes to write for `text`. If `text` equals the loaded text, the
    /// original bytes are returned untouched (byte-identical save).
    pub fn encode(&self, text: &str) -> Vec<u8> {
        if text == self.text {
            return self.original.clone();
        }
        let mut out = String::with_capacity(text.len() + text.len() / 32 + BOM.len());
        if self.bom {
            out.push_str(BOM);
        }
        match self.eol {
            LineEnding::Lf => out.push_str(text),
            eol => out.push_str(&text.replace('\n', eol.as_str())),
        }
        out.into_bytes()
    }

    /// The state after `bytes` (produced by [`Self::encode`] for `text`) were
    /// written to disk.
    pub fn saved(&self, text: String, bytes: Vec<u8>) -> Self {
        // An edited save rewrites every terminator with the dominant style.
        let mixed_eol = self.mixed_eol && text == self.text;
        Self {
            original: bytes,
            text,
            bom: self.bom,
            eol: self.eol,
            mixed_eol,
        }
    }
}

fn count_line_endings(s: &str) -> (usize, usize, usize) {
    let bytes = s.as_bytes();
    let (mut crlf, mut cr, mut lf) = (0, 0, 0);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    (crlf, cr, lf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_unedited(bytes: &[u8]) {
        let f = TextFile::decode(bytes.to_vec()).unwrap();
        let text = f.text().to_owned();
        assert_eq!(
            f.encode(&text),
            bytes,
            "unedited save must be byte-identical"
        );
    }

    #[test]
    fn unedited_files_are_byte_identical() {
        roundtrip_unedited(b"hello\nworld\n");
        roundtrip_unedited(b"hello\r\nworld\r\n");
        roundtrip_unedited(b"mac\rclassic\r");
        roundtrip_unedited(b"mixed\r\nendings\nhere\r");
        roundtrip_unedited("\u{feff}bom \u{1F600} text\r\n".as_bytes());
        roundtrip_unedited(b"");
        roundtrip_unedited(b"no trailing newline");
    }

    #[test]
    fn editor_text_is_normalized() {
        let f = TextFile::decode("\u{feff}a\r\nb\rc\n".as_bytes().to_vec()).unwrap();
        assert_eq!(f.text(), "a\nb\nc\n");
        assert!(f.has_bom());
        assert!(f.has_mixed_line_endings());
    }

    #[test]
    fn edited_text_keeps_bom_and_crlf() {
        let f = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes().to_vec()).unwrap();
        assert_eq!(f.line_ending(), LineEnding::Crlf);
        assert_eq!(f.encode("a\nB\nc\n"), "\u{feff}a\r\nB\r\nc\r\n".as_bytes());
    }

    #[test]
    fn rejects_invalid_utf8() {
        let err = TextFile::decode(vec![b'o', b'k', 0xff, 0xfe]).unwrap_err();
        assert_eq!(err, DecodeError::NotUtf8 { offset: 2 });
    }

    #[test]
    fn saved_state_round_trips() {
        let f = TextFile::decode(b"x\r\n".to_vec()).unwrap();
        let bytes = f.encode("y\n");
        let g = f.saved("y\n".to_owned(), bytes.clone());
        assert_eq!(g.encode("y\n"), bytes);
    }
}
