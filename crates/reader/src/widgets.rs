//! Small widgets on the design tokens, after pdfcraft's `widgets.rs`: pill,
//! ghost and search-box buttons, section titles, key chips, segmented
//! choices, switches, settings groups and rows, card frames and toasts.

use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, vec2};

use crate::{
    icons,
    theme::{self, Tokens},
};

/// Icons the app names directly rather than through a command, section or
/// sidebar tab; a test checks that each exists.
#[cfg(test)]
pub const ICONS_IN_USE: &[&str] = &[
    "house",
    "monitor",
    "sun",
    "moon",
    "keyboard",
    "arrow-right",
    "file-text",
    "book-open",
    "book-open-text",
    "plus",
    "x",
    "chevron-down",
    "chevron-left",
    "chevron-right",
    "ellipsis",
    "search",
    "send",
    "message-square-plus",
    "clock",
    "copy",
    "trash-2",
    "check",
    "eye",
    "eye-off",
    "rotate-ccw",
    "refresh-cw",
    "download",
    "file-down",
    "pin",
    "triangle-alert",
    "info",
    "gamepad-2",
    "library",
    "pen-line",
    "highlighter",
    "sticky-note",
    "arrow-up",
    "arrow-down",
    "log-in",
    "log-out",
    "columns-2",
    "wand-sparkles",
    "image",
    "folder-open",
    "file-plus",
    "at-sign",
    "paperclip",
    "brain",
    "maximize-2",
    "minimize-2",
    "pencil",
    "circle-stop",
    "list-todo",
    "bot",
    "terminal",
    "search-check",
    "corner-down-left",
    "text-cursor",
    "undo-2",
    "redo-2",
    "circle-alert",
    "wrench",
    "chevrons-up-down",
    "file-search",
];

fn text_width(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
        .size()
        .x
}

/// A rounded pill; `primary` fills it with the accent. With an icon, the
/// icon comes first.
pub fn pill_button(ui: &mut egui::Ui, icon: Option<&str>, label: &str, primary: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(12.5);
    let icon_room = if icon.is_some() { 22.0 } else { 0.0 };
    let width = text_width(ui, label, &font) + 26.0 + icon_room;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let enabled = ui.is_enabled();
    let (fill, stroke, text) = if primary {
        let fill = if response.hovered() && enabled {
            t.accent_text
        } else {
            t.accent
        };
        (fill, Stroke::NONE, Color32::WHITE)
    } else {
        let fill = if response.hovered() && enabled {
            t.hover
        } else {
            t.card
        };
        (fill, Stroke::new(1.0, t.border), t.text)
    };
    let text = if enabled {
        text
    } else {
        text.gamma_multiply(0.5)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(15),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 13.0;
    if let Some(icon) = icon {
        icons::paint(
            ui,
            Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), vec2(16.0, 16.0)),
            icon,
            15.0,
            text,
        );
        x += icon_room;
    }
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        text,
    );
    response
}

