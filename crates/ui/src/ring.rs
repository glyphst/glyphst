//! The tool ring (pen component `ring`, frames 01, 02h and 02j option A):
//! eight tools around the pointer, picked by flicking toward one, with
//! `cancel` on the hub. Slot 0 is up and the slots go clockwise every 45°.
//! It has no outline: a soft disc, solid behind the labels, fades out at
//! its rim; the slot under the pointer is an `accent-soft` wedge fading the
//! same way; an `accent` notch on the hub and an `accent` dot show the
//! pointer.

use std::{f32::consts::TAU, time::Duration};

use egui::{Align2, Color32, Mesh, Pos2, Shape, Stroke, Vec2, vec2};

use crate::{
    icons,
    theme::{self, Tokens},
};

pub const DIAMETER: f32 = 320.0;
pub const HUB: f32 = 84.0;
pub const SLOTS: usize = 8;
/// From the centre to a slot's icon and label.
const SLOT_RADIUS: f32 = 106.0;
/// The disc's alpha from the centre (0) to the rim (1): solid behind the
/// labels, then fading out. The page shows through more than the design's
/// alphas suggest (egui blends in linear light), so it stays opaque longer.
const DISC: [(f32, f32); 5] = [
    (0.0, 1.0),
    (0.74, 1.0),
    (0.86, 0.85),
    (0.94, 0.4),
    (1.0, 0.0),
];
/// The hot wedge's alpha from the hub (0) to the rim (1).
const WEDGE: [(f32, f32); 4] = [(0.0, 1.0), (0.6, 0.9), (0.85, 0.45), (1.0, 0.0)];
/// How far the pointer moves with the button held before the ring opens.
pub const OPEN_DISTANCE: f32 = 12.0;
/// How long the button is held still before the ring opens.
pub const OPEN_HOLD: Duration = Duration::from_millis(300);

/// The slot that cancels on a ring the stick opens: a stick can't reach
/// the hub, since letting it go picks.
pub const CANCEL_SLOT: usize = 4;

/// What the hub shows.
#[derive(Clone, Copy, Debug)]
pub enum Hub<'a> {
    /// `cancel`, lit while the pointer is on it (the mouse's ring).
    Cancel,
    /// The aimed slot's name, or the `g▌` mark while none is (the stick's).
    Name(Option<&'a str>),
}

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

/// The slot a stick pushed toward `direction` points at.
pub fn slot_toward(direction: Vec2) -> usize {
    let turn = direction.x.atan2(-direction.y).rem_euclid(TAU) / TAU;
    (turn * SLOTS as f32 + 0.5) as usize % SLOTS
}

/// The pointer aiming at `slot`, for a stick that has come back toward
/// the centre with the slot still aimed.
pub fn slot_pointer(center: Pos2, slot: usize) -> Pos2 {
    center + direction(slot) * SLOT_RADIUS
}

