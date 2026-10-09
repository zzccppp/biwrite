//! LaTeX support for BiWrite: finding the project's root and engine,
//! compiling with the installed TeX distribution, reading the log,
//! SyncTeX in both directions, paper templates, and the Chinese mirror as
//! a compilable document. No Tauri dependency, so all of it is testable
//! with `cargo test`.

pub mod compile;
pub mod locate;
pub mod log;
pub mod mirror;
pub mod project;
pub mod synctex;
pub mod templates;
pub mod toolchain;

pub use compile::{CompileError, Compiled, Job, Outcome, compile};
pub use locate::locate;
pub use log::{Issue, Severity};
pub use mirror::{MIRROR_DIR, redirect_include, with_chinese};
pub use project::{
    Engine, ProjectError, clean, engine_for, find_main, is_main, project_engine, root_for,
    tex_files,
};
pub use synctex::{PdfBox, SourcePoint, SyncError};
pub use templates::{Manifest, Template, TemplateError};
pub use toolchain::{Toolchain, detect, find_bin};
