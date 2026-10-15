//! The glyph motifs of the design (frame 00 Direction): `── title ──── n`
//! rule titles, `├─ └─` outline trees, `━━━───` meters, `┌ ┐ └ ┘` corner
//! ticks on the focused page, and `·` dot fields for empty and loading
//! states. They carry structure, not decoration; with ornaments off
//! (Settings → Appearance) each draws a plain equivalent.

use std::time::Duration;

use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Stroke, Ui, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

/// The done and remaining glyphs of a meter `cells` wide at `fraction`.
pub fn meter_text(fraction: f32, cells: usize) -> (String, String) {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let done = ((fraction * cells as f32).round() as usize).min(cells);
    ("━".repeat(done), "─".repeat(cells - done))
}

/// The tree prefix of an outline row: `├─ ` or `└─ ` after a `│  ` or
/// `   ` per ancestor. `ancestors_last[i]` says whether the ancestor at
/// depth `i` was the last of its siblings; `last` whether this row is.
pub fn tree_prefix(ancestors_last: &[bool], last: bool) -> String {
    let mut prefix = String::new();
    for &ancestor_last in ancestors_last {
        prefix.push_str(if ancestor_last { "   " } else { "│  " });
    }
    prefix.push_str(if last { "└─ " } else { "├─ " });
    prefix
}

/// Whether each row of a tree given in pre-order as depths (outline
/// levels) is the last of its siblings: no later row at its depth comes
/// before one shallower than it. Linear, for long outlines.
pub fn last_siblings(depths: &[usize]) -> Vec<bool> {
    let mut last = vec![true; depths.len()];
    // Depths of the rows after the current one, nearest on top; a row
    // deeper than one before it can never be the nearest of anything
    // earlier.
    let mut after: Vec<usize> = Vec::new();
    for (index, &depth) in depths.iter().enumerate().rev() {
        while after.last().is_some_and(|&next| next > depth) {
            after.pop();
        }
        last[index] = after.last().is_none_or(|&next| next < depth);
        after.push(depth);
    }
    last
}

/// The `ancestors_last` and `last` of each row of a tree given in
/// pre-order as depths (see [`tree_prefix`]).
pub fn tree_shape(depths: &[usize]) -> Vec<(Vec<bool>, bool)> {
    let lasts = last_siblings(depths);
    let mut stack: Vec<bool> = Vec::new();
    depths
        .iter()
        .zip(&lasts)
        .map(|(&depth, &last)| {
            stack.truncate(depth);
            while stack.len() < depth {
                stack.push(false);
            }
            let shape = (stack.clone(), last);
            stack.push(last);
            shape
        })
        .collect()
}

/// Paints a rule title in `rect`: `── title ─────── trailing`. `rect` is
/// one line high; the title is mono 11 in `text-2`, the rules and trailing
/// text faint. Without ornaments only the title and trailing text show.
pub fn paint_rule_title(ui: &Ui, rect: Rect, title: &str, trailing: Option<&str>, lead: bool) {
    let t = Tokens::get(ui.ctx());
    let font = theme::mono(11.0);
    let y = rect.center().y;
    let mut x = rect.left();
    let rule = Stroke::new(1.0, t.rule_strong);
    if lead && t.ornaments {
        ui.painter().hline(x..=x + 12.0, y, rule);
        x += 20.0;
    }
    let title_rect = text::line(
        ui,
        egui::pos2(x, y),
        Align2::LEFT_CENTER,
        title,
        font.clone(),
        t.text_2,
        rect.right() - x,
    );
    let mut right = rect.right();
    if let Some(trailing) = trailing.filter(|trailing| !trailing.is_empty()) {
        let trailing_rect = text::line(
            ui,
            egui::pos2(right, y),
            Align2::RIGHT_CENTER,
            trailing,
            font,
            t.faint,
            (right - title_rect.right() - 16.0).max(0.0),
        );
        right = trailing_rect.left() - 8.0;
    }
    if t.ornaments && right > title_rect.right() + 16.0 {
        ui.painter()
            .hline(title_rect.right() + 8.0..=right, y, rule);
    }
}

/// A rule title as a row of its own, `height` high; the response senses
/// hover only.
pub fn rule_title(ui: &mut Ui, title: &str, trailing: Option<&str>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    paint_rule_title(ui, rect, title, trailing, true);
    response
}

