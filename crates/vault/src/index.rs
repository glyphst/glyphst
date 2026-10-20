//! Every note in the vault and what it links to, for backlinks: item
//! notes, concept pages and the user's own notes alike. A scan reads only
//! the notes that changed since the last one (by size and time).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{frontmatter::split, note_id, pages::page_id, wikilinks::link_key};

/// Whose a note is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    /// A library item's note (`glyphst_id`).
    Item(u64),
    /// A concept page (`glyphst_page`).
    Page(u64),
    /// The user's.
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedNote {
    /// From the vault's folder, with `/`.
    pub path: String,
    /// The name links find it by (`link_key`).
    pub key: String,
    /// Its `aliases`, folded as keys.
    pub aliases: Vec<String>,
    /// Its first `# heading`, else its file name.
    pub title: String,
    pub owner: Owner,
    /// What it links to, as keys, each once.
    pub links: Vec<String>,
    size: u64,
    modified: Option<SystemTime>,
}

/// The vault's notes and their links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultIndex {
    pub notes: Vec<IndexedNote>,
}

impl VaultIndex {
    /// The vault under `root` now, reusing `previous` for unchanged notes.
    /// Folders starting with `.` (`.obsidian`, `.trash`) are skipped.
    pub fn scan(root: &Path, previous: &VaultIndex) -> Self {
        let known = previous
            .notes
            .iter()
            .map(|note| (note.path.as_str(), note))
            .collect::<HashMap<_, _>>();
        let mut notes = Vec::new();
        let mut folders = vec![root.to_path_buf()];
        while let Some(folder) = folders.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    folders.push(path);
                    continue;
                }
                if !name.ends_with(".md") {
                    continue;
                }
                let relative = relative(root, &path);
                let modified = metadata.modified().ok();
                match known.get(relative.as_str()) {
                    Some(note) if note.size == metadata.len() && note.modified == modified => {
                        notes.push((*note).clone());
                    }
                    _ => {
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            notes.push(read(relative, &text, metadata.len(), modified));
                        }
                    }
                }
            }
        }
        notes.sort_by(|a, b| a.path.cmp(&b.path));
        Self { notes }
    }

    /// The note a link points at: by file name, else by alias.
    pub fn resolve(&self, target: &str) -> Option<&IndexedNote> {
        let key = link_key(target);
        self.notes
            .iter()
            .find(|note| note.key == key)
            .or_else(|| self.notes.iter().find(|note| note.aliases.contains(&key)))
    }

    pub fn note_of(&self, owner: Owner) -> Option<&IndexedNote> {
        self.notes.iter().find(|note| note.owner == owner)
    }

    /// The notes that link to `owner`'s note.
    pub fn backlinks(&self, owner: Owner) -> Vec<&IndexedNote> {
        let Some(target) = self.note_of(owner) else {
            return Vec::new();
        };
        let mut names = vec![target.key.as_str()];
        names.extend(target.aliases.iter().map(String::as_str));
        self.notes
            .iter()
            .filter(|note| note.path != target.path)
            .filter(|note| note.links.iter().any(|link| names.contains(&link.as_str())))
            .collect()
    }
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn read(path: String, text: &str, size: u64, modified: Option<SystemTime>) -> IndexedNote {
    let (frontmatter, body) = split(text);
    let owner = match (note_id(text), page_id(text)) {
        (Some(id), _) => Owner::Item(id),
        (None, Some(id)) => Owner::Page(id),
        (None, None) => Owner::User,
    };
    let aliases = frontmatter
        .get("aliases")
        .map(|value| value.as_list())
        .unwrap_or_default()
        .iter()
        .map(|alias| link_key(alias))
        .collect();
    let mut links = Vec::new();
    // Links in the frontmatter (a page's `sources`) count too.
    let sources = frontmatter
        .get("sources")
        .map(|value| value.as_list())
        .unwrap_or_default()
        .join(" ");
    for link in crate::wikilinks::wikilinks(&format!("{sources}\n{body}")) {
        let key = link.key();
        if !links.contains(&key) {
            links.push(key);
        }
    }
    let key = link_key(&path);
    let title = body
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|title| title.trim().to_string())
        .unwrap_or_else(|| {
            PathBuf::from(&path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    IndexedNote {
        path,
        key,
        aliases,
        title,
        owner,
        links,
        size,
        modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backlinks_come_from_pages_and_the_users_notes_and_scans_reuse_unchanged_notes() {
        let root = std::env::temp_dir().join(format!("glyphst-vault-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for folder in ["items", "concepts", "thesis", ".obsidian"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        let write = |path: &str, text: &str| std::fs::write(root.join(path), text).unwrap();
        write(
            "items/vaswani2017attention.md",
            "---\nglyphst_id: 12\naliases:\n  - Attention Is All You Need\n---\n# Attention Is All You Need\n",
        );
        write(
            "items/devlin2018bert.md",
            "---\nglyphst_id: 13\n---\n# BERT\n",
        );
        write(
            "concepts/attention.md",
            "---\nglyphst_page: 1\nsources:\n  - \"[[devlin2018bert]]\"\n---\n# Attention\nSee [[Vaswani2017Attention|the paper]].\n",
        );
        write(
            "thesis/chapter 2.md",
            "# Chapter 2\nAs [[Attention Is All You Need]] shows… `[[not-a-link]]`\n",
        );
        write(".obsidian/hidden.md", "[[vaswani2017attention]]");

        let index = VaultIndex::scan(&root, &VaultIndex::default());
        assert_eq!(index.notes.len(), 4, "{:#?}", index.notes);
        let backlinks = index
            .backlinks(Owner::Item(12))
            .iter()
            .map(|note| note.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(backlinks, ["concepts/attention.md", "thesis/chapter 2.md"]);
        // A page's sources link to the items it was written from.
        assert_eq!(index.backlinks(Owner::Item(13)).len(), 1);
        assert_eq!(index.backlinks(Owner::Page(1)).len(), 0);
        assert_eq!(
            index.resolve("attention").map(|note| note.owner),
            Some(Owner::Page(1))
        );
        assert_eq!(
            index
                .note_of(Owner::Page(1))
                .map(|note| note.title.as_str()),
            Some("Attention")
        );

        // An unchanged note is not read again: change its text behind the
        // index's back, keeping its size and time.
        let path = root.join("items/devlin2018bert.md");
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "---\nglyphst_id: 99\n---\n# BERT\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(time)
            .unwrap();
        let again = VaultIndex::scan(&root, &index);
        assert!(again.note_of(Owner::Item(13)).is_some(), "reused");
        let _ = std::fs::remove_dir_all(root);
    }
}
