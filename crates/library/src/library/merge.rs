//! Merging an item into the one it duplicates, and undoing it. The kept
//! item takes the other's files, tags, collections and links, fills its
//! empty fields from it, and the other's id is remembered so it is never
//! given out again. Everything commits in one transaction.

use super::ai_catalog::{AiRows, field_is_empty};
use super::*;

/// What an item had, to put it back after a merge.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct ItemState {
    item: LibraryItem,
    attachments: Vec<LibraryAttachment>,
    trash_origin: Option<LibraryTrashLocation>,
    collections: Vec<LibraryCollectionId>,
    tags: Vec<LibraryTagId>,
    related: Vec<DocumentId>,
    distinct: Vec<DocumentId>,
    ai: AiRows,
    vault_note: Option<VaultNoteRecord>,
}

impl ItemState {
    pub(super) fn id(&self) -> DocumentId {
        self.item.id
    }

    pub(super) fn title(&self) -> &str {
        &self.item.title
    }

    pub(super) fn item_fields(&self) -> LibraryItem {
        self.item.clone()
    }
}

/// The ai rows of `keep` once `absorbed` is merged into it.
fn merged_ai_rows(keep: &AiRows, absorbed: &AiRows) -> AiRows {
    let metadata = match (&keep.metadata, &absorbed.metadata) {
        (Some(keep), Some(absorbed)) => {
            let mut record = keep.clone();
            for id in &absorbed.external_ids {
                if !record.external_ids.contains(id) {
                    record.external_ids.push(id.clone());
                }
            }
            for field in &absorbed.locked_fields {
                if !record.locked_fields.contains(field) {
                    record.locked_fields.push(*field);
                }
            }
            Some(record)
        }
        (keep, absorbed) => keep.clone().or_else(|| absorbed.clone()),
    }
    .map(|mut record| {
        record.possible_duplicate_of = None;
        record
    });
    let mut assignments = keep.assignments.clone();
    for assignment in &absorbed.assignments {
        if !assignments
            .iter()
            .any(|existing| existing.target == assignment.target)
        {
            assignments.push(assignment.clone());
        }
    }
    AiRows {
        metadata,
        assignments,
        enrichment: keep
            .enrichment
            .clone()
            .or_else(|| absorbed.enrichment.clone()),
    }
}

fn without(ids: &[DocumentId], gone: &[DocumentId]) -> Vec<DocumentId> {
    ids.iter()
        .copied()
        .filter(|id| !gone.contains(id))
        .collect()
}

impl LibraryStore {
    fn item_state(&self, id: DocumentId) -> Result<ItemState> {
        let item = self
            .library_item(id)
            .cloned()
            .ok_or_else(|| anyhow!("library item {} does not exist", id.0))?;
        Ok(ItemState {
            item,
            attachments: self
                .library
                .library_attachments
                .iter()
                .filter(|attachment| attachment.item_id == id)
                .cloned()
                .collect(),
            trash_origin: self
                .library
                .library_item_locations
                .iter()
                .find(|location| location.item_id == id)
                .and_then(|location| location.trash_origin.clone()),
            collections: self
                .library
                .library_item_collections
                .iter()
                .filter(|membership| membership.item_id == id)
                .map(|membership| membership.collection_id)
                .collect(),
            tags: self.item_tag_ids(id),
            related: self.related_items(id),
            distinct: self.distinct_items(id),
            ai: self.ai_rows(id),
            vault_note: self.vault_note(id).cloned(),
        })
    }