/// Icon and label, transparent until hovered.
pub fn ghost_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(13.0);
    let width = text_width(ui, label, &font) + 38.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() && ui.is_enabled() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.hover);
    }
    let color = if ui.is_enabled() {
        t.text
    } else {
        t.text_faint
    };
    icons::paint(
        ui,
        Rect::from_min_size(rect.min + vec2(6.0, 6.0), vec2(18.0, 18.0)),
        icon,
        17.0,
        if ui.is_enabled() {
            t.icon
        } else {
            t.text_faint
        },
    );
    ui.painter().text(
        rect.left_center() + vec2(30.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    response
}

/// A search-field lookalike that opens the command palette, with its key at
/// the right. The placeholder is cut with "…" before it reaches the key.
/// The image coordinates shown at a rect's corners, clockwise from its top
/// left, when the image is turned `quarter_turns` clockwise: each corner
/// shows the image corner `quarter_turns` before it.
fn turned_uvs(quarter_turns: u8) -> [egui::Pos2; 4] {
    let uvs = [
        egui::pos2(0.0, 0.0),
        egui::pos2(1.0, 0.0),
        egui::pos2(1.0, 1.0),
        egui::pos2(0.0, 1.0),
    ];
    let turns = usize::from(quarter_turns % 4);
    std::array::from_fn(|corner| uvs[(corner + 4 - turns) % 4])
}

/// Paints `texture` (an upright page image) into `rect` turned
/// `quarter_turns` clockwise, as the page is laid out.
pub fn paint_turned_image(
    painter: &egui::Painter,
    texture: egui::TextureId,
    rect: Rect,
    quarter_turns: u8,
) {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let mut mesh = egui::Mesh::with_texture(texture);
    for (corner, uv) in corners.into_iter().zip(turned_uvs(quarter_turns)) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: corner,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    painter.add(mesh);
}

pub fn search_box(ui: &mut egui::Ui, placeholder: &str, shortcut: &str, width: f32) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), placeholder)
    });
    let fill = if response.hovered() { t.hover } else { t.field };
    ui.painter().rect(
        rect,
        CornerRadius::same(15),
        fill,
        Stroke::new(1.0, t.border),
        StrokeKind::Inside,
    );
    icons::paint(
        ui,
        Rect::from_min_size(rect.min + vec2(10.0, 7.0), vec2(16.0, 16.0)),
        "search",
        15.0,
        t.text_muted,
    );
    let shortcut =
        ui.painter()
            .layout_no_wrap(shortcut.to_owned(), theme::regular(11.5), t.text_faint);
    let shortcut_pos =
        rect.right_center() - vec2(12.0 + shortcut.size().x, shortcut.size().y * 0.5);
    let text_right = (shortcut_pos.x - 10.0).max(rect.left());
    let text_left = (rect.left() + 34.0).min(text_right);
    row_text(
        ui,
        egui::pos2(text_left, rect.center().y),
        placeholder,
        theme::regular(13.0),
        t.text_faint,
        text_right - text_left,
    );
    ui.painter()
        .with_clip_rect(rect)
        .galley(shortcut_pos, shortcut, t.text_faint);
    response.on_hover_cursor(egui::CursorIcon::Text)
}

/// A rounded field with a search icon, after the library's filter.
pub fn filter_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> Response {
    let tokens = Tokens::get(ui.ctx());
    egui::Frame::new()
        .fill(tokens.field)
        .stroke(Stroke::new(1.0, tokens.border))
        .corner_radius(15)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(icons::image("search", 14.0, tokens.text_muted));
                ui.add(
                    egui::TextEdit::singleline(text)
                        .hint_text(hint)
                        .frame(egui::Frame::NONE)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner
}

/// A small upper-case title over a group of controls or rows.
pub fn section_title(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(theme::semibold(10.5))
            .color(t.text_faint)
            .extra_letter_spacing(0.6),
    );
    ui.add_space(2.0);
}

/// One line of `text`, left-aligned and vertically centred on `pos`, cut
/// with "…" to at most `max_width`.
pub fn row_text(
    ui: &egui::Ui,
    pos: egui::Pos2,
    text: impl Into<String>,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) {
    let mut job = egui::text::LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    let galley = ui.painter().layout_job(job);
    let rect = Align2::LEFT_CENTER.anchor_size(pos, galley.size());
    ui.painter().galley(rect.min, galley, color);
}

/// The size of a key chip for `label`.
pub fn kbd_size(ui: &egui::Ui, label: &str) -> egui::Vec2 {
    vec2(text_width(ui, label, &theme::medium(11.0)) + 12.0, 20.0)
}

/// Paints a key chip ("Ctrl+K") at `rect`.
pub fn paint_kbd(ui: &egui::Ui, rect: Rect, label: &str) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect(
        rect,
        CornerRadius::same(5),
        t.hover,
        Stroke::new(1.0, t.border),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::medium(11.0),
        t.text_muted,
    );
}

