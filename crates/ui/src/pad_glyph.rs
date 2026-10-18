//! Gamepad glyphs and hints (frame 01b: pad/face, pad/button, pad/dpad,
//! pad-hint, each at chrome and couch size).
//!
//! Face buttons are lettered circles, as games draw them; the letter (or
//! PlayStation's shape) follows the pad's brand by position, which the app
//! sets with [`set_labels`]. Bumpers, triggers, start, select and the stick
//! clicks are `text` pills with their names; the d-pad is a cross with its
//! pressed arm filled.
//!
//! Hints follow the last input, as games show them: pad glyphs after a pad,
//! key caps after the keyboard. The app says which it was each frame with
//! [`set_input`].

use egui::{Align2, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, Widget, pos2, vec2};

use crate::{
    text,
    theme::{self, Tokens},
};

/// Where the last input came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputDevice {
    #[default]
    Pointer,
    Keyboard,
    Pad,
}

fn input_id() -> egui::Id {
    egui::Id::new("glyphst-pad-input")
}

pub fn set_input(ctx: &egui::Context, device: InputDevice) {
    ctx.data_mut(|data| data.insert_temp(input_id(), device));
}

pub fn input(ctx: &egui::Context) -> InputDevice {
    ctx.data(|data| data.get_temp(input_id()))
        .unwrap_or_default()
}

/// What the face buttons are labelled with: the pad's brand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Labels {
    /// Xbox's letters, which Steam's and most other pads share.
    #[default]
    Xbox,
    /// Nintendo's letters, which sit the other way round.
    Nintendo,
    /// PlayStation's shapes.
    PlayStation,
}

/// What a face button's circle holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Letter(&'static str),
    Cross,
    Circle,
    Square,
    Triangle,
}

impl Labels {
    pub fn mark(self, face: Face) -> Mark {
        match (self, face) {
            (Self::Xbox, Face::South) | (Self::Nintendo, Face::East) => Mark::Letter("A"),
            (Self::Xbox, Face::East) | (Self::Nintendo, Face::South) => Mark::Letter("B"),
            (Self::Xbox, Face::West) | (Self::Nintendo, Face::North) => Mark::Letter("X"),
            (Self::Xbox, Face::North) | (Self::Nintendo, Face::West) => Mark::Letter("Y"),
            (Self::PlayStation, Face::South) => Mark::Cross,
            (Self::PlayStation, Face::East) => Mark::Circle,
            (Self::PlayStation, Face::West) => Mark::Square,
            (Self::PlayStation, Face::North) => Mark::Triangle,
        }
    }
}

fn labels_id() -> egui::Id {
    egui::Id::new("glyphst-pad-labels")
}

pub fn set_labels(ctx: &egui::Context, labels: Labels) {
    ctx.data_mut(|data| data.insert_temp(labels_id(), labels));
}

pub fn labels(ctx: &egui::Context) -> Labels {
    ctx.data(|data| data.get_temp(labels_id()))
        .unwrap_or_default()
}

fn faces_id() -> egui::Id {
    egui::Id::new("glyphst-pad-faces")
}

/// The pad's face buttons pressed this frame, for prompts and toasts; the
/// app sets them before the frame.
pub fn push_faces(ctx: &egui::Context, faces: &[Face]) {
    ctx.data_mut(|data| data.insert_temp(faces_id(), faces.to_vec()));
}

pub fn pressed_faces(ctx: &egui::Context) -> Vec<Face> {
    ctx.data(|data| data.get_temp::<Vec<Face>>(faces_id()))
        .unwrap_or_default()
}

/// A face button by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    North,
    East,
    South,
    West,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arm {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Face(Face),
    /// A pill with a name: `lb`, `rt`, `≡` (start), `select`.
    Button(&'static str),
    /// The d-pad, with the arm pressed (none: any).
    Dpad(Option<Arm>),
    /// A stick.
    Stick,
}

/// The two sizes of the design: beside the chrome's mono 12, and in
/// gamepad mode's hint bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scale {
    #[default]
    Chrome,
    Couch,
}

impl Scale {
    /// A d-pad arm's size; the cross, and a face button, are three across.
    fn dot(self) -> f32 {
        match self {
            Self::Chrome => 5.0,
            Self::Couch => 8.0,
        }
    }

