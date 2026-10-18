//! Gamepad mode (frames 09c–j): a fullscreen shell for reading with a pad
//! from across the room, after Steam's Big Picture. Home is a hero of the
//! focused book over a shelf of the recent ones; reading is the page alone,
//! with a hint bar that fades; contents, go to page and a menu come over it
//! as sheets. Everything is driven by the arrow keys, Enter, Escape and
//! Tab the pad presses (`pad::route`), so the keyboard works too.

pub mod draw;
pub mod home;
pub mod reader;

/// How long the reader's hints stay once shown, in seconds.
pub const HINTS_SECONDS: f64 = 3.0;

#[derive(Debug, Default)]
pub struct Couch {
    pub overlay: Option<Overlay>,
    /// The focused book on home's shelf.
    pub shelf: usize,
    /// Until when (egui time) the reader's hints show.
    pub hints_until: f64,
    /// The right stick's click pinned the hints on.
    pub hints_pinned: bool,
    /// A book came forward: its hints show for a while.
    pub book_opened: bool,
    /// The keys home takes this frame, taken before the keymap could.
    pub home_navs: Vec<Nav>,
    /// Whether the window was fullscreen before gamepad mode, to put back.
    pub saved_fullscreen: Option<bool>,
    /// The menu's fit: the whole page rather than the width.
    pub fit_page: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// The outline, with the entry focused.
    Contents {
        cursor: usize,
    },
    /// A page being chosen (from 0).
    GoTo {
        page: usize,
    },
    Menu {
        cursor: usize,
    },
}

/// The menu's rows (frame 09i).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuItem {
    TextSize,
    Theme,
    Fit,
    Smooth,
    Library,
    Exit,
}

impl MenuItem {
    /// The rows while reading; home leaves out what needs a book.
    pub fn rows(reading: bool) -> &'static [Self] {
        if reading {
            &[
                Self::TextSize,
                Self::Theme,
                Self::Fit,
                Self::Smooth,
                Self::Library,
                Self::Exit,
            ]
        } else {
            &[Self::TextSize, Self::Theme, Self::Smooth, Self::Exit]
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::TextSize => "Text size",
            Self::Theme => "Theme",
            Self::Fit => "Fit",
            Self::Smooth => "Smooth scrolling",
            Self::Library => "Back to library",
            Self::Exit => "Exit gamepad mode",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::TextSize => "a-large-small",
            Self::Theme => "sun-moon",
            Self::Fit => "maximize-2",
            Self::Smooth => "arrow-down",
            Self::Library => "house",
            Self::Exit => "log-out",
        }
    }

    /// Whether left and right change it (a value), rather than Enter
    /// running it.
    pub fn has_value(self) -> bool {
        matches!(
            self,
            Self::TextSize | Self::Theme | Self::Fit | Self::Smooth
        )
    }
}

/// A key the overlays take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    /// Tab and Shift+Tab: the bumpers.
    Next,
    Previous,
}

impl Nav {
    /// `consume_key` ignores Shift, so Shift+Tab comes before Tab or Tab
    /// would take it.
    pub const KEYS: [(egui::Key, egui::Modifiers, Nav); 8] = [
        (egui::Key::ArrowUp, egui::Modifiers::NONE, Nav::Up),
        (egui::Key::ArrowDown, egui::Modifiers::NONE, Nav::Down),
        (egui::Key::ArrowLeft, egui::Modifiers::NONE, Nav::Left),
        (egui::Key::ArrowRight, egui::Modifiers::NONE, Nav::Right),
        (egui::Key::Enter, egui::Modifiers::NONE, Nav::Enter),
        (egui::Key::Escape, egui::Modifiers::NONE, Nav::Back),
        (egui::Key::Tab, egui::Modifiers::SHIFT, Nav::Previous),
        (egui::Key::Tab, egui::Modifiers::NONE, Nav::Next),
    ];