    /// Merges `absorb` into `keep` and returns the edit that undoes it.
    pub fn merge_library_items(
        &mut self,
        keep: DocumentId,
        absorb: DocumentId,
    ) -> Result<LibraryEdit> {
        let edit = self.merge_items(keep, absorb)?;
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(edit.clone());
        }
        Ok(edit)
    }

    pub(super) fn merge_items(
        &mut self,
        keep: DocumentId,
        absorb: DocumentId,
    ) -> Result<LibraryEdit> {
        if keep == absorb {
            return Err(anyhow!("an item cannot be merged into itself"));
        }
        let keep_before = self.item_state(keep)?;
        let absorbed = self.item_state(absorb)?;
        if keep_before.item.trashed_at.is_some() {
            return Err(anyhow!("“{}” is in the trash", keep_before.item.title));
        }

        let locked = keep_before
            .ai
            .metadata
            .as_ref()
            .map(|record| record.locked_fields.clone())
            .unwrap_or_default();
        let mut item = keep_before.item.clone();
        for field in MetadataField::ALL {
            if field_is_empty(field, &item)
                && !locked.contains(&field)
                && !field_is_empty(field, &absorbed.item)
            {
                field.copy(&absorbed.item, &mut item);
            }
        }
        let note = absorbed.item.work_note.trim();
        if !note.is_empty() && !item.work_note.contains(note) {
            item.work_note = if item.work_note.trim().is_empty() {
                note.to_string()
            } else {
                format!("{}\n\n{note}", item.work_note.trim_end())
            };
        }
        item.favorite |= absorbed.item.favorite;
        let now = current_timestamp();
        item.updated = now;

        let ai = merged_ai_rows(&keep_before.ai, &absorbed.ai);
        self.commit_with_side_rows(
            |library| {
                if let Some(existing) = library.library_items.iter_mut().find(|i| i.id == keep) {
                    *existing = item;
                }
                for attachment in &mut library.library_attachments {
                    if attachment.item_id == absorb {
                        attachment.item_id = keep;
                        attachment.primary = false;
                    }
                }
                for relation in &mut library.library_item_tags {
                    if relation.item_id == absorb {
                        relation.item_id = keep;
                    }
                }
                for membership in &mut library.library_item_collections {
                    if membership.item_id == absorb {
                        membership.item_id = keep;
                    }
                }
                for pairs in [
                    &mut library.library_related_items,
                    &mut library.library_distinct_items,
                ] {
                    for pair in pairs.iter_mut() {
                        if let Some(other) = pair.other(absorb) {
                            *pair = LibraryItemPair::new(keep, other);
                        }
                    }
                }
                for merge in &mut library.library_item_merges {
                    if merge.into == absorb {
                        merge.into = keep;
                    }
                }
                library.library_item_merges.push(LibraryItemMerge {
                    absorbed: absorb,
                    into: keep,
                    at: now,
                });
                library.library_items.retain(|item| item.id != absorb);
                library
                    .library_item_locations
                    .retain(|location| location.item_id != absorb);
                Ok(())
            },
            vec![(keep, ai), (absorb, AiRows::default())],
            vec![(absorb, None)],
            |id, other| [keep, absorb].contains(&id) || [keep, absorb].contains(&other),
        )?;
        let keep_after = self.item_state(keep)?;
        Ok(LibraryEdit::merge(keep_before, keep_after, absorbed))
    }

    /// Puts `absorbed` back as it was and takes from `keep` what the merge
    /// gave it. Refuses when `keep`'s merged fields or files changed since.
    pub(super) fn unmerge_items(
        &mut self,
        keep_before: &ItemState,
        keep_after: &ItemState,
        absorbed: &ItemState,
    ) -> Result<()> {
        let keep = keep_after.id();
        let absorb = absorbed.id();
        let current = self.item_state(keep)?;
        let moved = absorbed
            .attachments
            .iter()
            .map(|attachment| attachment.id)
            .collect::<Vec<_>>();
        if MetadataField::ALL
            .into_iter()
            .any(|field| field.differs(&current.item, &keep_after.item))
            || current.item.work_note != keep_after.item.work_note
            || !moved.iter().all(|id| {
                current
                    .attachments
                    .iter()
                    .any(|attachment| attachment.id == *id)
            })
        {
            return Err(DocumentEditConflict("merged library item").into());
        }
        if self.library_item(absorb).is_some() {
            return Err(DocumentEditConflict("merged library item").into());
        }

        // What only the absorbed item brought comes off `keep`.
        let added_tags = absorbed
            .tags
            .iter()
            .filter(|tag| !keep_before.tags.contains(tag))
            .copied()
            .collect::<Vec<_>>();
        let added_collections = absorbed
            .collections
            .iter()
            .filter(|collection| !keep_before.collections.contains(collection))
            .copied()
            .collect::<Vec<_>>();
        let added_related = without(&absorbed.related, &keep_before.related);
        let added_distinct = without(&absorbed.distinct, &keep_before.distinct);
        let mut item = current.item.clone();
        for field in MetadataField::ALL {
            field.copy(&keep_before.item, &mut item);
        }
        item.work_note = keep_before.item.work_note.clone();
        item.favorite = keep_before.item.favorite;

        self.commit_with_side_rows(
            |library| {
                if let Some(existing) = library.library_items.iter_mut().find(|i| i.id == keep) {
                    *existing = item;
                }
                library.library_items.push(absorbed.item.clone());
                library.library_item_locations.push(LibraryItemLocation {
                    item_id: absorb,
                    parent_id: None,
                    trash_origin: absorbed.trash_origin.clone(),
                });
                for attachment in &mut library.library_attachments {
                    if let Some(before) = absorbed
                        .attachments
                        .iter()
                        .find(|before| before.id == attachment.id)
                    {
                        attachment.item_id = absorb;
                        attachment.primary = before.primary;
                    }
                }
                library.library_item_tags.retain(|relation| {
                    relation.item_id != keep || !added_tags.contains(&relation.tag_id)
                });
                library
                    .library_item_tags
                    .extend(absorbed.tags.iter().map(|tag_id| LibraryItemTag {
                        item_id: absorb,
                        tag_id: *tag_id,
                    }));
                library.library_item_collections.retain(|membership| {
                    membership.item_id != keep
                        || !added_collections.contains(&membership.collection_id)
                });
                library
                    .library_item_collections
                    .extend(absorbed.collections.iter().map(|collection_id| {
                        LibraryItemCollection {
                            item_id: absorb,
                            collection_id: *collection_id,
                        }
                    }));
                for (pairs, added, own) in [
                    (
                        &mut library.library_related_items,
                        &added_related,
                        &absorbed.related,
                    ),
                    (
                        &mut library.library_distinct_items,
                        &added_distinct,
                        &absorbed.distinct,
                    ),
                ] {
                    pairs.retain(|pair| {
                        pair.other(keep).is_none_or(|other| !added.contains(&other))
                    });
                    pairs.extend(own.iter().map(|other| LibraryItemPair::new(absorb, *other)));
                }
                library
                    .library_item_merges
                    .retain(|merge| merge.absorbed != absorb);
                Ok(())
            },
            vec![
                (keep, keep_before.ai.clone()),
                (absorb, absorbed.ai.clone()),
            ],
            vec![(absorb, absorbed.vault_note.clone())],
            |_, _| false,
        )
    }

    /// Changes the catalog and the given items' AI and vault rows in one
    /// transaction, putting everything back if it fails.
    fn commit_with_side_rows(
        &mut self,
        change: impl FnOnce(&mut Library) -> Result<()>,
        ai: Vec<(DocumentId, AiRows)>,
        vault: Vec<(DocumentId, Option<VaultNoteRecord>)>,
        clear_duplicates: impl Fn(DocumentId, DocumentId) -> bool,
    ) -> Result<()> {
        let previous = (
            self.library.clone(),
            self.ai_catalog.clone(),
            self.vault_notes.clone(),
        );
        let result = (|| -> Result<()> {
            change(&mut self.library)?;
            self.library.sanitize_catalog();
            let mut ai_ids = ai.iter().map(|(id, _)| *id).collect::<Vec<_>>();
            for (id, rows) in ai {
                self.put_ai_rows(id, rows);
            }
            ai_ids.extend(self.clear_possible_duplicates_in_memory(clear_duplicates));
            for (id, note) in &vault {
                match note {
                    Some(note) => self.vault_notes.insert(*id, note.clone()),
                    None => self.vault_notes.remove(id),
                };
            }
            let Some(database) = &self.database else {
                return Ok(());
            };
            let root = self
                .managed_library_root
                .as_deref()
                .ok_or_else(|| anyhow!("managed library storage is unavailable"))?;
            self.synced_write();
            let write_txn = database.begin_write()?;
            storage::write_catalog_tables(&write_txn, &self.library, root)?;
            self.write_ai_rows_in(&write_txn, &ai_ids)?;
            for (id, note) in &vault {
                vault_notes::write_vault_note_in(&write_txn, *id, note.as_ref())?;
            }
            write_txn.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            (self.library, self.ai_catalog, self.vault_notes) = previous;
            return Err(error);
        }
        self.catalog_revision = self.catalog_revision.wrapping_add(1);
        self.sanitize_ai_catalog();
        self.prune_library_jobs();
        Ok(())
    }
}