    fn label(self) -> egui::FontId {
        match self {
            Self::Chrome => theme::mono(12.0),
            Self::Couch => theme::mono(16.0),
        }
    }

    fn pill(self) -> (egui::FontId, f32, f32) {
        // Font, side padding and height.
        match self {
            Self::Chrome => (theme::mono_bold(10.0), 7.0, 15.0),
            Self::Couch => (theme::mono_bold(15.0), 12.0, 24.0),
        }
    }

    fn gap(self) -> f32 {
        match self {
            Self::Chrome => 6.0,
            Self::Couch => 10.0,
        }
    }
}

/// The glyph's size at `scale`.
pub fn size(ui: &Ui, glyph: Glyph, scale: Scale) -> Vec2 {
    let d = scale.dot();
    match glyph {
        Glyph::Face(_) | Glyph::Dpad(_) => Vec2::splat(3.0 * d),
        Glyph::Button(name) => {
            let (font, pad, height) = scale.pill();
            vec2(text::width(ui, name, &font) + 2.0 * pad, height)
        }
        Glyph::Stick => {
            let font = scale.label();
            vec2(text::width(ui, "◎", &font), 3.0 * d)
        }
    }
}

/// Paints `glyph` in `rect`. `on` is the colour of what is pressed (and the
/// pill), `off` the outline of the rest; `None` takes `text` and `muted`.
pub fn paint(ui: &Ui, rect: Rect, glyph: Glyph, scale: Scale, on: Option<egui::Color32>) {
    let t = Tokens::get(ui.ctx());
    let (on, off) = match on {
        Some(color) => (color, Tokens::fade(color, 0.55)),
        None => (t.text, t.muted),
    };
    let painter = ui.painter();
    let d = scale.dot();
    // The diamond's four places, from its top-left corner.
    let corner = rect.center() - Vec2::splat(1.5 * d);
    let place = |x: f32, y: f32| corner + vec2(x * d, y * d);
    match glyph {
        Glyph::Face(face) => {
            let center = rect.center();
            let radius = 1.5 * d;
            painter.circle_filled(center, radius, on);
            // The mark in whichever of `bg` and `text` stands out on the
            // circle (an accent choice's circle is dark).
            let lightness = |c: egui::Color32| egui::Rgba::from(c).intensity();
            let mark = if (lightness(on) - lightness(t.bg)).abs()
                >= (lightness(on) - lightness(t.text)).abs()
            {
                t.bg
            } else {
                t.text
            };
            let ink = Stroke::new(d / 4.0, mark);
            let r = radius * 0.45;
            match labels(ui.ctx()).mark(face) {
                Mark::Letter(letter) => {
                    let (font, _, _) = scale.pill();
                    painter.text(center, Align2::CENTER_CENTER, letter, font, mark);
                }
                Mark::Cross => {
                    painter.line_segment([center + vec2(-r, -r), center + vec2(r, r)], ink);
                    painter.line_segment([center + vec2(-r, r), center + vec2(r, -r)], ink);
                }
                Mark::Circle => {
                    painter.circle_stroke(center, r, ink);
                }
                Mark::Square => {
                    let square = Rect::from_center_size(center, Vec2::splat(1.6 * r));
                    painter.rect_stroke(square, 0.0, ink, StrokeKind::Middle);
                }
                Mark::Triangle => {
                    let points = [-90.0_f32, 30.0, 150.0].map(|degrees| {
                        let angle = degrees.to_radians();
                        center + vec2(angle.cos(), angle.sin() + 0.15) * r * 1.1
                    });
                    painter.add(egui::Shape::closed_line(points.to_vec(), ink));
                }
            }
        }
        Glyph::Dpad(arm) => {
            let arms = [
                (Arm::Up, place(1.5, 0.5)),
                (Arm::Left, place(0.5, 1.5)),
                (Arm::Right, place(2.5, 1.5)),
                (Arm::Down, place(1.5, 2.5)),
            ];
            for (which, at) in arms {
                let square = Rect::from_center_size(at, Vec2::splat(d));
                if arm == Some(which) {
                    painter.rect_filled(square, 1.0, on);
                } else {
                    painter.rect_stroke(square, 1.0, Stroke::new(1.5, off), StrokeKind::Inside);
                }
            }
        }
        Glyph::Button(name) => {
            let (font, _, height) = scale.pill();
            let pill = Rect::from_center_size(rect.center(), vec2(rect.width(), height));
            painter.rect_filled(pill, height / 2.0, on);
            painter.text(pill.center(), Align2::CENTER_CENTER, name, font, t.bg);
        }
        Glyph::Stick => {
            painter.text(rect.center(), Align2::CENTER_CENTER, "◎", scale.label(), on);
        }
    }
}

