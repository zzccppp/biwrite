//! Pure document logic for BiWrite. No Tauri, no async, no IO.
//!
//! - [`segment`]: split a source document into translation segments.
//! - [`hash`]: normalized content hashing (segment keys).
//! - [`document`]: stable segment IDs via LCS alignment across edits.
//! - [`compose`]: build the other-language document (pane swap).
//! - [`bilingual`]: bilingual Markdown export.
//! - [`glossary`]: term matching per segment, CSV import/export ([`csv`]).
//! - [`lang`]: translation direction, CJK-aware tokens.
//! - [`protect`]: placeholder protection of math, citations, references.
//! - [`textfile`]: byte-exact file round-tripping (BOM, line endings).
//! - [`utf16`]: byte offsets to editor (UTF-16) offsets.

pub mod assist;
pub mod bilingual;
pub mod compose;
pub mod csv;
pub mod document;
pub mod glossary;
pub mod hash;
pub mod lang;
pub mod mode;
pub mod pair;
pub mod protect;
pub mod segment;
pub mod textfile;
pub mod utf16;

pub use bilingual::{Bilingual, bilingual_markdown};
pub use compose::{ComposeError, Composed, Insert, compose};
pub use document::{ApplyReport, DocSegment, DocumentModel, SegmentId, similarity};
pub use glossary::GlossaryEntry;
pub use hash::ContentHash;
pub use lang::Direction;
pub use mode::Mode;
pub use protect::{PlaceholderError, Protector, RestoreReport};
pub use segment::{Segment, SegmentKind, SkipReason};
pub use textfile::{DecodeError, LineEnding, TextFile};
