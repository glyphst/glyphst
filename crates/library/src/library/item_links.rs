//! Tag colours, related items, pairs the user kept apart, and merged items.
//! They live on `Library` beside the catalog and are written with it, so a
//! merge commits in one transaction.

use super::storage_helpers::{decode, encode};
use super::*;

/// A tag's colour, one of the highlight colours.
pub(super) const LIBRARY_TAG_COLORS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_tag_colors_v1");
/// Under the lower item id of each pair: the items related to it, and the
/// ones the user said are not the same work.
pub(super) const LIBRARY_ITEM_RELATIONS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_relations_v1");
/// Items merged into another, so their ids are never reused and old links
/// still find the kept item.
pub(super) const LIBRARY_ITEM_MERGES_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_item_merges_v1");
const ITEM_LINKS_STORAGE_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryTagColor {
    pub tag_id: LibraryTagId,
    pub color: HighlightColor,
}

/// Two items, the lower id first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LibraryItemPair(pub DocumentId, pub DocumentId);

impl LibraryItemPair {
    pub fn new(a: DocumentId, b: DocumentId) -> Self {
        if a.0 <= b.0 { Self(a, b) } else { Self(b, a) }
    }

    pub fn other(self, id: DocumentId) -> Option<DocumentId> {
        if self.0 == id {
            Some(self.1)
        } else if self.1 == id {
            Some(self.0)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItemMerge {
    pub absorbed: DocumentId,
    pub into: DocumentId,
    pub at: u64,
}

#[derive(Serialize, Deserialize)]
struct StoredTagColorV1 {
    version: u8,
    color: HighlightColor,
}

#[derive(Serialize, Deserialize)]
struct StoredItemRelationsV1 {
    version: u8,
    related: Vec<DocumentId>,
    distinct: Vec<DocumentId>,
}

#[derive(Serialize, Deserialize)]
struct StoredItemMergeV1 {
    version: u8,
    into: DocumentId,
    at: u64,
}

pub(super) fn ensure_item_link_tables(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(LIBRARY_TAG_COLORS_V1_TABLE)?;
    write_txn.open_table(LIBRARY_ITEM_RELATIONS_V1_TABLE)?;
    write_txn.open_table(LIBRARY_ITEM_MERGES_V1_TABLE)?;
    Ok(())
}

pub(super) fn load_item_links(
    read_txn: &redb::ReadTransaction,
    library: &mut Library,
) -> Result<()> {
    {
        let table = read_txn.open_table(LIBRARY_TAG_COLORS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredTagColorV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, ITEM_LINKS_STORAGE_VERSION)?;
                Ok(stored.color)
            }) {
                Ok(color) => library.library_tag_colors.push(LibraryTagColor {
                    tag_id: LibraryTagId(id.value()),
                    color,
                }),
                Err(error) => eprintln!("failed to decode a tag colour: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ITEM_RELATIONS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            let id = DocumentId(id.value());
            match decode::<StoredItemRelationsV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, ITEM_LINKS_STORAGE_VERSION)?;
                Ok(stored)
            }) {
                Ok(stored) => {
                    library.library_related_items.extend(
                        stored
                            .related
                            .into_iter()
                            .map(|other| LibraryItemPair::new(id, other)),
                    );
                    library.library_distinct_items.extend(
                        stored
                            .distinct
                            .into_iter()
                            .map(|other| LibraryItemPair::new(id, other)),
                    );
                }
                Err(error) => eprintln!("failed to decode related items: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_ITEM_MERGES_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredItemMergeV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, ITEM_LINKS_STORAGE_VERSION)?;
                Ok(stored)
            }) {
                Ok(stored) => library.library_item_merges.push(LibraryItemMerge {
                    absorbed: DocumentId(id.value()),
                    into: stored.into,
                    at: stored.at,
                }),
                Err(error) => eprintln!("failed to decode a merged item: {error:#}"),
            }
        }
    }
    Ok(())
}

