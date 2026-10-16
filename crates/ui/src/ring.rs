//! The tool ring (pen component `ring`, frames 01 and 02h): eight tools
//! around the pointer, picked by flicking toward one, with `cancel` on the
//! hub. Slot 0 is up and the slots go clockwise every 45°; the slot under
//! the pointer is an `accent-soft` wedge.

use std::{f32::consts::TAU, time::Duration};

use egui::{Align2, Color32, Mesh, Pos2, Shape, Stroke, Vec2, vec2};

use crate::{
    icons,
    theme::{self, Tokens},
};

pub const DIAMETER: f32 = 232.0;
pub const HUB: f32 = 64.0;
pub const SLOTS: usize = 8;
/// From the centre to a slot's icon and label.
const SLOT_RADIUS: f32 = 76.0;
/// How far the pointer moves with the button held before the ring opens.
pub const OPEN_DISTANCE: f32 = 12.0;
/// How long the button is held still before the ring opens.
pub const OPEN_HOLD: Duration = Duration::from_millis(300);

/// What releasing the button at a point picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Slot(usize),
    Cancel,
}

/// A tool in a slot: its Lucide icon and its short label (`end draw`).
#[derive(Clone, Copy, Debug)]
pub struct Slot<'a> {
    pub icon: &'a str,
    pub label: &'a str,
}

/// Whether a press that moved `moved` points and was held `held` opens the
/// ring; one that opens neither is a plain click.
pub fn opens(moved: f32, held: Duration) -> bool {
    moved > OPEN_DISTANCE || held >= OPEN_HOLD
}

/// The slot toward `pointer` from the ring's `center`, or cancel on the
/// hub. Past the ring's edge the direction still picks: a flick is enough.
pub fn pick(center: Pos2, pointer: Pos2) -> Pick {
    let offset = pointer - center;
    if offset.length() <= HUB / 2.0 {
        return Pick::Cancel;
    }
    // Clockwise from up, with each slot centred on its direction.
    let turn = offset.x.atan2(-offset.y).rem_euclid(TAU) / TAU;
    let slot = (turn * SLOTS as f32 + 0.5) as usize % SLOTS;
    Pick::Slot(slot)
}

/// Where a ring opened at `pointer` goes so it fits inside `screen`.
pub fn center_within(pointer: Pos2, screen: egui::Rect) -> Pos2 {
    let radius = DIAMETER / 2.0 + 1.0;
    let room = screen.shrink(radius);
    if room.is_negative() {
        return screen.center();
    }
    room.clamp(pointer)
}

/// The direction of slot `slot`, from the centre.
fn direction(slot: usize) -> Vec2 {
    let angle = slot as f32 / SLOTS as f32 * TAU;
    vec2(angle.sin(), -angle.cos())
}