/// `┌ ┐ └ ┘` ticks `arm` long just outside `rect`'s corners: the focused
/// page. Without ornaments, a 1px outline.
pub fn corner_ticks(ui: &Ui, rect: Rect, gap: f32, arm: f32, color: Color32) {
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter();
    let stroke = Stroke::new(1.5, color);
    let r = rect.expand(gap);
    if !t.ornaments {
        painter.rect_stroke(r, 0, Stroke::new(1.0, color), egui::StrokeKind::Outside);
        return;
    }
    for (corner, dx, dy) in [
        (r.left_top(), 1.0, 1.0),
        (r.right_top(), -1.0, 1.0),
        (r.left_bottom(), 1.0, -1.0),
        (r.right_bottom(), -1.0, -1.0),
    ] {
        painter.line_segment([corner, corner + vec2(dx * arm, 0.0)], stroke);
        painter.line_segment([corner, corner + vec2(0.0, dy * arm)], stroke);
    }
}

/// A field of dots `step` apart filling `rect`; `lit` gives the strength
/// (0..=1) of the dot at a grid position, mixing `dot` toward `accent`.
pub fn dot_field(ui: &Ui, rect: Rect, step: f32, radius: f32, lit: impl Fn(usize, usize) -> f32) {
    let t = Tokens::get(ui.ctx());
    if !t.ornaments {
        return;
    }
    let painter = ui.painter();
    let cols = (rect.width() / step).floor().max(1.0) as usize;
    let rows = (rect.height() / step).floor().max(1.0) as usize;
    let origin = rect.center()
        - vec2(
            (cols.saturating_sub(1)) as f32 * step / 2.0,
            (rows.saturating_sub(1)) as f32 * step / 2.0,
        );
    for row in 0..rows {
        for col in 0..cols {
            let strength = lit(col, row).clamp(0.0, 1.0);
            let center = origin + vec2(col as f32 * step, row as f32 * step);
            let color = theme::mix(t.dot, t.accent, strength);
            painter.circle_filled(center, radius * (1.0 + strength * 0.6), color);
        }
    }
}

/// The loading dot field: a scan line sweeping down, lighting the dots it
/// passes (transitions.dev's generation: dots → scan line).
pub fn scanning_dots(ui: &Ui, rect: Rect, step: f32) {
    let time = ui.input(|i| i.time) as f32;
    let rows = (rect.height() / step).floor().max(1.0);
    let scan = (time * 0.6).fract() * (rows + 4.0) - 2.0;
    dot_field(ui, rect, step, 1.0, |col, row| {
        let distance = (row as f32 - scan).abs();
        let wave = ((col as f32 * 0.7 + time * 2.0).sin() * 0.5 + 0.5) * 0.25;
        (1.0 - distance / 2.0).max(0.0) + wave * (distance < 3.0) as u8 as f32
    });
    ui.ctx().request_repaint_after(Duration::from_millis(33));
}

/// A small glyph mark: dots arranged as a diamond, accent at the centre
/// (the empty assistant, covers).
pub fn dot_mark(ui: &Ui, center: Pos2, step: f32) {
    let rect = Rect::from_center_size(center, vec2(step * 5.0, step * 5.0));
    dot_field(ui, rect, step, 1.3, |col, row| {
        let distance = (col as i32 - 2).abs() + (row as i32 - 2).abs();
        match distance {
            0 => 1.0,
            1 => 0.85,
            2 => 0.35,
            _ => 0.0,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_glyphs_split_by_the_fraction() {
        assert_eq!(meter_text(0.58, 12), ("━".repeat(7), "─".repeat(5)));
        assert_eq!(meter_text(0.0, 4), (String::new(), "─".repeat(4)));
        assert_eq!(meter_text(1.5, 4), ("━".repeat(4), String::new()));
        assert_eq!(meter_text(f32::NAN, 2), (String::new(), "─".repeat(2)));
    }

    #[test]
    fn tree_prefixes_draw_branches_and_continuations() {
        assert_eq!(tree_prefix(&[], false), "├─ ");
        assert_eq!(tree_prefix(&[], true), "└─ ");
        assert_eq!(tree_prefix(&[false], false), "│  ├─ ");
        assert_eq!(tree_prefix(&[false, true], true), "│     └─ ");
    }

    #[test]
    fn last_siblings_match_a_direct_scan() {
        let depths = [0, 1, 2, 2, 1, 0, 1, 1, 2, 3, 0];
        let direct: Vec<bool> = (0..depths.len())
            .map(|index| {
                let depth = depths[index];
                depths[index + 1..]
                    .iter()
                    .find(|&&later| later <= depth)
                    .is_none_or(|&later| later < depth)
            })
            .collect();
        assert_eq!(last_siblings(&depths), direct);
    }

    #[test]
    fn tree_shape_follows_the_outline() {
        // 1, 1.1, 1.2, 2, 2.1
        let shape = tree_shape(&[0, 1, 1, 0, 1]);
        let prefixes: Vec<String> = shape
            .iter()
            .map(|(ancestors, last)| tree_prefix(ancestors, *last))
            .collect();
        assert_eq!(prefixes, ["├─ ", "│  ├─ ", "│  └─ ", "└─ ", "   └─ "]);
    }
}