/// A glyph and what it does (pen component `pad-hint`): `(A) select`.
#[derive(Clone, Copy)]
pub struct Hint<'a> {
    pub glyphs: &'a [Glyph],
    pub label: &'a str,
    pub scale: Scale,
    /// The label's colour; `text_2` by default.
    pub color: Option<egui::Color32>,
}

impl<'a> Hint<'a> {
    pub fn new(glyphs: &'a [Glyph], label: &'a str) -> Self {
        Self {
            glyphs,
            label,
            scale: Scale::Chrome,
            color: None,
        }
    }

    pub fn couch(mut self) -> Self {
        self.scale = Scale::Couch;
        self
    }

    pub fn color(mut self, color: egui::Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Its width and height.
    pub fn size(&self, ui: &Ui) -> Vec2 {
        let gap = self.scale.gap();
        let mut width = 0.0;
        let mut height: f32 = 0.0;
        for glyph in self.glyphs {
            let size = size(ui, *glyph, self.scale);
            width += size.x + gap / 2.0;
            height = height.max(size.y);
        }
        let font = self.scale.label();
        width += gap / 2.0 + text::width(ui, self.label, &font);
        vec2(width, height.max(font.size * 1.3))
    }

    /// Paints it with its left edge at `left`, centred on `middle`.
    pub fn paint(&self, ui: &Ui, left: f32, middle: f32) {
        let t = Tokens::get(ui.ctx());
        let gap = self.scale.gap();
        let mut x = left;
        for glyph in self.glyphs {
            let size = size(ui, *glyph, self.scale);
            let rect = Rect::from_min_size(pos2(x, middle - size.y / 2.0), size);
            paint(ui, rect, *glyph, self.scale, None);
            x += size.x + gap / 2.0;
        }
        x += gap / 2.0;
        ui.painter().text(
            Pos2::new(x, middle),
            Align2::LEFT_CENTER,
            self.label,
            self.scale.label(),
            self.color.unwrap_or(t.text_2),
        );
    }
}

impl Widget for Hint<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(self.size(ui), Sense::hover());
        self.paint(ui, rect.left(), rect.center().y);
        response
    }
}

/// The gallery's sample: every glyph, and hints at both sizes.
pub fn sample(ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for face in [Face::South, Face::East, Face::West, Face::North] {
            ui.add(Hint::new(&[Glyph::Face(face)], ""));
        }
        for name in ["lb", "rb", "lt", "rt", "≡", "select"] {
            ui.add(Hint::new(&[Glyph::Button(name)], ""));
        }
        ui.add(Hint::new(&[Glyph::Dpad(Some(Arm::Up))], ""));
        ui.add(Hint::new(&[Glyph::Stick], ""));
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 24.0;
        ui.add(Hint::new(&[Glyph::Face(Face::South)], "select"));
        ui.add(Hint::new(&[Glyph::Face(Face::East)], "back"));
        ui.add(Hint::new(&[Glyph::Face(Face::South)], "select").couch());
        ui.add(Hint::new(&[Glyph::Button("lb"), Glyph::Button("rb")], "page").couch());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn face_buttons_are_labelled_by_brand_and_position() {
        let marks = |labels: Labels| {
            [Face::South, Face::East, Face::West, Face::North].map(|face| labels.mark(face))
        };
        let letters = |l: [&'static str; 4]| l.map(Mark::Letter);
        assert_eq!(marks(Labels::Xbox), letters(["A", "B", "X", "Y"]));
        assert_eq!(
            marks(Labels::Nintendo),
            letters(["B", "A", "Y", "X"]),
            "Nintendo's sit the other way round"
        );
        assert_eq!(
            marks(Labels::PlayStation),
            [Mark::Cross, Mark::Circle, Mark::Square, Mark::Triangle]
        );
    }
}
