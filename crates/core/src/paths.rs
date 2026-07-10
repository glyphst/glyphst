use std::path::{Path, PathBuf};

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
}

pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
}
