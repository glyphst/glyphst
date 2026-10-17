//! Gamepad mode in the app: turning it on and off, its home in place of
//! the docked one, and its sheets over the book (see `crate::couch`).

use glyphst_ui::theme::{ThemePreference, Tokens};

use super::ReaderApp;
use crate::{
    couch::{
        self, Book, MenuItem, Nav, Outcome, Overlay,
        home::{HomeAction, HomeView},
        reader::ReaderView,
    },
    document_tab::ViewAction,
    pad::Power,
};

/// How many recent books home's shelf holds.
const SHELF: usize = 24;

impl ReaderApp {
    /// Turns gamepad mode on or off: fullscreen on the way in, the window
    /// as it was on the way out.
    pub(crate) fn set_gamepad_mode(&mut self, on: bool, ctx: &egui::Context) {
        if self.prefs.gamepad_mode == on {
            return;
        }
        self.prefs.gamepad_mode = on;
        self.couch.overlay = None;
        if self.persist {
            if on {
                self.couch.saved_fullscreen = Some(self.fullscreen);
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            } else if let Some(was) = self.couch.saved_fullscreen.take() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(was));
            }
        }
        self.rebuild_keymap();
    }

    /// The chrome is hidden: zen mode, or gamepad mode.
    pub(crate) fn bare(&self) -> bool {
        self.zen || self.prefs.gamepad_mode
    }

    fn book(&self) -> Book {
        match self.active {
            Some(active) => {
                let tab = &self.tabs[active];
                Book {
                    entries: tab.outline.len(),
                    pages: tab.layout.len(),
                    reading: true,
                }
            }
            None => Book {
                entries: 0,
                pages: 0,
                reading: false,
            },
        }
    }

    /// The entry being read, for the contents' focus.
    fn current_entry(&self) -> Option<usize> {
        let tab = &self.tabs[self.active?];
        let page = tab.current_page();
        tab.outline
            .iter()
            .rposition(|entry| entry.page.is_some_and(|start| start <= page))
    }

    /// Opens a sheet: the contents on the entry being read, the go-to
    /// sheet on the page being read, or the menu.
    pub(crate) fn open_couch(&mut self, sheet: Overlay) {
        self.couch.overlay = Some(match sheet {
            Overlay::Contents { .. } => Overlay::Contents {
                cursor: self.current_entry().unwrap_or(0),
            },
            Overlay::GoTo { .. } => Overlay::GoTo {
                page: self
                    .active
                    .map_or(0, |active| self.tabs[active].current_page()),
            },
            menu @ Overlay::Menu { .. } => menu,
        });
    }

    /// The sheets take the arrows, Enter, Escape and Tab before anything
    /// else reads them.
    pub(super) fn couch_input(&mut self, ctx: &egui::Context) {
        if self.couch.overlay.is_none() {
            return;
        }
        let book = self.book();
        for nav in Nav::take(ctx) {
            match self.couch.press(nav, book) {
                Outcome::GoToEntry(entry) => {
                    self.pending_actions.push(ViewAction::GoToEntry(entry))
                }
                Outcome::GoToPage(page) => self.pending_actions.push(ViewAction::GoToPage(page)),
                Outcome::Menu(item, step) => self.couch_menu(item, step, ctx),
                Outcome::Nothing | Outcome::Closed => {}
            }
        }
    }

    fn couch_menu(&mut self, item: MenuItem, step: i32, ctx: &egui::Context) {
        match item {
            MenuItem::TextSize => {
                self.prefs.couch_text = couch::step_text_size(self.prefs.couch_text, step);
            }
            MenuItem::Theme => {
                let all = ThemePreference::ALL;
                let at = all.iter().position(|t| *t == self.prefs.theme).unwrap_or(0) as i32;
                let next = (at + step).rem_euclid(all.len() as i32) as usize;
                self.prefs.theme = all[next];
            }
            MenuItem::Fit => {
                self.couch.fit_page = !self.couch.fit_page;
                self.pending_actions.push(if self.couch.fit_page {
                    ViewAction::FitPage
                } else {
                    ViewAction::FitMargins
                });
            }
            MenuItem::Smooth => self.prefs.smooth_scrolling = !self.prefs.smooth_scrolling,
            MenuItem::Library => self.activate(None),
            MenuItem::Exit => self.set_gamepad_mode(false, ctx),
        }
    }

    fn clock(&self) -> String {
        if self.fixed_clock {
            "21:30".to_string()
        } else {
            chrono::Local::now().format("%H:%M").to_string()
        }
    }

    /// Gamepad mode's home, in place of the docked one.
    pub(super) fn couch_home(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut docs = crate::home::documents(&self.library, &self.prefs.page_counts);
        docs.truncate(SHELF);
        let pads = self.pads.connected();
        let battery = pads.iter().find_map(|pad| match pad.power {
            Power::Battery { percent, .. } => Some(percent),
            _ => None,
        });
        let clock = self.clock();
        let view = HomeView {
            docs: &docs,
            covers: &mut self.covers,
            clock,
            battery,
            pad: !pads.is_empty(),
            input: !self.prompts.is_open()
                && !self.settings_window.open
                && !self.palette.open
                && self.couch.overlay.is_none(),
        };
        let action = couch::home::show(ui, &mut self.couch, view);
        match action {
            Some(HomeAction::Open(path)) => self.open(&path, &ctx, None),
            Some(HomeAction::Contents(path)) => {
                self.open(&path, &ctx, None);
                self.couch.overlay = Some(Overlay::Contents { cursor: 0 });
            }
            Some(HomeAction::Menu) => self.couch.overlay = Some(Overlay::Menu { cursor: 0 }),
            None => {}
        }
    }

    /// Gamepad mode's chips, hint bar and sheets over the book; the go-to
    /// sheet outside it too.
    pub(super) fn couch_overlay(&mut self, ctx: &egui::Context) {
        let gamepad = self.prefs.gamepad_mode;
        if !gamepad && self.couch.overlay.is_none() {
            return;
        }
        let theme = match self.prefs.theme {
            ThemePreference::System => "system",
            ThemePreference::Light => "light",
            ThemePreference::Dark => "dark",
        };
        let Some(active) = self.active else {
            // Home's menu.
            if let Some(Overlay::Menu { cursor }) = self.couch.overlay {
                let view = ReaderView {
                    title: "Library",
                    section: String::new(),
                    page: 0,
                    pages: 0,
                    outline: &[],
                    current: None,
                    overview: false,
                    text_size: self.prefs.couch_text,
                    theme,
                    smooth: self.prefs.smooth_scrolling,
                    fit_page: self.couch.fit_page,
                    couch: true,
                };
                let screen = ctx.content_rect();
                egui::Area::new(egui::Id::new("couch-home-menu"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(screen.min)
                    .interactable(false)
                    .show(ctx, |ui| {
                        let t = Tokens::get(ctx);
                        ui.painter().rect_filled(screen, 0, t.scrim);
                        couch::reader::menu(ui, screen, &view, cursor, false, &t);
                    });
            }
            return;
        };
        let current = self.current_entry();
        let tab = &self.tabs[active];
        let page = tab.current_page();
        let section = crate::shell::section_path(
            tab.outline
                .iter()
                .map(|entry| (entry.depth, entry.page, entry.title.as_str())),
            page,
        )
        .last()
        .map(|title| (*title).to_string())
        .unwrap_or_default();
        let title = tab.title.clone();
        let view = ReaderView {
            title: &title,
            section,
            page,
            pages: tab.layout.len(),
            outline: &tab.outline,
            current,
            overview: tab.overview.is_some(),
            text_size: self.prefs.couch_text,
            theme,
            smooth: self.prefs.smooth_scrolling,
            fit_page: self.couch.fit_page,
            couch: gamepad,
        };
        couch::reader::show(ctx, &self.couch, &view);
    }
}