/// Paints the ring at `center`; `hot` is what releasing now picks.
pub fn paint(ui: &egui::Ui, center: Pos2, slots: &[Option<Slot<'_>>; SLOTS], hot: Pick) {
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter();
    let outer = DIAMETER / 2.0;
    let inner = HUB / 2.0;
    painter.circle(center, outer, t.raised, Stroke::new(1.0, t.rule_strong));
    if let Pick::Slot(slot) = hot
        && slots[slot].is_some()
    {
        painter.add(wedge(center, inner, outer, slot, t.accent_soft));
    }
    for spoke in 0..SLOTS {
        let angle = (spoke as f32 + 0.5) / SLOTS as f32 * TAU;
        let along = vec2(angle.sin(), -angle.cos());
        painter.line_segment(
            [
                center + along * (inner + 1.0),
                center + along * (outer - 1.0),
            ],
            Stroke::new(1.0, t.rule),
        );
    }
    for (index, slot) in slots.iter().enumerate() {
        let Some(slot) = slot else { continue };
        let on = hot == Pick::Slot(index);
        let at = center + direction(index) * SLOT_RADIUS;
        let icon = egui::Rect::from_center_size(at - vec2(0.0, 9.0), Vec2::splat(14.0));
        icons::paint(
            ui,
            icon,
            slot.icon,
            14.0,
            if on { t.accent_text } else { t.muted },
        );
        painter.text(
            at + vec2(0.0, 9.0),
            Align2::CENTER_CENTER,
            slot.label,
            theme::mono(10.0),
            if on { t.accent_text } else { t.text_2 },
        );
    }
    let hub = if hot == Pick::Cancel {
        t.hover
    } else {
        t.surface
    };
    painter.circle(center, inner, hub, Stroke::new(1.0, t.rule_strong));
    let x = egui::Rect::from_center_size(center - vec2(0.0, 8.0), Vec2::splat(12.0));
    icons::paint(ui, x, "x", 12.0, t.faint);
    painter.text(
        center + vec2(0.0, 8.0),
        Align2::CENTER_CENTER,
        "cancel",
        theme::mono(10.0),
        if hot == Pick::Cancel { t.text } else { t.muted },
    );
}

/// The ring's 45° between `inner` and `outer` about slot `slot`.
fn wedge(center: Pos2, inner: f32, outer: f32, slot: usize, color: Color32) -> Shape {
    const STEPS: usize = 12;
    let mut mesh = Mesh::default();
    let start = (slot as f32 - 0.5) / SLOTS as f32 * TAU;
    let span = TAU / SLOTS as f32;
    for step in 0..=STEPS {
        let angle = start + span * step as f32 / STEPS as f32;
        let along = vec2(angle.sin(), -angle.cos());
        mesh.colored_vertex(center + along * inner, color);
        mesh.colored_vertex(center + along * outer, color);
    }
    for step in 0..STEPS as u32 {
        let i = step * 2;
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    }
    Shape::mesh(mesh)
}

/// The gallery's sample: the ring with `draw` under the pointer.
pub fn sample(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(DIAMETER + 2.0), egui::Sense::hover());
    let labels = [
        ("pencil", "draw"),
        ("highlighter", "highlight"),
        ("mouse-pointer-2", "cursor"),
        ("undo-2", "undo"),
        ("eraser", "eraser"),
        ("trash-2", "clear"),
        ("square", "black"),
        ("timer", "timer"),
    ];
    let slots = labels.map(|(icon, label)| Some(Slot { icon, label }));
    paint(ui, rect.center(), &slots, Pick::Slot(0));
}

#[cfg(test)]
mod tests {
    use egui::pos2;

    use super::*;

    #[test]
    fn the_direction_picks_the_slot_clockwise_from_up() {
        let center = pos2(100.0, 100.0);
        let at = |x: f32, y: f32| pick(center, center + vec2(x, y));
        assert_eq!(at(0.0, -80.0), Pick::Slot(0));
        assert_eq!(at(60.0, -60.0), Pick::Slot(1));
        assert_eq!(at(80.0, 0.0), Pick::Slot(2));
        assert_eq!(at(60.0, 60.0), Pick::Slot(3));
        assert_eq!(at(0.0, 80.0), Pick::Slot(4));
        assert_eq!(at(-60.0, 60.0), Pick::Slot(5));
        assert_eq!(at(-80.0, 0.0), Pick::Slot(6));
        assert_eq!(at(-60.0, -60.0), Pick::Slot(7));
        // Just either side of the line between up and up-right.
        assert_eq!(at(39.0, -100.0), Pick::Slot(0));
        assert_eq!(at(43.0, -100.0), Pick::Slot(1));
        // A flick past the edge still picks.
        assert_eq!(at(400.0, 5.0), Pick::Slot(2));
    }

    #[test]
    fn the_hub_cancels() {
        let center = pos2(0.0, 0.0);
        assert_eq!(pick(center, center), Pick::Cancel);
        assert_eq!(pick(center, pos2(HUB / 2.0 - 1.0, 0.0)), Pick::Cancel);
        assert_eq!(pick(center, pos2(HUB / 2.0 + 1.0, 0.0)), Pick::Slot(2));
    }

    #[test]
    fn a_short_still_press_is_a_click() {
        assert!(!opens(4.0, Duration::from_millis(120)));
        assert!(opens(OPEN_DISTANCE + 1.0, Duration::ZERO));
        assert!(opens(0.0, OPEN_HOLD));
    }

    #[test]
    fn the_ring_stays_on_screen() {
        let screen = egui::Rect::from_min_size(Pos2::ZERO, vec2(1440.0, 920.0));
        let corner = center_within(pos2(2.0, 3.0), screen);
        assert!(corner.x - DIAMETER / 2.0 >= 0.0 && corner.y - DIAMETER / 2.0 >= 0.0);
        let middle = pos2(700.0, 400.0);
        assert_eq!(center_within(middle, screen), middle);
    }
}
