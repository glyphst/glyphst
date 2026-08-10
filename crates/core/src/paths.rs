use std::{
    fs, io,
    path::{Path, PathBuf},
};

const APP_DIR_NAME: &str = "glyphst-reader";
const LEGACY_APP_DIR_NAME: &str = "glyphst";

pub fn config_dir() -> PathBuf {
    config_base_dir().join(APP_DIR_NAME)
}

pub fn state_dir() -> PathBuf {
    state_base_dir().join(APP_DIR_NAME)
}

pub fn data_dir() -> PathBuf {
    data_base_dir().join(APP_DIR_NAME)
}

pub fn migrate_legacy_app_dirs() -> io::Result<()> {
    migrate_legacy_dir(&config_base_dir())?;
    migrate_legacy_dir(&state_base_dir())?;
    migrate_legacy_dir(&data_base_dir())
}

fn config_base_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn state_base_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn data_base_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn migrate_legacy_dir(base_dir: &Path) -> io::Result<()> {
    let legacy_dir = base_dir.join(LEGACY_APP_DIR_NAME);
    let app_dir = base_dir.join(APP_DIR_NAME);

    if legacy_dir.try_exists()? && !app_dir.try_exists()? {
        fs::rename(legacy_dir, app_dir)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn test_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "glyphst-reader-paths-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn migrates_legacy_directory() {
        let root = test_dir("migrate");
        let legacy_dir = root.join(LEGACY_APP_DIR_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("config.kdl"), "theme \"test\"").unwrap();

        migrate_legacy_dir(&root).unwrap();

        assert!(!legacy_dir.exists());
        assert_eq!(
            fs::read_to_string(root.join(APP_DIR_NAME).join("config.kdl")).unwrap(),
            "theme \"test\""
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn keeps_legacy_directory_when_new_directory_exists() {
        let root = test_dir("existing");
        let legacy_dir = root.join(LEGACY_APP_DIR_NAME);
        let app_dir = root.join(APP_DIR_NAME);
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::create_dir_all(&app_dir).unwrap();
        fs::write(legacy_dir.join("marker"), "legacy").unwrap();
        fs::write(app_dir.join("marker"), "reader").unwrap();

        migrate_legacy_dir(&root).unwrap();

        assert_eq!(
            fs::read_to_string(legacy_dir.join("marker")).unwrap(),
            "legacy"
        );
        assert_eq!(
            fs::read_to_string(app_dir.join("marker")).unwrap(),
            "reader"
        );

        fs::remove_dir_all(root).ok();
    }
}