pub(super) fn write_item_link_tables(
    write_txn: &redb::WriteTransaction,
    library: &Library,
) -> Result<()> {
    {
        let mut table = write_txn.open_table(LIBRARY_TAG_COLORS_V1_TABLE)?;
        let rows = library
            .library_tag_colors
            .iter()
            .map(|entry| {
                let stored = StoredTagColorV1 {
                    version: ITEM_LINKS_STORAGE_VERSION,
                    color: entry.color,
                };
                Ok((entry.tag_id.0, encode(&stored)?))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        replace_rows(&mut table, rows)?;
    }
    {
        let mut grouped: HashMap<u64, StoredItemRelationsV1> = HashMap::new();
        let empty = || StoredItemRelationsV1 {
            version: ITEM_LINKS_STORAGE_VERSION,
            related: Vec::new(),
            distinct: Vec::new(),
        };
        for pair in &library.library_related_items {
            grouped
                .entry(pair.0.0)
                .or_insert_with(empty)
                .related
                .push(pair.1);
        }
        for pair in &library.library_distinct_items {
            grouped
                .entry(pair.0.0)
                .or_insert_with(empty)
                .distinct
                .push(pair.1);
        }
        let rows = grouped
            .into_iter()
            .map(|(id, stored)| Ok((id, encode(&stored)?)))
            .collect::<Result<HashMap<_, _>>>()?;
        let mut table = write_txn.open_table(LIBRARY_ITEM_RELATIONS_V1_TABLE)?;
        replace_rows(&mut table, rows)?;
    }
    {
        let rows = library
            .library_item_merges
            .iter()
            .map(|merge| {
                let stored = StoredItemMergeV1 {
                    version: ITEM_LINKS_STORAGE_VERSION,
                    into: merge.into,
                    at: merge.at,
                };
                Ok((merge.absorbed.0, encode(&stored)?))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let mut table = write_txn.open_table(LIBRARY_ITEM_MERGES_V1_TABLE)?;
        replace_rows(&mut table, rows)?;
    }
    Ok(())
}

fn replace_rows(
    table: &mut redb::Table<'_, u64, &[u8]>,
    rows: HashMap<u64, Vec<u8>>,
) -> Result<()> {
    let stale = table
        .iter()?
        .filter_map(|entry| entry.ok().map(|(id, _)| id.value()))
        .filter(|id| !rows.contains_key(id))
        .collect::<Vec<_>>();
    for id in stale {
        table.remove(id)?;
    }
    for (id, encoded) in rows {
        table.insert(id, encoded.as_slice())?;
    }
    Ok(())
}

impl Library {
    /// Drops colours of tags that are gone and pairs with an item that is
    /// gone; keeps each pair once.
    pub(super) fn sanitize_item_links(
        &mut self,
        item_ids: &HashSet<DocumentId>,
        tag_ids: &HashSet<LibraryTagId>,
    ) {
        self.library_tag_colors
            .retain(|entry| tag_ids.contains(&entry.tag_id));
        self.library_tag_colors.sort_by_key(|entry| entry.tag_id.0);
        self.library_tag_colors.dedup_by_key(|entry| entry.tag_id.0);
        for pairs in [
            &mut self.library_related_items,
            &mut self.library_distinct_items,
        ] {
            for pair in pairs.iter_mut() {
                *pair = LibraryItemPair::new(pair.0, pair.1);
            }
            pairs.retain(|pair| {
                pair.0 != pair.1 && item_ids.contains(&pair.0) && item_ids.contains(&pair.1)
            });
            pairs.sort();
            pairs.dedup();
        }
        self.library_item_merges
            .sort_by_key(|merge| merge.absorbed.0);
        self.library_item_merges
            .dedup_by_key(|merge| merge.absorbed.0);
    }

    /// The highest item id ever given out, merged-away ones included.
    pub(super) fn highest_merged_item_id(&self) -> u64 {
        self.library_item_merges
            .iter()
            .map(|merge| merge.absorbed.0)
            .max()
            .unwrap_or(0)
    }
}

impl LibraryStore {
    pub fn tag_color(&self, id: LibraryTagId) -> Option<HighlightColor> {
        self.library
            .library_tag_colors
            .iter()
            .find(|entry| entry.tag_id == id)
            .map(|entry| entry.color)
    }

    pub fn tag_colors(&self) -> &[LibraryTagColor] {
        &self.library.library_tag_colors
    }

    pub fn set_tag_color(&mut self, id: LibraryTagId, color: Option<HighlightColor>) -> Result<()> {
        if self.tag_color(id) == color {
            return Ok(());
        }
        if !self.library.library_tags.iter().any(|tag| tag.id == id) {
            return Err(anyhow!("library tag {} does not exist", id.0));
        }
        self.persist_catalog_change(|library| {
            library
                .library_tag_colors
                .retain(|entry| entry.tag_id != id);
            if let Some(color) = color {
                library
                    .library_tag_colors
                    .push(LibraryTagColor { tag_id: id, color });
            }
            Ok(())
        })
    }

    /// The items related to `id`, in id order.
    pub fn related_items(&self, id: DocumentId) -> Vec<DocumentId> {
        self.library
            .library_related_items
            .iter()
            .filter_map(|pair| pair.other(id))
            .collect()
    }

    /// Makes `others` exactly the items related to `id`.
    pub fn set_related_items(&mut self, id: DocumentId, others: &[DocumentId]) -> Result<()> {
        let before = self.related_items(id);
        let mut after = others
            .iter()
            .copied()
            .filter(|other| *other != id)
            .collect::<Vec<_>>();
        after.sort_by_key(|other| other.0);
        after.dedup();
        if before == after {
            return Ok(());
        }
        for item in std::iter::once(&id).chain(&after) {
            if self.library_item(*item).is_none() {
                return Err(anyhow!("library item {} does not exist", item.0));
            }
        }
        let title = self
            .library_item(id)
            .map(|item| item.title.clone())
            .unwrap_or_default();
        self.persist_related_items(id, &after)?;
        let after = self.related_items(id);
        if let Some(recorder) = &mut self.edit_recorder {
            recorder.push(LibraryEdit::related(id, title, before, after));
        }
        Ok(())
    }

    pub(super) fn persist_related_items(
        &mut self,
        id: DocumentId,
        others: &[DocumentId],
    ) -> Result<()> {
        self.persist_catalog_change(|library| {
            library
                .library_related_items
                .retain(|pair| pair.other(id).is_none());
            library
                .library_related_items
                .extend(others.iter().map(|other| LibraryItemPair::new(id, *other)));
            Ok(())
        })
    }

    /// Relates each pair, keeping what the items are already related to;
    /// pairs with an item not in the library are left out. Returns how many
    /// links are new.
    pub fn add_related_pairs(&mut self, pairs: &[(DocumentId, DocumentId)]) -> Result<usize> {
        let mut new = pairs
            .iter()
            .filter(|(a, b)| {
                a != b && self.library_item(*a).is_some() && self.library_item(*b).is_some()
            })
            .map(|(a, b)| LibraryItemPair::new(*a, *b))
            .filter(|pair| !self.library.library_related_items.contains(pair))
            .collect::<Vec<_>>();
        new.sort();
        new.dedup();
        if new.is_empty() {
            return Ok(0);
        }
        let count = new.len();
        self.persist_catalog_change(|library| {
            library.library_related_items.extend(new);
            Ok(count)
        })
    }

    /// Gives each tag its colour unless it has one. Returns how many it
    /// coloured.
    pub fn fill_tag_colors(&mut self, colors: &[(LibraryTagId, HighlightColor)]) -> Result<usize> {
        let mut new = Vec::<LibraryTagColor>::new();
        for (tag_id, color) in colors {
            if self.tag_color(*tag_id).is_none()
                && self
                    .library
                    .library_tags
                    .iter()
                    .any(|tag| tag.id == *tag_id)
                && !new.iter().any(|entry| entry.tag_id == *tag_id)
            {
                new.push(LibraryTagColor {
                    tag_id: *tag_id,
                    color: *color,
                });
            }
        }
        if new.is_empty() {
            return Ok(0);
        }
        let count = new.len();
        self.persist_catalog_change(|library| {
            library.library_tag_colors.extend(new);
            Ok(count)
        })
    }

    /// The items the user said are not the same work as `id`.
    pub fn distinct_items(&self, id: DocumentId) -> Vec<DocumentId> {
        self.library
            .library_distinct_items
            .iter()
            .filter_map(|pair| pair.other(id))
            .collect()
    }

    /// "Keep both": `a` and `b` are different works, so neither is flagged
    /// as the other's duplicate again.
    pub fn mark_distinct(&mut self, a: DocumentId, b: DocumentId) -> Result<()> {
        if a == b {
            return Err(anyhow!("an item cannot be kept apart from itself"));
        }
        if self.library_item(a).is_none() || self.library_item(b).is_none() {
            return Err(anyhow!("both items must be in the library"));
        }
        let pair = LibraryItemPair::new(a, b);
        if !self.library.library_distinct_items.contains(&pair) {
            self.persist_catalog_change(|library| {
                library.library_distinct_items.push(pair);
                Ok(())
            })?;
        }
        self.clear_possible_duplicates(|id, other| {
            (id == a && other == b) || (id == b && other == a)
        })
    }

    /// The item `id` now lives in: itself, or the item it was merged into.
    pub fn resolve_item_id(&self, id: DocumentId) -> DocumentId {
        let mut current = id;
        for _ in 0..self.library.library_item_merges.len() {
            match self
                .library
                .library_item_merges
                .iter()
                .find(|merge| merge.absorbed == current)
            {
                Some(merge) => current = merge.into,
                None => break,
            }
        }
        current
    }

    pub fn library_item_merges(&self) -> &[LibraryItemMerge] {
        &self.library.library_item_merges
    }
}
