//! Reading a Zotero library for import: its items with their fields,
//! creators, tags, collections, attachments, notes and PDF annotations,
//! from a copy of `zotero.sqlite` (Zotero keeps the live file locked).
//! Read only; mapping into Glyphst's library is the caller's.

mod notes;
pub mod sample;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};
use rusqlite::{Connection, OpenFlags, OptionalExtension as _};

pub use notes::html_to_markdown;

/// A Zotero item (not a note, attachment or annotation).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: i64,
    pub key: String,
    pub library_id: i64,
    /// `journalArticle`, `book`, `preprint`…
    pub item_type: String,
    pub fields: HashMap<String, String>,
    pub creators: Vec<Creator>,
    /// The name, and whether Zotero added it itself (an automatic tag).
    pub tags: Vec<(String, bool)>,
    pub collections: Vec<i64>,
    pub attachments: Vec<Attachment>,
    /// Child notes, as HTML.
    pub notes: Vec<String>,
    pub trashed: bool,
    /// Keys of related items.
    pub related: Vec<String>,
}

impl Item {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
    }

    /// The year of the item's date (`2019-03-00 March 2019`).
    pub fn year(&self) -> Option<i32> {
        self.field("date")
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok())
            .filter(|year| *year > 0)
    }

    /// The arXiv id: from the archive id, the extra field or the URL.
    pub fn arxiv(&self) -> Option<String> {
        let candidates = [
            self.field("archiveID").map(str::to_string),
            self.field("extra").map(str::to_string),
            self.field("url").map(str::to_string),
            self.field("DOI")
                .map(|doi| doi.replace("10.48550/arXiv.", "arXiv:")),
        ];
        candidates.into_iter().flatten().find_map(|text| {
            let lower = text.to_lowercase();
            let at = lower
                .find("arxiv:")
                .map(|at| at + 6)
                .or_else(|| lower.find("arxiv.org/abs/").map(|at| at + 14))?;
            let id = text[at..]
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',')
                .next()?;
            let id = match id.rfind('v') {
                Some(version)
                    if version > 0 && id[version + 1..].chars().all(|c| c.is_ascii_digit()) =>
                {
                    &id[..version]
                }
                _ => id,
            };
            (!id.is_empty()).then(|| id.to_lowercase())
        })
    }

    /// The citation key: the field, else Better BibTeX's line in Extra.
    pub fn citation_key(&self) -> Option<String> {
        self.field("citationKey").map(str::to_string).or_else(|| {
            self.field("extra")?.lines().find_map(|line| {
                line.strip_prefix("Citation Key:")
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty())
            })
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Creator {
    pub first: String,
    pub last: String,
    /// `author`, `editor`…
    pub role: String,
}

impl Creator {
    pub fn name(&self) -> String {
        format!("{} {}", self.first, self.last).trim().to_string()
    }
}

/// How an attachment's file is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMode {
    /// In Zotero's `storage/` (a file, or a web snapshot).
    Stored,
    /// Linked where the user keeps it.
    Linked,
    /// A web link, no file.
    Url,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub key: String,
    pub link_mode: LinkMode,
    pub content_type: String,
    /// The file, when it could be found.
    pub file: Option<PathBuf>,
    /// Where Zotero says it is, for a report: `storage/KEY/name.pdf`,
    /// `attachments:papers/name.pdf` or an absolute path.
    pub zotero_path: String,
    pub annotations: Vec<Annotation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationKind {
    Highlight,
    Note,
    Image,
    Ink,
    Underline,
    Text,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub kind: AnnotationKind,
    /// `#ffd400`.
    pub color: String,
    pub text: String,
    pub comment: String,
    pub page_label: String,
    /// The page (from 0) and the rects in PDF user space, `[x1, y1, x2, y2]`
    /// with the origin at the bottom left; `None` for EPUB positions.
    pub position: Option<(usize, Vec<[f32; 4]>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
}

/// A Zotero library as read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub items: Vec<Item>,
    pub collections: Vec<Collection>,
    /// Coloured tags: name and colour.
    pub tag_colors: Vec<(String, String)>,
    /// Notes that belong to no item: title and HTML.
    pub standalone_notes: Vec<(String, String)>,
}

impl Library {
    /// A collection's path from the top, by name.
    pub fn collection_path(&self, id: i64) -> Vec<String> {
        let mut path = Vec::new();
        let mut cursor = Some(id);
        while let Some(id) = cursor {
            let Some(collection) = self
                .collections
                .iter()
                .find(|collection| collection.id == id)
            else {
                break;
            };
            if path.len() > 32 {
                break;
            }
            path.push(collection.name.clone());
            cursor = collection.parent;
        }
        path.reverse();
        path
    }

    pub fn annotation_count(&self) -> usize {
        self.items
            .iter()
            .flat_map(|item| &item.attachments)
            .map(|attachment| attachment.annotations.len())
            .sum()
    }
}

/// Zotero's data folder: `~/Zotero`, or the one its profile names.
pub fn find_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let default = home.join("Zotero");
    if default.join("zotero.sqlite").is_file() {
        return Some(default);
    }
    // A data folder moved elsewhere is in the profile's prefs.js.
    for profiles in [
        home.join(".zotero/zotero"),
        home.join("Library/Application Support/Zotero/Profiles"),
    ] {
        let Ok(entries) = fs::read_dir(&profiles) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(prefs) = fs::read_to_string(entry.path().join("prefs.js")) else {
                continue;
            };
            if let Some(dir) = pref_value(&prefs, "extensions.zotero.dataDir")
                && Path::new(&dir).join("zotero.sqlite").is_file()
            {
                return Some(PathBuf::from(dir));
            }
        }
    }
    None
}

