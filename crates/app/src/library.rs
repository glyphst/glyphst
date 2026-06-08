use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use gpui::Global;
use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::theme::{push_value_node, value_to_f32};

const MAX_HISTORY_ENTRIES: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentPosition {
    pub scroll_x_doc: f32,
    pub scroll_y_doc: f32,
    pub zoom: f32,
    pub page: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub path: PathBuf,
    pub opened: u64,
    pub position: DocumentPosition,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    pub id: u64,
    pub path: PathBuf,
    pub label: String,
    pub position: DocumentPosition,
    pub created: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    pub history: Vec<HistoryEntry>,
    pub bookmarks: Vec<Bookmark>,
    next_bookmark_id: u64,
}

pub struct LibraryStore {
    pub library: Library,
    pub path: PathBuf,
}

impl Global for LibraryStore {}

impl LibraryStore {
    pub fn load() -> Self {
        let path = library_path();
        let library = match load_library(&path) {
            Ok(library) => library,
            Err(error) => {
                eprintln!("failed to load library from {}: {error:#}", path.display());
                Library::default()
            }
        };

        Self { library, path }
    }

    pub fn history_entries(&self) -> &[HistoryEntry] {
        &self.library.history
    }

    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.library.bookmarks
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
        let id = self
            .library
            .add_bookmark_at(path, label, position, current_timestamp());
        self.save()?;
        Ok(id)
    }

    pub fn remove_bookmark(&mut self, id: u64) -> Result<bool> {
        let removed = self.library.remove_bookmark(id);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    pub fn save(&self) -> Result<()> {
        write_kdl(&self.path, self.library.to_kdl_string())
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

    fn add_bookmark_at(
        &mut self,
        path: &Path,
        label: impl Into<String>,
        position: DocumentPosition,
        created: u64,
    ) -> u64 {
        let id = self.next_bookmark_id.max(1);
        self.next_bookmark_id = id + 1;
        self.bookmarks.push(Bookmark {
            id,
            path: normalize_path(path),
            label: label.into(),
            position,
            created,
        });
        self.sanitize();
        id
    }

    fn remove_bookmark(&mut self, id: u64) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks.retain(|bookmark| bookmark.id != id);
        before != self.bookmarks.len()
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

        self.next_bookmark_id = self.next_bookmark_id.max(
            self.bookmarks
                .iter()
                .map(|bookmark| bookmark.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
    }

    fn from_document(document: &KdlDocument) -> Self {
        let mut library = Self::default();

        if let Some(history) = document.get("history").and_then(KdlNode::children) {
            library.history = history
                .nodes()
                .iter()
                .filter(|node| node.name().value() == "entry")
                .filter_map(parse_history_entry)
                .collect();
        }

        if let Some(bookmarks) = document.get("bookmarks").and_then(KdlNode::children) {
            library.bookmarks = bookmarks
                .nodes()
                .iter()
                .filter(|node| node.name().value() == "bookmark")
                .filter_map(parse_bookmark)
                .collect();
        }

        library.sanitize();
        library
    }

    fn to_kdl_string(&self) -> String {
        let mut document = KdlDocument::new();

        if !self.history.is_empty() {
            let mut history = KdlNode::new("history");
            let children = history.ensure_children();
            for entry in &self.history {
                let mut node = KdlNode::new("entry");
                node.push(entry.path.to_string_lossy().to_string());
                let entry_children = node.ensure_children();
                push_value_node(entry_children, "opened", entry.opened as i128);
                write_position(entry_children, entry.position);
                children.nodes_mut().push(node);
            }
            document.nodes_mut().push(history);
        }

        if !self.bookmarks.is_empty() {
            let mut bookmarks = KdlNode::new("bookmarks");
            let children = bookmarks.ensure_children();
            for bookmark in &self.bookmarks {
                let mut node = KdlNode::new("bookmark");
                node.push(bookmark.id as i128);
                node.push(bookmark.path.to_string_lossy().to_string());
                let bookmark_children = node.ensure_children();
                push_value_node(bookmark_children, "label", bookmark.label.clone());
                push_value_node(bookmark_children, "created", bookmark.created as i128);
                write_position(bookmark_children, bookmark.position);
                children.nodes_mut().push(node);
            }
            document.nodes_mut().push(bookmarks);
        }

        document.autoformat();
        document.to_string()
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
        .join("library.kdl")
}

fn load_library(path: &Path) -> Result<Library> {
    if !path.exists() {
        return Ok(Library::default());
    }

    fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .and_then(|text| parse_library(&text))
}

fn parse_library(text: &str) -> Result<Library> {
    let document = text.parse::<KdlDocument>()?;
    Ok(Library::from_document(&document))
}

fn write_kdl(path: &Path, contents: String) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

fn parse_history_entry(node: &KdlNode) -> Option<HistoryEntry> {
    let path = PathBuf::from(node.get(0)?.as_string()?);
    let children = node.children()?;
    Some(HistoryEntry {
        path: normalize_path(&path),
        opened: child_u64(children, "opened")?,
        position: parse_position(children)?,
    })
}

fn parse_bookmark(node: &KdlNode) -> Option<Bookmark> {
    let id = child_arg_u64(node, 0)?;
    let path = PathBuf::from(node.get(1)?.as_string()?);
    let children = node.children()?;
    Some(Bookmark {
        id,
        path: normalize_path(&path),
        label: child_string(children, "label")?.to_string(),
        created: child_u64(children, "created")?,
        position: parse_position(children)?,
    })
}

fn parse_position(document: &KdlDocument) -> Option<DocumentPosition> {
    Some(DocumentPosition {
        scroll_x_doc: child_f32(document, "scroll-x").unwrap_or(0.0),
        scroll_y_doc: child_f32(document, "scroll-y").unwrap_or(0.0),
        zoom: child_f32(document, "zoom").unwrap_or(1.0),
        page: child_usize(document, "page").unwrap_or(1).max(1),
    })
}

fn write_position(document: &mut KdlDocument, position: DocumentPosition) {
    push_value_node(document, "scroll-x", position.scroll_x_doc as f64);
    push_value_node(document, "scroll-y", position.scroll_y_doc as f64);
    push_value_node(document, "zoom", position.zoom as f64);
    push_value_node(document, "page", position.page as i128);
}

fn child_value<'a>(document: &'a KdlDocument, name: &str) -> Option<&'a KdlValue> {
    document.get(name)?.get(0)
}

fn child_string<'a>(document: &'a KdlDocument, name: &str) -> Option<&'a str> {
    child_value(document, name)?.as_string()
}

fn child_u64(document: &KdlDocument, name: &str) -> Option<u64> {
    child_value(document, name)?
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
}

fn child_usize(document: &KdlDocument, name: &str) -> Option<usize> {
    child_value(document, name)?
        .as_integer()
        .and_then(|value| usize::try_from(value).ok())
}

fn child_arg_u64(node: &KdlNode, ix: usize) -> Option<u64> {
    node.get(ix)?
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
}

fn child_f32(document: &KdlDocument, name: &str) -> Option<f32> {
    child_value(document, name).and_then(value_to_f32)
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
    fn round_trips_library_kdl() {
        let mut library = Library::default();
        library.record_history_at(Path::new("/tmp/a.pdf"), position(4), 20);
        library.add_bookmark_at(Path::new("/tmp/a.pdf"), "Chapter 1", position(5), 21);

        let exported = library.to_kdl_string();
        let parsed = parse_library(&exported).unwrap();

        assert_eq!(parsed.history, library.history);
        assert_eq!(parsed.bookmarks, library.bookmarks);
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
}
