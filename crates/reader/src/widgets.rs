//! The reader's names for glyphst-ui's components, kept while screens move
//! onto the components directly; nothing here draws on its own.

use egui::{Rect, Response};

use glyphst_ui::{
    button::Button, field::TextField, key, pane, segmented as segment, settings, text,
    theme::Tokens, toggle::Switch,
};

pub use glyphst_ui::overlay::Toasts;
pub use glyphst_ui::paint::{
    chevron, loading_dots, motion, paint_turned_image, pulse, shimmer_text,
};
pub use glyphst_ui::settings::Group;

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

/// The main action filled with the accent (`primary`), or an outlined one.
pub fn pill_button(ui: &mut egui::Ui, icon: Option<&str>, label: &str, primary: bool) -> Response {
    let mut button = if primary {
        Button::primary(label)
    } else {
        Button::ghost(label)
    };
    if let Some(icon) = icon {
        button = button.icon(icon);
    }
    button.show(ui)
}

/// Icon and label, transparent until hovered.
pub fn ghost_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    Button::plain(label).icon(icon).show(ui)
}

/// A search field.
pub fn filter_field(ui: &mut egui::Ui, text: &mut String, hint: &str) -> Response {
    TextField::search(text, hint).show(ui)
}

/// One line of `text`, left-aligned and vertically centred on `pos`, cut
/// with "…" to at most `max_width`.
pub fn row_text(
    ui: &egui::Ui,
    pos: egui::Pos2,
    content: impl Into<String>,
    font: egui::FontId,
    color: egui::Color32,
    max_width: f32,
) {
    text::row(ui, pos, content, font, color, max_width);
}

/// The size of a key chip for `label`.
pub fn kbd_size(ui: &egui::Ui, label: &str) -> egui::Vec2 {
    key::size(ui, &key::display(label))
}

/// Paints a key chip at `rect`.
pub fn paint_kbd(ui: &egui::Ui, rect: Rect, label: &str) {
    key::paint(ui, rect, &key::display(label), None);
}

/// egui's slider on the design's rail: 2px, accent fill, no handle circle.
pub fn slider(ui: &mut egui::Ui, slider: egui::Slider<'_>) -> Response {
    let t = Tokens::get(ui.ctx());
    ui.scope(|ui| {
        let visuals = &mut ui.style_mut().visuals;
        visuals.widgets.inactive.bg_fill = t.rule_strong;
        visuals.widgets.inactive.weak_bg_fill = t.rule_strong;
        visuals.widgets.inactive.fg_stroke.color = t.text;
        visuals.widgets.hovered.fg_stroke.color = t.text;
        visuals.selection.bg_fill = t.accent;
        visuals.slider_trailing_fill = true;
        ui.spacing_mut().slider_rail_height = 2.0;
        ui.add(slider.handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.3 }))
    })
    .inner
}

/// One choice of several. Returns whether the choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> bool {
    let options: Vec<(T, String)> = options
        .iter()
        .map(|&(option, label)| (option, label.to_lowercase()))
        .collect();
    let options: Vec<(T, &str)> = options
        .iter()
        .map(|(option, label)| (*option, label.as_str()))
        .collect();
    segment::labels(ui, value, &options)
}

/// An on/off switch; the response is changed when it flips.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> Response {
    ui.add(Switch(on))
}

/// The look of egui's own controls on a form.
pub fn form_style(ui: &mut egui::Ui) {
    settings::form_style(ui);
}

/// A titled group of settings rows split by hairlines.
pub fn settings_group<R>(
    ui: &mut egui::Ui,
    title: &str,
    add: impl FnOnce(&mut Group<'_>) -> R,
) -> R {
    settings::group(ui, &title.to_lowercase(), add)
}

/// The frame of popovers: the find bar, menus.
pub fn card_frame(ctx: &egui::Context) -> egui::Frame {
    pane::popover(ctx).inner_margin(egui::Margin::same(12))
}

/// A row that fills the width, on the hover fill when hovered and on
/// `accent-soft` when `selected`. Returns its rect and response.
pub fn list_row(ui: &mut egui::Ui, height: f32, selected: bool) -> (Rect, Response) {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    if selected {
        ui.painter().rect_filled(rect, 0, t.accent_soft);
    } else if response.contains_pointer() {
        ui.painter().rect_filled(rect, 0, t.hover);
    }
    (rect, response)
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_icon_in_use_exists() {
        use crate::{commands::Command, settings_window::Section, sidebar::SidebarTab};
        let names = Command::ALL
            .iter()
            .map(|command| command.icon())
            .chain(Section::ALL.iter().map(|section| section.icon()))
            .chain(SidebarTab::ALL.iter().map(|tab| tab.icon()))
            .chain(super::ICONS_IN_USE.iter().copied());
        for name in names {
            assert!(crate::icons::exists(name), "missing icon {name}");
        }
    }
}