/// Where the pointer is for a stick at `stick` (-1..=1): out from the
/// centre, short of the rim.
pub fn stick_pointer(center: Pos2, stick: Vec2) -> Pos2 {
    let stick = if stick.length() > 1.0 {
        stick.normalized()
    } else {
        stick
    };
    center + stick * (DIAMETER / 2.0 - 12.0)
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

/// Paints the ring at `center` with the pointer at `pointer`: what
/// releasing there picks is lit, and the notch and dot show where it is.
pub fn paint(
    ui: &egui::Ui,
    center: Pos2,
    slots: &[Option<Slot<'_>>; SLOTS],
    pointer: Pos2,
    hub_shows: Hub<'_>,
) {
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter();
    let named = matches!(hub_shows, Hub::Name(_));
    let hot = match pick(center, pointer) {
        // A stick ring has no cancel on its hub.
        Pick::Cancel if named => Pick::Slot(usize::MAX),
        hot => hot,
    };
    let outer = DIAMETER / 2.0;
    let inner = if named { 48.0 } else { HUB / 2.0 };
    painter.add(disc(center, outer, t.raised));
    if let Pick::Slot(slot) = hot
        && slots[slot].is_some()
    {
        let color = theme::mix(t.accent_soft, t.accent, 0.12);
        painter.add(wedge(center, inner, outer, slot, color));
    }
    for (index, slot) in slots.iter().enumerate() {
        let Some(slot) = slot else { continue };
        let on = hot == Pick::Slot(index);
        let at = center + direction(index) * SLOT_RADIUS;
        let icon = egui::Rect::from_center_size(at - vec2(0.0, 10.0), Vec2::splat(16.0));
        icons::paint(
            ui,
            icon,
            slot.icon,
            16.0,
            if on { t.accent_text } else { t.muted },
        );
        painter.text(
            at + vec2(0.0, 10.0),
            Align2::CENTER_CENTER,
            slot.label,
            theme::mono(11.0),
            if on { t.accent_text } else { t.text_2 },
        );
    }
    let hub = if hot == Pick::Cancel {
        t.rule_strong
    } else {
        t.hover
    };
    painter.circle_filled(center, inner, hub);
    match hub_shows {
        Hub::Cancel => {
            let x = egui::Rect::from_center_size(center - vec2(0.0, 9.0), Vec2::splat(13.0));
            icons::paint(ui, x, "x", 13.0, t.faint);
            painter.text(
                center + vec2(0.0, 9.0),
                Align2::CENTER_CENTER,
                "cancel",
                theme::mono(11.0),
                if hot == Pick::Cancel { t.text } else { t.muted },
            );
        }
        Hub::Name(Some(name)) => {
            let mut job = egui::text::LayoutJob::simple(
                name.to_string(),
                theme::regular(12.0),
                t.accent_text,
                inner * 2.0 - 14.0,
            );
            job.halign = egui::Align::Center;
            job.wrap.max_rows = 2;
            job.wrap.overflow_character = Some('…');
            let galley = painter.layout_job(job);
            let at = center - vec2(0.0, galley.size().y / 2.0);
            painter.galley(at, galley, t.accent_text);
        }
        Hub::Name(None) => {
            // The `g▌` mark.
            let g = painter.text(
                center - vec2(5.0, 0.0),
                Align2::CENTER_CENTER,
                "g",
                theme::mono_bold(18.0),
                t.text,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(g.right() + 1.0, center.y - 10.0),
                    vec2(8.0, 20.0),
                ),
                0,
                t.accent,
            );
        }
    }
    // Where the pointer is: a notch on the hub toward it, and its dot.
    let offset = pointer - center;
    if offset.length() > 2.0 {
        let along = offset.normalized();
        let across = vec2(-along.y, along.x);
        let tip = center + along * (inner + 8.0);
        let base = center + along * (inner + 1.5);
        painter.add(Shape::convex_polygon(
            vec![tip, base + across * 6.0, base - across * 6.0],
            t.accent,
            Stroke::NONE,
        ));
    }
    let reach = outer - 8.0;
    let dot = if offset.length() > reach {
        center + offset.normalized() * reach
    } else {
        pointer
    };
    painter.circle(dot, 5.0, t.accent, Stroke::new(1.5, t.bg));
}

/// A filled circle whose alpha follows [`DISC`] from the centre out.
fn disc(center: Pos2, radius: f32, color: Color32) -> Shape {
    const STEPS: u32 = 64;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color.gamma_multiply(DISC[0].1));
    for &(r, alpha) in &DISC[1..] {
        for step in 0..STEPS {
            let angle = step as f32 / STEPS as f32 * TAU;
            mesh.colored_vertex(
                center + vec2(angle.sin(), -angle.cos()) * radius * r,
                color.gamma_multiply(alpha),
            );
        }
    }
    let at = |band: u32, step: u32| 1 + band * STEPS + step % STEPS;
    for step in 0..STEPS {
        mesh.add_triangle(0, at(0, step), at(0, step + 1));
    }
    for band in 0..DISC.len() as u32 - 2 {
        for step in 0..STEPS {
            let (a, b) = (at(band, step), at(band, step + 1));
            let (c, d) = (at(band + 1, step), at(band + 1, step + 1));
            mesh.add_triangle(a, c, b);
            mesh.add_triangle(b, c, d);
        }
    }
    Shape::mesh(mesh)
}

/// The ring's 45° between `inner` and `outer` about slot `slot`, its
/// alpha following [`WEDGE`] outward.
fn wedge(center: Pos2, inner: f32, outer: f32, slot: usize, color: Color32) -> Shape {
    const STEPS: u32 = 12;
    let bands = WEDGE.len() as u32;
    let mut mesh = Mesh::default();
    let start = (slot as f32 - 0.5) / SLOTS as f32 * TAU;
    let span = TAU / SLOTS as f32;
    for step in 0..=STEPS {
        let angle = start + span * step as f32 / STEPS as f32;
        let along = vec2(angle.sin(), -angle.cos());
        for &(r, alpha) in &WEDGE {
            mesh.colored_vertex(
                center + along * (inner + (outer - inner) * r),
                color.gamma_multiply(alpha),
            );
        }
    }
    for step in 0..STEPS {
        for band in 0..bands - 1 {
            let i = step * bands + band;
            let j = i + bands;
            mesh.add_triangle(i, i + 1, j);
            mesh.add_triangle(i + 1, j + 1, j);
        }
    }
    Shape::mesh(mesh)
}

/// The gallery's sample: the ring with the pointer over `end draw`.
pub fn sample(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(DIAMETER + 2.0), egui::Sense::hover());
    let labels = [
        ("pencil", "end draw"),
        ("highlighter", "highlight"),
        ("mouse-pointer-2", "cursor"),
        ("undo-2", "undo"),
        ("eraser", "eraser"),
        ("trash-2", "clear"),
        ("square", "black"),
        ("timer", "timer"),
    ];
    let slots = labels.map(|(icon, label)| Some(Slot { icon, label }));
    paint(
        ui,
        rect.center(),
        &slots,
        rect.center() - vec2(0.0, DIAMETER / 2.0 - 12.0),
        Hub::Cancel,
    );
}

#[cfg(test)]
mod tests {
    use egui::pos2;

    use super::*;

    #[test]
    fn sticks_point_at_slots_clockwise_from_up() {
        assert_eq!(slot_toward(vec2(0.0, -1.0)), 0);
        assert_eq!(slot_toward(vec2(0.7, -0.7)), 1);
        assert_eq!(slot_toward(vec2(1.0, 0.0)), 2);
        assert_eq!(slot_toward(vec2(0.0, 1.0)), 4);
        assert_eq!(slot_toward(vec2(-1.0, 0.1)), 6);
        let center = pos2(100.0, 100.0);
        assert_eq!(
            pick(center, stick_pointer(center, vec2(1.0, 0.0))),
            Pick::Slot(2)
        );
        assert_eq!(
            pick(center, stick_pointer(center, Vec2::ZERO)),
            Pick::Cancel
        );
    }

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
