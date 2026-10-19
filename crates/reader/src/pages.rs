//! Tabs that are not documents: the keyboard shortcuts, a new tab's
//! launcher (frame 04b), the reading activity (frame 04c) and the library
//! (frame 04d). They sit in
//! the tab bar after the documents and show in place of home; while one
//! shows, `ReaderApp::active` is `None`.

use crate::{
    activity_view::ActivityView, keymap_page::KeymapPage, launcher::Launcher,
    library_tab::LibraryTab,
};

pub enum Page {
    Keymap(KeymapPage),
    NewTab(Launcher),
    Activity(ActivityView),
    Library(Box<LibraryTab>),
}

impl Page {
    pub fn title(&self) -> &'static str {
        match self {
            Self::Keymap(_) => "Keyboard shortcuts",
            Self::NewTab(_) => "new tab",
            Self::Activity(_) => "reading activity",
            Self::Library(_) => "library",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Keymap(_) => "keyboard",
            Self::NewTab(_) => "plus",
            Self::Activity(_) => "activity",
            Self::Library(_) => "library",
        }
    }

    /// Whether there is at most one tab of its kind: opening another goes
    /// to it. New tabs are as many as asked for.
    pub fn single(&self) -> bool {
        !matches!(self, Self::NewTab(_))
    }

    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    /// Whether key presses go to a shortcut being recorded.
    pub fn is_recording(&self) -> bool {
        matches!(self, Self::Keymap(page) if page.is_recording())
    }
}
