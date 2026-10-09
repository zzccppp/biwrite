//! Byte offsets (Rust) to UTF-16 offsets (JavaScript / CodeMirror positions).

/// Convert byte offsets into UTF-16 code-unit offsets in one forward pass.
///
/// Offsets should be non-decreasing for linear time; an offset smaller than
/// the previous one restarts the scan. Offsets must lie on char boundaries
/// (segment ranges always do); an offset past the end maps to the end.
pub fn byte_to_utf16(text: &str, offsets: impl IntoIterator<Item = usize>) -> Vec<usize> {
    let mut byte_pos = 0;
    let mut u16_pos = 0;
    offsets
        .into_iter()
        .map(|target| {
            let target = target.min(text.len());
            if target < byte_pos {
                byte_pos = 0;
                u16_pos = 0;
            }
            if let Some(slice) = text.get(byte_pos..target) {
                u16_pos += slice.chars().map(char::len_utf16).sum::<usize>();
                byte_pos = target;
            }
            u16_pos
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_identity() {
        assert_eq!(byte_to_utf16("hello", [0, 2, 5]), vec![0, 2, 5]);
    }

    #[test]
    fn multibyte_and_astral() {
        let text = "a数😀b";
        // bytes: a=1, 数=3, 😀=4 ; utf16: a=1, 数=1, 😀=2
        assert_eq!(byte_to_utf16(text, [0, 1, 4, 8, 9]), vec![0, 1, 2, 4, 5]);
    }

    #[test]
    fn unsorted_offsets_restart() {
        assert_eq!(byte_to_utf16("数数", [6, 3, 0]), vec![2, 1, 0]);
    }
}
