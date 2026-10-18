//! Fields (pen component `input/search`): a 28pt raised field with a
//! hairline edge, a leading icon, faint mono placeholder and a key chip or
//! toggles at the end. [`SearchButton`] is the lookalike in the tab bar that
//! opens the palette.

use egui::{Rect, Response, Sense, StrokeKind, Ui, Vec2, Widget, vec2};

use crate::{
    icons, key, text,
    theme::{self, Tokens},
};

/// A search-field lookalike that opens the command palette, with its key at
/// the right.
pub struct SearchButton<'a> {
    pub placeholder: &'a str,
    pub shortcut: &'a str,
    pub width: f32,
}

impl Widget for SearchButton<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(vec2(self.width, 28.0), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), self.placeholder)
        });
        let fill = if response.hovered() {
            t.hover
        } else {
            t.raised
        };
        ui.painter()
            .rect(rect, t.ctl_radius(), fill, t.edge(), StrokeKind::Inside);
        icons::paint(
            ui,
            Rect::from_center_size(rect.left_center() + vec2(15.0, 0.0), Vec2::splat(14.0)),
            "search",
            14.0,
            t.faint,
        );
        let key_size = key::size(ui, self.shortcut);
        let key_rect = Rect::from_min_size(
            egui::pos2(
                rect.right() - 8.0 - key_size.x,
                rect.center().y - key_size.y / 2.0,
            ),
            key_size,
        );
        if !self.shortcut.is_empty() {
            key::paint(ui, key_rect, self.shortcut, None);
        }
        let left = rect.left() + 30.0;
        text::row(
            ui,
            egui::pos2(left, rect.center().y),
            self.placeholder,
            theme::mono(12.0),
            t.faint,
            key_rect.left() - 8.0 - left,
        );
        response.on_hover_cursor(egui::CursorIcon::Text)
    }
}

/// A toggle at the end of a field (`Aa` for case, `.*` for patterns).
pub struct FieldToggle<'a> {
    pub label: &'a str,
    pub on: &'a mut bool,
    pub tooltip: &'a str,
}

/// A text field with an icon, a placeholder, and at its end a key chip or
/// toggles. The response is the text edit's.
#[must_use = "call `.show(ui)`"]
pub struct TextField<'a> {
    text: &'a mut String,
    hint: &'a str,
    icon: Option<&'a str>,
    shortcut: Option<&'a str>,
    toggles: Vec<FieldToggle<'a>>,
    width: Option<f32>,
    id: Option<egui::Id>,
}

impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String, hint: &'a str) -> Self {
        Self {
            text,
            hint,
            icon: None,
            shortcut: None,
            toggles: Vec::new(),
            width: None,
            id: None,
        }
    }

    /// A field with the search icon.
    pub fn search(text: &'a mut String, hint: &'a str) -> Self {
        Self::new(text, hint).icon("search")
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut).filter(|shortcut| !shortcut.is_empty());
        self
    }

    pub fn toggle(mut self, toggle: FieldToggle<'a>) -> Self {
        self.toggles.push(toggle);
        self
    }

    /// The field's width; the available width by default.
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let (rect, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        ui.painter().rect_filled(rect, t.ctl_radius(), t.raised);
        let mut right = rect.right() - 6.0;
        // Toggles and the key are laid out from the right.
        let font = theme::mono(10.0);
        let mut toggle_rects = Vec::new();
        for toggle in self.toggles.iter().rev() {
            let size = vec2(text::width(ui, toggle.label, &font) + 8.0, 17.0);
            let cell = Rect::from_min_size(
                egui::pos2(right - size.x, rect.center().y - size.y / 2.0),
                size,
            );
            right = cell.left() - 3.0;
            toggle_rects.push(cell);
        }
        toggle_rects.reverse();
        if let Some(shortcut) = self.shortcut {
            let size = key::size(ui, shortcut);
            let cell = Rect::from_min_size(
                egui::pos2(right - size.x - 2.0, rect.center().y - size.y / 2.0),
                size,
            );
            key::paint(ui, cell, shortcut, None);
            right = cell.left() - 6.0;
        }
        let left = rect.left() + if self.icon.is_some() { 30.0 } else { 8.0 };
        let edit_rect = Rect::from_min_max(
            egui::pos2(left, rect.top() + 1.0),
            egui::pos2(right, rect.bottom() - 1.0),
        );
        let mut edit = egui::TextEdit::singleline(self.text)
            .hint_text(egui::RichText::new(self.hint).color(t.faint))
            .font(theme::mono(12.0))
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(edit_rect.width());
        if let Some(id) = self.id {
            edit = edit.id(id);
        }
        let response = ui.put(edit_rect, edit);
        let focused = response.has_focus();
        let stroke = if focused {
            egui::Stroke::new(1.0, t.accent)
        } else {
            t.edge()
        };
        ui.painter()
            .rect_stroke(rect, t.ctl_radius(), stroke, StrokeKind::Inside);
        if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(rect.left_center() + vec2(15.0, 0.0), Vec2::splat(14.0)),
                icon,
                14.0,
                if focused { t.muted } else { t.faint },
            );
        }
        for (toggle, cell) in self.toggles.into_iter().zip(toggle_rects) {
            let hit = ui.interact(cell, response.id.with(toggle.label), Sense::click());
            let hit = crate::overlay::tooltip(hit, toggle.tooltip, None);
            if hit.clicked() {
                *toggle.on = !*toggle.on;
            }
            let (fill, color) = if *toggle.on {
                (t.accent_soft, t.accent_text)
            } else if hit.hovered() {
                (t.hover, t.text)
            } else {
                (egui::Color32::TRANSPARENT, t.muted)
            };
            ui.painter()
                .rect(cell, t.ctl_radius(), fill, t.edge(), StrokeKind::Inside);
            ui.painter().text(
                cell.center(),
                egui::Align2::CENTER_CENTER,
                toggle.label,
                font.clone(),
                color,
            );
        }
        response
    }
}