/// A slider with a visible rail: the theme's flat inactive widgets would
/// leave egui's slider without one.
pub fn slider(ui: &mut egui::Ui, slider: egui::Slider<'_>) -> Response {
    let tokens = Tokens::get(ui.ctx());
    ui.scope(|ui| {
        let visuals = &mut ui.style_mut().visuals;
        visuals.widgets.inactive.bg_fill = tokens.divider;
        visuals.widgets.inactive.weak_bg_fill = tokens.divider;
        visuals.selection.bg_fill = tokens.accent;
        visuals.slider_trailing_fill = true;
        ui.add(slider)
    })
    .inner
}

/// One choice of several, drawn as a row of joined buttons. Returns whether
/// the choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let options: Vec<(T, Option<&str>, &str)> = options
        .iter()
        .map(|&(option, label)| (option, None, label))
        .collect();
    segmented_with_icons(ui, value, &options)
}

/// [`segmented`] with an icon before each label.
pub fn segmented_with_icons<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, Option<&str>, &str)],
) -> bool {
    let t = Tokens::get(ui.ctx());
    let font = theme::medium(12.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, icon, label)| {
            text_width(ui, label, &font) + 24.0 + if icon.is_some() { 20.0 } else { 0.0 }
        })
        .collect();
    let (rect, _) =
        ui.allocate_exact_size(vec2(widths.iter().sum::<f32>() + 4.0, 30.0), Sense::hover());
    ui.painter().rect(
        rect,
        CornerRadius::same(8),
        t.hover,
        Stroke::new(1.0, t.divider),
        StrokeKind::Inside,
    );
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    let enabled = ui.is_enabled();
    for ((option, icon, label), width) in options.iter().zip(widths) {
        let cell = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(width, 26.0));
        x += width;
        let response = ui.interact(cell, ui.id().with(("segment", label)), Sense::click());
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, enabled, selected, *label)
        });
        if selected {
            ui.painter().rect(
                cell,
                CornerRadius::same(6),
                t.card,
                Stroke::new(1.0, t.border),
                StrokeKind::Inside,
            );
        } else if response.hovered() && enabled {
            ui.painter()
                .rect_filled(cell, CornerRadius::same(6), t.pressed);
        }
        let mut color = if selected { t.text } else { t.text_muted };
        if !enabled {
            color = color.gamma_multiply(0.5);
        }
        let mut center = cell.center();
        if let Some(icon) = icon {
            let label_width = text_width(ui, label, &font);
            let left = cell.center().x - (label_width + 20.0) / 2.0;
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(left + 7.0, center.y), vec2(14.0, 14.0)),
                icon,
                14.0,
                if selected { t.accent } else { color },
            );
            center.x = left + 20.0 + label_width / 2.0;
        }
        ui.painter()
            .text(center, Align2::CENTER_CENTER, *label, font.clone(), color);
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    changed
}

/// An on/off switch; the response is changed when it flips.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, mut response) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, *on, ""));
    let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
    let off = if response.hovered() && enabled {
        t.text_faint
    } else {
        t.border
    };
    let mut track = off.lerp_to_gamma(t.accent, how_on);
    let mut knob = Color32::WHITE;
    if !enabled {
        track = track.gamma_multiply(0.45);
        knob = knob.gamma_multiply(0.8);
    }
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), track);
    let x = egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), how_on);
    let center = egui::pos2(x, rect.center().y);
    ui.painter()
        .circle_filled(center + vec2(0.0, 0.5), 8.0, Color32::from_black_alpha(30));
    ui.painter().circle_filled(center, 8.0, knob);
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(2.0),
            CornerRadius::same(12),
            Stroke::new(1.5, t.accent),
            StrokeKind::Outside,
        );
    }
    response
}

