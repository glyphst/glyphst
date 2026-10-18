//! The status bar (pen component `status-seg`): a 24pt bar along the
//! window's bottom, segments of an icon and mono 11 text split by
//! hairlines. Segments on the left read left to right, those on the right
//! right to left; when the bar is too narrow the lowest-priority segments
//! go first.

use egui::{Align2, Rect, Response, Sense, Ui, Vec2, vec2};

use crate::{
    icons, text,
    theme::{self, Tokens},
};

/// The bar's height.
pub const HEIGHT: f32 = 24.0;

/// Which end of the bar a segment sits at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Custom drawing of a segment in its rect.
pub type Draw<'a> = Box<dyn FnOnce(&mut Ui, Rect) + 'a>;

/// What a segment draws.
pub enum Content<'a> {
    /// An icon and text; either may be empty.
    Label {
        icon: Option<&'a str>,
        text: String,
        accent: bool,
    },
    /// Custom drawing in a fixed width (a meter, a page field).
    Custom { width: f32, draw: Draw<'a> },
}

/// One segment; `priority` decides which stay when narrow (higher stays).
pub struct Segment<'a> {
    pub id: &'static str,
    pub side: Side,
    pub priority: u8,
    pub content: Content<'a>,
    pub tooltip: String,
    /// Clickable: hover fill and a click response.
    pub clickable: bool,
    /// No hairline after it, to group with its neighbour.
    pub joined: bool,
}

impl<'a> Segment<'a> {
    pub fn label(
        id: &'static str,
        side: Side,
        icon: Option<&'a str>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            id,
            side,
            priority: 5,
            content: Content::Label {
                icon,
                text: text.into(),
                accent: false,
            },
            tooltip: String::new(),
            clickable: true,
            joined: false,
        }
    }

    pub fn custom(
        id: &'static str,
        side: Side,
        width: f32,
        draw: impl FnOnce(&mut Ui, Rect) + 'a,
    ) -> Self {
        Self {
            id,
            side,
            priority: 5,
            content: Content::Custom {
                width,
                draw: Box::new(draw),
            },
            tooltip: String::new(),
            clickable: false,
            joined: false,
        }
    }

    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    /// Accent text and icon: the assistant while it is open.
    pub fn accent(mut self, on: bool) -> Self {
        if let Content::Label { accent, .. } = &mut self.content {
            *accent = on;
        }
        self
    }

    pub fn static_text(mut self) -> Self {
        self.clickable = false;
        self
    }

    pub fn joined(mut self) -> Self {
        self.joined = true;
        self
    }

    fn width(&self, ui: &Ui) -> f32 {
        match &self.content {
            Content::Label { icon, text, .. } => {
                let mut width = 20.0;
                if icon.is_some() {
                    width += 12.0;
                }
                if !text.is_empty() {
                    width += text::width(ui, text, &theme::mono(11.0));
                    if icon.is_some() {
                        width += 6.0;
                    }
                }
                width
            }
            Content::Custom { width, .. } => *width,
        }
    }
}

/// Which segments fit in `available`: indices kept, dropping the lowest
/// priority first (the later one among equals).
pub fn fit(widths: &[f32], priorities: &[u8], available: f32) -> Vec<bool> {
    let mut keep = vec![true; widths.len()];
    let mut total: f32 = widths.iter().sum();
    let mut order: Vec<usize> = (0..widths.len()).collect();
    order.sort_by_key(|&index| (priorities[index], std::cmp::Reverse(index)));
    for index in order {
        if total <= available {
            break;
        }
        keep[index] = false;
        total -= widths[index];
    }
    keep
}

/// Draws `segments` across `ui`'s full width and returns each clickable
/// segment's response by id.
pub fn bar(ui: &mut Ui, segments: Vec<Segment<'_>>) -> Vec<(&'static str, Response)> {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0, t.surface);
    ui.painter()
        .hline(rect.x_range(), rect.top() + 0.5, t.hairline());
    let widths: Vec<f32> = segments.iter().map(|segment| segment.width(ui)).collect();
    let priorities: Vec<u8> = segments.iter().map(|segment| segment.priority).collect();
    let keep = fit(&widths, &priorities, rect.width());
    let mut left = rect.left();
    let mut right = rect.right();
    let mut responses = Vec::new();
    for ((segment, width), keep) in segments.into_iter().zip(widths).zip(keep) {
        if !keep {
            continue;
        }
        let cell = match segment.side {
            Side::Left => {
                let cell =
                    Rect::from_min_size(egui::pos2(left, rect.top()), vec2(width, rect.height()));
                left += width;
                cell
            }
            Side::Right => {
                let cell = Rect::from_min_size(
                    egui::pos2(right - width, rect.top()),
                    vec2(width, rect.height()),
                );
                right -= width;
                cell
            }
        };
        if !segment.joined {
            let x = match segment.side {
                Side::Left => cell.right() - 0.5,
                Side::Right => cell.left() + 0.5,
            };
            ui.painter().vline(x, cell.y_range(), t.hairline());
        }
        let id = ui.scope_id().with(("status", segment.id));
        let sense = if segment.clickable {
            Sense::click()
        } else {
            Sense::hover()
        };
        let mut response = ui.interact(cell, id, sense);
        if !segment.tooltip.is_empty() {
            response = crate::overlay::tooltip(response, &segment.tooltip, None);
        }
        match segment.content {
            Content::Label { icon, text, accent } => {
                let fill = if segment.clickable && response.hovered() {
                    t.hover
                } else {
                    egui::Color32::TRANSPARENT
                };
                ui.painter().rect_filled(
                    cell.shrink2(vec2(1.0, 1.0)),
                    0,
                    crate::motion::color(ui.ctx(), response.id, fill),
                );
                let hot = segment.clickable && response.hovered();
                let (icon_color, text_color) = if accent {
                    (t.accent_text, t.accent_text)
                } else if hot {
                    (t.text, t.text)
                } else {
                    (t.faint, t.muted)
                };
                let mut x = cell.left() + 10.0;
                if let Some(icon) = icon {
                    icons::paint(
                        ui,
                        Rect::from_center_size(
                            egui::pos2(x + 6.0, cell.center().y),
                            Vec2::splat(12.0),
                        ),
                        icon,
                        12.0,
                        icon_color,
                    );
                    x += 18.0;
                }
                if !text.is_empty() {
                    ui.painter().text(
                        egui::pos2(x, cell.center().y),
                        Align2::LEFT_CENTER,
                        text,
                        theme::mono(11.0),
                        text_color,
                    );
                }
            }
            Content::Custom { draw, .. } => {
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(cell)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                draw(&mut child, cell);
            }
        }
        responses.push((segment.id, response));
    }
    responses
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn narrow_bars_drop_the_lowest_priority_first() {
        let widths = [50.0, 50.0, 50.0, 50.0];
        let priorities = [9, 2, 5, 2];
        assert_eq!(fit(&widths, &priorities, 400.0), [true; 4]);
        // Of the two at priority 2, the later one goes first.
        assert_eq!(fit(&widths, &priorities, 160.0), [true, true, true, false]);
        assert_eq!(fit(&widths, &priorities, 100.0), [true, false, true, false]);
        assert_eq!(fit(&widths, &priorities, 0.0), [false; 4]);
    }
}
