//! A paragraph's inlines as one wrapped galley: text runs, code spans,
//! links and formulas. A formula takes a transparent placeholder glyph led
//! by enough space to make up its width, and is painted over that slot,
//! ending at the glyph, once the galley is placed.

use std::{ops::Range, sync::Arc};

use egui::{
    Color32, CursorIcon, FontId, Galley, Label, OpenUrl, Pos2, Rect, Stroke, Ui, Vec2,
    text::{LayoutJob, TextFormat},
    vec2,
};

use crate::{MarkdownStyle, math, parse::Inline};

/// Stands in for a formula: a glyph every text face has (private-use
/// codepoints land in egui's icon font, spaced its own way) and models
/// all but never write.
const PLACEHOLDER: char = '¤';

/// A laid out paragraph, ready to place.
pub(crate) struct Laid {
    pub galley: Arc<Galley>,
    formulas: Vec<Arc<math::Formula>>,
    /// Char ranges of links and where they go.
    links: Vec<(Range<usize>, String)>,
}

/// The font and colour a paragraph's plain text takes.
#[derive(Clone)]
pub(crate) struct Base {
    pub font: FontId,
    pub strong: FontId,
    pub color: Color32,
    /// Everything strong, as in table headers and headings.
    pub all_strong: bool,
}

pub(crate) fn layout(
    ui: &Ui,
    style: &MarkdownStyle,
    base: &Base,
    inlines: &[Inline],
    wrap_width: f32,
) -> Laid {
    let line_height = (base.font.size * style.line_height).round();
    let format = |font: &FontId, color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        line_height: Some(line_height),
        valign: egui::Align::Center,
        ..TextFormat::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap_width;
    let mut formulas = Vec::new();
    let mut links = Vec::new();
    let mut chars = 0;
    let mut append = |job: &mut LayoutJob, text: &str, leading: f32, format: TextFormat| {
        job.append(text, leading, format);
        let start = chars;
        chars += text.chars().count();
        start..chars
    };
    for inline in inlines {
        match inline {
            Inline::Text {
                text,
                strong,
                emphasis,
                strike,
                link,
            } => {
                let strong = *strong || base.all_strong;
                let color = match (link, strong) {
                    (Some(_), _) => style.link,
                    (None, true) => style.strong_text,
                    (None, false) => base.color,
                };
                let mut format = format(if strong { &base.strong } else { &base.font }, color);
                format.italics = *emphasis;
                if *strike {
                    format.strikethrough = Stroke::new(1.0, color);
                }
                if link.is_some() {
                    format.underline = Stroke::new(1.0, style.link.gamma_multiply(0.5));
                }
                let range = append(&mut job, text, 0.0, format);
                if let Some(url) = link {
                    links.push((range, url.clone()));
                }
            }
            Inline::Code(code) => {
                let mut format = format(&style.code, base.color);
                format.background = style.code_bg;
                // Thin spaces pad the background without a wide gap.
                append(&mut job, &format!("\u{2009}{code}\u{2009}"), 0.0, format);
            }
            Inline::Math(tex) => {
                let size = base.font.size * style.math_scale;
                match math::formula(ui.ctx(), tex, false, size, base.color) {
                    Ok(formula) => {
                        let mut format = format(&base.font, Color32::TRANSPARENT);
                        format.line_height = Some(line_height.max(formula.size.y.ceil()));
                        let leading = ui
                            .ctx()
                            .fonts_mut(|fonts| slot(fonts, &format.font_id, formula.size.x));
                        append(&mut job, &PLACEHOLDER.to_string(), leading, format);
                        formulas.push(formula);
                    }
                    Err(_) => {
                        let mut format = format(&style.code, base.color);
                        format.background = style.code_bg;
                        append(&mut job, &format!("${tex}$"), 0.0, format);
                    }
                }
            }
            Inline::Break => {
                append(&mut job, "\n", 0.0, format(&base.font, base.color));
            }
        }
    }
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    Laid {
        galley,
        formulas,
        links,
    }
}

/// The space before a placeholder in `font` that makes its slot `width`
/// points wide.
fn slot(fonts: &mut egui::epaint::text::FontsView<'_>, font: &FontId, width: f32) -> f32 {
    (width - fonts.glyph_width(font, PLACEHOLDER)).max(0.0)
}