/// The look of egui's own controls on a form: fields, buttons and menus
/// framed on the field colour, an accent focus ring, checkboxes and
/// radios with a border. The theme leaves them flat, for toolbars.
pub fn form_style(ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let style = ui.style_mut();
    let radius = CornerRadius::same(t.radius);
    let border = Stroke::new(1.0, t.border);
    let widgets = &mut style.visuals.widgets;
    widgets.inactive.bg_fill = t.field;
    widgets.inactive.weak_bg_fill = t.field;
    widgets.inactive.bg_stroke = border;
    widgets.hovered.weak_bg_fill = t.hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0, t.text_faint);
    widgets.active.weak_bg_fill = t.pressed;
    widgets.active.bg_stroke = Stroke::new(1.0, t.accent);
    widgets.open.weak_bg_fill = t.hover;
    widgets.open.bg_stroke = Stroke::new(1.0, t.accent);
    for state in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.corner_radius = radius;
    }
    style.visuals.selection.stroke = Stroke::new(1.5, t.accent);
    style.spacing.interact_size.y = 28.0;
    style.spacing.button_padding = vec2(12.0, 5.0);
}

/// A titled group of settings rows on its own surface, rows split by
/// hairlines; `add` adds them through [`Group`].
pub fn settings_group<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut Group<'_>) -> R,
) -> R {
    let t = Tokens::get(ui.ctx());
    if !title.is_empty() {
        ui.add_space(14.0);
        ui.label(
            egui::RichText::new(title)
                .font(theme::semibold(13.0))
                .color(t.text),
        );
        ui.add_space(4.0);
    }
    egui::Frame::new()
        .fill(t.group)
        .stroke(Stroke::new(1.0, t.divider))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(14, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut group = Group { ui, rows: 0 };
            add(&mut group)
        })
        .inner
}

/// The rows of a [`settings_group`].
pub struct Group<'a> {
    ui: &'a mut egui::Ui,
    rows: usize,
}

impl Group<'_> {
    pub fn ctx(&self) -> &egui::Context {
        self.ui.ctx()
    }

    /// A hairline before every row but the first.
    fn next_row(&mut self) {
        if self.rows > 0 {
            let t = Tokens::get(self.ui.ctx());
            let (rect, _) = self
                .ui
                .allocate_exact_size(vec2(self.ui.available_width(), 1.0), Sense::hover());
            self.ui
                .painter()
                .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, t.divider));
        }
        self.rows += 1;
    }

    /// A setting: its name and what it does on the left, `control` at the
    /// right (laid out right to left, so add the rightmost first).
    pub fn row<R>(
        &mut self,
        label: &str,
        description: Option<&str>,
        control: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let t = Tokens::get(self.ui.ctx());
        let description = description.map(|text| {
            egui::RichText::new(text)
                .font(theme::regular(11.5))
                .color(t.text_muted)
        });
        self.row_with(label, description, control)
    }

    /// [`Self::row`] with a description styled by the caller (a warning
    /// colour, say).
    pub fn row_with<R>(
        &mut self,
        label: &str,
        description: Option<egui::RichText>,
        control: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        self.next_row();
        let t = Tokens::get(self.ui.ctx());
        let ui = &mut *self.ui;
        let width = ui.available_width();
        let label_width = (width * 0.45).clamp(140.0, 320.0);
        let wrap = Some(egui::TextWrapMode::Wrap);
        let galley = |ui: &egui::Ui, text: egui::RichText| {
            egui::WidgetText::from(text).into_galley(
                ui,
                wrap,
                label_width,
                egui::FontSelection::Default,
            )
        };
        let title = galley(
            ui,
            egui::RichText::new(label)
                .font(theme::medium(13.0))
                .color(t.text),
        );
        let description = description.map(|text| galley(ui, text));
        let text_height = title.size().y
            + description
                .as_ref()
                .map_or(0.0, |galley| 2.0 + galley.size().y);
        // The label and the control are centred on the row's height.
        let height = (text_height + 20.0).max(48.0);
        let top_left = ui.cursor().min;
        let rect = Rect::from_min_size(top_left, vec2(width, height));
        let mut y = rect.center().y - text_height / 2.0;
        let title_height = title.size().y;
        ui.painter()
            .galley(egui::pos2(rect.left(), y), title, t.text);
        y += title_height + 2.0;
        if let Some(description) = description {
            ui.painter()
                .galley(egui::pos2(rect.left(), y), description, t.text_muted);
        }
        let controls = Rect::from_min_max(
            egui::pos2(rect.left() + label_width + 12.0, rect.top()),
            rect.max,
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        child.spacing_mut().item_spacing = vec2(8.0, 4.0);
        let inner = control(&mut child);
        let used = child.min_rect().union(rect);
        ui.allocate_rect(
            Rect::from_min_max(rect.min, egui::pos2(rect.right(), used.bottom())),
            Sense::hover(),
        );
        inner
    }

    /// A row the width of the group, for galleries, lists and notes.
    pub fn full<R>(&mut self, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        self.next_row();
        self.ui
            .vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.add_space(10.0);
                let inner = add(ui);
                ui.add_space(10.0);
                inner
            })
            .inner
    }

    /// A muted line of explanation.
    pub fn note(&mut self, text: &str) {
        self.full(|ui| {
            let t = Tokens::get(ui.ctx());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(theme::regular(11.5))
                        .color(t.text_muted),
                )
                .wrap(),
            );
        });
    }

    /// A row that goes somewhere else: an icon, `label` and a chevron;
    /// returns whether it was clicked.
    pub fn link(&mut self, icon: &str, label: &str) -> bool {
        self.next_row();
        link_row(self.ui, icon, label, "chevron-right").clicked()
    }
}

