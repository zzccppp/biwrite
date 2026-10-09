//! Pure document logic for BiWrite. No Tauri, no async, no IO.
//!
//! - [`segment`]: split a source document into translation segments.
//! - [`hash`]: normalized content hashing (segment keys).
//! - [`document`]: stable segment IDs via LCS alignment across edits.
//! - [`compose`]: build the other-language document (pane swap).
//! - [`lang`]: translation direction, CJK-aware tokens.
//! - [`textfile`]: byte-exact file round-tripping (BOM, line endings).
//! - [`utf16`]: byte offsets to editor (UTF-16) offsets.

pub mod compose;
pub mod document;
pub mod hash;
pub mod lang;
pub mod mode;
pub mod segment;
pub mod textfile;
pub mod utf16;

pub use compose::{ComposeError, Composed, Insert, compose};
pub use document::{ApplyReport, DocSegment, DocumentModel, SegmentId, similarity};
pub use hash::ContentHash;
pub use lang::Direction;
pub use mode::Mode;
pub use segment::{Segment, SegmentKind, SkipReason};
pub use textfile::{DecodeError, LineEnding, TextFile};
