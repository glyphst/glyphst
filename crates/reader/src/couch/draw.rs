//! Pieces gamepad mode draws on every screen: the hint bar (component
//! `hint-bar`), chips, and the gradients of home's hero.

use egui::{Align2, Color32, Mesh, Pos2, Rect, Shape, TextureId, pos2, vec2};
use glyphst_ui::{
    pad_glyph::Hint,
    theme::{self, Tokens},
};

/// The hint bar's height.
pub const BAR: f32 = 64.0;

/// The bar along the bottom of `screen`: `left` (the menu) at the left,
/// the screen's actions at the right.
pub fn hint_bar(ui: &egui::Ui, screen: Rect, left: &[Hint<'_>], right: &[Hint<'_>]) {
    let t = Tokens::get(ui.ctx());
    let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - BAR), screen.max);
    ui.painter().rect_filled(bar, 0, Tokens::fade(t.bg, 0.9));
    ui.painter()
        .hline(bar.x_range(), bar.top() + 0.5, t.hairline());
    let middle = bar.center().y;
    let mut x = bar.left() + 40.0;
    for hint in left {
        hint.paint(ui, x, middle);
        x += hint.size(ui).x + 28.0;
    }
    let mut x = bar.right() - 40.0;
    for hint in right.iter().rev() {
        x -= hint.size(ui).x;
        hint.paint(ui, x, middle);
        x -= 28.0;
    }
}

/// A chip of mono 15 text on `surface`, at `at` aligned by `align`; an
/// accented part follows the text.
pub fn chip(ui: &egui::Ui, at: Pos2, align: Align2, text: &str, accent: Option<&str>) -> Rect {
    let t = Tokens::get(ui.ctx());
    let font = theme::mono(15.0);
    let painter = ui.painter();
    let main = painter.layout_no_wrap(text.to_string(), font.clone(), t.text_2);
    let extra =
        accent.map(|accent| painter.layout_no_wrap(accent.to_string(), font, t.accent_text));
    let gap = 10.0;
    let width = main.size().x + extra.as_ref().map_or(0.0, |e| e.size().x + gap);
    let size = vec2(width + 28.0, main.size().y + 16.0);
    let rect = align.anchor_size(at, size);
    painter.rect_filled(rect, t.pop_radius(), Tokens::fade(t.surface, 0.9));
    painter.rect_stroke(rect, t.pop_radius(), t.hairline(), egui::StrokeKind::Inside);
    let left = rect.left() + 14.0;
    let top = rect.center().y - main.size().y / 2.0;
    let main_width = main.size().x;
    painter.galley(pos2(left, top), main, t.text_2);
    if let Some(extra) = extra {
        painter.galley(pos2(left + main_width + gap, top), extra, t.accent_text);
    }
    rect
}

/// `rect` filled from `top` to `bottom` colour, straight down.
pub fn vertical_fade(painter: &egui::Painter, rect: Rect, top: Color32, bottom: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(Shape::mesh(mesh));
}

/// `rect` filled from `left` to `right` colour, across.
pub fn horizontal_fade(painter: &egui::Painter, rect: Rect, left: Color32, right: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), left);
    mesh.colored_vertex(rect.right_top(), right);
    mesh.colored_vertex(rect.left_bottom(), left);
    mesh.colored_vertex(rect.right_bottom(), right);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(Shape::mesh(mesh));
}

/// A soft glow of `color` about `center`, gone at `radius` (an ellipse
/// `stretch` times as wide as tall).
pub fn glow(painter: &egui::Painter, center: Pos2, radius: f32, stretch: f32, color: Color32) {
    const STEPS: u32 = 64;
    let bands = [(0.0, 1.0), (0.35, 0.65), (0.7, 0.2), (1.0, 0.0)];
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    for &(r, alpha) in &bands[1..] {
        for step in 0..STEPS {
            let angle = step as f32 / STEPS as f32 * std::f32::consts::TAU;
            let at = center + vec2(angle.cos() * stretch, angle.sin()) * radius * r;
            mesh.colored_vertex(at, color.gamma_multiply(alpha));
        }
    }
    let at = |band: u32, step: u32| 1 + band * STEPS + step % STEPS;
    for step in 0..STEPS {
        mesh.add_triangle(0, at(0, step), at(0, step + 1));
    }
    for band in 0..bands.len() as u32 - 2 {
        for step in 0..STEPS {
            let (a, b) = (at(band, step), at(band, step + 1));
            let (c, d) = (at(band + 1, step), at(band + 1, step + 1));
            mesh.add_triangle(a, c, b);
            mesh.add_triangle(b, c, d);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// `texture` in `rect`, fading out over its lower `fade` (0..1) part.
pub fn image_fading(
    painter: &egui::Painter,
    rect: Rect,
    texture: TextureId,
    fade: f32,
    alpha: f32,
) {
    let mut mesh = Mesh::with_texture(texture);
    let solid = Color32::WHITE.gamma_multiply(alpha);
    let split = 1.0 - fade.clamp(0.0, 1.0);
    let rows = [(0.0, solid), (split, solid), (1.0, Color32::TRANSPARENT)];
    for &(v, color) in &rows {
        let y = rect.top() + rect.height() * v;
        mesh.vertices.push(egui::epaint::Vertex {
            pos: pos2(rect.left(), y),
            uv: pos2(0.0, v),
            color,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: pos2(rect.right(), y),
            uv: pos2(1.0, v),
            color,
        });
    }
    for row in 0..2u32 {
        let i = row * 2;
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    }
    painter.add(Shape::mesh(mesh));
}

/// A progress meter in glyphs: `━━━───` `width` long.
pub fn meter(progress: f32, width: usize) -> (String, String) {
    let filled = ((progress.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    ("━".repeat(filled), "─".repeat(width - filled))
}
