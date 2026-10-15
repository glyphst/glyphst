use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn parse_settings(text: &str) -> Result<Settings> {
    let document = text
        .parse::<KdlDocument>()
        .context("failed to parse KDL document")?;
    Ok(Settings::from_document(&document))
}

pub(super) fn config_path() -> PathBuf {
    crate::app_paths::config_dir().join("config.kdl")
}

/// Where the settings file is, for showing and opening it.
pub fn settings_file() -> PathBuf {
    config_path()
}

pub fn user_themes_dir() -> PathBuf {
    crate::app_paths::config_dir().join("themes")
}

pub(super) fn state_path() -> PathBuf {
    crate::app_paths::state_dir().join("config.kdl")
}

pub(super) fn load_settings(path: &Path) -> Result<Settings> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .and_then(|text| parse_settings(&text))
}

pub(super) fn write_kdl(path: &Path, contents: String) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_directory(parent)?;
    }

    atomic_write_private(path, contents.as_bytes())
}

fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let already_exists = path.exists();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        if !already_exists {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }

    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary_path)
            .with_context(|| format!("failed to create {}", temporary_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", temporary_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary_path.display()))?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;

        #[cfg(unix)]
        {
            let directory = fs::File::open(parent)
                .with_context(|| format!("failed to open {}", parent.display()))?;
            directory
                .sync_all()
                .with_context(|| format!("failed to sync {}", parent.display()))?;
        }

        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}