/// A full-width row with an icon, `label` and a trailing icon, highlighted
/// on hover.
pub fn link_row(ui: &mut egui::Ui, icon: &str, label: &str, trailing: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(vec2(6.0, -4.0)),
            CornerRadius::same(t.radius),
            t.hover,
        );
    }
    icons::paint(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(8.0, 0.0), vec2(16.0, 16.0)),
        icon,
        16.0,
        t.icon,
    );
    row_text(
        ui,
        rect.left_center() + vec2(26.0, 0.0),
        label,
        theme::medium(13.0),
        t.text,
        rect.width() - 52.0,
    );
    icons::paint(
        ui,
        Rect::from_center_size(rect.right_center() - vec2(8.0, 0.0), vec2(14.0, 14.0)),
        trailing,
        14.0,
        t.text_muted,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The frame of floating cards: popovers, the find bar, drawers.
pub fn card_frame(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(CornerRadius::same(10))
        .shadow(t.card_shadow())
        .inner_margin(egui::Margin::same(12))
}

/// A row that fills the width, highlighted on hover (and when `selected`),
/// for lists in panels. Returns its rect and response. The highlight stays
/// while the pointer is on buttons drawn over the row.
pub fn list_row(ui: &mut egui::Ui, height: f32, selected: bool) -> (Rect, Response) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.accent_soft);
    } else if response.contains_pointer() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.hover);
    }
    (rect, response)
}

/// How long a toast stays.
const TOAST_SECONDS: f64 = 6.0;

/// Transient messages at the bottom centre, after pdfcraft's `toast`: the
/// latest shows on an inverted card for a few seconds, longer while hovered,
/// and a click dismisses it.
#[derive(Default)]
pub struct Toasts {
    messages: Vec<String>,
    /// When the current message was first shown, in egui time.
    shown_at: Option<f64>,
}

impl Toasts {
    pub fn push(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
        self.shown_at = None;
    }

    #[cfg(test)]
    pub fn latest(&self) -> Option<&str> {
        self.messages.last().map(String::as_str)
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.shown_at = None;
    }