/// Adds `laid` at the cursor: selectable text, clickable links and the
/// formulas over their placeholders.
pub(crate) fn show(ui: &mut Ui, laid: Laid) {
    let response = ui.add(Label::new(laid.galley.clone()).selectable(true));
    place(ui, &laid, response.rect.min);
    if laid.links.is_empty() {
        return;
    }
    let link_at = |pos: Pos2| {
        let index = laid.galley.cursor_from_pos(pos - response.rect.min).index.0;
        laid.links
            .iter()
            .find(|(range, _)| range.contains(&index))
            .map(|(_, url)| url)
    };
    if let Some(pos) = response.hover_pos()
        && link_at(pos).is_some()
    {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    if response.clicked()
        && let Some(url) = response.interact_pointer_pos().and_then(link_at)
    {
        ui.ctx().open_url(OpenUrl::new_tab(url));
    }
}

/// Paints the formulas of a galley drawn at `origin`.
pub(crate) fn place(ui: &Ui, laid: &Laid, origin: Pos2) {
    if laid.formulas.is_empty() {
        return;
    }
    let mut formulas = laid.formulas.iter();
    let placeholders = laid.galley.rows.iter().flat_map(|row| {
        row.glyphs
            .iter()
            .filter(|glyph| glyph.chr == PLACEHOLDER)
            .map(move |glyph| (row, glyph))
    });
    for (row, glyph) in placeholders {
        let Some(formula) = formulas.next() else {
            return;
        };
        let end = origin + row.pos.to_vec2() + vec2(glyph.max_x(), glyph.pos.y);
        let rect = Rect::from_min_size(
            Pos2::new(end.x - formula.size.x, end.y - formula.ascent),
            formula.size,
        );
        ui.painter().image(
            formula.texture.id(),
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// The unwrapped width of `inlines`, for sizing table columns.
pub(crate) fn natural_width(
    ui: &Ui,
    style: &MarkdownStyle,
    base: &Base,
    inlines: &[Inline],
) -> f32 {
    layout(ui, style, base, inlines, f32::INFINITY)
        .galley
        .size()
        .x
}

/// The widest piece of `inlines` that can't wrap: a word, a CJK character,
/// a code span or a formula, so a column never cuts a word in two.
pub(crate) fn min_width(ui: &Ui, style: &MarkdownStyle, base: &Base, inlines: &[Inline]) -> f32 {
    let measure = |text: &str, font: &FontId| {
        ui.ctx().fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let mut widest = 0.0_f32;
    for inline in inlines {
        let width = match inline {
            Inline::Text { text, strong, .. } => {
                let font = if *strong || base.all_strong {
                    &base.strong
                } else {
                    &base.font
                };
                text.split(|c: char| c.is_whitespace() || is_wide(c))
                    .map(|word| measure(word, font))
                    .chain(
                        text.chars()
                            .filter(|c| is_wide(*c))
                            .map(|c| measure(&c.to_string(), font)),
                    )
                    .fold(0.0, f32::max)
            }
            Inline::Code(code) => measure(code, &style.code),
            Inline::Math(tex) => {
                let size = base.font.size * style.math_scale;
                math::formula(ui.ctx(), tex, false, size, base.color)
                    .map_or_else(|_| measure(tex, &style.code), |formula| formula.size.x)
            }
            Inline::Break => 0.0,
        };
        widest = widest.max(width);
    }
    widest
}

/// CJK and other wide characters, between any two of which a line may
/// break.
fn is_wide(c: char) -> bool {
    matches!(c, '\u{2E80}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}')
}

/// The size `laid` takes.
pub(crate) fn size(laid: &Laid) -> Vec2 {
    laid.galley.size()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slot_spans_its_width_after_the_text() {
        let ctx = egui::Context::default();
        let mut laid = None;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            laid = Some(ui.ctx().fonts_mut(|fonts| {
                let font = FontId::proportional(14.0);
                let format = TextFormat::simple(font.clone(), Color32::WHITE);
                let mut job = LayoutJob::default();
                job.append("needs ", 0.0, format.clone());
                let leading = slot(fonts, &font, 37.0);
                job.append(&PLACEHOLDER.to_string(), leading, format.clone());
                job.append(" after", 0.0, format);
                let before = fonts
                    .layout_no_wrap("needs ".into(), font, Color32::WHITE)
                    .size()
                    .x;
                (fonts.layout_job(job), before)
            }));
        });
        out.textures_delta.clear();
        let (galley, before) = laid.unwrap();
        let glyph = galley.rows[0]
            .glyphs
            .iter()
            .find(|glyph| glyph.chr == PLACEHOLDER)
            .unwrap();
        // Glyph positions snap to whole pixels.
        let start = glyph.max_x() - 37.0;
        assert!((start - before).abs() < 1.0, "{start} vs {before}");
    }
}
