//! The ring the right stick opens (frames 09b, 09e): the reading ring's
//! commands, or presenting's tools while presenting. Pushing the stick
//! opens it at the screen's centre and aims; letting go picks the slot last
//! aimed at, B cancels.

use std::time::Instant;

use egui::{Pos2, Rect, Vec2, pos2, vec2};
use glyphst_ui::{
    motion,
    pad_glyph::{Face, Glyph, Hint},
    ring,
    theme::Tokens,
};

use super::ReaderApp;
use crate::{
    commands::{Category, Command},
    pad::route::RingStep,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RingKind {
    Reading,
    Present,
}

/// The open ring and where the stick points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PadRing {
    kind: RingKind,
    aim: Vec2,
}

/// A command's short name on the reading ring.
fn ring_label(command: Command) -> &'static str {
    match command {
        Command::Outline => "contents",
        Command::Bookmark => "bookmark",
        Command::FitWidth => "fit width",
        Command::FitPage => "fit page",
        Command::PageOverview => "overview",
        Command::GoToPage => "go to page",
        Command::Back => "back",
        Command::Forward => "forward",
        Command::Home => "home",
        Command::FirstPage => "first page",
        Command::LastPage => "last page",
        Command::ZoomIn => "zoom in",
        Command::ZoomOut => "zoom out",
        Command::Present => "present",
        Command::ToggleFullscreen => "full screen",
        Command::Zen => "zen",
        Command::ToggleSidebar => "sidebar",
        other => other.label(),
    }
}

/// Whether `command` may sit on the reading ring: what reading needs, not
/// presenting's tools or what wants typing.
pub(crate) fn fits_reading_ring(command: Command) -> bool {
    matches!(
        command.category(),
        Category::Navigation | Category::View | Category::Text
    ) && !matches!(
        command,
        Command::Find | Command::FindNext | Command::FindPrevious | Command::RegionSelect
    ) || matches!(
        command,
        Command::Home | Command::Library | Command::ToggleSidebar
    )
}

impl ReaderApp {
    fn reading_ring_commands(&self) -> [Option<Command>; ring::SLOTS] {
        self.prefs.reading_ring.clone().map(|id| {
            id.as_deref()
                .and_then(Command::from_id)
                .filter(|&command| fits_reading_ring(command))
        })
    }

    pub(super) fn pad_ring_step(&mut self, step: RingStep, ctx: &egui::Context, now: Instant) {
        match step {
            RingStep::Open(aim) => {
                let kind = if self.presenting.is_some() {
                    RingKind::Present
                } else {
                    RingKind::Reading
                };
                self.pad_ring = Some(PadRing { kind, aim });
            }
            RingStep::Aim(aim) => {
                if let Some(ring) = &mut self.pad_ring {
                    ring.aim = aim;
                }
            }
            RingStep::Cancel => self.pad_ring = None,
            RingStep::Pick(slot) => {
                let Some(ring) = self.pad_ring.take() else {
                    return;
                };
                match ring.kind {
                    RingKind::Reading => {
                        if let Some(command) = self.reading_ring_commands()[slot] {
                            self.run_or_notify(command, ctx);
                        }
                    }
                    RingKind::Present => {
                        if let Some(command) = self.ring_commands()[slot] {
                            self.run_present(command, now);
                        }
                    }
                }
            }
        }
    }

    /// The ring at the screen's centre, with how to use it under it.
    pub(super) fn pad_ring_overlay(&mut self, ctx: &egui::Context) {
        let shown = motion::smooth_bool(
            ctx,
            egui::Id::new("pad-ring-shown"),
            self.pad_ring.is_some(),
            motion::HOVER,
        );
        let Some(pad_ring) = self.pad_ring else {
            return;
        };
        let commands = match pad_ring.kind {
            RingKind::Reading => self.reading_ring_commands(),
            RingKind::Present => self.ring_commands(),
        };
        let present = self.presenting.as_ref();
        let slots = commands.map(|command| {
            command.map(|command| match (pad_ring.kind, present) {
                (RingKind::Present, Some(state)) => {
                    let (label, icon) = state.slot(command);
                    ring::Slot { icon, label }
                }
                _ => ring::Slot {
                    icon: command.icon(),
                    label: ring_label(command),
                },
            })
        });
        let screen = ctx.content_rect();
        let center = screen.center();
        // Gamepad mode's ring is 420 across (frame 09e).
        let id = egui::Id::new("pad-ring");
        let scale = if self.prefs.gamepad_mode {
            420.0 / ring::DIAMETER
        } else {
            1.0
        };
        let about = center.to_vec2();
        ctx.set_transform_layer(
            egui::LayerId::new(egui::Order::Foreground, id),
            egui::emath::TSTransform::from_translation(about)
                * egui::emath::TSTransform::from_scaling(scale)
                * egui::emath::TSTransform::from_translation(-about),
        );
        egui::Area::new(id)
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                ui.multiply_opacity(shown);
                ring::paint(
                    ui,
                    center,
                    &slots,
                    ring::stick_pointer(center, pad_ring.aim),
                );
                ring_hints(ui, center + vec2(0.0, ring::DIAMETER / 2.0 + 24.0));
            });
    }
}

/// `◎ aim · let go to pick   (B) cancel` on a chip under the ring.
fn ring_hints(ui: &egui::Ui, top: Pos2) {
    let t = Tokens::get(ui.ctx());
    let hints = [
        Hint::new(&[Glyph::Stick], "aim · let go to pick"),
        Hint::new(&[Glyph::Face(Face::East)], "cancel"),
    ];
    let gap = 20.0;
    let sizes: Vec<Vec2> = hints.iter().map(|hint| hint.size(ui)).collect();
    let width = sizes.iter().map(|size| size.x).sum::<f32>() + gap * (hints.len() - 1) as f32;
    let height = sizes.iter().map(|size| size.y).fold(0.0, f32::max);
    let chip = Rect::from_min_size(
        pos2(top.x - width / 2.0 - 12.0, top.y),
        vec2(width + 24.0, height + 12.0),
    );
    ui.painter().rect_filled(chip, t.pop_radius(), t.surface);
    ui.painter()
        .rect_stroke(chip, t.pop_radius(), t.hairline(), egui::StrokeKind::Inside);
    let mut x = chip.left() + 12.0;
    for (hint, size) in hints.iter().zip(&sizes) {
        hint.paint(ui, x, chip.center().y);
        x += size.x + gap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reading_ring_holds_reading_commands() {
        for command in crate::present::READING_RING_DEFAULTS.into_iter().flatten() {
            assert!(fits_reading_ring(command), "{}", command.id());
        }
        assert!(!fits_reading_ring(Command::PresentDraw));
        assert!(!fits_reading_ring(Command::Find), "it wants typing");
    }
}
