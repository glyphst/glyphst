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
/// Back and forward after the mark.
const NAV_WIDTH: f32 = 64.0;

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
        let pinned_flags: Vec<bool> = self.tabs.iter().map(|tab| tab.pinned).collect();
        let doc_tab = |index: usize, active: bool| DocTab {
            title: &titles[index],
            icon: Some(looks[index].0),
            active,
            dirty: looks[index].1,
            closable: true,
            pinned: pinned_flags[index],
        };
        // Pinned tabs keep their place at the left; the others share the
        // rest of the bar.
        let pinned = self.pinned_count();
        let natural: Vec<f32> = (pinned..self.tabs.len())
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
                pinned: false,
            })
            .collect();
        let search_width = if bar.width() > SEARCH_FROM {
            SEARCH_WIDTH
        } else {
            28.0
        };
        let page_widths: Vec<f32> = pages.iter().map(|page| page.natural_width(ui)).collect();
        let controls = ui_tab::BAR_HEIGHT // logo
            + NAV_WIDTH
            + 36.0 // +
            + search_width
            + 3.0 * 30.0
            + 24.0
            + page_widths.iter().sum::<f32>()
            + pinned as f32 * ui_tab::PINNED_WIDTH;
        let fit = glyphst_ui::craft::tabs::fit(
            &natural,
            self.active
                .and_then(|active| active.checked_sub(pinned))
                .unwrap_or(0),
            (bar.width() - controls).max(0.0),
            TAB_MIN_WIDTH,
            TAB_OVERFLOW_WIDTH,
        );
        let shown: Vec<(usize, f32)> = (0..pinned)
            .map(|index| (index, ui_tab::PINNED_WIDTH))
            .chain(
                fit.shown
                    .iter()
                    .map(|&(index, width)| (index + pinned, width)),
            )
            .collect();
        let overflow: Vec<usize> = fit.overflow.iter().map(|index| index + pinned).collect();
        let mut select = None;
        let mut close = None;
        let mut run = None;
        let mut tab_menu = None;
        let mut page_menu = None;
        let mut drag = None;
        // A tab dragged along the bar: where it is, and where it would land.
        let mut reorder = None;
        let mut page_reorder = None;
        let pointer = ctx.pointer_latest_pos();
        // Within this of the bar a drag reorders; farther, it is over the
        // pages (and opens beside a pane).
        let on_bar = |pos: Option<egui::Pos2>| {
            pos.is_some_and(|pos| bar.expand2(vec2(0.0, 24.0)).contains(pos))
        };
        let (mut doc_rects, mut page_rects) = (Vec::new(), Vec::new());
        let mut lifted = None;
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
        // Back and forward through everywhere the user has been.
        {
            let rect = Rect::from_min_size(pos2(x, bar.top()), vec2(NAV_WIDTH, bar.height()));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink2(vec2(4.0, 0.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.spacing_mut().item_spacing.x = 0.0;
            let steps = [
                (Command::Back, "arrow-left", "back", self.nav.can_go_back()),
                (
                    Command::Forward,
                    "arrow-right",
                    "forward",
                    self.nav.can_go_forward(),
                ),
            ];
            for (command, icon, label, can) in steps {
                let tip = self.keymap.tip(label, command);
                let clicked = child
                    .add_enabled_ui(can, |ui| icons::button(ui, icon, 28.0, false, &tip))
                    .inner
                    .clicked();
                if clicked {
                    run = Some(command);
                }
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
        let can_reopen = self.can_reopen_tab();
        for &(index, width) in &shown {
            let tab = doc_tab(index, self.active == Some(index));
            let (response, closed) = cell(ui, BarTab::Document(index), &tab, width, &mut x);
            doc_rects.push((index, response.rect));
            let response = response.tip(&full_titles[index]);
            if closed || response.middle_clicked() {
                close = Some(index);
            } else if response.clicked() {
                select = Some(Some(index));
            }
            // Dragged along the bar it moves; onto the pages, it opens
            // beside a pane.
            if response.drag_started() {
                drag = Some(ids[index]);
            }
            if response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                if on_bar(pointer) {
                    lifted = Some((BarTab::Document(index), response.clone()));
                }
            }
            if response.drag_stopped()
                && on_bar(pointer)
                && let Some(pointer) = pointer
            {
                reorder = Some((index, pointer.x));
            }
            let facts = TabMenuFacts {
                count: ids.len(),
                pinned: pinned_flags[index],
                others: tabs_to_close(&pinned_flags, index, TabMenu::CloseOthers).len(),
                left: tabs_to_close(&pinned_flags, index, TabMenu::CloseToTheLeft).len(),
                right: tabs_to_close(&pinned_flags, index, TabMenu::CloseToTheRight).len(),
                shown: self.panes.contains(ids[index]),
                can_reopen,
            };
            let keymap = &self.keymap;
            response.context_menu(|ui| {
                ui_menu::style(ui);
                if let Some(action) = tab_menu_entries(ui, facts, keymap) {
                    tab_menu = Some((index, action));
                    ui.close();
                }
            });
        }
        for (index, (page, &width)) in pages.iter().zip(&page_widths).enumerate() {
            let (response, closed) = cell(ui, BarTab::Page(index), page, width, &mut x);
            page_rects.push((index, response.rect));
            if closed || response.middle_clicked() {
                close_page = Some(index);
            } else if response.clicked() {
                open_page = Some(index);
            }
            if response.dragged() && on_bar(pointer) {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                lifted = Some((BarTab::Page(index), response.clone()));
            }
            if response.drag_stopped()
                && on_bar(pointer)
                && let Some(pointer) = pointer
            {
                page_reorder = Some((index, pointer.x));
            }
            let count = pages.len();
            response.context_menu(|ui| {
                ui_menu::style(ui);
                if ui.add(ui_menu::Item::new("Close").icon("x")).clicked() {
                    page_menu = Some((index, false));
                    ui.close();
                }
                if ui
                    .add_enabled(count > 1, ui_menu::Item::new("Close other tabs").icon("x"))
                    .clicked()
                {
                    page_menu = Some((index, true));
                    ui.close();
                }
            });
        }
        // The tab being moved lifts off the bar and follows the pointer; a
        // line marks where it lands, and its own place fades (frame 02l).
        if let Some((which, response)) = &lifted
            && let Some(pointer) = pointer
        {
            let (rects, tab) = match *which {
                BarTab::Document(index) => (&doc_rects, doc_tab(index, self.active == Some(index))),
                BarTab::Page(index) => (
                    &page_rects,
                    DocTab {
                        active: false,
                        ..pages[index]
                    },
                ),
            };
            let index = match *which {
                BarTab::Document(index) | BarTab::Page(index) => index,
            };
            let rects: Vec<(usize, Rect)> = match *which {
                BarTab::Document(_) => rects
                    .iter()
                    .copied()
                    .filter(|&(other, _)| pinned_flags[other] == pinned_flags[index])
                    .collect(),
                BarTab::Page(_) => rects.clone(),
            };
            let rect = response.rect;
            ui.painter()
                .rect_filled(rect, 0, tokens.bg.gamma_multiply(0.65));
            let slot = drop_slot(&rects, pointer.x);
            let line_x = rects.iter().find(|&&(other, _)| other == slot).map_or_else(
                || rects.last().map_or(rect.right(), |(_, last)| last.right()),
                |(_, other)| other.left(),
            );
            let line = Rect::from_center_size(pos2(line_x, bar.center().y), vec2(2.0, 24.0));
            let origin = ctx.input(|i| i.pointer.press_origin());
            let offset = pointer.x - origin.map_or(pointer.x, |origin| origin.x);
            let ghost = rect.translate(vec2(offset, 0.0)).shrink2(vec2(0.0, 6.0));
            egui::Area::new(egui::Id::unique("tab-lifted"))
                .order(egui::Order::Tooltip)
                .fixed_pos(ghost.min)
                .interactable(false)
                .show(&ctx, |ui| {
                    let (ghost, _) = ui.allocate_exact_size(ghost.size(), Sense::hover());
                    ui.painter().add(
                        egui::epaint::Shadow {
                            offset: [0, 4],
                            blur: 12,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(100),
                        }
                        .as_shape(ghost, tokens.ctl_radius()),
                    );
                    ui.painter().rect(
                        ghost,
                        tokens.ctl_radius(),
                        tokens.raised,
                        egui::Stroke::new(1.0, tokens.rule_strong),
                        egui::StrokeKind::Inside,
                    );
                    let tab = DocTab {
                        closable: false,
                        ..tab
                    };
                    tab.paint(ui, ghost, response, false);
                    // Over the lifted tab, so it shows where the tab lands
                    // even under it.
                    ui.ctx()
                        .layer_painter(ui.layer_id())
                        .rect_filled(line, 1.0, tokens.accent);
                });
        }
        let rest = Rect::from_min_max(pos2(x, bar.top()), bar.max);
        let mut rest_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rest.shrink2(vec2(4.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let ui = &mut rest_ui;
        ui.spacing_mut().item_spacing.x = 2.0;
        if !overflow.is_empty() {
            let response =
                icons::button(ui, "chevron-down", TAB_OVERFLOW_WIDTH, false, "more tabs");
            egui::Popup::menu(&response).show(|ui| {
                ui_menu::style(ui);
                for &index in &overflow {
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
        self.chrome.tab_rects = doc_rects.clone();
        if let Some(command) = run {
            self.run_or_notify(command, &ctx);
        }
        if let Some(index) = close_page {
            self.close_page(index);
        } else if let Some(index) = open_page {
            self.focus_page(index);
        }
        if let Some((from, x)) = reorder {
            let rects: Vec<(usize, Rect)> = doc_rects
                .iter()
                .copied()
                .filter(|&(other, _)| pinned_flags[other] == pinned_flags[from])
                .collect();
            self.move_tab(from, drop_slot(&rects, x));
        }
        if let Some((from, x)) = page_reorder {
            self.move_page(from, drop_slot(&page_rects, x));
        }
        if let Some((index, others)) = page_menu {
            if others {
                for other in (0..self.pages.len()).rev().filter(|&other| other != index) {
                    self.close_page(other);
                }
            } else {
                self.close_page(index);
            }
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

/// The slot a tab dropped at `x` goes before: the first of `placed` (each
/// tab's index and rect, in order) whose middle is past `x`, else after
/// the last.
fn drop_slot(placed: &[(usize, Rect)], x: f32) -> usize {
    match placed.iter().find(|(_, rect)| rect.center().x >= x) {
        Some(&(index, _)) => index,
        None => placed.last().map_or(0, |&(index, _)| index + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_goes_before_the_first_tab_past_its_middle() {
        let tab = |index: usize| {
            let left = index as f32 * 100.0;
            (
                index,
                Rect::from_min_max(pos2(left, 0.0), pos2(left + 100.0, 36.0)),
            )
        };
        let placed = [tab(2), tab(3), tab(4)];
        assert_eq!(drop_slot(&placed, 120.0), 2, "before the first");
        assert_eq!(drop_slot(&placed, 260.0), 3);
        assert_eq!(drop_slot(&placed, 380.0), 4);
        assert_eq!(drop_slot(&placed, 460.0), 5, "after the last");
        assert_eq!(drop_slot(&[], 10.0), 0);
    }
}
