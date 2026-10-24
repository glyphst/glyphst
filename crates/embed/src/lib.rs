//! Meaning search's engine: downloading llama.cpp's `llama-server` and
//! EmbeddingGemma 2 (pinned, resumable), running the server, and turning
//! text into vectors through it.

pub use glyphst_models::download;
pub mod install;
pub mod llama;
pub mod manifest;
pub mod server;
pub mod setup;
