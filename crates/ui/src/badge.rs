//! Badges (pen component `badge`) and context chips (`chip/context`): a
//! count on a hover fill, and an accent `@p.12` chip for what the assistant
//! is given.

use egui::{Align2, Rect, Response, Sense, Ui, Vec2, Widget, vec2};

use crate::{
    icons, text,
    theme::{self, Tokens},
};

/// A count or short tag: `24`, `pdf`.
pub struct Badge<'a>(pub &'a str);

impl Badge<'_> {
    pub fn size(&self, ui: &Ui) -> Vec2 {
        vec2(text::width(ui, self.0, &theme::mono(10.0)) + 8.0, 13.0)
    }

    pub fn paint(&self, ui: &Ui, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        ui.painter().rect_filled(rect, t.ctl_radius(), t.hover);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            self.0,
            theme::mono(10.0),
            t.muted,
        );
    }
}

impl Widget for Badge<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(self.size(ui), Sense::hover());
        self.paint(ui, rect);
        response
    }
}

/// A context chip: an optional icon and a label in accent on `accent-soft`,
/// optionally with a close mark. The response is a click on the chip; see
/// [`ChipResponse::closed`].
#[must_use = "call `.show(ui)`"]
pub struct Chip<'a> {
    label: &'a str,
    icon: Option<&'a str>,
    closable: bool,
    muted: bool,
}

pub struct ChipResponse {
    pub response: Response,
    pub closed: bool,
}

impl<'a> Chip<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            icon: None,
            closable: false,
            muted: false,
        }
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// On the hover fill in muted text, for context that is not attached.
    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub fn size(&self, ui: &Ui) -> Vec2 {
        let mut width = text::width(ui, self.label, &theme::mono(11.0)) + 12.0;
        if self.icon.is_some() {
            width += 15.0;
        }
        if self.closable {
            width += 14.0;
        }
        vec2(width, 19.0)
    }

    pub fn show(self, ui: &mut Ui) -> ChipResponse {
        let t = Tokens::get(ui.ctx());
        let (rect, response) = ui.allocate_exact_size(self.size(ui), Sense::click());
        let (fill, color) = if self.muted {
            (t.hover, t.muted)
        } else {
            (t.accent_soft, t.accent_text)
        };
        let fill = if response.hovered() {
            theme::mix(fill, color, 0.12)
        } else {
            fill
        };
        ui.painter().rect_filled(rect, t.ctl_radius(), fill);
        let mut x = rect.left() + 6.0;
        if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + 5.5, rect.center().y), Vec2::splat(11.0)),
                icon,
                11.0,
                color,
            );
            x += 15.0;
        }
        ui.painter().text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            self.label,
            theme::mono(11.0),
            color,
        );
        let mut closed = false;
        if self.closable {
            let close = Rect::from_center_size(
                egui::pos2(rect.right() - 9.0, rect.center().y),
                Vec2::splat(12.0),
            );
            let close_response = ui.interact(close, response.id.with("close"), Sense::click());
            icons::paint(
                ui,
                close,
                "x",
                10.0,
                if close_response.hovered() {
                    t.text
                } else {
                    color
                },
            );
            closed = close_response.clicked();
        }
        ChipResponse { response, closed }
    }
}