    /// The keys pressed this frame, taken so nothing else sees them.
    pub fn take(ctx: &egui::Context) -> Vec<Nav> {
        ctx.input_mut(|input| {
            let mut taken = Vec::new();
            for (key, modifiers, nav) in Self::KEYS {
                while input.consume_key(modifiers, key) {
                    taken.push(nav);
                }
            }
            taken
        })
    }
}

/// What a key did to an overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Closed,
    GoToEntry(usize),
    GoToPage(usize),
    /// A menu row changed by `step` (-1, +1), or run (0).
    Menu(MenuItem, i32),
}

/// What the overlays need to know of the book.
#[derive(Clone, Copy, Debug)]
pub struct Book {
    pub entries: usize,
    pub pages: usize,
    pub reading: bool,
}

/// How far the bumpers jump on the go-to sheet.
pub const PAGE_JUMP: usize = 10;

impl Couch {
    /// Moves the open overlay by `nav`; what came of it.
    pub fn press(&mut self, nav: Nav, book: Book) -> Outcome {
        let Some(overlay) = &mut self.overlay else {
            return Outcome::Nothing;
        };
        if nav == Nav::Back {
            self.overlay = None;
            return Outcome::Closed;
        }
        let step = |at: usize, by: isize, len: usize| {
            (at as isize + by).clamp(0, len.saturating_sub(1) as isize) as usize
        };
        match overlay {
            Overlay::Contents { cursor } => match nav {
                Nav::Up => *cursor = step(*cursor, -1, book.entries),
                Nav::Down => *cursor = step(*cursor, 1, book.entries),
                Nav::Previous => *cursor = step(*cursor, -5, book.entries),
                Nav::Next => *cursor = step(*cursor, 5, book.entries),
                Nav::Enter if book.entries > 0 => {
                    let entry = *cursor;
                    self.overlay = None;
                    return Outcome::GoToEntry(entry);
                }
                _ => {}
            },
            Overlay::GoTo { page } => match nav {
                Nav::Left | Nav::Up => *page = step(*page, -1, book.pages),
                Nav::Right | Nav::Down => *page = step(*page, 1, book.pages),
                Nav::Previous => *page = step(*page, -(PAGE_JUMP as isize), book.pages),
                Nav::Next => *page = step(*page, PAGE_JUMP as isize, book.pages),
                Nav::Enter => {
                    let page = *page;
                    self.overlay = None;
                    return Outcome::GoToPage(page);
                }
                Nav::Back => {}
            },
            Overlay::Menu { cursor } => {
                let rows = MenuItem::rows(book.reading);
                *cursor = (*cursor).min(rows.len() - 1);
                let item = rows[*cursor];
                match nav {
                    Nav::Up => *cursor = step(*cursor, -1, rows.len()),
                    Nav::Down => *cursor = step(*cursor, 1, rows.len()),
                    Nav::Left if item.has_value() => return Outcome::Menu(item, -1),
                    Nav::Right if item.has_value() => return Outcome::Menu(item, 1),
                    Nav::Enter => {
                        if !item.has_value() {
                            self.overlay = None;
                        }
                        return Outcome::Menu(item, if item.has_value() { 1 } else { 0 });
                    }
                    _ => {}
                }
            }
        }
        Outcome::Nothing
    }

    /// Moves home's focus by `by` books, kept on the shelf.
    pub fn move_shelf(&mut self, by: isize, books: usize) {
        self.shelf = (self.shelf as isize + by).clamp(0, books.saturating_sub(1) as isize) as usize;
    }
}

/// The text sizes gamepad mode offers, as zoom over the design's.
pub const TEXT_SIZES: [f32; 6] = [0.8, 0.9, 1.0, 1.15, 1.3, 1.5];

