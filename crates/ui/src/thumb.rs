//! Page thumbnails (pen component `page-thumb`): a page on `page` with its
//! number under it in faint mono; the current page gets an accent ring and
//! `● current`. Without a rendered image, placeholder text lines.

use egui::{Align2, Rect, Response, Sense, StrokeKind, Ui, vec2};

use crate::theme::{self, Tokens};

/// A thumbnail of `page_size` (the page's own size, any unit) fitting in
/// `width`, with `label` under it. `texture` draws the rendered page.
pub struct PageThumb<'a> {
    pub label: &'a str,
    pub current: bool,
    pub texture: Option<egui::TextureId>,
    pub aspect: f32,
    pub width: f32,
}

impl PageThumb<'_> {
    pub fn height(&self) -> f32 {
        self.width / self.aspect.max(0.1) + 19.0
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) =
            ui.allocate_exact_size(vec2(self.width, self.height()), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::Button,
                ui.is_enabled(),
                self.current,
                self.label,
            )
        });
        self.paint(ui, rect, response.hovered());
        response
    }

    /// The page's rect in a thumbnail at `rect`.
    pub fn page_rect(&self, rect: Rect) -> Rect {
        Rect::from_min_size(
            rect.min,
            vec2(self.width, self.width / self.aspect.max(0.1)),
        )
    }

    pub fn paint(&self, ui: &Ui, rect: Rect, hovered: bool) {
        let t = Tokens::get(ui.ctx());
        let page = self.page_rect(rect);
        ui.painter().rect_filled(page, 0, t.page);
        match self.texture {
            Some(texture) => {
                ui.painter().image(
                    texture,
                    page,
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => paint_placeholder_lines(ui, page),
        }
        if self.current {
            ui.painter().rect_stroke(
                page.expand(1.0),
                0,
                egui::Stroke::new(2.0, t.accent),
                StrokeKind::Outside,
            );
        } else if hovered {
            ui.painter()
                .rect_stroke(page, 0, t.edge(), StrokeKind::Outside);
        }
        let label = if self.current {
            format!("{} ● current", self.label)
        } else {
            self.label.to_owned()
        };
        ui.painter().text(
            egui::pos2(page.center().x, page.bottom() + 6.0),
            Align2::CENTER_TOP,
            label,
            theme::mono(10.0),
            if self.current { t.accent_text } else { t.faint },
        );
    }
}

/// Grey text lines on a page that has not rendered yet.
pub fn paint_placeholder_lines(ui: &Ui, page: Rect) {
    let t = Tokens::get(ui.ctx());
    let ink = Tokens::fade(t.page_ink, 0.15);
    let inner = page.shrink2(vec2(page.width() * 0.1, page.height() * 0.1));
    let widths = [0.8, 1.0, 1.0, 0.92, 1.0, 0.55, 1.0, 0.95, 1.0, 0.76];
    let step = (inner.height() / widths.len() as f32).max(4.0);
    for (index, width) in widths.iter().enumerate() {
        let y = inner.top() + index as f32 * step;
        if y + 3.0 > inner.bottom() {
            break;
        }
        ui.painter().rect_filled(
            Rect::from_min_size(
                egui::pos2(inner.left(), y),
                vec2(inner.width() * width, 3.0_f32.min(step * 0.4)),
            ),
            0,
            ink,
        );
    }
}
