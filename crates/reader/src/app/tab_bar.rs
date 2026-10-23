//! The 36pt tab bar (frame 02a, pen components `tab` and `tab/active`):
//! the `g▌` mark (home, on `surface` while home shows), the document
//! tabs, the page tabs (shortcuts, new tabs, reading activity), `+` for a
//! new tab, and at the right the palette's search field, theme, settings
//! and the menu. Tabs are laid out by `glyphst_ui::craft::tabs` and painted by
//! `glyphst_ui::tab::DocTab`.

use egui::{Rect, Sense, pos2, vec2};
use glyphst_ui::{
    Tip,
    field::SearchButton,
    key, menu as ui_menu,
    tab::{self as ui_tab, DocTab},
};

use super::*;
use crate::translation::SessionStatus;

/// Width of the search field when the bar has room for it.
const SEARCH_WIDTH: f32 = 300.0;
/// The bar shows the search field from this width.
const SEARCH_FROM: f32 = 900.0;

/// A tab in the bar: a document or a page.
#[derive(Clone, Copy, PartialEq)]
enum BarTab {
    Document(usize),
    Page(usize),
}

impl ReaderApp {
    /// The tab bar.
    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let tokens = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let bar = ui.max_rect();
        ui.painter().rect_filled(bar, 0, tokens.bg);
        ui.painter()
            .hline(bar.x_range(), bar.bottom() - 0.5, tokens.hairline());
        let titles: Vec<String> = self
            .tabs
            .iter()
            .map(|tab| {
                let name = tab
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| tab.title.clone());
                truncate(&name, TAB_MAX_LABEL)
            })
            .collect();
        // Plain copies, so the tabs can be changed while the bar draws.
        let looks: Vec<(&str, bool)> = self
            .tabs
            .iter()
            .map(|tab| {
                let icon = if tab.is_reflowed() {
                    "book-open"
                } else {
                    "file-text"
                };
                // A translation running marks the tab, as unsaved work does.
                let busy = tab
                    .translation
                    .as_ref()
                    .is_some_and(|session| matches!(session.status, SessionStatus::Running));
                (icon, busy)
            })
            .collect();
        let ids: Vec<u64> = self.tabs.iter().map(|tab| tab.id).collect();
        let full_titles: Vec<String> = self.tabs.iter().map(|tab| tab.title.clone()).collect();
        let doc_tab = |index: usize, active: bool| DocTab {
            title: &titles[index],
            icon: Some(looks[index].0),
            active,
            dirty: looks[index].1,
            closable: true,
        };
        let natural: Vec<f32> = (0..self.tabs.len())
            .map(|index| doc_tab(index, self.active == Some(index)).natural_width(ui))
            .collect();
        let home_active =
            self.active.is_none() && self.page_focused.is_none() && self.gallery.is_none();
        let pages: Vec<DocTab> = self
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| DocTab {
                title: page.title(),
                icon: Some(page.icon()),
                active: self.page_focused == Some(index) && self.active.is_none(),
                dirty: false,
                closable: true,
            })
            .collect();
        let search_width = if bar.width() > SEARCH_FROM {
            SEARCH_WIDTH
        } else {
            28.0
        };
        let page_widths: Vec<f32> = pages.iter().map(|page| page.natural_width(ui)).collect();
        let controls = ui_tab::BAR_HEIGHT // logo
            + 36.0 // +
            + search_width
            + 3.0 * 30.0
            + 24.0
            + page_widths.iter().sum::<f32>();
        let fit = glyphst_ui::craft::tabs::fit(
            &natural,
            self.active.unwrap_or(0),
            (bar.width() - controls).max(0.0),
            TAB_MIN_WIDTH,
            TAB_OVERFLOW_WIDTH,
        );
        let mut select = None;
        let mut close = None;
        let mut run = None;
        let mut tab_menu = None;
        let mut drag = None;
        let (mut open_page, mut close_page) = (None, None);
        let mut x = bar.left();
        // The mark; it goes home.
        {
            let rect = Rect::from_min_size(pos2(x, bar.top()), vec2(40.0, bar.height()));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            if ui_tab::logo(&mut child, home_active).tip("home").clicked() {
                select = Some(None);
            }
            x = rect.right();
        }
        let cell = |ui: &mut egui::Ui, which: BarTab, tab: &DocTab, width: f32, x: &mut f32| {
            let rect = Rect::from_min_size(pos2(*x, bar.top()), vec2(width, bar.height()));
            *x = rect.right();
            let id = ui.scope_id().with(match which {
                BarTab::Page(index) => ("bar-page", index as u64),
                BarTab::Document(index) => ("bar-tab", ids[index]),
            });
            let response = glyphst_ui::craft::tabs::Tab::new(id, tab.title, tab.active)
                .sense(Sense::click_and_drag())
                .focus_stroke(egui::Stroke::new(1.0, tokens.accent))
                .show_at(ui, rect, |_, _| {});
            let close_rect = tab.close_rect(rect);
            let close_response = close_rect
                .map(|close_rect| ui.interact(close_rect, id.with("close"), Sense::click()));
            let close_hovered = close_response.as_ref().is_some_and(|close| close.hovered());
            tab.paint(ui, rect, &response, close_hovered);
            let closed = close_response.is_some_and(|close| close.tip("close").clicked());
            (response, closed)
        };
        for &(index, width) in &fit.shown {
            let tab = doc_tab(index, self.active == Some(index));
            let (response, closed) = cell(ui, BarTab::Document(index), &tab, width, &mut x);
            let response = response.tip(&full_titles[index]);
            if closed || response.middle_clicked() {
                close = Some(index);
            } else if response.clicked() {
                select = Some(Some(index));
            }
            // Dragged onto the pages, it opens beside a pane.
            if response.drag_started() {
                drag = Some(ids[index]);
            }
            if response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            let shown = self.panes.contains(ids[index]);
            let count = ids.len();
            response.context_menu(|ui| {
                ui_menu::style(ui);
                if let Some(action) = tab_menu_entries(ui, index, count, shown) {
                    tab_menu = Some((index, action));
                    ui.close();
                }
            });
        }
        for (index, (page, &width)) in pages.iter().zip(&page_widths).enumerate() {
            let (response, closed) = cell(ui, BarTab::Page(index), page, width, &mut x);
            if closed || response.middle_clicked() {
                close_page = Some(index);
            } else if response.clicked() {
                open_page = Some(index);
            }
        }
        let rest = Rect::from_min_max(pos2(x, bar.top()), bar.max);
        let mut rest_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rest.shrink2(vec2(4.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut rest_ui;
        ui.spacing_mut().item_spacing.x = 2.0;
        if !fit.overflow.is_empty() {
            let response =
                icons::button(ui, "chevron-down", TAB_OVERFLOW_WIDTH, false, "more tabs");
            egui::Popup::menu(&response).show(|ui| {
                ui_menu::style(ui);
                for &index in &fit.overflow {
                    if ui_menu::Item::new(&titles[index])
                        .icon("file-text")
                        .show(ui)
                        .clicked()
                    {
                        select = Some(Some(index));
                    }
                }
            });
        }
        let tip = self.keymap.tip("new tab", Command::NewTab);
        if icons::button(ui, "plus", 28.0, false, &tip).clicked() {
            run = Some(Command::NewTab);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            let response = icons::button(ui, "ellipsis", 28.0, false, "menu");
            egui::Popup::menu(&response).show(|ui| {
                ui_menu::style(ui);
                self.menu(ui)
            });
            let tip = self.keymap.tip("settings", Command::Settings);
            if icons::button(ui, "settings", 28.0, self.settings_window.open, &tip).clicked() {
                run = Some(Command::Settings);
            }
            let response = icons::button(
                ui,
                "contrast",
                28.0,
                false,
                &format!("theme: {}", self.prefs.theme.label()),
            );
            egui::Popup::menu(&response).show(|ui| {
                ui_menu::style(ui);
                for (command, preference, icon) in [
                    (Command::ThemeSystem, ThemePreference::System, "monitor"),
                    (Command::ThemeLight, ThemePreference::Light, "sun"),
                    (Command::ThemeDark, ThemePreference::Dark, "moon"),
                ] {
                    if ui_menu::Item::new(preference.label())
                        .icon(icon)
                        .checked(self.prefs.theme == preference)
                        .show(ui)
                        .clicked()
                    {
                        run = Some(command);
                    }
                }
            });
            ui.add_space(6.0);
            let palette_key = self
                .keymap
                .label(Command::Palette)
                .map(|label| key::display(&label))
                .unwrap_or_default();
            if search_width > 28.0 {
                if ui
                    .add(SearchButton {
                        placeholder: "search · run · open",
                        shortcut: &palette_key,
                        width: search_width,
                    })
                    .clicked()
                {
                    run = Some(Command::Palette);
                }
            } else {
                let tip = self.keymap.tip("command palette", Command::Palette);
                if icons::button(ui, "search", 28.0, self.palette.open, &tip).clicked() {
                    run = Some(Command::Palette);
                }
            }
        });
        if let Some(id) = drag {
            self.chrome.tab_drag = Some(id);
        }
        if let Some(command) = run {
            self.run_or_notify(command, &ctx);
        }
        if let Some(index) = close_page {
            self.close_page(index);
        } else if let Some(index) = open_page {
            self.focus_page(index);
        }
        if let Some((index, action)) = tab_menu {
            self.tab_menu_action(index, action, &ctx);
        } else if let Some(index) = close {
            self.close(index);
        } else if let Some(index) = select {
            self.gallery = None;
            self.activate(index);
        }
    }
}
