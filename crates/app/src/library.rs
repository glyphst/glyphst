use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use gpui::Global;
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const MAX_HISTORY_ENTRIES: usize = 100;
const HISTORY_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("history");
const BOOKMARKS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("bookmarks");
const HIGHLIGHTS_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("highlights");
const NOTES_TABLE: TableDefinition<u64, &[u8]> = TableDefinition::new("notes");

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentPosition {
    pub scroll_x_doc: f32,
    pub scroll_y_doc: f32,
    pub zoom: f32,
    pub page: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub opened: u64,
    pub position: DocumentPosition,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: u64,
    pub path: PathBuf,
    pub label: String,
    pub position: DocumentPosition,
    pub created: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HighlightColor {
    Yellow,
    Green,
    Blue,
    Pink,
    Orange,
}

impl HighlightColor {
    pub const ALL: [Self; 5] = [
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Pink,
        Self::Orange,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HighlightRect {
    pub page_idx: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl HighlightRect {
    pub fn contains(&self, page_idx: usize, x: f32, y: f32) -> bool {
        self.page_idx == page_idx && x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Highlight {
    pub id: u64,
    pub path: PathBuf,
    pub color: HighlightColor,
    pub rects: Vec<HighlightRect>,
    pub text: String,
    pub note: String,
    pub created: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: u64,
    pub path: PathBuf,
    pub color: HighlightColor,
    pub rects: Vec<HighlightRect>,
    pub text: String,
    pub body: String,
    pub created: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
    pub highlights: Vec<Highlight>,
    pub notes: Vec<Note>,
    next_bookmark_id: u64,
    next_highlight_id: u64,
    next_note_id: u64,
}

pub struct LibraryStore {
    pub library: Library,
    database: Database,
}

impl Global for LibraryStore {}

impl LibraryStore {
    pub fn load() -> Self {
        let path = library_path();
        Self::open(path).expect("failed to open PDF library database")
    }

    fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let database = if path.exists() {
            Database::open(&path).with_context(|| format!("failed to open {}", path.display()))?
        } else {
            Database::create(&path)
                .with_context(|| format!("failed to create {}", path.display()))?
        };
        ensure_tables(&database)?;
        let library = load_library(&database)?;

        Ok(Self { library, database })
    }

    pub fn history_entries(&self) -> &[HistoryEntry] {
        &self.library.history
    }

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.library.bookmarks
    }

    pub fn highlights_for(&self, path: &Path) -> Vec<Highlight> {
        let path = normalize_path(path);
        self.library
            .highlights
            .iter()
            .filter(|highlight| highlight.path == path)
            .cloned()
            .collect()
    }

    pub fn notes_for(&self, path: &Path) -> Vec<Note> {
        let path = normalize_path(path);
        self.library
            .notes
            .iter()
            .filter(|note| note.path == path)
            .cloned()
            .collect()
    }

    pub fn history_position(&self, path: &Path) -> Option<DocumentPosition> {
        let path = normalize_path(path);
        self.library
            .history
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.position)
    }

    pub fn record_history(&mut self, path: &Path, position: DocumentPosition) {
        self.library
            .record_history_at(path, position, current_timestamp());
    }

    pub fn add_bookmark(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
    ) -> Result<u64> {
        let bookmark = self
            .library
            .build_bookmark(path, label, position, current_timestamp());
        self.write_bookmark(&bookmark)?;
        self.library.bookmarks.push(bookmark.clone());
        self.library.sanitize();
        Ok(bookmark.id)
    }

    pub fn remove_bookmark(&mut self, id: u64) -> Result<bool> {
        let removed = self.library.remove_bookmark(id);
        if removed {
            self.remove_bookmark_from_database(id)?;
        }
        Ok(removed)
    }

    pub fn add_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<Highlight> {
        let highlight =
            self.library
                .build_highlight(path, color, rects, text, note, current_timestamp());
        self.write_highlight(&highlight)?;
        self.library.highlights.push(highlight.clone());
        self.library.sanitize();
        Ok(highlight)
    }

    pub fn add_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<Note> {
        let note = self
            .library
            .build_note(path, color, rects, text, body, current_timestamp());
        self.write_note(&note)?;
        self.library.notes.push(note.clone());
        self.library.sanitize();
        Ok(note)
    }

    pub fn remove_highlight(&mut self, id: u64) -> Result<bool> {
        let removed = self.library.remove_highlight(id);
        if removed {
            self.remove_highlight_from_database(id)?;
        }
        Ok(removed)
    }

    pub fn remove_note(&mut self, id: u64) -> Result<bool> {
        let removed = self.library.remove_note(id);
        if removed {
            self.remove_note_from_database(id)?;
        }
        Ok(removed)
    }

    pub fn set_highlight_color(&mut self, id: u64, color: HighlightColor) -> Result<bool> {
        let Some(highlight_ix) = self
            .library
            .highlights
            .iter()
            .position(|highlight| highlight.id == id)
        else {
            return Ok(false);
        };

        let mut highlight = self.library.highlights[highlight_ix].clone();
        highlight.color = color;
        self.write_highlight(&highlight)?;
        self.library.highlights[highlight_ix] = highlight;
        Ok(true)
    }

    pub fn update_note(
        &mut self,
        id: u64,
        body: impl Into<String>,
        color: HighlightColor,
    ) -> Result<bool> {
        let Some(note_ix) = self.library.notes.iter().position(|note| note.id == id) else {
            return Ok(false);
        };

        let mut note = self.library.notes[note_ix].clone();
        note.body = body.into();
        note.color = color;
        self.write_note(&note)?;
        self.library.notes[note_ix] = note;
        self.library.sanitize();
        Ok(true)
    }

    pub fn save(&self) -> Result<()> {
        self.write_history()
    }

    fn write_history(&self) -> Result<()> {
        let valid_paths = self
            .library
            .history
            .iter()
            .map(|entry| entry.path.to_string_lossy().to_string())
            .collect::<HashSet<_>>();
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(HISTORY_TABLE)?;
            let mut existing_paths = Vec::new();
            for entry in table.iter()? {
                let (path, _) = entry?;
                existing_paths.push(path.value().to_string());
            }

            for path in existing_paths {
                if !valid_paths.contains(&path) {
                    table.remove(path.as_str())?;
                }
            }

            for entry in &self.library.history {
                let key = entry.path.to_string_lossy();
                let encoded = encode(entry)?;
                table.insert(key.as_ref(), encoded.as_slice())?;
            }
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_bookmark(&self, bookmark: &Bookmark) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_TABLE)?;
            let encoded = encode(bookmark)?;
            table.insert(bookmark.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_bookmark_from_database(&self, id: u64) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(BOOKMARKS_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_highlight(&self, highlight: &Highlight) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_TABLE)?;
            let encoded = encode(highlight)?;
            table.insert(highlight.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_highlight_from_database(&self, id: u64) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(HIGHLIGHTS_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn write_note(&self, note: &Note) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_TABLE)?;
            let encoded = encode(note)?;
            table.insert(note.id, encoded.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn remove_note_from_database(&self, id: u64) -> Result<()> {
        let write_txn = self.database.begin_write()?;
        {
            let mut table = write_txn.open_table(NOTES_TABLE)?;
            table.remove(id)?;
        }
        write_txn.commit()?;
        Ok(())
    }
}

impl Library {
    fn record_history_at(&mut self, path: &Path, position: DocumentPosition, opened: u64) {
        let path = normalize_path(path);
        self.history.retain(|entry| entry.path != path);
        self.history.push(HistoryEntry {
            path,
            opened,
            position,
        });
        self.sanitize();
    }

    fn build_bookmark(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
        created: u64,
    ) -> Bookmark {
        let id = self.next_bookmark_id.max(1);
        self.next_bookmark_id = id + 1;
        Bookmark {
            id,
            path: normalize_path(path),
            label: label.into(),
            position,
            created,
        }
    }

    fn build_highlight(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        note: impl Into<String>,
        created: u64,
    ) -> Highlight {
        let id = self.next_highlight_id.max(1);
        self.next_highlight_id = id + 1;
        Highlight {
            id,
            path: normalize_path(path),
            color,
            rects,
            text: text.into(),
            note: note.into(),
            created,
        }
    }

    fn build_note(
        &mut self,
        path: &Path,
        color: HighlightColor,
        rects: Vec<HighlightRect>,
        text: impl Into<String>,
        body: impl Into<String>,
        created: u64,
    ) -> Note {
        let id = self.next_note_id.max(1);
        self.next_note_id = id + 1;
        Note {
            id,
            path: normalize_path(path),
            color,
            rects,
            text: text.into(),
            body: body.into(),
            created,
        }
    }

    fn remove_bookmark(&mut self, id: u64) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| bookmark.id != id);
        before != self.bookmarks.len()
    }

    fn remove_highlight(&mut self, id: u64) -> bool {
        let before = self.highlights.len();
        self.highlights.retain(|highlight| highlight.id != id);
        before != self.highlights.len()
    }

    fn remove_note(&mut self, id: u64) -> bool {
        let before = self.notes.len();
        self.notes.retain(|note| note.id != id);
        before != self.notes.len()
    }

    fn sanitize(&mut self) {
        self.history
            .retain(|entry| !entry.path.as_os_str().is_empty());
        self.history.sort_by(|left, right| {
            right
                .opened
                .cmp(&left.opened)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.history.dedup_by(|left, right| left.path == right.path);
        self.history.truncate(MAX_HISTORY_ENTRIES);

        self.bookmarks.retain_mut(|bookmark| {
            bookmark.label = bookmark.label.trim().to_string();
            !bookmark.path.as_os_str().is_empty() && !bookmark.label.is_empty()
        });
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });

        self.highlights.retain_mut(|highlight| {
            highlight.text = highlight.text.trim().to_string();
            highlight.note = highlight.note.trim().to_string();
            !highlight.path.as_os_str().is_empty() && !highlight.rects.is_empty()
        });
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.notes.retain_mut(|note| {
            note.text = note.text.trim().to_string();
            note.body = note.body.trim().to_string();
            !note.path.as_os_str().is_empty() && !note.rects.is_empty() && !note.body.is_empty()
        });
        self.notes.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.next_bookmark_id = self.next_bookmark_id.max(
            self.bookmarks
                .iter()
                .map(|bookmark| bookmark.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_highlight_id = self.next_highlight_id.max(
            self.highlights
                .iter()
                .map(|highlight| highlight.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_note_id = self
            .next_note_id
            .max(self.notes.iter().map(|note| note.id).max().unwrap_or(0) + 1);
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    })
}

fn library_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("glyphst")
        .join("library.redb")
}

fn ensure_tables(database: &Database) -> Result<()> {
    let write_txn = database.begin_write()?;
    {
        write_txn.open_table(HISTORY_TABLE)?;
    }
    {
        write_txn.open_table(BOOKMARKS_TABLE)?;
    }
    {
        write_txn.open_table(HIGHLIGHTS_TABLE)?;
    }
    {
        write_txn.open_table(NOTES_TABLE)?;
    }
    write_txn.commit()?;
    Ok(())
}

fn load_library(database: &Database) -> Result<Library> {
    let read_txn = database.begin_read()?;
    let mut library = Library::default();

    {
        let table = read_txn.open_table(HISTORY_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<HistoryEntry>(value.value()) {
                Ok(mut history_entry) => {
                    history_entry.path = normalize_path(&history_entry.path);
                    library.history.push(history_entry);
                }
                Err(error) => eprintln!("failed to decode PDF history entry: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(BOOKMARKS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<Bookmark>(value.value()) {
                Ok(mut bookmark) => {
                    bookmark.path = normalize_path(&bookmark.path);
                    library.bookmarks.push(bookmark);
                }
                Err(error) => eprintln!("failed to decode PDF bookmark: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(HIGHLIGHTS_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<Highlight>(value.value()) {
                Ok(mut highlight) => {
                    highlight.path = normalize_path(&highlight.path);
                    library.highlights.push(highlight);
                }
                Err(error) => eprintln!("failed to decode PDF highlight: {error:#}"),
            }
        }
    }

    {
        let table = read_txn.open_table(NOTES_TABLE)?;
        for entry in table.iter()? {
            let (_, value) = entry?;
            match decode::<Note>(value.value()) {
                Ok(mut note) => {
                    note.path = normalize_path(&note.path);
                    library.notes.push(note);
                }
                Err(error) => eprintln!("failed to decode PDF note: {error:#}"),
            }
        }
    }

    library.sanitize();
    Ok(library)
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    bincode::serialize(value).context("failed to encode library value")
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    bincode::deserialize(bytes).context("failed to decode library value")
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(page: usize) -> DocumentPosition {
        DocumentPosition {
            scroll_x_doc: page as f32,
            scroll_y_doc: page as f32 * 10.0,
            zoom: 1.25,
            page,
        }
    }

    fn temp_library_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "glyphst-{name}-{}-{nanos}.redb",
            std::process::id()
        ))
    }

    #[test]
    fn history_is_deduplicated_and_sorted() {
        let mut library = Library::default();
        library.record_history_at(Path::new("/tmp/a.pdf"), position(1), 10);
        library.record_history_at(Path::new("/tmp/b.pdf"), position(2), 11);
        library.record_history_at(Path::new("/tmp/a.pdf"), position(3), 12);

        assert_eq!(library.history.len(), 2);
        assert_eq!(
            library.history[0].path,
            normalize_path(Path::new("/tmp/a.pdf"))
        );
        assert_eq!(library.history[0].position.page, 3);
        assert_eq!(
            library.history[1].path,
            normalize_path(Path::new("/tmp/b.pdf"))
        );
    }

    #[test]
    fn caps_history_entries() {
        let mut library = Library::default();
        for ix in 0..MAX_HISTORY_ENTRIES + 5 {
            library.record_history_at(
                Path::new(&format!("/tmp/{ix}.pdf")),
                position(ix + 1),
                ix as u64,
            );
        }

        assert_eq!(library.history.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(library.history[0].opened, (MAX_HISTORY_ENTRIES + 4) as u64);
    }

    #[test]
    fn persists_history_bookmarks_and_highlights() {
        let path = temp_library_path("round-trip");
        let document_path = Path::new("/tmp/a.pdf");
        let highlight_rect = HighlightRect {
            page_idx: 0,
            x0: 10.0,
            y0: 20.0,
            x1: 30.0,
            y1: 40.0,
        };
        let highlight_id;
        let bookmark_id;
        let note_id;

        {
            let mut store = LibraryStore::open(path.clone()).unwrap();
            store.record_history(document_path, position(4));
            store.save().unwrap();
            bookmark_id = store
                .add_bookmark(document_path, "Chapter 1", position(5))
                .unwrap();
            let highlight = store
                .add_highlight(
                    document_path,
                    HighlightColor::Yellow,
                    vec![highlight_rect],
                    "selected text",
                    "",
                )
                .unwrap();
            highlight_id = highlight.id;
            assert!(
                store
                    .set_highlight_color(highlight_id, HighlightColor::Blue)
                    .unwrap()
            );
            let note = store
                .add_note(
                    document_path,
                    HighlightColor::Yellow,
                    vec![highlight_rect],
                    "selected text",
                    "initial note",
                )
                .unwrap();
            note_id = note.id;
            assert!(
                store
                    .update_note(note_id, "updated note", HighlightColor::Green)
                    .unwrap()
            );
        }

        {
            let mut store = LibraryStore::open(path.clone()).unwrap();
            assert_eq!(store.history_position(document_path).unwrap().page, 4);
            assert_eq!(store.bookmarks()[0].id, bookmark_id);
            let highlights = store.highlights_for(document_path);
            assert_eq!(highlights.len(), 1);
            assert_eq!(highlights[0].id, highlight_id);
            assert_eq!(highlights[0].color, HighlightColor::Blue);
            assert_eq!(highlights[0].rects, vec![highlight_rect]);
            let notes = store.notes_for(document_path);
            assert_eq!(notes.len(), 1);
            assert_eq!(notes[0].id, note_id);
            assert_eq!(notes[0].color, HighlightColor::Green);
            assert_eq!(notes[0].rects, vec![highlight_rect]);
            assert_eq!(notes[0].body, "updated note");

            assert!(store.remove_highlight(highlight_id).unwrap());
            assert!(store.remove_note(note_id).unwrap());
            assert!(store.remove_bookmark(bookmark_id).unwrap());
        }

        {
            let store = LibraryStore::open(path.clone()).unwrap();
            assert!(store.bookmarks().is_empty());
            assert!(store.highlights_for(document_path).is_empty());
            assert!(store.notes_for(document_path).is_empty());
        }

        let _ = fs::remove_file(path);
    }
}
