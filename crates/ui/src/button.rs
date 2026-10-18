//! Buttons (pen components `btn/primary`, `btn/ghost`, `btn/bracket` and
//! `btn/icon`): a filled accent button for the one main action, an outlined
//! one beside it, `[ action ]` text buttons for everything inline, and
//! square icon buttons in bars.

use egui::{Align2, Color32, Rect, Response, Sense, StrokeKind, Ui, Vec2, Widget, vec2};

use crate::{
    icons, key, text,
    theme::{self, Tokens},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `btn/primary`: accent fill, label on accent.
    Primary,
    /// `btn/ghost`: a `rule-strong` outline.
    Ghost,
    /// `btn/bracket`: `[ label ]` in accent text.
    Bracket,
    /// `[ label ]` in muted text, for secondary inline actions
    /// (`[ copy ] [ retry ]` under an answer).
    BracketMuted,
    /// A bare label, highlighted on hover: menus of the status bar.
    Plain,
}

/// A text button; `icon` goes before the label and `shortcut` (a key chip)
/// after it.
#[must_use = "call `.show(ui)` or `ui.add(...)`"]
pub struct Button<'a> {
    label: &'a str,
    kind: Kind,
    icon: Option<&'a str>,
    trailing_icon: Option<&'a str>,
    shortcut: Option<&'a str>,
    selected: bool,
    min_width: f32,
    small: bool,
}

impl<'a> Button<'a> {
    pub fn new(label: &'a str, kind: Kind) -> Self {
        Self {
            label,
            kind,
            icon: None,
            trailing_icon: None,
            shortcut: None,
            selected: false,
            min_width: 0.0,
            small: false,
        }
    }

    pub fn primary(label: &'a str) -> Self {
        Self::new(label, Kind::Primary)
    }

    pub fn ghost(label: &'a str) -> Self {
        Self::new(label, Kind::Ghost)
    }

    pub fn bracket(label: &'a str) -> Self {
        Self::new(label, Kind::Bracket)
    }

    pub fn bracket_muted(label: &'a str) -> Self {
        Self::new(label, Kind::BracketMuted)
    }

    pub fn plain(label: &'a str) -> Self {
        Self::new(label, Kind::Plain)
    }

    pub fn icon(mut self, icon: &'a str) -> Self {
        self.icon = Some(icon);
        self
    }

    /// An icon after the label, as `continue →`.
    pub fn trailing_icon(mut self, icon: &'a str) -> Self {
        self.trailing_icon = Some(icon);
        self
    }

    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut).filter(|shortcut| !shortcut.is_empty());
        self
    }

    /// Toggled on: plain buttons get the hover fill and full text.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    /// 24 points high instead of 28, in bars.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    fn label_text(&self) -> String {
        match self.kind {
            Kind::Bracket | Kind::BracketMuted => format!("[ {} ]", self.label),
            _ => self.label.to_owned(),
        }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = Tokens::get(ui.ctx());
        let font = match self.kind {
            Kind::Primary => theme::mono_strong(12.0),
            _ => theme::mono(12.0),
        };
        let label = self.label_text();
        let bracket = matches!(self.kind, Kind::Bracket | Kind::BracketMuted);
        let pad = if bracket { 2.0 } else { 12.0 };
        let icon_size = 14.0;
        let gap = 6.0;
        let mut width = text::width(ui, &label, &font) + pad * 2.0;
        if self.icon.is_some() {
            width += icon_size + gap;
        }
        if self.trailing_icon.is_some() {
            width += icon_size + gap;
        }
        let key_size = self.shortcut.map(|shortcut| key::size(ui, shortcut));
        if let Some(size) = key_size {
            width += size.x + gap;
        }
        let height = if self.small || bracket {
            24.0
        } else {
            t.row(28.0)
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(width.max(self.min_width), height), Sense::click());
        let enabled = ui.is_enabled();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, enabled, self.selected, self.label)
        });
        let hovered = response.hovered() && enabled;
        let pressed = response.is_pointer_button_down_on() && enabled;
        let (fill, stroke, mut color) = match self.kind {
            Kind::Primary => (
                if pressed || hovered {
                    t.accent_text
                } else {
                    t.accent
                },
                egui::Stroke::NONE,
                t.on_accent,
            ),
            Kind::Ghost => (
                if pressed {
                    t.rule_strong
                } else if hovered || self.selected {
                    t.hover
                } else {
                    Color32::TRANSPARENT
                },
                t.edge(),
                t.text,
            ),
            Kind::Bracket => (
                if hovered {
                    t.accent_soft
                } else {
                    Color32::TRANSPARENT
                },
                egui::Stroke::NONE,
                t.accent_text,
            ),
            Kind::BracketMuted => (
                if hovered {
                    t.hover
                } else {
                    Color32::TRANSPARENT
                },
                egui::Stroke::NONE,
                if hovered { t.text } else { t.muted },
            ),
            Kind::Plain => (
                if pressed {
                    t.rule_strong
                } else if hovered || self.selected {
                    t.hover
                } else {
                    Color32::TRANSPARENT
                },
                egui::Stroke::NONE,
                if hovered || self.selected {
                    t.text
                } else {
                    t.muted
                },
            ),
        };
        if !enabled {
            color = t.faint;
        }
        let fill = if enabled { fill } else { Color32::TRANSPARENT };
        let fill = crate::motion::color(ui.ctx(), response.id, fill);
        ui.painter()
            .rect(rect, t.ctl_radius(), fill, stroke, StrokeKind::Inside);
        crate::paint::focus_ring(ui, &response, rect);
        // Content centred when the button is wider than it needs.
        let mut x = rect.center().x - width / 2.0 + pad;
        let y = rect.center().y;
        if let Some(icon) = self.icon {
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + icon_size / 2.0, y), Vec2::splat(icon_size)),
                icon,
                icon_size,
                color,
            );
            x += icon_size + gap;
        }
        let galley = ui.painter().layout_no_wrap(label, font, color);
        let label_width = galley.size().x;
        ui.painter()
            .galley(egui::pos2(x, y - galley.size().y / 2.0), galley, color);
        x += label_width;
        if let Some(icon) = self.trailing_icon {
            x += gap;
            icons::paint(
                ui,
                Rect::from_center_size(egui::pos2(x + icon_size / 2.0, y), Vec2::splat(icon_size)),
                icon,
                icon_size,
                color,
            );
            x += icon_size;
        }
        if let (Some(shortcut), Some(size)) = (self.shortcut, key_size) {
            x += gap;
            key::paint(
                ui,
                Rect::from_min_size(egui::pos2(x, y - size.y / 2.0), size),
                shortcut,
                matches!(self.kind, Kind::Primary).then_some(t.on_accent),
            );
        }
        if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand)
        } else {
            response
        }
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui)
    }
}

/// `btn/icon`: a square icon button, transparent until hovered; `selected`
/// gets the hover fill and full-strength icon. `tooltip` is also its
/// accessible name.
pub fn icon(ui: &mut Ui, name: &str, size: f32, selected: bool, tooltip: &str) -> Response {
    icons::button(ui, name, size, selected, tooltip)
}

/// A label painted as a `[ label ]` link at `pos`, for text that flows
/// (the toast's `[ undo ]`). Returns the painted rect.
pub fn paint_bracket(ui: &Ui, pos: egui::Pos2, align: Align2, label: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    text::line(
        ui,
        pos,
        align,
        format!("[ {label} ]"),
        theme::mono(12.0),
        t.accent_text,
        f32::INFINITY,
    )
}
