//! Panes and their headers (pen component `pane-header`): docked panes are
//! flat `surface` with 1px `rule` edges; each starts with a 32pt header
//! `title ──────── count` and its actions at the right.

use egui::{Rect, Response, Sense, Ui, vec2};

use crate::{button, motif, theme::Tokens};

/// The height of pane headers.
pub const HEADER_HEIGHT: f32 = 32.0;
/// The height of the breadcrumb over the pages and of split panes' headers.
pub const BREADCRUMB_HEIGHT: f32 = 28.0;

/// The frame of a docked pane: `surface`, no margin, no rounding.
pub fn frame(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new().fill(t.surface)
}

/// The frame of popovers, menus and modals: `raised` with a `rule-strong`
/// edge, radius 4, no shadow.
pub fn popover(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.raised)
        .stroke(t.edge())
        .corner_radius(t.pop_radius())
        .inner_margin(egui::Margin::same(4))
}

/// A box inside a pane (the document card, an error): `raised` with a
/// `rule` edge, radius 2.
pub fn card(ctx: &egui::Context) -> egui::Frame {
    let t = Tokens::get(ctx);
    egui::Frame::new()
        .fill(t.raised)
        .stroke(t.hairline())
        .corner_radius(t.ctl_radius())
        .inner_margin(egui::Margin::same(12))
}

/// An action at the right of a header: an icon and its tooltip.
pub struct Action<'a> {
    pub icon: &'a str,
    pub tooltip: &'a str,
    pub selected: bool,
}

impl<'a> Action<'a> {
    pub fn new(icon: &'a str, tooltip: &'a str) -> Self {
        Self {
            icon,
            tooltip,
            selected: false,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

/// A pane header: `title ──── trailing` and `actions` (rightmost last).
/// Returns the index of the action clicked. The bottom hairline is drawn.
#[must_use = "call `.show(ui)`"]
pub struct Header<'a> {
    title: &'a str,
    trailing: Option<&'a str>,
    /// A faint subtitle after the title, before the rule
    /// (`assistant ── why scale by √dₖ`).
    subtitle: Option<&'a str>,
    actions: Vec<Action<'a>>,
    height: f32,
}

pub struct HeaderResponse {
    pub response: Response,
    /// The action clicked, by index.
    pub clicked: Option<usize>,
    /// Each action's response, by index, for menus anchored to them.
    pub actions: Vec<Response>,
}

impl<'a> Header<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            trailing: None,
            subtitle: None,
            actions: Vec::new(),
            height: HEADER_HEIGHT,
        }
    }

    pub fn trailing(mut self, trailing: impl Into<Option<&'a str>>) -> Self {
        self.trailing = trailing.into();
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<Option<&'a str>>) -> Self {
        self.subtitle = subtitle.into().filter(|subtitle| !subtitle.is_empty());
        self
    }

    pub fn action(mut self, action: Action<'a>) -> Self {
        self.actions.push(action);
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn show(self, ui: &mut Ui) -> HeaderResponse {
        let t = Tokens::get(ui.ctx());
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        ui.painter()
            .hline(rect.x_range(), rect.bottom() - 0.5, t.hairline());
        let action_size = 24.0;
        let actions_width = self.actions.len() as f32 * (action_size + 2.0);
        let actions_rect = Rect::from_min_max(
            egui::pos2(rect.right() - 8.0 - actions_width, rect.top()),
            egui::pos2(rect.right() - 8.0, rect.bottom()),
        );
        let title_rect = Rect::from_min_max(
            egui::pos2(rect.left() + 12.0, rect.top()),
            egui::pos2(actions_rect.left() - 8.0, rect.bottom()),
        );
        if let Some(subtitle) = self.subtitle {
            paint_with_subtitle(ui, title_rect, self.title, subtitle, self.trailing);
        } else {
            motif::paint_rule_title(ui, title_rect, self.title, self.trailing, false);
        }
        let mut actions = Vec::new();
        let mut clicked = None;
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(actions_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.spacing_mut().item_spacing.x = 2.0;
        for (index, action) in self.actions.into_iter().enumerate() {
            let response = button::icon(
                &mut child,
                action.icon,
                action_size,
                action.selected,
                action.tooltip,
            );
            if response.clicked() {
                clicked = Some(index);
            }
            actions.push(response);
        }
        HeaderResponse {
            response,
            clicked,
            actions,
        }
    }
}

/// `title ── subtitle ──── trailing`, the subtitle faint and cut first.
fn paint_with_subtitle(ui: &Ui, rect: Rect, title: &str, subtitle: &str, trailing: Option<&str>) {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::mono(11.0);
    let y = rect.center().y;
    let title_rect = crate::text::line(
        ui,
        egui::pos2(rect.left(), y),
        egui::Align2::LEFT_CENTER,
        title,
        font.clone(),
        t.text_2,
        rect.width(),
    );
    let mut x = title_rect.right() + 8.0;
    if t.ornaments {
        ui.painter()
            .hline(x..=x + 12.0, y, egui::Stroke::new(1.0, t.rule_strong));
        x += 20.0;
    }
    let rest = Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
    let trailing_width = trailing.map_or(0.0, |trailing| {
        crate::text::width(ui, trailing, &font) + 8.0
    });
    crate::text::line(
        ui,
        egui::pos2(x, y),
        egui::Align2::LEFT_CENTER,
        subtitle,
        font.clone(),
        t.faint,
        (rest.width() - trailing_width).max(0.0),
    );
    if let Some(trailing) = trailing {
        crate::text::line(
            ui,
            egui::pos2(rect.right(), y),
            egui::Align2::RIGHT_CENTER,
            trailing,
            font,
            t.faint,
            trailing_width,
        );
    }
}

/// A 1px vertical hairline at `x` over `y`.
pub fn vline(ui: &Ui, x: f32, y: egui::Rangef) {
    let t = Tokens::get(ui.ctx());
    ui.painter().vline(x, y, t.hairline());
}

/// A 1px horizontal hairline at `y` over `x`.
pub fn hline(ui: &Ui, x: egui::Rangef, y: f32) {
    let t = Tokens::get(ui.ctx());
    ui.painter().hline(x, y, t.hairline());
}