    /// Draws the latest message `lift` points above the bottom.
    pub fn show(&mut self, ctx: &egui::Context, lift: f32) {
        let Some(message) = self.messages.last().cloned() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let shown_at = *self.shown_at.get_or_insert(now);
        let t = Tokens::get(ctx);
        let (fill, text) = t.toast();
        let screen = ctx.content_rect();
        let area = egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Tooltip)
            .sense(Sense::click())
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(screen.center_bottom() - vec2(0.0, lift))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(fill)
                    .corner_radius(CornerRadius::same(8))
                    .shadow(t.popup_shadow())
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.set_max_width((screen.width() - 64.0).clamp(1.0, 560.0));
                        ui.add(egui::Label::new(egui::RichText::new(message).color(text)).wrap());
                    });
            });
        let response = area.response;
        if response.clicked() {
            self.clear();
            return;
        }
        if response.hovered() {
            // Hovering restarts the clock, so a long message can be read.
            self.shown_at = Some(now);
        } else if now - shown_at > TOAST_SECONDS {
            self.clear();
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quarter_turn_shows_the_images_bottom_left_at_the_top_left() {
        use egui::pos2;
        assert_eq!(turned_uvs(0)[0], pos2(0.0, 0.0));
        let turned = turned_uvs(1);
        assert_eq!(turned[0], pos2(0.0, 1.0));
        assert_eq!(turned[1], pos2(0.0, 0.0));
        assert_eq!(turned_uvs(2)[0], pos2(1.0, 1.0));
        assert_eq!(turned_uvs(5), turned);
    }

    fn run(ctx: &egui::Context, time: f64, toasts: &mut Toasts) {
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| toasts.show(ui.ctx(), 28.0));
        output.textures_delta.clear();
    }

    #[test]
    fn a_toast_leaves_after_a_few_seconds() {
        let ctx = egui::Context::default();
        let mut toasts = Toasts::default();
        toasts.push("Could not save");
        run(&ctx, 1.0, &mut toasts);
        assert_eq!(toasts.latest(), Some("Could not save"));
        run(&ctx, 1.0 + TOAST_SECONDS - 0.5, &mut toasts);
        assert!(toasts.latest().is_some());
        run(&ctx, 1.0 + TOAST_SECONDS + 0.5, &mut toasts);
        assert_eq!(toasts.latest(), None);
    }

    #[test]
    fn segmented_choice_changes_on_click() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut value = 0;
        let mut changed = false;
        let mut frame = |events: Vec<egui::Event>, value: &mut i32| {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                changed |= segmented(ui, value, &[(0, "System"), (1, "Light"), (2, "Dark")]);
            });
            output.textures_delta.clear();
        };
        frame(Vec::new(), &mut value);
        // Well right of the first cell, which is about 70 points wide.
        let pos = egui::pos2(150.0, 15.0);
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            vec![egui::Event::PointerMoved(pos), press(true)],
            &mut value,
        );
        frame(vec![press(false)], &mut value);
        assert!(changed);
        assert_ne!(value, 0);
    }
}

/// Durations of the assistant's small motions, after Zeron's `motion.rs`.
pub mod motion {
    /// A disclosure opening or closing.
    pub const COLLAPSE: f32 = 0.18;
    /// A chevron turning.
    pub const CHEVRON: f32 = 0.2;
    /// Controls fading in on hover.
    pub const HOVER_FADE: f32 = 0.15;
    /// A menu appearing.
    pub const MENU_IN: f32 = 0.14;
    /// One sweep of a shimmer over a running label.
    pub const SHIMMER_PERIOD: f64 = 2.2;
}

