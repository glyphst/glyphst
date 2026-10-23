//! Back and forward through everywhere the user has been: tabs, library
//! views and jumps inside documents, in the order they happened (as VS
//! Code's Go Back and Vim's Ctrl+O across buffers).
//!
//! A jump inside a document keeps its place in the document's own history
//! (`DocumentTab`'s back and forward); here it is a `Jump` marking that
//! the document's history has one more step. Everything else is a
//! `Location`, noted when the place shown changes.

use std::path::PathBuf;

use glyphst_library::DocumentId;

use crate::library_tab::View;

const MAX_ENTRIES: usize = 200;

/// A tab that is not a document or the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Keymap,
    Activity,
    NewTab,
}

/// Where the user was.
#[derive(Clone, Debug, PartialEq)]
pub enum Location {
    Home,
    Document {
        tab: u64,
        path: PathBuf,
    },
    /// The library tab, on a view (its filter and item as they were).
    Library {
        view: View,
        filter: String,
        selected: Option<DocumentId>,
    },
    Page(PageKind),
}

impl Location {
    /// Whether two locations are one place: the same document tab, library
    /// view or page. Typing in the library's filter or picking another
    /// item does not move the user anywhere.
    fn same_place(&self, other: &Location) -> bool {
        match (self, other) {
            (Location::Document { tab: a, .. }, Location::Document { tab: b, .. }) => a == b,
            (Location::Library { view: a, .. }, Location::Library { view: b, .. }) => a == b,
            (a, b) => a == b,
        }
    }
}

/// A step back or forward.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    /// The document had a jump: its own history goes a step.
    Jump { tab: u64, path: PathBuf },
    /// A place to return to.
    At(Location),
}

#[derive(Default)]
pub struct NavHistory {
    back: Vec<Entry>,
    forward: Vec<Entry>,
    /// The place shown when last observed.
    last: Option<Location>,
}

impl NavHistory {
    /// The place shown this frame: a different place than before leaves
    /// the old one to go back to.
    pub fn observe(&mut self, now: Location) {
        match &self.last {
            Some(last) if last.same_place(&now) => {}
            Some(last) => {
                let left = last.clone();
                self.push(Entry::At(left));
            }
            None => {}
        }
        self.last = Some(now);
    }

    /// The user arrived somewhere by going back or forward: nothing to
    /// note.
    pub fn arrived(&mut self, now: Location) {
        self.last = Some(now);
    }

    /// A jump inside a document.
    pub fn jumped(&mut self, tab: u64, path: PathBuf) {
        self.push(Entry::Jump { tab, path });
    }

    fn push(&mut self, entry: Entry) {
        self.back.push(entry);
        if self.back.len() > MAX_ENTRIES {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The step to go back, and the step that undoes it is noted for
    /// forward: a jump stays a jump; a location leaves the place shown
    /// (`now`).
    pub fn back(&mut self, now: Location) -> Option<Entry> {
        let entry = self.back.pop()?;
        self.forward.push(match &entry {
            Entry::Jump { .. } => entry.clone(),
            Entry::At(_) => Entry::At(now),
        });
        Some(entry)
    }

    /// The step forward, noting its undoing for back.
    pub fn forward(&mut self, now: Location) -> Option<Entry> {
        let entry = self.forward.pop()?;
        self.back.push(match &entry {
            Entry::Jump { .. } => entry.clone(),
            Entry::At(_) => Entry::At(now),
        });
        Some(entry)
    }

    /// A tab closed: its jumps can no longer be retraced, so they become
    /// places to reopen its file at.
    pub fn closed(&mut self, tab: u64) {
        for entry in self.back.iter_mut().chain(self.forward.iter_mut()) {
            if let Entry::Jump { tab: jumped, path } = entry
                && *jumped == tab
            {
                *entry = Entry::At(Location::Document {
                    tab,
                    path: path.clone(),
                });
            }
        }
        // Two places in a row to the same file are one.
        self.back.dedup();
        self.forward.dedup();
    }
}

#[cfg(test)]
mod tests {
    use glyphst_library::LibraryTagId;

    use super::*;

    fn library(view: View, selected: Option<u64>) -> Location {
        Location::Library {
            view,
            filter: String::new(),
            selected: selected.map(DocumentId),
        }
    }

    fn document(tab: u64) -> Location {
        Location::Document {
            tab,
            path: PathBuf::from(format!("/d{tab}.pdf")),
        }
    }

    #[test]
    fn back_from_a_tag_returns_to_all_items_with_the_item_chosen() {
        let mut nav = NavHistory::default();
        nav.observe(library(View::All, None));
        // Choosing an item is not a place of its own.
        nav.observe(library(View::All, Some(4)));
        assert!(!nav.can_go_back());
        nav.observe(library(View::Tag(LibraryTagId(1)), None));
        let now = library(View::Tag(LibraryTagId(1)), None);
        assert_eq!(
            nav.back(now.clone()),
            Some(Entry::At(library(View::All, Some(4))))
        );
        nav.arrived(library(View::All, Some(4)));
        assert_eq!(
            nav.forward(library(View::All, Some(4))),
            Some(Entry::At(now))
        );
    }

    #[test]
    fn back_from_the_library_returns_to_the_document_and_its_jumps() {
        let mut nav = NavHistory::default();
        nav.observe(document(1));
        nav.jumped(1, "/d1.pdf".into());
        nav.observe(document(1));
        nav.observe(library(View::All, None));
        assert_eq!(
            nav.back(library(View::All, None)),
            Some(Entry::At(document(1)))
        );
        nav.arrived(document(1));
        assert_eq!(
            nav.back(document(1)),
            Some(Entry::Jump {
                tab: 1,
                path: "/d1.pdf".into()
            })
        );
        assert!(!nav.can_go_back());
        // Forward retraces the jump, then the library.
        assert!(matches!(nav.forward(document(1)), Some(Entry::Jump { .. })));
        assert_eq!(
            nav.forward(document(1)),
            Some(Entry::At(library(View::All, None)))
        );
    }

    #[test]
    fn a_new_step_clears_forward() {
        let mut nav = NavHistory::default();
        nav.observe(document(1));
        nav.observe(document(2));
        nav.back(document(2));
        nav.arrived(document(1));
        assert!(nav.can_go_forward());
        nav.jumped(1, "/d1.pdf".into());
        assert!(!nav.can_go_forward());
    }

    #[test]
    fn a_closed_tabs_jumps_become_its_file() {
        let mut nav = NavHistory::default();
        nav.observe(document(1));
        nav.jumped(1, "/d1.pdf".into());
        nav.jumped(1, "/d1.pdf".into());
        nav.observe(document(2));
        nav.closed(1);
        assert_eq!(nav.back(document(2)), Some(Entry::At(document(1))));
        assert_eq!(nav.back(document(2)), None);
    }
}
