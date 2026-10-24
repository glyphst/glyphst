//! Where meaning search keeps its engine and models; putting files in place
//! is the models crate's.

use std::path::PathBuf;

pub use glyphst_models::install::*;

/// The folder meaning search's engine and models live in.
pub fn root() -> PathBuf {
    glyphst_models::catalog::Task::Embedding.root()
}
