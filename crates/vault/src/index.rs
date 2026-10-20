//! Every note in the vault and what it links to, for backlinks: item
//! notes, concept pages and the user's own notes alike. A scan reads only
//! the notes that changed since the last one (by size and time).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

use crate::{
    frontmatter::split,
    note_id, note_uid,
    pages::{page_id, page_uid},
    wikilinks::link_key,
};

/// Whose a note is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    /// A library item's note, by the item's local number.
    Item(u64),
    /// A concept page, by its local number.
    Page(u64),
    /// The user's.
    User,
}

/// What a note names its owner by: a uid (the same on every computer,
/// looked up in [`Owners`]), else a local number (older notes).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Named {
    ItemUid(String),
    PageUid(String),
    Local(Owner),
}

/// Local owners by uid, for the notes that name theirs by uid.
pub type Owners = HashMap<String, Owner>;

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
    /// Another note is its owner's, and this one is a sync tool's copy of
    /// it (`.sync-conflict-…`, `(conflicted copy)`, `(conflict …)`): not
    /// the owner's note, listed so the user can merge it.
    pub conflict_copy: bool,
    named: Named,
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
    /// `owners` finds the local owner of a note that names its by uid.
    pub fn scan(root: &Path, previous: &VaultIndex, owners: &Owners) -> Self {
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
        for note in &mut notes {
            note.owner = match &note.named {
                Named::ItemUid(uid) | Named::PageUid(uid) => {
                    owners.get(uid).copied().unwrap_or(Owner::User)
                }
                Named::Local(owner) => *owner,
            };
            note.conflict_copy = false;
        }
        // Two notes of one owner: a sync tool's copy is not the note.
        let mut claimed = HashMap::new();
        for (index, note) in notes.iter().enumerate() {
            if note.owner != Owner::User {
                claimed
                    .entry(note.owner)
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        for indexes in claimed.into_values().filter(|indexes| indexes.len() > 1) {
            let original = indexes
                .iter()
                .copied()
                .min_by_key(|index| {
                    (
                        is_conflict_name(&notes[*index].path),
                        notes[*index].path.len(),
                    )
                })
                .unwrap_or(indexes[0]);
            for index in indexes {
                if index != original {
                    notes[index].conflict_copy = true;
                    notes[index].owner = Owner::User;
                }
            }
        }
        Self { notes }
    }

    /// Sync tools' copies of owned notes.
    pub fn conflict_copies(&self) -> impl Iterator<Item = &IndexedNote> {
        self.notes.iter().filter(|note| note.conflict_copy)
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
    let named = if let Some(uid) = note_uid(text) {
        Named::ItemUid(uid)
    } else if let Some(uid) = page_uid(text) {
        Named::PageUid(uid)
    } else {
        Named::Local(match (note_id(text), page_id(text)) {
            (Some(id), _) => Owner::Item(id),
            (None, Some(id)) => Owner::Page(id),
            (None, None) => Owner::User,
        })
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
        owner: Owner::User,
        links,
        conflict_copy: false,
        named,
        size,
        modified,
    }
}

/// A file name a sync tool gives the copy it keeps when two computers
/// changed a file: Syncthing's, Dropbox's, Nextcloud's and Glyphst's own.
pub fn is_conflict_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    name.contains(".sync-conflict-")
        || name.contains("conflicted copy")
        || name.contains("(conflict ")
        || name.contains("_conflict-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_named_by_uid_find_their_owner_and_a_sync_tools_copy_is_not_it() {
        let root = std::env::temp_dir().join(format!("glyphst-vault-uids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("items")).unwrap();
        let note = "---\nglyphst_uid: u1\n---\n# Paper\n";
        std::fs::write(root.join("items/paper.md"), note).unwrap();
        std::fs::write(
            root.join("items/paper.sync-conflict-20261020-120000-ABCD.md"),
            note,
        )
        .unwrap();
        std::fs::write(
            root.join("items/older.md"),
            "---\nglyphst_id: 9\n---\n# Older\n",
        )
        .unwrap();
        std::fs::write(root.join("items/unknown.md"), "---\nglyphst_uid: u2\n---\n").unwrap();
        let owners = Owners::from([("u1".to_string(), Owner::Item(4))]);
        let index = VaultIndex::scan(&root, &VaultIndex::default(), &owners);
        assert_eq!(
            index.note_of(Owner::Item(4)).unwrap().path,
            "items/paper.md"
        );
        assert_eq!(
            index.note_of(Owner::Item(9)).unwrap().path,
            "items/older.md"
        );
        let copies = index
            .conflict_copies()
            .map(|note| note.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            copies,
            vec!["items/paper.sync-conflict-20261020-120000-ABCD.md"]
        );
        // Not (yet) in this library: the user's, for now.
        let unknown = index
            .notes
            .iter()
            .find(|note| note.path == "items/unknown.md")
            .unwrap();
        assert_eq!(unknown.owner, Owner::User);
        // Known later, a rescan finds it without reading it again.
        let owners = Owners::from([
            ("u1".to_string(), Owner::Item(4)),
            ("u2".to_string(), Owner::Item(5)),
        ]);
        let again = VaultIndex::scan(&root, &index, &owners);
        assert_eq!(
            again.note_of(Owner::Item(5)).unwrap().path,
            "items/unknown.md"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

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

        let index = VaultIndex::scan(&root, &VaultIndex::default(), &Owners::new());
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
        let again = VaultIndex::scan(&root, &index, &Owners::new());
        assert!(again.note_of(Owner::Item(13)).is_some(), "reused");
        let _ = std::fs::remove_dir_all(root);
    }
}
