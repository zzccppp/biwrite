//! Errors returned to the frontend. Serialized as plain message strings.

use biwrite_core::DecodeError;
use biwrite_engine::{EngineError, TranslateError};

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Decode {
        path: String,
        #[source]
        source: DecodeError,
    },
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("background task failed: {0}")]
    Task(String),
    #[error("{0}")]
    Settings(String),
    /// Provider errors are already redacted at the source.
    #[error(transparent)]
    Provider(#[from] TranslateError),
}

impl CommandError {
    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

impl serde::Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type CommandResult<T> = Result<T, CommandError>;