/// A string pref's value in a `prefs.js`.
fn pref_value(prefs: &str, name: &str) -> Option<String> {
    prefs.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("user_pref(\"")?;
        let rest = rest.strip_prefix(name)?.strip_prefix("\",")?.trim();
        let value = rest
            .strip_suffix(");")?
            .trim()
            .strip_prefix('"')?
            .strip_suffix('"')?;
        Some(value.replace("\\\\", "\\"))
    })
}

/// Whether Zotero has the database open (its write-ahead log is not empty).
pub fn is_running(data_dir: &Path) -> bool {
    fs::metadata(data_dir.join("zotero.sqlite-wal")).is_ok_and(|metadata| metadata.len() > 0)
}

/// Copies the database (with its write-ahead log, so recent changes come
/// too) into `scratch` and opens the copy.
pub fn open_copy(data_dir: &Path, scratch: &Path) -> Result<Connection> {
    fs::create_dir_all(scratch)?;
    let source = data_dir.join("zotero.sqlite");
    let copy = scratch.join("zotero.sqlite");
    fs::copy(&source, &copy).with_context(|| format!("failed to copy {}", source.display()))?;
    let wal = data_dir.join("zotero.sqlite-wal");
    let wal_copy = scratch.join("zotero.sqlite-wal");
    let _ = fs::remove_file(&wal_copy);
    if wal.is_file() {
        fs::copy(&wal, &wal_copy).with_context(|| format!("failed to copy {}", wal.display()))?;
    }
    // Read-write, so SQLite can replay the log into the copy.
    let connection = Connection::open_with_flags(
        &copy,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {}", copy.display()))?;
    connection.pragma_update(None, "locking_mode", "NORMAL")?;
    Ok(connection)
}

/// Reads the library from `connection`. Files are looked for under
/// `data_dir/storage`, and linked files relative to `base` (Zotero's
/// "linked attachment base directory") when they start `attachments:`.
pub fn read(connection: &Connection, data_dir: &Path, base: Option<&Path>) -> Result<Library> {
    let combined = |name: &str| {
        connection
            .query_row(
                "SELECT name FROM sqlite_master WHERE name = ?1",
                [format!("{name}Combined")],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or_else(|| name.to_string())
    };
    let item_types = combined("itemTypes");
    let fields = combined("fields");

    // Libraries that are not feeds.
    let feeds = connection
        .prepare("SELECT libraryID FROM libraries WHERE type = 'feed'")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();

    let mut statement = connection.prepare(&format!(
        "SELECT i.itemID, i.key, i.libraryID, t.typeName FROM items i JOIN {item_types} t USING (itemTypeID)"
    ))?;
    let mut items = HashMap::<i64, Item>::new();
    let mut kinds = HashMap::<i64, (String, String, i64)>::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (id, key, library_id, kind) = row?;
        if feeds.contains(&library_id) {
            continue;
        }
        if matches!(kind.as_str(), "note" | "attachment" | "annotation") {
            kinds.insert(id, (kind, key, library_id));
            continue;
        }
        items.insert(
            id,
            Item {
                id,
                key,
                library_id,
                item_type: kind,
                ..Item::default()
            },
        );
    }

    let mut statement = connection.prepare(&format!(
        "SELECT d.itemID, f.fieldName, v.value FROM itemData d JOIN {fields} f USING (fieldID) JOIN itemDataValues v USING (valueID)"
    ))?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, rusqlite::types::Value>(2)?,
        ))
    })? {
        let (id, field, value) = row?;
        let value = match value {
            rusqlite::types::Value::Text(text) => text,
            rusqlite::types::Value::Integer(number) => number.to_string(),
            rusqlite::types::Value::Real(number) => number.to_string(),
            _ => continue,
        };
        if let Some(item) = items.get_mut(&id) {
            item.fields.insert(field, value);
        }
    }

    let mut statement = connection.prepare(
        "SELECT ic.itemID, c.firstName, c.lastName, ct.creatorType FROM itemCreators ic JOIN creators c USING (creatorID) JOIN creatorTypes ct USING (creatorTypeID) ORDER BY ic.itemID, ic.orderIndex",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        let (id, first, last, role) = row?;
        if let Some(item) = items.get_mut(&id) {
            item.creators.push(Creator {
                first: first.unwrap_or_default(),
                last: last.unwrap_or_default(),
                role,
            });
        }
    }

    let mut statement = connection
        .prepare("SELECT it.itemID, t.name, it.type FROM itemTags it JOIN tags t USING (tagID)")?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })? {
        let (id, name, kind) = row?;
        if let Some(item) = items.get_mut(&id) {
            item.tags.push((name, kind == 1));
        }
    }

    let mut collections = Vec::new();
    let mut statement = connection.prepare(
        "SELECT collectionID, collectionName, parentCollectionID, libraryID FROM collections",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })? {
        let (id, name, parent, library_id) = row?;
        if !feeds.contains(&library_id) {
            collections.push(Collection { id, name, parent });
        }
    }
    let trashed_collections = query_ids(connection, "SELECT collectionID FROM deletedCollections");
    collections.retain(|collection| !trashed_collections.contains(&collection.id));
    let mut statement = connection.prepare("SELECT collectionID, itemID FROM collectionItems")?;
    for row in statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))? {
        let (collection, id) = row?;
        if let Some(item) = items.get_mut(&id)
            && collections.iter().any(|known| known.id == collection)
        {
            item.collections.push(collection);
        }
    }

    // Attachments, and which item each belongs to.
    let mut attachments = HashMap::<i64, (Option<i64>, Attachment)>::new();
    let mut statement = connection
        .prepare("SELECT itemID, parentItemID, linkMode, contentType, path FROM itemAttachments")?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })? {
        let (id, parent, link_mode, content_type, path) = row?;
        let Some((_, key, _)) = kinds.get(&id) else {
            continue;
        };
        let link_mode = match link_mode {
            Some(0 | 1) => LinkMode::Stored,
            Some(2) => LinkMode::Linked,
            Some(3) => LinkMode::Url,
            _ => LinkMode::Other,
        };
        let file = path.as_deref().and_then(|path| match link_mode {
            LinkMode::Stored => path
                .strip_prefix("storage:")
                .map(|name| data_dir.join("storage").join(key).join(name)),
            LinkMode::Linked => match path.strip_prefix("attachments:") {
                Some(relative) => base.map(|base| base.join(relative)),
                None => Some(PathBuf::from(path)),
            },
            _ => None,
        });
        attachments.insert(
            id,
            (
                parent,
                Attachment {
                    key: key.clone(),
                    link_mode,
                    content_type: content_type.unwrap_or_default(),
                    file: file.filter(|file| file.is_file()),
                    zotero_path: match (&path, link_mode) {
                        (Some(path), LinkMode::Stored) => match path.strip_prefix("storage:") {
                            Some(name) => format!("storage/{key}/{name}"),
                            None => path.clone(),
                        },
                        (Some(path), _) => path.clone(),
                        (None, _) => String::new(),
                    },
                    annotations: Vec::new(),
                },
            ),
        );
    }

    let mut statement = connection.prepare(
        "SELECT parentItemID, type, color, text, comment, pageLabel, position FROM itemAnnotations ORDER BY sortIndex",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
        ))
    })? {
        let (parent, kind, color, text, comment, page_label, position) = row?;
        let kind = match kind {
            1 => AnnotationKind::Highlight,
            2 => AnnotationKind::Note,
            3 => AnnotationKind::Image,
            4 => AnnotationKind::Ink,
            5 => AnnotationKind::Underline,
            _ => AnnotationKind::Text,
        };
        if let Some((_, attachment)) = attachments.get_mut(&parent) {
            attachment.annotations.push(Annotation {
                kind,
                color: color.unwrap_or_default(),
                text: text.unwrap_or_default(),
                comment: comment.unwrap_or_default(),
                page_label: page_label.unwrap_or_default(),
                position: parse_position(&position),
            });
        }
    }
    for (_, (parent, attachment)) in attachments {
        if let Some(item) = parent.and_then(|parent| items.get_mut(&parent)) {
            item.attachments.push(attachment);
        }
    }

    let mut standalone_notes = Vec::new();
    let mut statement =
        connection.prepare("SELECT itemID, parentItemID, note, title FROM itemNotes")?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (id, parent, note, title) = row?;
        let note = note.unwrap_or_default();
        match parent.and_then(|parent| items.get_mut(&parent)) {
            Some(item) => item.notes.push(note),
            None if kinds.contains_key(&id) => {
                standalone_notes.push((title.unwrap_or_default(), note));
            }
            None => {}
        }
    }

    for id in query_ids(connection, "SELECT itemID FROM deletedItems") {
        if let Some(item) = items.get_mut(&id) {
            item.trashed = true;
        }
    }

    if let Ok(mut statement) = connection.prepare(
        "SELECT r.itemID, r.object FROM itemRelations r JOIN relationPredicates p USING (predicateID) WHERE p.predicate = 'dc:relation'",
    ) && let Ok(rows) = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))
    {
        for (id, object) in rows.flatten() {
            if let (Some(item), Some(key)) = (items.get_mut(&id), object.rsplit('/').next()) {
                item.related.push(key.to_string());
            }
        }
    }

    let tag_colors = connection
        .query_row(
            "SELECT value FROM syncedSettings WHERE setting = 'tagColors' LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|value| value.as_array().cloned())
        .map(|colors| {
            colors
                .iter()
                .filter_map(|entry| {
                    Some((
                        entry["name"].as_str()?.to_string(),
                        entry["color"].as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut items = items.into_values().collect::<Vec<_>>();
    items.sort_by_key(|item| item.id);
    for item in &mut items {
        item.collections.sort();
        item.collections.dedup();
    }
    Ok(Library {
        items,
        collections,
        tag_colors,
        standalone_notes,
    })
}

fn query_ids(connection: &Connection, sql: &str) -> Vec<i64> {
    connection
        .prepare(sql)
        .and_then(|mut statement| {
            statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// A PDF annotation's page and rects from its position JSON.
fn parse_position(json: &str) -> Option<(usize, Vec<[f32; 4]>)> {
    let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
    let page = usize::try_from(value["pageIndex"].as_u64()?).ok()?;
    let rects = value["rects"]
        .as_array()?
        .iter()
        .filter_map(|rect| {
            let numbers = rect.as_array()?;
            let at = |index: usize| numbers.get(index)?.as_f64().map(|value| value as f32);
            Some([at(0)?, at(1)?, at(2)?, at(3)?])
        })
        .collect::<Vec<_>>();
    (!rects.is_empty()).then_some((page, rects))
}

/// Zotero's rect (PDF user space, origin at the bottom left) in MuPDF's
/// page space (origin at the top left) for a page `height` points high
/// whose crop box starts at `origin`.
pub fn to_top_left(rect: [f32; 4], height: f32, origin: (f32, f32)) -> [f32; 4] {
    let [x1, y1, x2, y2] = rect;
    let (x0, y0) = origin;
    [
        x1.min(x2) - x0,
        height - (y1.max(y2) - y0),
        x1.max(x2) - x0,
        height - (y1.min(y2) - y0),
    ]
}

/// Opens Zotero's data folder and reads it, through a copy in `scratch`.
pub fn read_data_dir(data_dir: &Path, scratch: &Path, base: Option<&Path>) -> Result<Library> {
    if !data_dir.join("zotero.sqlite").is_file() {
        return Err(anyhow!("no zotero.sqlite in {}", data_dir.display()));
    }
    let connection = open_copy(data_dir, scratch)?;
    read(&connection, data_dir, base)
}

#[cfg(test)]
mod tests;