/// `galley` at `pos` in `base`, with a brighter band (`peak`) sweeping
/// across it: the label of a step that is running. The band is painted as
/// thin clipped strips of the same galley, so glyphs never split (Zeron's
/// clip-strip shimmer).
pub fn shimmer_text(
    ui: &egui::Ui,
    pos: egui::Pos2,
    galley: std::sync::Arc<egui::Galley>,
    base: Color32,
    peak: Color32,
) {
    let painter = ui.painter();
    painter.galley_with_override_text_color(pos, galley.clone(), base);
    let width = galley.size().x.max(1.0);
    let band = (width * 0.36).max(24.0);
    let time = ui.input(|i| i.time);
    let phase = (time % motion::SHIMMER_PERIOD / motion::SHIMMER_PERIOD) as f32;
    let center = pos.x - band + phase * (width + 2.0 * band);
    let rect = Rect::from_min_size(pos, galley.size());
    const STRIP: f32 = 2.0;
    let mut x = (center - band).max(rect.left());
    while x < (center + band).min(rect.right()) {
        let distance = ((x + STRIP / 2.0 - center) / band).abs();
        let strength = (1.0 - distance).clamp(0.0, 1.0).powi(2);
        if strength > 0.02 {
            let strip = Rect::from_min_max(
                egui::pos2(x, rect.top()),
                egui::pos2(x + STRIP, rect.bottom()),
            );
            painter
                .with_clip_rect(strip.intersect(painter.clip_rect()))
                .galley_with_override_text_color(
                    pos,
                    galley.clone(),
                    theme::mix(base, peak, strength),
                );
        }
        x += STRIP;
    }
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// Three dots pulsing in turn, centred in `rect`: something is loading.
pub fn loading_dots(ui: &egui::Ui, rect: Rect, color: Color32) {
    let time = ui.input(|i| i.time);
    for index in 0..3 {
        let phase = ((time / 0.9 - f64::from(index) * 0.18).rem_euclid(1.0)) as f32;
        let alpha = 0.3 + 0.7 * (1.0 - (phase * 2.0 - 1.0).abs());
        let center = rect.center() + vec2((index as f32 - 1.0) * 8.0, 0.0);
        ui.painter()
            .circle_filled(center, 2.2, color.gamma_multiply(alpha));
    }
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// A value going between `low` and `high` and back every `period` seconds,
/// for labels that pulse while something is pending.
pub fn pulse(ui: &egui::Ui, low: f32, high: f32, period: f64) -> f32 {
    let time = ui.input(|i| i.time);
    let phase = ((time / period).rem_euclid(1.0) * std::f64::consts::TAU).cos() as f32;
    ui.ctx().request_repaint_after(Duration::from_millis(33));
    low + (high - low) * (0.5 - 0.5 * phase)
}

/// A chevron pointing right, turned a quarter clockwise for each unit of
/// `open`: 1 points down, 3 up.
pub fn chevron(painter: &egui::Painter, center: egui::Pos2, size: f32, open: f32, color: Color32) {
    let angle = open.clamp(0.0, 4.0) * std::f32::consts::FRAC_PI_2;
    let (sin, cos) = angle.sin_cos();
    let turn = |x: f32, y: f32| center + vec2(x * cos - y * sin, x * sin + y * cos) * size;
    painter.line(
        vec![turn(-0.18, -0.36), turn(0.18, 0.0), turn(-0.18, 0.36)],
        Stroke::new(1.5, color),
    );
}

/// A ring filled clockwise from the top to `fraction`, over a faint track.
pub fn progress_ring(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    fraction: f32,
    color: Color32,
    track: Color32,
) {
    painter.circle_stroke(center, radius, Stroke::new(2.0, track));
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction <= 0.0 {
        return;
    }
    let steps = (48.0 * fraction).ceil().max(2.0) as usize;
    let points = (0..=steps)
        .map(|step| {
            let angle = -std::f32::consts::FRAC_PI_2
                + std::f32::consts::TAU * fraction * step as f32 / steps as f32;
            center + vec2(angle.cos(), angle.sin()) * radius
        })
        .collect();
    painter.line(points, Stroke::new(2.0, color));
}
