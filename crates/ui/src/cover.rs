//! Document covers (pen component `doc-cover`): a 132 × 176 card on
//! `raised` with the kind and length (`pdf · 15p`), a block of ASCII art
//! picked from the title, the title in mono and a progress meter. Real
//! covers are not rendered; the art is stable per title.

use egui::{Align2, Color32, Rect, Response, Sense, StrokeKind, Ui, vec2};

use crate::{
    meter::Meter,
    theme::{self, Tokens},
};

/// The tone of a cover's art.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Info,
    Ok,
    Err,
    Text,
}

impl Tone {
    fn color(self, t: &Tokens) -> Color32 {
        match self {
            Self::Accent => t.accent,
            Self::Info => t.info,
            Self::Ok => t.ok,
            Self::Err => t.err,
            Self::Text => t.text_2,
        }
    }
}

/// ASCII art for a cover: lines of mono text and their tone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub lines: Vec<String>,
    pub tone: Tone,
}

/// FNV-1a, so the art is stable across runs and platforms.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The art for `title`, one of six patterns picked from its hash.
pub fn art(title: &str) -> Art {
    let h = hash(title);
    let word = title
        .split(|c: char| !c.is_alphanumeric())
        .find(|word| word.len() >= 3)
        .unwrap_or("text")
        .to_uppercase();
    let word: String = word.chars().take(6).collect();
    let lines = |lines: &[&str]| lines.iter().map(|line| (*line).to_owned()).collect();
    match h % 6 {
        0 => Art {
            lines: lines(&[
                "  ·  ·  ●  ·  · ",
                " ·  •  ●  •  ·  ",
                "·  •  ●●●  •  · ",
                " ·  •  ●  •  ·  ",
                "  ·  ·  ●  ·  · ",
                "·  ·  ·  ·  ·  ·",
            ]),
            tone: Tone::Accent,
        },
        1 => Art {
            lines: vec![
                "/\\/\\/\\/\\/\\/\\".into(),
                "\\/\\/\\/\\/\\/\\/".into(),
                format!("/\\[{word}]\\/\\"),
                "\\/\\/\\/\\/\\/\\/".into(),
                "/\\/\\/\\/\\/\\/\\".into(),
            ],
            tone: Tone::Info,
        },
        2 => Art {
            lines: lines(&["  (λ (x)", "    (λ (y)", "      (x y))", "  )", "  ; "])
                .into_iter()
                .enumerate()
                .map(|(index, line): (usize, String)| {
                    if index == 4 {
                        format!("{line}{}", word.to_lowercase())
                    } else {
                        line
                    }
                })
                .collect(),
            tone: Tone::Ok,
        },
        3 => Art {
            lines: lines(&[
                "┌──────────┐",
                "│▇▇▇▇▇▇▇▇▇▇│",
                "├──────────┤",
                "│▇▇▇▇▇▇▇▇▇▇│",
                "└──────────┘",
            ]),
            tone: Tone::Text,
        },
        4 => Art {
            lines: lines(&[
                "fn main() {",
                "  let r = &x;",
                "  borrow(r);",
                "}  // 'a",
                "<T: Send>",
            ]),
            tone: Tone::Err,
        },
        _ => {
            let bars: String = (0..12)
                .map(|index| match (h >> (index * 2)) & 3 {
                    0 => '█',
                    1 => '▌',
                    2 => '▐',
                    _ => '▀',
                })
                .collect();
            Art {
                lines: vec![
                    bars.clone(),
                    bars.chars().rev().collect(),
                    bars,
                    format!("  vol. {:02}", h % 40 + 1),
                ],
                tone: Tone::Text,
            }
        }
    }
}

/// A cover card. Returns its response (click to open).
#[must_use = "call `.show(ui)`"]
pub struct DocCover<'a> {
    pub title: &'a str,
    /// `pdf · 15p`.
    pub kind: &'a str,
    pub progress: Option<f32>,
    /// The text after the meter: `27%`, `done`, `new`.
    pub progress_label: &'a str,
    pub size: egui::Vec2,
}

impl<'a> DocCover<'a> {
    pub fn new(title: &'a str, kind: &'a str) -> Self {
        Self {
            title,
            kind,
            progress: None,
            progress_label: "",
            size: vec2(132.0, 176.0),
        }
    }

    pub fn progress(mut self, progress: Option<f32>, label: &'a str) -> Self {
        self.progress = progress;
        self.progress_label = label;
        self
    }

    pub fn size(mut self, size: egui::Vec2) -> Self {
        self.size = size;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(self.size, Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), self.title)
        });
        self.paint(ui, rect, response.hovered());
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    }

    pub fn paint(&self, ui: &Ui, rect: Rect, hovered: bool) {
        let t = Tokens::get(ui.ctx());
        let edge = if hovered {
            egui::Stroke::new(1.0, t.accent)
        } else {
            t.hairline()
        };
        ui.painter()
            .rect(rect, 0, t.raised, edge, StrokeKind::Inside);
        let inner = rect.shrink(10.0);
        let painter = ui.painter_at(rect);
        painter.text(
            inner.left_top(),
            Align2::LEFT_TOP,
            self.kind,
            theme::mono(9.0),
            t.faint,
        );
        let scale = (rect.width() / 132.0).clamp(0.5, 2.0);
        let art = art(self.title);
        let art_font = theme::mono(9.0 * scale);
        let mut y = inner.top() + 18.0 * scale;
        let line_height = 11.0 * scale;
        for line in &art.lines {
            painter.text(
                egui::pos2(inner.left(), y),
                Align2::LEFT_TOP,
                line,
                art_font.clone(),
                art.tone.color(&t),
            );
            y += line_height;
        }
        // The title, up to two lines, above the meter.
        let title_font = theme::mono(11.0 * scale.min(1.2));
        let mut job = egui::text::LayoutJob::simple(
            self.title.to_lowercase(),
            title_font,
            t.text,
            inner.width(),
        );
        job.wrap.max_rows = 2;
        job.wrap.break_anywhere = false;
        job.wrap.overflow_character = Some('…');
        let galley = ui.painter().layout_job(job);
        let meter_top = inner.bottom() - 12.0;
        let title_top = meter_top - 6.0 - galley.size().y;
        painter.galley(egui::pos2(inner.left(), title_top), galley, t.text);
        let meter_rect = Rect::from_min_size(
            egui::pos2(inner.left(), meter_top),
            vec2(inner.width().min(90.0 * scale), 12.0),
        );
        let label = if self.progress_label.is_empty() {
            None
        } else {
            Some(self.progress_label)
        };
        let mut meter = Meter::new(self.progress.unwrap_or(0.0))
            .bar()
            .color(t.text_2);
        if let Some(label) = label {
            meter = meter.label(label);
        }
        meter.paint(ui, meter_rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_is_stable_per_title_and_varies_between_titles() {
        assert_eq!(
            art("Attention Is All You Need"),
            art("Attention Is All You Need")
        );
        let titles = [
            "Attention Is All You Need",
            "BERT: Pre-training",
            "Structure and Interpretation",
            "Designing Data-Intensive Applications",
            "The Rust Programming Language",
            "Blame! vol. 3",
            "Deep Learning",
            "Crafting Interpreters",
        ];
        let distinct: std::collections::HashSet<_> =
            titles.iter().map(|title| art(title).lines).collect();
        assert!(distinct.len() >= 3, "covers should differ");
        for title in titles {
            assert!(!art(title).lines.is_empty());
        }
    }
}
