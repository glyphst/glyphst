//! Painting helpers shared by components: page images turned by quarter
//! turns, and the assistant's small motions (shimmer, dots, pulse,
//! chevrons, rings).

use std::time::Duration;

use egui::{Color32, Rect, Stroke, vec2};

use crate::{motion, theme};

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
/// Whether `response` has the keys, whether or not the window does (egui's
/// `has_focus` also asks the window): a pad works an unfocused window.
pub fn has_focus(response: &egui::Response) -> bool {
    response.ctx.memory(|memory| memory.has_focus(response.id))
}

/// The keyboard's (and the pad's) focus on `rect`: a 2pt `accent` ring
/// just outside it.
pub fn focus_ring(ui: &egui::Ui, response: &egui::Response, rect: Rect) {
    if has_focus(response) {
        let t = crate::theme::Tokens::get(ui.ctx());
        ui.painter().rect_stroke(
            rect.expand(3.0),
            t.ctl_radius(),
            egui::Stroke::new(2.0, t.accent),
            egui::StrokeKind::Outside,
        );
    }
}

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

/// A rounded bar in `base` with a brighter band (`peak`) sweeping across
/// it: a placeholder where something being worked out will appear (frame
/// 04w). With reduced motion it stays still. Bars painted in the same
/// frame sweep together, as one band across the screen.
pub fn shimmer_rect(ui: &egui::Ui, rect: Rect, base: Color32, peak: Color32) {
    let painter = ui.painter();
    let radius = (rect.height() / 3.0).min(3.0);
    painter.rect_filled(rect, radius, base);
    if motion::is_reduced(ui.ctx()) {
        return;
    }
    // The band runs across the whole window, so neighbouring bars line up.
    let screen = ui.ctx().content_rect();
    let band = 140.0;
    let time = ui.input(|i| i.time);
    let phase = (time % motion::SHIMMER_PERIOD / motion::SHIMMER_PERIOD) as f32;
    let center = screen.left() - band + phase * (screen.width() + 2.0 * band);
    const STRIP: f32 = 3.0;
    let clipped = painter.with_clip_rect(rect.intersect(painter.clip_rect()));
    let mut x = (center - band).max(rect.left());
    while x < (center + band).min(rect.right()) {
        let distance = ((x + STRIP / 2.0 - center) / band).abs();
        let strength = (1.0 - distance).clamp(0.0, 1.0).powi(2);
        if strength > 0.02 {
            clipped.rect_filled(
                Rect::from_min_max(
                    egui::pos2(x, rect.top()),
                    egui::pos2(x + STRIP, rect.bottom()),
                ),
                0,
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
}
