//! Local models: what can run on this computer (the catalog), what suits
//! it, and getting each one: pinned, resumable downloads, engines unpacked
//! into place, and files removed again. Meaning search (`glyphst-embed`)
//! and local OCR (`glyphst-ocr`) set their models up through it.

pub mod catalog;
pub mod download;
pub mod install;
pub mod machine;
pub mod manifest;
pub mod setup;

pub use manifest::{Asset, Engine};