/// The text size `step`s from `size`, kept in [`TEXT_SIZES`].
pub fn step_text_size(size: f32, step: i32) -> f32 {
    let at = TEXT_SIZES
        .iter()
        .position(|&s| (s - size).abs() < 0.01)
        .unwrap_or(2) as i32;
    TEXT_SIZES[(at + step).clamp(0, TEXT_SIZES.len() as i32 - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOK: Book = Book {
        entries: 14,
        pages: 15,
        reading: true,
    };

    #[test]
    fn contents_move_and_open_an_entry() {
        let mut couch = Couch {
            overlay: Some(Overlay::Contents { cursor: 5 }),
            ..Couch::default()
        };
        couch.press(Nav::Down, BOOK);
        couch.press(Nav::Next, BOOK);
        assert_eq!(couch.overlay, Some(Overlay::Contents { cursor: 11 }));
        couch.press(Nav::Next, BOOK);
        assert_eq!(
            couch.overlay,
            Some(Overlay::Contents { cursor: 13 }),
            "kept on the last entry"
        );
        assert_eq!(couch.press(Nav::Enter, BOOK), Outcome::GoToEntry(13));
        assert_eq!(couch.overlay, None);
    }

    #[test]
    fn go_to_page_steps_and_jumps_ten() {
        let mut couch = Couch {
            overlay: Some(Overlay::GoTo { page: 3 }),
            ..Couch::default()
        };
        couch.press(Nav::Next, BOOK);
        assert_eq!(couch.overlay, Some(Overlay::GoTo { page: 13 }));
        couch.press(Nav::Next, BOOK);
        couch.press(Nav::Right, BOOK);
        assert_eq!(couch.overlay, Some(Overlay::GoTo { page: 14 }));
        couch.press(Nav::Previous, BOOK);
        couch.press(Nav::Left, BOOK);
        assert_eq!(couch.press(Nav::Enter, BOOK), Outcome::GoToPage(3));
    }

    #[test]
    fn b_closes_any_overlay() {
        let mut couch = Couch {
            overlay: Some(Overlay::Menu { cursor: 0 }),
            ..Couch::default()
        };
        assert_eq!(couch.press(Nav::Back, BOOK), Outcome::Closed);
        assert_eq!(couch.press(Nav::Back, BOOK), Outcome::Nothing);
    }

    #[test]
    fn the_menu_changes_values_sideways_and_runs_actions_with_enter() {
        let mut couch = Couch {
            overlay: Some(Overlay::Menu { cursor: 0 }),
            ..Couch::default()
        };
        assert_eq!(
            couch.press(Nav::Right, BOOK),
            Outcome::Menu(MenuItem::TextSize, 1)
        );
        for _ in 0..10 {
            couch.press(Nav::Down, BOOK);
        }
        assert_eq!(
            couch.press(Nav::Right, BOOK),
            Outcome::Nothing,
            "exit has no value"
        );
        assert_eq!(
            couch.press(Nav::Enter, BOOK),
            Outcome::Menu(MenuItem::Exit, 0)
        );
        assert_eq!(couch.overlay, None);
        let home = Book {
            reading: false,
            ..BOOK
        };
        assert!(!MenuItem::rows(home.reading).contains(&MenuItem::Library));
    }

    #[test]
    fn shift_tab_is_previous_and_tab_next() {
        let ctx = egui::Context::default();
        let key = |modifiers| egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let input = egui::RawInput {
            events: vec![key(egui::Modifiers::SHIFT), key(egui::Modifiers::NONE)],
            ..Default::default()
        };
        let mut taken = Vec::new();
        let mut output = ctx.run_ui(input, |ui| taken = Nav::take(ui.ctx()));
        output.textures_delta.clear();
        taken.sort_by_key(|nav| *nav as u8);
        assert_eq!(taken, [Nav::Next, Nav::Previous]);
    }

    #[test]
    fn the_shelf_and_text_sizes_stay_in_range() {
        let mut couch = Couch::default();
        couch.move_shelf(-1, 5);
        assert_eq!(couch.shelf, 0);
        couch.move_shelf(9, 5);
        assert_eq!(couch.shelf, 4);
        assert_eq!(step_text_size(1.0, 1), 1.15);
        assert_eq!(step_text_size(1.5, 1), 1.5);
        assert_eq!(step_text_size(0.8, -1), 0.8);
    }
}
