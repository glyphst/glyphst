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

    /// Whether the user chose the files folder (else it is the default one
    /// in the app's data folder).
    pub fn files_root_chosen(&self) -> bool {
        self.files_root_chosen
    }

    /// Copies every managed file (and the assistant's pictures) into
    /// `new_root`, checks each copy, points the library there, and only
    /// then deletes the old copies. Interrupted, the library stays where it
    /// was; run again, files already copied are kept. The Reader runs the
    /// copying on a worker: [`Self::plan_files_move`],
    /// [`copy_planned_files`], then [`Self::commit_files_move`].
    pub fn move_files_root(
        &mut self,
        new_root: &Path,
        cancelled: &AtomicBool,
        progress: impl FnMut(usize, usize),
    ) -> Result<FilesMove> {
        let plan = self.plan_files_move(new_root)?;
        copy_planned_files(&plan, cancelled, progress)?;
        self.commit_files_move(&plan)
    }

    /// What moving the files to `new_root` copies.
    pub fn plan_files_move(&self, new_root: &Path) -> Result<FilesMovePlan> {
        let Some(old_root) = self.managed_library_root.clone() else {
            return Err(anyhow!("this library keeps no files"));
        };
        let new_root = normalize_path(new_root);
        if !new_root.is_absolute() {
            return Err(anyhow!("the library folder needs an absolute path"));
        }
        if new_root != old_root
            && (new_root.starts_with(&old_root) || old_root.starts_with(&new_root))
        {
            return Err(anyhow!(
                "the new library folder cannot be inside the old one or hold it"
            ));
        }
        let mut copies = Vec::new();
        for attachment in self
            .library
            .library_attachments
            .iter()
            .filter(|attachment| !attachment.is_linked())
        {
            let relative = attachment.path.strip_prefix(&old_root).with_context(|| {
                format!(
                    "{} is outside the library folder",
                    attachment.path.display()
                )
            })?;
            copies.push(PlannedCopy {
                from: attachment.path.clone(),
                to: new_root.join(relative),
                sha256: Some(attachment.content_sha256),
                size: attachment.file_size,
            });
        }
        if let Ok(entries) = fs::read_dir(old_root.join("ai-images")) {
            for path in entries.flatten().map(|entry| entry.path()) {
                if let (true, Some(name)) = (path.is_file(), path.file_name()) {
                    copies.push(PlannedCopy {
                        to: new_root.join("ai-images").join(name),
                        from: path,
                        sha256: None,
                        size: 0,
                    });
                }
            }
        }
        Ok(FilesMovePlan {
            old_root,
            new_root,
            copies,
            copy_linked: false,
        })
    }

    /// Points the library at the copies `plan` made, records the folder,
    /// and deletes the old copies. Refuses when files were added since the
    /// plan (run the move again).
    pub fn commit_files_move(&mut self, plan: &FilesMovePlan) -> Result<FilesMove> {
        if plan.new_root == plan.old_root {
            return Ok(FilesMove::default());
        }
        if self.managed_library_root.as_deref() != Some(plan.old_root.as_path()) {
            return Err(anyhow!("the library moved while its files were copied"));
        }
        let planned = plan
            .copies
            .iter()
            .map(|copy| copy.from.as_path())
            .collect::<HashSet<_>>();
        if self.library.library_attachments.iter().any(|attachment| {
            !attachment.is_linked() && !planned.contains(attachment.path.as_path())
        }) {
            return Err(anyhow!(
                "files were added while the library moved; move it again"
            ));
        }
        let moves = plan
            .copies
            .iter()
            .filter(|copy| copy.sha256.is_some())
            .map(|copy| (copy.from.clone(), copy.to.clone()))
            .collect::<Vec<_>>();
        let moved = FilesMove {
            files: moves.len(),
            bytes: plan
                .copies
                .iter()
                .filter(|copy| copy.sha256.is_some())
                .map(|copy| copy.size)
                .sum(),
        };

        // Point everything that names a file by its path at the copy.
        let previous = self.library.clone();
        for attachment in &mut self.library.library_attachments {
            if !attachment.is_linked()
                && let Ok(relative) = attachment.path.strip_prefix(&plan.old_root)
            {
                attachment.path = plan.new_root.join(relative);
            }
        }
        for (old, new) in &moves {
            migrate_document_path_in_library(&mut self.library, old, new);
        }
        self.library.sanitize();
        let was_chosen = self.files_root_chosen;
        self.managed_library_root = Some(plan.new_root.clone());
        self.files_root_chosen = true;
        let written = self
            .write_relinked_library()
            .and_then(|()| self.write_files_root(&plan.new_root));
        if let Err(error) = written {
            self.library = previous;
            self.managed_library_root = Some(plan.old_root.clone());
            self.files_root_chosen = was_chosen;
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

        for copy in &plan.copies {
            let _ = fs::remove_file(&copy.from);
            if copy.sha256.is_some()
                && let Some(directory) = copy.from.parent()
            {
                let _ = fs::remove_dir(directory);
            }
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

/// The files a move copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilesMovePlan {
    old_root: PathBuf,
    new_root: PathBuf,
    copies: Vec<PlannedCopy>,
    /// Copies linked files into the folder rather than moving it.
    copy_linked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedCopy {
    from: PathBuf,
    to: PathBuf,
    /// A managed file's hash, checked on the copy; `None` for a picture.
    sha256: Option<[u8; 32]>,
    size: u64,
}

impl FilesMovePlan {
    pub fn new_root(&self) -> &Path {
        &self.new_root
    }

    pub fn files(&self) -> usize {
        self.copies.len()
    }

    /// Whether this copies linked files into the folder (see
    /// [`LibraryStore::plan_copy_linked`]).
    pub fn copies_linked(&self) -> bool {
        self.copy_linked
    }

    pub fn bytes(&self) -> u64 {
        self.copies.iter().map(|copy| copy.size).sum()
    }
}

/// Makes the plan's copies (on a worker thread). A copy an earlier,
/// interrupted run made is kept.
pub fn copy_planned_files(
    plan: &FilesMovePlan,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> Result<()> {
    if plan.new_root == plan.old_root && !plan.copy_linked {
        return Ok(());
    }
    ensure_managed_library_directories(&plan.new_root)?;
    let total = plan.copies.len();
    for (index, copy) in plan.copies.iter().enumerate() {
        if cancelled.load(AtomicOrdering::Acquire) {
            return Err(anyhow!("moving the library was cancelled"));
        }
        if !copy.from.exists() && copy.to.exists() {
            // Copied by an earlier run.
        } else {
            copy_checked(&copy.from, &copy.to, copy.sha256)?;
        }
        progress(index + 1, total);
    }
    Ok(())
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

impl LibraryStore {
    /// Moves what this library keeps about the document at `old` (its
    /// reading position, highlights, notes, bookmarks, preferences) to
    /// `new`: a file opened from outside the library, now copied in.
    pub fn adopt_document_path(&mut self, old: &Path, new: &Path) -> Result<()> {
        let (old, new) = (normalize_path(old), normalize_path(new));
        if old == new {
            return Ok(());
        }
        let previous = self.library.clone();
        migrate_document_path_in_library(&mut self.library, &old, &new);
        self.library.sanitize();
        if let Err(error) = self.write_relinked_library() {
            self.library = previous;
            return Err(error);
        }
        self.document_edit_histories.remove(&old);
        self.document_edit_histories.remove(&new);
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        Ok(())
    }
}

impl LibraryStore {
    /// How many items' files are linked (left where they were, outside
    /// the library folder) and their size.
    pub fn linked_files(&self) -> (usize, u64) {
        let linked = self
            .library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.is_linked() && attachment.path.exists());
        linked.fold((0, 0), |(count, bytes), attachment| {
            (count + 1, bytes + attachment.file_size)
        })
    }

    /// Copying every linked file that is still there into the library
    /// folder, each under a key of its own (`files/<key>/<name>`). The
    /// originals stay where they are.
    pub fn plan_copy_linked(&self) -> Result<FilesMovePlan> {
        let Some(root) = self.managed_library_root.clone() else {
            return Err(anyhow!("this library keeps no files"));
        };
        let copies = self
            .library
            .library_attachments
            .iter()
            .filter(|attachment| attachment.is_linked() && attachment.path.exists())
            .map(|attachment| {
                let name = attachment
                    .path
                    .file_name()
                    .map(|name| name.to_os_string())
                    .unwrap_or_else(|| "document".into());
                PlannedCopy {
                    from: attachment.path.clone(),
                    to: root
                        .join("files")
                        .join(Uuid::new_v4().to_string())
                        .join(name),
                    sha256: Some(attachment.content_sha256),
                    size: attachment.file_size,
                }
            })
            .collect();
        Ok(FilesMovePlan {
            old_root: root.clone(),
            new_root: root,
            copies,
            copy_linked: true,
        })
    }

    /// Points the linked items `plan` copied at their copies, which the
    /// library now keeps, with their positions, highlights and notes.
    /// Returns how many moved in.
    pub fn commit_copy_linked(&mut self, plan: &FilesMovePlan) -> Result<usize> {
        let previous = self.library.clone();
        let mut moved = Vec::new();
        for copy in plan.copies.iter().filter(|copy| copy.to.is_file()) {
            let Some(attachment) = self
                .library
                .library_attachments
                .iter_mut()
                .find(|attachment| attachment.is_linked() && attachment.path == copy.from)
            else {
                continue;
            };
            attachment.storage = AttachmentStorage::Managed;
            attachment.storage_key = copy
                .to
                .parent()
                .and_then(Path::file_name)
                .map(|key| key.to_string_lossy().into_owned())
                .unwrap_or_default();
            attachment.path = copy.to.clone();
            attachment.file_identity = None;
            moved.push((copy.from.clone(), copy.to.clone()));
        }
        for (old, new) in &moved {
            migrate_document_path_in_library(&mut self.library, old, new);
        }
        self.library.sanitize();
        if let Err(error) = self.write_relinked_library() {
            self.library = previous;
            return Err(error);
        }
        for (old, new) in &moved {
            self.document_edit_histories.remove(old);
            self.document_edit_histories.remove(new);
        }
        self.history_entry_revisions
            .lock()
            .expect("history persistence revisions lock poisoned")
            .clear();
        self.bookmarks_revision = self.bookmarks_revision.wrapping_add(1);
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        Ok(moved.len())
    }
}

#[cfg(test)]
mod adopt_tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glyphst-adopt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rect() -> HighlightRect {
        HighlightRect {
            page_idx: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        }
    }

    #[test]
    fn a_file_copied_in_keeps_its_place_and_highlights() {
        let dir = folder("path");
        let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let outside = dir.join("report.pdf");
        fs::write(&outside, b"%PDF-1.4 report").unwrap();
        store
            .add_highlight(&outside, HighlightColor::Yellow, vec![rect()], "seen", "")
            .unwrap();
        store
            .replace_history_entries(vec![HistoryEntry {
                path: normalize_path(&outside),
                opened: 1,
                position: DocumentPosition::fixed(0.0, 0.0, 1.0, 7),
            }])
            .unwrap();
        let copy = dir.join("files/k/report.pdf");
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::copy(&outside, &copy).unwrap();

        store.adopt_document_path(&outside, &copy).unwrap();
        assert_eq!(store.highlights_for(&copy).len(), 1);
        assert!(store.highlights_for(&outside).is_empty());
        assert_eq!(store.history_position(&copy).unwrap().page, 7);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn linked_files_are_copied_into_the_folder_and_the_originals_stay() {
        let dir = folder("linked");
        let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let outside = dir.join("elsewhere/paper.pdf");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::write(&outside, b"%PDF-1.4 linked paper").unwrap();
        let LibraryImportOutcome::Added(id) = store
            .link_item(&outside, LibraryItemMetadata::for_path(&outside), None)
            .unwrap()
        else {
            panic!("expected an item");
        };
        store
            .add_highlight(&outside, HighlightColor::Green, vec![rect()], "kept", "")
            .unwrap();
        assert_eq!(store.linked_files().0, 1);

        let plan = store.plan_copy_linked().unwrap();
        assert!(plan.copies_linked());
        assert_eq!(plan.files(), 1);
        copy_planned_files(&plan, &AtomicBool::new(false), |_, _| {}).unwrap();
        assert_eq!(store.commit_copy_linked(&plan).unwrap(), 1);

        let attachment = store.primary_attachment(id).unwrap().clone();
        assert!(!attachment.is_linked());
        assert!(attachment.path.starts_with(store.files_root().unwrap()));
        assert!(attachment.path.is_file());
        assert!(outside.is_file(), "the original stays");
        assert_eq!(store.highlights_for(&attachment.path).len(), 1);
        assert_eq!(store.linked_files().0, 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_linked_file_changed_since_is_not_copied() {
        let dir = folder("changed");
        let mut store = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let outside = dir.join("paper.pdf");
        fs::write(&outside, b"%PDF-1.4 before").unwrap();
        store
            .link_item(&outside, LibraryItemMetadata::for_path(&outside), None)
            .unwrap();
        fs::write(&outside, b"%PDF-1.4 after, edited").unwrap();
        let plan = store.plan_copy_linked().unwrap();
        assert!(copy_planned_files(&plan, &AtomicBool::new(false), |_, _| {}).is_err());
        // Nothing committed: still linked.
        assert_eq!(store.linked_files().0, 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
