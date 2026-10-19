//! Where the library keeps its files: the folder the user chose (frame
//! 04k), recorded in the database so it and the files never disagree, and
//! moving them there. Linked files the user moved are found again by their
//! inode or content.

use super::catalog::{hash_file, migrate_document_path_in_library};
use super::storage_helpers::{
    create_private_directory, decode, encode, ensure_managed_library_directories,
};
use super::*;

const LIBRARY_FILES_ROOT_KEY: &str = "library_files_root";
const FILES_ROOT_STORAGE_VERSION: u8 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredFilesRootV1 {
    version: u8,
    path: StoredPathV1,
}

/// The files folder recorded in `database`, if the user chose one.
pub(super) fn read_files_root(database: &Database) -> Result<Option<PathBuf>> {
    let read_txn = database.begin_read()?;
    let table = read_txn.open_table(UI_STATE_V1_TABLE)?;
    let Some(value) = table.get(LIBRARY_FILES_ROOT_KEY)? else {
        return Ok(None);
    };
    let stored = decode::<StoredFilesRootV1>(value.value())?;
    require_storage_version(stored.version, FILES_ROOT_STORAGE_VERSION)?;
    Ok(Some(stored.path.into_path()?))
}

/// What moving the files did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesMove {
    pub files: usize,
    pub bytes: u64,
}

impl LibraryStore {
    /// The folder managed files are kept in.
    pub fn files_root(&self) -> Option<&Path> {
        self.managed_library_root.as_deref()
    }

    /// Copies every managed file (and the assistant's pictures) into
    /// `new_root`, checks each copy, points the library there, and only
    /// then deletes the old copies. Interrupted, the library stays where it
    /// was; run again, files already copied are kept.
    pub fn move_files_root(
        &mut self,
        new_root: &Path,
        cancelled: &AtomicBool,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<FilesMove> {
        let Some(old_root) = self.managed_library_root.clone() else {
            return Err(anyhow!("this library keeps no files"));
        };
        let new_root = normalize_path(new_root);
        if !new_root.is_absolute() {
            return Err(anyhow!("the library folder needs an absolute path"));
        }
        if new_root == old_root {
            return Ok(FilesMove::default());
        }
        if new_root.starts_with(&old_root) || old_root.starts_with(&new_root) {
            return Err(anyhow!(
                "the new library folder cannot be inside the old one or hold it"
            ));
        }
        ensure_managed_library_directories(&new_root)?;

        let managed = self
            .library
            .library_attachments
            .iter()
            .filter(|attachment| !attachment.is_linked())
            .cloned()
            .collect::<Vec<_>>();
        let pictures = fs::read_dir(old_root.join("ai-images"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_file())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let total = managed.len() + pictures.len();
        let mut moved = FilesMove::default();
        let mut moves = Vec::new();
        for (index, attachment) in managed.iter().enumerate() {
            if cancelled.load(AtomicOrdering::Acquire) {
                return Err(anyhow!("moving the library was cancelled"));
            }
            let relative = attachment.path.strip_prefix(&old_root).with_context(|| {
                format!(
                    "{} is outside the library folder",
                    attachment.path.display()
                )
            })?;
            let target = new_root.join(relative);
            if !attachment.path.exists() && target.exists() {
                // Copied by an earlier, interrupted move.
            } else {
                copy_checked(&attachment.path, &target, Some(attachment.content_sha256))?;
            }
            moved.files += 1;
            moved.bytes += attachment.file_size;
            moves.push((attachment.path.clone(), target));
            progress(index + 1, total);
        }
        for (index, picture) in pictures.iter().enumerate() {
            if let Some(name) = picture.file_name() {
                copy_checked(picture, &new_root.join("ai-images").join(name), None)?;
            }
            progress(managed.len() + index + 1, total);
        }

        // Point everything that names a file by its path at the copy.
        let previous = self.library.clone();
        for attachment in &mut self.library.library_attachments {
            if !attachment.is_linked()
                && let Ok(relative) = attachment.path.strip_prefix(&old_root)
            {
                attachment.path = new_root.join(relative);
            }
        }
        for (old, new) in &moves {
            migrate_document_path_in_library(&mut self.library, old, new);
        }
        self.library.sanitize();
        self.managed_library_root = Some(new_root.clone());
        let written = self
            .write_relinked_library()
            .and_then(|()| self.write_files_root(&new_root));
        if let Err(error) = written {
            self.library = previous;
            self.managed_library_root = Some(old_root);
            return Err(error);
        }
        for (old, new) in &moves {
            self.document_edit_histories.remove(old);
            self.document_edit_histories.remove(new);
        }
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        self.catalog_revision = self.catalog_revision.wrapping_add(1);

        for (old, _) in &moves {
            let _ = fs::remove_file(old);
            if let Some(directory) = old.parent() {
                let _ = fs::remove_dir(directory);
            }
        }
        for picture in &pictures {
            let _ = fs::remove_file(picture);
        }
        Ok(moved)
    }

    fn write_files_root(&self, root: &Path) -> Result<()> {
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        {
            let mut table = write_txn.open_table(UI_STATE_V1_TABLE)?;
            let encoded = encode(&StoredFilesRootV1 {
                version: FILES_ROOT_STORAGE_VERSION,
                path: StoredPathV1::from_path(root),
            })?;
            table.insert(LIBRARY_FILES_ROOT_KEY, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    /// Linked files that are no longer where they were, with where each
    /// seems to have gone: a file with the same inode, or the same size and
    /// content, in its old folder or the folders around it.
    pub fn find_moved_linked_files(&self) -> Vec<(DocumentId, PathBuf)> {
        self.library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.is_linked() && attachment.primary)
            .filter(|attachment| !attachment.path.exists())
            .filter_map(|attachment| {
                locate_moved_file(attachment).map(|path| (attachment.item_id, path))
            })
            .collect()
    }
}

/// Copies `source` to `target` through a temporary file and checks the
/// copy's content.
fn copy_checked(source: &Path, target: &Path, expected: Option<[u8; 32]>) -> Result<()> {
    if let Some(parent) = target.parent() {
        create_private_directory(parent)?;
    }
    let temporary = target.with_extension("glyphst-moving");
    fs::copy(source, &temporary).with_context(|| format!("failed to copy {}", source.display()))?;
    if let Some(expected) = expected {
        let copied = hash_file(&temporary, &AtomicBool::new(false))?;
        if copied != expected {
            let _ = fs::remove_file(&temporary);
            return Err(anyhow!("the copy of {} differs", source.display()));
        }
    }
    fs::rename(&temporary, target)
        .with_context(|| format!("failed to place {}", target.display()))?;
    Ok(())
}

fn locate_moved_file(attachment: &LibraryAttachment) -> Option<PathBuf> {
    let folder = attachment.path.parent()?;
    let mut folders = vec![folder.to_path_buf()];
    if let Some(parent) = folder.parent() {
        folders.push(parent.to_path_buf());
        if let Ok(siblings) = fs::read_dir(parent) {
            folders.extend(
                siblings
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir() && path != folder),
            );
        }
    }
    let mut same_size = Vec::new();
    for folder in folders {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            if attachment
                .file_identity
                .is_some_and(|identity| FileIdentity::of(&metadata) == Some(identity))
            {
                return Some(path);
            }
            if metadata.len() == attachment.file_size {
                same_size.push(path);
            }
        }
    }
    same_size.into_iter().find(|path| {
        hash_file(path, &AtomicBool::new(false)).is_ok_and(|hash| hash == attachment.content_sha256)
    })
}
