//! Document tabs (pen components `tab` and `tab/active`): 36pt cells split
//! by hairlines; the active one is `surface` with a `▌` accent mark in the
//! icon's place, the title in full text and a dirty dot; others are faint
//! on the base. Laid out with `craft_ui::tabs`, painted here.

use egui::{Align2, Rect, Response, Ui, Vec2, vec2};

use crate::{
    icons, text,
    theme::{self, Tokens},
};

/// The tab bar's height.
pub const BAR_HEIGHT: f32 = 36.0;

/// The icon's room before the title, 13pt and a gap; the active tab's
/// mark takes the same room, so a tab keeps its width when activated.
const ICON_SLOT: f32 = 21.0;

/// What a tab shows.
pub struct DocTab<'a> {
    pub title: &'a str,
    pub icon: Option<&'a str>,
    pub active: bool,
    /// Unsaved or running: a small accent dot after the title.
    pub dirty: bool,
    pub closable: bool,
}

impl DocTab<'_> {
    /// The width the tab wants for its title.
    pub fn natural_width(&self, ui: &Ui) -> f32 {
        let mut width = 12.0 + text::width(ui, self.title, &theme::mono(12.0)) + 10.0;
        width += if self.has_slot() { ICON_SLOT } else { 0.0 };
        width += if self.dirty { 14.0 } else { 0.0 };
        width += if self.closable { 20.0 } else { 0.0 };
        width.clamp(80.0, 240.0)
    }

    /// Whether the tab shows an icon or the active mark before its title.
    fn has_slot(&self) -> bool {
        self.icon.is_some() || self.active
    }

    /// The close button's rect in a tab at `rect`.
    pub fn close_rect(&self, rect: Rect) -> Option<Rect> {
        self.closable.then(|| {
            Rect::from_center_size(
                egui::pos2(rect.right() - 16.0, rect.center().y),
                Vec2::splat(16.0),
            )
        })
    }

    /// Paints the tab at `rect`; `response` is the tab's, `close_hovered`
    /// whether the pointer is on its close button.
    pub fn paint(&self, ui: &Ui, rect: Rect, response: &Response, close_hovered: bool) {
        let t = Tokens::get(ui.ctx());
        let hovered = response.hovered() || close_hovered;
        if self.active {
            ui.painter().rect_filled(rect, 0, t.surface);
        } else if hovered {
            ui.painter().rect_filled(rect, 0, t.hover);
        }
        ui.painter()
            .vline(rect.right() - 0.5, rect.y_range(), t.hairline());
        if !self.active {
            ui.painter()
                .hline(rect.x_range(), rect.bottom() - 0.5, t.hairline());
        }
        let y = rect.center().y;
        let mut x = rect.left() + 12.0;
        if self.active {
            ui.painter().rect_filled(
                Rect::from_center_size(egui::pos2(x + 6.5, y), vec2(4.0, 13.0)),
                0,
                t.accent,
            );
        } else if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 6.5, y), Vec2::splat(13.0)),
                icon,
                13.0,
                t.faint,
            );
        }
        if self.has_slot() {
            x += ICON_SLOT;
        }
        let close = self.close_rect(rect);
        let mut right = close.map_or(rect.right() - 10.0, |close| close.left() - 4.0);
        if self.dirty {
            ui.painter()
                .circle_filled(egui::pos2(right - 3.0, y), 3.0, t.accent_text);
            right -= 12.0;
        }
        let title_rect = text::line(
            ui,
            egui::pos2(x, y),
            Align2::LEFT_CENTER,
            self.title,
            theme::mono(12.0),
            if self.active || hovered {
                t.text
            } else {
                t.muted
            },
            right - x,
        );
        let _ = title_rect;
        if let Some(close) = close
            && (self.active || hovered)
        {
            if close_hovered {
                ui.painter()
                    .rect_filled(close, t.ctl_radius(), t.rule_strong);
            }
            icons::paint(
                ui,
                close,
                "x",
                12.0,
                if close_hovered {
                    t.text
                } else if self.active {
                    t.muted
                } else {
                    t.faint
                },
            );
        }
    }
}

/// The `g▌` mark at the start of the tab bar.
pub fn logo(ui: &mut Ui, active: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(40.0, BAR_HEIGHT), egui::Sense::click());
    // Home has no tab of its own: the mark is on `surface` while it shows.
    if active {
        ui.painter().rect_filled(rect, 0, t.surface.to_opaque());
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0, t.hover);
    }
    ui.painter()
        .vline(rect.right() - 0.5, rect.y_range(), t.hairline());
    let galley = ui
        .painter()
        .layout_no_wrap("g".into(), theme::mono_bold(15.0), t.text);
    let width = galley.size().x;
    let x = rect.center().x - (width + 6.0) / 2.0;
    ui.painter().galley(
        egui::pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        t.text,
    );
    ui.painter().rect_filled(
        Rect::from_min_size(
            egui::pos2(x + width + 1.0, rect.center().y - 7.0),
            vec2(6.0, 14.0),
        ),
        0,
        t.accent,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activating_a_tab_keeps_its_width() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let tab = |active| DocTab {
                title: "attention.pdf",
                icon: Some("file-text"),
                active,
                dirty: false,
                closable: true,
            };
            assert_eq!(tab(true).natural_width(ui), tab(false).natural_width(ui));
        });
        output.textures_delta.clear();
    }
}
