//! Rows (pen components `row/list`, `row/list-active` and `row/tree`): one
//! line in a pane, an icon, a title and faint meta at the right; the active
//! row sits on `accent-soft` with accent icon and meta. Tree rows draw the
//! outline's `├─` branches before the title.

use egui::{Align2, Rect, Response, Sense, Ui, Vec2, vec2};

use crate::{
    icons, motif, text,
    theme::{self, Tokens},
};

/// A row that fills the width, on the hover fill when hovered and on
/// `accent-soft` when `selected`, for rows a caller draws itself.
/// Returns its rect and response.
pub fn list_row(ui: &mut Ui, height: f32, selected: bool) -> (Rect, Response) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 0, t.accent_soft);
    } else if response.contains_pointer() {
        ui.painter().rect_filled(rect, 0, t.hover);
    }
    (rect, response)
}

/// A list row the width of the pane.
#[must_use = "call `.show(ui)`"]
pub struct ListRow<'a> {
    title: &'a str,
    icon: Option<&'a str>,
    meta: Option<&'a str>,
    key: Option<&'a str>,
    active: bool,
    height: f32,
    indent: f32,
    title_color: Option<egui::Color32>,
}

impl<'a> ListRow<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            icon: None,
            meta: None,
            key: None,
            active: false,
            height: 28.0,
            indent: 12.0,
            title_color: None,
        }
    }

    pub fn icon(mut self, icon: impl Into<Option<&'a str>>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn meta(mut self, meta: impl Into<Option<&'a str>>) -> Self {
        self.meta = meta.into().filter(|meta| !meta.is_empty());
        self
    }

    /// A key chip at the right, after the meta (menus, the palette).
    pub fn key(mut self, key: impl Into<Option<&'a str>>) -> Self {
        self.key = key.into().filter(|key| !key.is_empty());
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// The left padding (12 by default).
    pub fn indent(mut self, indent: f32) -> Self {
        self.indent = indent;
        self
    }

    pub fn title_color(mut self, color: egui::Color32) -> Self {
        self.title_color = Some(color);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), t.row(self.height)),
            Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                self.active,
                self.title,
            )
        });
        self.paint(ui, rect, &response);
        response
    }

    /// Paints the row in `rect` for `response` (for rows laid out by the
    /// caller, as in virtual lists).
    pub fn paint(&self, ui: &Ui, rect: Rect, response: &Response) {
        let t = Tokens::get(ui.ctx());
        let hovered = response.contains_pointer();
        if self.active {
            ui.painter().rect_filled(rect, 0, t.accent_soft);
        } else if hovered {
            ui.painter().rect_filled(rect, 0, t.hover);
        }
        let (icon_color, title_color, meta_color) = if self.active {
            (t.accent_text, t.text, t.accent_text)
        } else if hovered {
            (t.muted, t.text, t.muted)
        } else {
            (t.faint, t.text_2, t.faint)
        };
        let y = rect.center().y;
        let mut x = rect.left() + self.indent;
        if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 6.5, y), Vec2::splat(13.0)),
                icon,
                13.0,
                icon_color,
            );
            x += 21.0;
        }
        let mut right = rect.right() - 12.0;
        if let Some(key) = self.key {
            let size = crate::key::size(ui, key);
            let cell = Rect::from_min_size(egui::pos2(right - size.x, y - size.y / 2.0), size);
            crate::key::paint(ui, cell, key, None);
            right = cell.left() - 8.0;
        }
        if let Some(meta) = self.meta {
            let meta_rect = text::line(
                ui,
                egui::pos2(right, y),
                Align2::RIGHT_CENTER,
                meta,
                theme::mono(11.0),
                meta_color,
                (right - x) * 0.45,
            );
            right = meta_rect.left() - 8.0;
        }
        text::row(
            ui,
            egui::pos2(x, y),
            self.title,
            theme::mono(12.0),
            self.title_color.unwrap_or(title_color),
            right - x,
        );
    }
}

/// An outline row: tree branches, a title and the page at the right.
#[must_use = "call `.show(ui)`"]
pub struct TreeRow<'a> {
    pub title: &'a str,
    pub page: Option<&'a str>,
    /// Whether each ancestor was the last of its siblings (see
    /// [`motif::tree_shape`]).
    pub ancestors_last: &'a [bool],
    pub last: bool,
    pub active: bool,
}

impl TreeRow<'_> {
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), t.row(26.0)), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                ui.is_enabled(),
                self.active,
                self.title,
            )
        });
        let hovered = response.contains_pointer();
        if self.active {
            ui.painter().rect_filled(rect, 0, t.accent_soft);
        } else if hovered {
            ui.painter().rect_filled(rect, 0, t.hover);
        }
        let font = theme::mono(12.0);
        let y = rect.center().y;
        let mut x = rect.left() + 12.0;
        let depth = self.ancestors_last.len();
        if t.ornaments {
            let prefix = motif::tree_prefix(self.ancestors_last, self.last);
            let galley = ui.painter().layout_no_wrap(prefix, font.clone(), t.faint);
            let width = galley.size().x;
            ui.painter()
                .galley(egui::pos2(x, y - galley.size().y / 2.0), galley, t.faint);
            x += width;
        } else {
            x += depth as f32 * 14.0;
        }
        let mut right = rect.right() - 12.0;
        if let Some(page) = self.page {
            let meta = text::line(
                ui,
                egui::pos2(right, y),
                Align2::RIGHT_CENTER,
                page,
                theme::mono(11.0),
                if self.active { t.accent_text } else { t.faint },
                48.0,
            );
            right = meta.left() - 8.0;
        }
        let color = if self.active || hovered {
            t.text
        } else {
            t.text_2
        };
        text::row(ui, egui::pos2(x, y), self.title, font, color, right - x);
        response
    }
}

/// A key-value line of the document card under the outline
/// (`title  Attention Is All You Need`).
pub fn field_line(ui: &mut Ui, label: &str, value: &str, label_width: f32) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 17.0), Sense::hover());
    let y = rect.center().y;
    text::row(
        ui,
        egui::pos2(rect.left(), y),
        label,
        theme::mono(11.0),
        t.faint,
        label_width - 8.0,
    );
    text::row(
        ui,
        egui::pos2(rect.left() + label_width, y),
        value,
        theme::mono(11.0),
        t.text_2,
        rect.width() - label_width,
    );
}
