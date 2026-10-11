use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result};
use glyphst_pdf::DocumentPassword;
use gpui::{App, Global, Task};
use sha2::{Digest as _, Sha256};

use crate::library::normalize_path;

const CREDENTIAL_URL_PREFIX: &str = "https://glyphst.app/credentials/pdf/v1/";
const CREDENTIAL_USERNAME: &str = "PDF";
type CredentialReadResult = Result<Option<(String, Vec<u8>)>>;

pub(crate) enum StoredPdfPassword {
    Missing,
    Found(DocumentPassword),
    Corrupt,
}

trait CredentialBackend: Send + Sync {
    fn read(&self, url: &str, cx: &App) -> Task<CredentialReadResult>;
    fn write(&self, url: &str, password: &[u8], cx: &App) -> Task<Result<()>>;
    fn delete(&self, url: &str, cx: &App) -> Task<Result<()>>;
}

struct PlatformCredentialBackend;

impl CredentialBackend for PlatformCredentialBackend {
    fn read(&self, url: &str, cx: &App) -> Task<CredentialReadResult> {
        cx.read_credentials(url)
    }

    fn write(&self, url: &str, password: &[u8], cx: &App) -> Task<Result<()>> {
        cx.write_credentials(url, CREDENTIAL_USERNAME, password)
    }

    fn delete(&self, url: &str, cx: &App) -> Task<Result<()>> {
        cx.delete_credentials(url)
    }
}

#[derive(Clone)]
pub(crate) struct PdfPasswordStore {
    backend: Arc<dyn CredentialBackend>,
    known_saved: Arc<Mutex<HashSet<String>>>,
}

impl Global for PdfPasswordStore {}

impl Default for PdfPasswordStore {
    fn default() -> Self {
        Self {
            backend: Arc::new(PlatformCredentialBackend),
            known_saved: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

impl PdfPasswordStore {
    pub(crate) fn ensure(cx: &mut App) -> Self {
        if !cx.has_global::<Self>() {
            cx.set_global(Self::default());
        }
        cx.global::<Self>().clone()
    }

    pub(crate) fn read(&self, path: &Path, cx: &mut App) -> Task<Result<StoredPdfPassword>> {
        let url = credential_url(path);
        let task = self.backend.read(&url, cx);
        let known_saved = self.known_saved.clone();
        cx.spawn(async move |_| {
            let stored = task.await?;
            let Some((_, bytes)) = stored else {
                known_saved
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&url);
                return Ok(StoredPdfPassword::Missing);
            };
            known_saved
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(url);
            match String::from_utf8(bytes) {
                Ok(password) => Ok(StoredPdfPassword::Found(DocumentPassword::new(password))),
                Err(_) => Ok(StoredPdfPassword::Corrupt),
            }
        })
    }

    pub(crate) fn write(
        &self,
        path: &Path,
        password: DocumentPassword,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let url = credential_url(path);
        let task = self
            .backend
            .write(&url, password.expose_secret().as_bytes(), cx);
        let known_saved = self.known_saved.clone();
        cx.spawn(async move |_| {
            task.await.context("failed to save PDF password")?;
            known_saved
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(url);
            Ok(())
        })
    }

    pub(crate) fn delete(&self, path: &Path, cx: &mut App) -> Task<Result<()>> {
        let url = credential_url(path);
        let task = self.backend.delete(&url, cx);
        let known_saved = self.known_saved.clone();
        cx.spawn(async move |_| {
            task.await.context("failed to forget PDF password")?;
            known_saved
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&url);
            Ok(())
        })
    }

    pub(crate) fn is_known_saved(&self, path: &Path) -> bool {
        self.known_saved
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&credential_url(path))
    }

    #[cfg(test)]
    pub(crate) fn memory() -> Self {
        Self {
            backend: Arc::new(MemoryCredentialBackend::default()),
            known_saved: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}

fn credential_url(path: &Path) -> String {
    let path = normalize_path(path);
    let mut hasher = Sha256::new();
    hash_path(&mut hasher, &path);
    format!(
        "{CREDENTIAL_URL_PREFIX}{}",
        glyphst_core::lower_hex(hasher.finalize())
    )
}

#[cfg(unix)]
fn hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;

    hasher.update(path.as_os_str().as_bytes());
}

#[cfg(windows)]
fn hash_path(hasher: &mut Sha256, path: &Path) {
    use std::os::windows::ffi::OsStrExt as _;

    for unit in path.as_os_str().encode_wide() {
        hasher.update(unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn hash_path(hasher: &mut Sha256, path: &Path) {
    hasher.update(path.to_string_lossy().as_bytes());
}

#[cfg(test)]
#[derive(Default)]
struct MemoryCredentialBackend {
    entries: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

#[cfg(test)]
impl CredentialBackend for MemoryCredentialBackend {
    fn read(&self, url: &str, _: &App) -> Task<CredentialReadResult> {
        let stored = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(url)
            .cloned()
            .map(|password| (CREDENTIAL_USERNAME.to_string(), password));
        Task::ready(Ok(stored))
    }

    fn write(&self, url: &str, password: &[u8], _: &App) -> Task<Result<()>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(url.to_string(), password.to_vec());
        Task::ready(Ok(()))
    }

    fn delete(&self, url: &str, _: &App) -> Task<Result<()>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(url);
        Task::ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn credential_url_is_stable_and_does_not_expose_the_path() {
        let path = Path::new("/tmp/Private Papers/protected.pdf");
        let first = credential_url(path);
        let second = credential_url(path);

        assert_eq!(first, second);
        assert!(first.starts_with(CREDENTIAL_URL_PREFIX));
        assert!(!first.contains("Private"));
        assert!(!first.contains("protected.pdf"));
        assert_ne!(first, credential_url(Path::new("/tmp/other.pdf")));
    }

    #[gpui::test]
    fn memory_store_round_trips_and_forgets_passwords(cx: &mut TestAppContext) {
        let store = PdfPasswordStore::memory();
        let path = Path::new("/tmp/protected.pdf");

        let write = cx.update(|cx| store.write(path, DocumentPassword::new("secret"), cx));
        cx.foreground_executor().block_test(write).unwrap();
        assert!(store.is_known_saved(path));

        let read = cx.update(|cx| store.read(path, cx));
        let stored = cx.foreground_executor().block_test(read).unwrap();
        let StoredPdfPassword::Found(password) = stored else {
            panic!("expected the saved password");
        };
        assert_eq!(password.expose_secret(), "secret");

        let delete = cx.update(|cx| store.delete(path, cx));
        cx.foreground_executor().block_test(delete).unwrap();
        assert!(!store.is_known_saved(path));

        let read = cx.update(|cx| store.read(path, cx));
        assert!(matches!(
            cx.foreground_executor().block_test(read).unwrap(),
            StoredPdfPassword::Missing
        ));
    }
}
