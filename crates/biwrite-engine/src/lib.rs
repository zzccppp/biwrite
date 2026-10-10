//! BiWrite translation engine: keeps the right pane in sync with the source.
//!
//! The engine owns the [`biwrite_core::DocumentModel`], decides which segments
//! need translating (cache lookups, revise mode), and runs a bounded queue of
//! provider requests with per-segment generations so stale results are
//! cancelled or discarded. It has no Tauri dependency; the app delivers its
//! events through an [`EventSink`].

mod engine;
mod queue;
mod random;
mod state;
mod swap;

pub mod cache;
pub mod events;
pub mod mock;
pub mod sqlite_cache;
pub mod translator;

pub use cache::{CacheError, CacheGroup, CacheKey, CacheStats, MemoryCache, TranslationCache};
pub use engine::{Engine, EngineError};
pub use events::{
    EventSink, Fill, NullSink, SegmentLayout, SegmentState, SegmentStatus, SessionUsage, Snapshot,
};
pub use mock::MockTranslator;
pub use sqlite_cache::SqliteCache;
pub use state::{EngineSettings, MAX_BATCH};
pub use swap::{Mirror, Swapped};
pub use translator::{
    BatchOutput, BatchPartialFn, BoxFuture, GlossaryEntry, PartialFn, Revision, TokenUsage,
    TranslateError, TranslationOutput, TranslationRequest, Translator,
};
