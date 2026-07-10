use std::path::PathBuf;

pub fn config_dir() -> PathBuf {
    glyphst_core::paths::config_dir()
}

pub fn state_dir() -> PathBuf {
    glyphst_core::paths::state_dir()
}
