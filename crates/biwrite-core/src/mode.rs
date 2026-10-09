//! Document modes (how the source is segmented and highlighted).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Source syntax of the document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Plain,
    Markdown,
    Latex,
}

impl Mode {
    /// Pick a mode from a file extension; unknown extensions are plain text.
    pub fn from_path(path: &Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("tex" | "latex" | "ltx" | "sty" | "cls") => Self::Latex,
            Some("md" | "markdown" | "mdown" | "mkd") => Self::Markdown,
            _ => Self::Plain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_from_extension() {
        assert_eq!(Mode::from_path(Path::new("paper.TEX")), Mode::Latex);
        assert_eq!(Mode::from_path(Path::new("notes.md")), Mode::Markdown);
        assert_eq!(Mode::from_path(Path::new("draft.txt")), Mode::Plain);
        assert_eq!(Mode::from_path(Path::new("README")), Mode::Plain);
    }
}
