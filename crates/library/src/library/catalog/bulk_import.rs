//! Bringing many items in from another manager (Zotero) a batch at a time:
//! one catalog write per batch carries the items, their files, notes,
//! tags, collections and trash state, so a large library imports in
//! linear time.

use super::*;

/// One item an import brings in.
#[derive(Debug, Default)]
pub struct ImportEntry {
    pub file: Option<ImportFile>,
    pub metadata: LibraryItemMetadata,
    pub note: String,
    pub tags: Vec<String>,
    /// Tags the other manager added by itself; kept, and marked as the
    /// import's so they can be told apart.
    pub source_tags: Vec<String>,
    /// Collection paths from the top, created when missing.
    pub collections: Vec<Vec<String>>,
    /// Where it came from; its `external_ids` make a second run skip it.
    pub record: MetadataRecord,
    pub trashed: bool,
}

#[derive(Debug)]
pub enum ImportFile {
    Managed(PreparedManagedImport),
    Linked(PreparedLinkedImport),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportEntryOutcome {
    Added(DocumentId),
    /// The file was in the library already; the item there gets the
    /// entry's external ids.
    Duplicate(DocumentId),
}

impl LibraryStore {
    /// Adds `entries` in one catalog write. `source` names the manager, for
    /// the marks on `source_tags`.
    pub fn import_entries(
        &mut self,
        entries: Vec<ImportEntry>,
        source: &str,
    ) -> Result<Vec<ImportEntryOutcome>> {
        let root = self.managed_library_root.clone();
        let mut outcomes = Vec::with_capacity(entries.len());
        let mut adding = Vec::new();
        let mut installed = Vec::<(PathBuf, PathBuf)>::new();
        let mut seen = Vec::<([u8; 32], u64, usize)>::new();
        let mut failure = None;
        for entry in entries {
            let (sha, size, path) = match &entry.file {
                Some(ImportFile::Managed(prepared)) => {
                    (Some(prepared.content_sha256), prepared.file_size, None)
                }
                Some(ImportFile::Linked(prepared)) => (
                    Some(prepared.content_sha256),
                    prepared.file_size,
                    Some(prepared.path.clone()),
                ),
                None => (None, 0, None),
            };
            if let Some(sha) = sha {
                let existing = self.library.library_attachments.iter().find(|attachment| {
                    path.as_ref().is_some_and(|path| &attachment.path == path)
                        || (attachment.content_sha256 == sha && attachment.file_size == size)
                });
                if let Some(attachment) = existing {
                    outcomes.push(Err((attachment.item_id, entry.record.external_ids)));
                    continue;
                }
                if let Some((_, _, index)) = seen
                    .iter()
                    .find(|(seen_sha, seen_size, _)| *seen_sha == sha && *seen_size == size)
                {
                    // The same file twice in one batch: the first one wins.
                    outcomes.push(Ok(*index));
                    continue;
                }
                seen.push((sha, size, adding.len()));
            }
            let file = match entry.file {
                Some(ImportFile::Managed(prepared)) => {
                    match install_managed(root.as_deref(), &prepared) {
                        Ok(final_path) => {
                            installed.push((
                                final_path
                                    .parent()
                                    .map(Path::to_path_buf)
                                    .unwrap_or_default(),
                                prepared.staged_directory.clone(),
                            ));
                            Some((
                                final_path,
                                prepared.storage_key.clone(),
                                prepared.content_sha256,
                                prepared.format,
                                prepared.file_size,
                                prepared.modified,
                                AttachmentStorage::Managed,
                                None,
                            ))
                        }
                        Err(error) => {
                            cleanup_prepared_import(&prepared);
                            failure = Some(error);
                            break;
                        }
                    }
                }
                Some(ImportFile::Linked(prepared)) => Some((
                    prepared.path,
                    String::new(),
                    prepared.content_sha256,
                    prepared.format,
                    prepared.file_size,
                    prepared.modified,
                    AttachmentStorage::Linked,
                    prepared.file_identity,
                )),
                None => None,
            };
            outcomes.push(Ok(adding.len()));
            adding.push((
                file,
                entry.metadata,
                entry.note,
                entry.tags,
                entry.source_tags,
                entry.collections,
                entry.record,
                entry.trashed,
            ));
        }
        let rollback = |installed: &[(PathBuf, PathBuf)]| {
            for (final_directory, staged) in installed {
                let _ = fs::rename(final_directory, staged);
                let _ = fs::remove_dir_all(staged);
            }
        };
        if let Some(error) = failure {
            rollback(&installed);
            return Err(error);
        }

        let records = adding
            .iter()
            .map(|entry| entry.6.clone())
            .collect::<Vec<_>>();
        let result = self.persist_catalog_change(|library| {
            let now = current_timestamp();
            let mut added = Vec::with_capacity(adding.len());
            for (file, mut metadata, note, tags, source_tags, collections, _, trashed) in adding {
                let collection_ids = collections
                    .iter()
                    .filter_map(|path| ensure_collection_path_in(library, path))
                    .fold(Vec::new(), |mut ids, id| {
                        if !ids.contains(&id) {
                            ids.push(id);
                        }
                        ids
                    });
                let parent_id = collection_ids.first().copied();
                let (item_id, attachment_id) = allocate_library_ids(library);
                match file {
                    Some((path, key, sha, format, size, modified, storage, identity)) => {
                        if metadata.title.trim().is_empty() {
                            metadata.title = LibraryItemMetadata::for_path(&path).title;
                        }
                        let key = if storage == AttachmentStorage::Linked {
                            linked_storage_key(attachment_id)
                        } else {
                            key
                        };
                        push_library_item(
                            library,
                            item_id,
                            attachment_id,
                            path,
                            key,
                            sha,
                            format,
                            size,
                            modified,
                            metadata,
                            parent_id,
                            storage,
                            identity,
                        );
                    }
                    None => {
                        push_library_item_without_file(library, item_id, metadata, parent_id);
                    }
                }
                for collection_id in collection_ids.iter().skip(1) {
                    library
                        .library_item_collections
                        .push(LibraryItemCollection {
                            item_id,
                            collection_id: *collection_id,
                        });
                }
                let mut tag_ids = Vec::new();
                let mut source_tag_ids = Vec::new();
                for (name, from_source) in tags
                    .iter()
                    .map(|name| (name, false))
                    .chain(source_tags.iter().map(|name| (name, true)))
                {
                    let Some(tag_id) = ensure_tag_in(library, name) else {
                        continue;
                    };
                    if tag_ids.contains(&tag_id) {
                        continue;
                    }
                    tag_ids.push(tag_id);
                    library
                        .library_item_tags
                        .push(LibraryItemTag { item_id, tag_id });
                    if from_source {
                        source_tag_ids.push((tag_id, clean_text(name)));
                    }
                }
                if let Some(item) = library
                    .library_items
                    .iter_mut()
                    .find(|item| item.id == item_id)
                {
                    item.work_note = note;
                    if trashed {
                        item.trashed_at = Some(now);
                    }
                }
                if trashed {
                    let origin = LibraryTrashLocation {
                        parent_id,
                        path: parent_id
                            .map(|id| collection_path_names_in(library, id))
                            .unwrap_or_default(),
                    };
                    if let Some(location) = library
                        .library_item_locations
                        .iter_mut()
                        .find(|location| location.item_id == item_id)
                    {
                        location.trash_origin = Some(origin);
                    }
                }
                added.push((item_id, source_tag_ids));
            }
            Ok(added)
        });
        let added = match result {
            Ok(added) => added,
            Err(error) => {
                rollback(&installed);
                return Err(error);
            }
        };

        let mut touched = Vec::new();
        for ((id, source_tags), record) in added.iter().zip(records) {
            self.note_imported(*id, record, source_tags, source);
            touched.push(*id);
        }
        let outcomes = outcomes
            .into_iter()
            .map(|outcome| match outcome {
                Ok(index) => ImportEntryOutcome::Added(added[index].0),
                Err((id, external_ids)) => {
                    let mut record = self.metadata_record(id).cloned().unwrap_or_default();
                    let mut changed = false;
                    for external in external_ids {
                        if !record.external_ids.contains(&external) {
                            record.external_ids.push(external);
                            changed = true;
                        }
                    }
                    if changed {
                        self.note_imported(id, record, &[], source);
                        touched.push(id);
                    }
                    ImportEntryOutcome::Duplicate(id)
                }
            })
            .collect();
        self.write_ai_rows(&touched)?;
        Ok(outcomes)
    }
}

/// Moves a staged copy into `files/`; its final path.
fn install_managed(root: Option<&Path>, prepared: &PreparedManagedImport) -> Result<PathBuf> {
    let root = root.ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
    let final_directory = root.join("files").join(&prepared.storage_key);
    let file_name = prepared
        .staged_path
        .file_name()
        .ok_or_else(|| anyhow!("staged import has no filename"))?;
    if final_directory.exists() {
        return Err(anyhow!(
            "managed storage key already exists: {}",
            prepared.storage_key
        ));
    }
    fs::rename(&prepared.staged_directory, &final_directory).with_context(|| {
        format!(
            "failed to install managed attachment at {}",
            final_directory.display()
        )
    })?;
    Ok(final_directory.join(file_name))
}

fn push_library_item_without_file(
    library: &mut Library,
    id: DocumentId,
    metadata: LibraryItemMetadata,
    parent_id: Option<LibraryCollectionId>,
) {
    let now = current_timestamp();
    library.library_items.push(LibraryItem {
        id,
        kind: metadata.kind,
        title: metadata.title,
        creators: metadata.creators,
        year: metadata.year,
        publication: metadata.publication,
        publisher: metadata.publisher,
        abstract_text: metadata.abstract_text,
        subject: metadata.subject,
        language: metadata.language,
        identifiers: metadata.identifiers,
        embedded_keywords: metadata.embedded_keywords,
        favorite: false,
        status: ReadingStatus::ToRead,
        work_note: String::new(),
        created: now,
        updated: now,
        trashed_at: None,
    });
    library.library_item_locations.push(LibraryItemLocation {
        item_id: id,
        parent_id,
        trash_origin: None,
    });
    if let Some(collection_id) = parent_id {
        library
            .library_item_collections
            .push(LibraryItemCollection {
                item_id: id,
                collection_id,
            });
    }
}

fn ensure_tag_in(library: &mut Library, name: &str) -> Option<LibraryTagId> {
    let normalized = normalize_tag_name(name);
    if normalized.is_empty() {
        return None;
    }
    if let Some(tag) = library
        .library_tags
        .iter()
        .find(|tag| tag.normalized_name == normalized)
    {
        return Some(tag.id);
    }
    let id = LibraryTagId(library.next_library_tag_id.max(1));
    library.next_library_tag_id = id.0.saturating_add(1);
    library.library_tags.push(LibraryTag {
        id,
        name: clean_text(name),
        normalized_name: normalized,
    });
    Some(id)
}

fn ensure_collection_path_in(
    library: &mut Library,
    path: &[String],
) -> Option<LibraryCollectionId> {
    let mut cursor = None;
    for component in path {
        let name = clean_text(component);
        let normalized_name = normalize_collection_name(&name);
        if normalized_name.is_empty() {
            continue;
        }
        if let Some(existing) = library.library_collections.iter().find(|collection| {
            collection.parent_id == cursor && collection.normalized_name == normalized_name
        }) {
            cursor = Some(existing.id);
            continue;
        }
        let id = LibraryCollectionId(library.next_library_collection_id.max(1));
        library.next_library_collection_id = id.0.saturating_add(1);
        let now = current_timestamp();
        library.library_collections.push(LibraryCollection {
            id,
            parent_id: cursor,
            name,
            normalized_name,
            created: now,
            updated: now,
        });
        cursor = Some(id);
    }
    cursor
}
