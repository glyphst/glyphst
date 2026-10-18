//! Segmented choices (pen component `segmented`): one of a few options on a
//! hairline outline, the chosen one on the hover fill.

use egui::{Align2, Rect, Response, Sense, StrokeKind, Ui, Vec2, vec2};

use crate::{
    icons, text,
    theme::{self, Tokens},
};

/// Draws the choices of `options` (value, optional icon, label) and returns
/// whether the choice changed.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, Option<&str>, &str)],
) -> bool {
    show(ui, value, options).1
}

/// [`segmented`] with labels only.
pub fn labels<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let options: Vec<(T, Option<&str>, &str)> = options
        .iter()
        .map(|&(option, label)| (option, None, label))
        .collect();
    segmented(ui, value, &options)
}

/// The outer response and whether the choice changed.
pub fn show<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, Option<&str>, &str)],
) -> (Response, bool) {
    let t = Tokens::get(ui.ctx());
    let font = theme::mono(11.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|(_, icon, label)| {
            text::width(ui, &label.to_lowercase(), &font)
                + 20.0
                + if icon.is_some() { 18.0 } else { 0.0 }
        })
        .collect();
    let inner: f32 = widths.iter().sum::<f32>() + 2.0 * (options.len().saturating_sub(1)) as f32;
    let (rect, outer) = ui.allocate_exact_size(vec2(inner + 4.0, 25.0), Sense::hover());
    ui.painter()
        .rect_stroke(rect, t.ctl_radius(), t.edge(), StrokeKind::Inside);
    let mut x = rect.left() + 2.0;
    let mut changed = false;
    let enabled = ui.is_enabled();
    for ((option, icon, label), width) in options.iter().zip(widths) {
        let cell = Rect::from_min_size(egui::pos2(x, rect.top() + 2.0), vec2(width, 21.0));
        x += width + 2.0;
        let response = ui.interact(
            cell,
            ui.scope_id().with(("segment", *label)),
            Sense::click(),
        );
        let selected = *value == *option;
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, enabled, selected, *label)
        });
        let fill = if selected {
            t.hover
        } else if response.hovered() && enabled {
            Tokens::fade(t.hover, 0.6)
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter()
            .rect_filled(cell, 1, crate::motion::color(ui.ctx(), response.id, fill));
        let mut color = if selected || (response.hovered() && enabled) {
            t.text
        } else {
            t.muted
        };
        if !enabled {
            color = t.faint;
        }
        let mut center = cell.center();
        if let Some(icon) = icon {
            let label_width = text::width(ui, &label.to_lowercase(), &font);
            let left = cell.center().x - (label_width + 18.0) / 2.0;
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(left + 6.0, center.y), Vec2::splat(12.0)),
                icon,
                12.0,
                if selected { t.accent } else { color },
            );
            center.x = left + 18.0 + label_width / 2.0;
        }
        // Choices are written in lower case, as the design does.
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            label.to_lowercase(),
            font.clone(),
            color,
        );
        if response.clicked() && !selected {
            *value = *option;
            changed = true;
        }
    }
    (outer, changed)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_click_changes_the_choice() {
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
                changed |= super::labels(ui, value, &[(0, "system"), (1, "light"), (2, "dark")]);
            });
            output.textures_delta.clear();
        };
        frame(Vec::new(), &mut value);
        // Right of the first cell, which is about 60 points wide.
        let pos = egui::pos2(120.0, 12.0);
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
