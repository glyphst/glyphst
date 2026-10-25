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

/// Where disposable caches go.
pub fn cache_dir() -> PathBuf {
    cache_base_dir().join(APP_DIR_NAME)
}

/// The CJK fonts' folder (read at run time), whose `fonts/` holds the
/// faces: `$GLYPHST_FONTS_DIR` when set, else the packaged copy at
/// `<exe>/../share/glyphst-reader/fonts`, else the downloaded copy, else
/// `fonts/` beside this repository's checkout. A glyphst-fonts checkout
/// holds `fonts/manifest.txt`; a download is there once its first face is
/// (each file arrives whole).
pub fn fonts_dir() -> Option<PathBuf> {
    if let Some(dir) = fonts_dir_override() {
        return Some(dir);
    }
    let packaged = std::env::current_exe().ok().and_then(|exe| {
        Some(
            exe.parent()?
                .parent()?
                .join("share")
                .join(APP_DIR_NAME)
                .join("fonts"),
        )
    });
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../fonts");
    let downloaded = fonts_download_dir();
    let is_checkout = |dir: &PathBuf| dir.join("fonts/manifest.txt").is_file();
    packaged
        .filter(is_checkout)
        .or_else(|| {
            downloaded
                .join(DOWNLOADED_FACE)
                .is_file()
                .then_some(downloaded)
        })
        .or_else(|| Some(checkout).filter(is_checkout))
}

/// Where the CJK fonts download to: `$GLYPHST_FONTS_DIR` when set (so the
/// download is what the Reader reads), else `fonts/` in the data folder.
pub fn fonts_download_dir() -> PathBuf {
    fonts_dir_override().unwrap_or_else(|| data_dir().join("fonts"))
}

fn fonts_dir_override() -> Option<PathBuf> {
    std::env::var_os("GLYPHST_FONTS_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// The face whose presence marks the downloaded CJK fonts.
const DOWNLOADED_FACE: &str = "fonts/noto-sans-cjk-sc/NotoSansCJKsc-Regular.otf";

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

fn cache_base_dir() -> PathBuf {
    dirs::cache_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
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
