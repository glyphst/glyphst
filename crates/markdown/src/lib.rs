//! Markdown for the Reader's assistant: paragraphs that wrap at any width,
//! real bold, tables whose cells wrap, LaTeX formulas and mermaid diagrams.
//! Parsed once per text ([`MarkdownCache`]) into blocks laid out with egui
//! galleys.

mod dollars;
mod inline;
mod math;
mod mermaid;
mod parse;
mod table;

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use egui::{
    Color32, CornerRadius, FontId, Frame, Label, Margin, Rect, ScrollArea, Sense, Stroke, Ui,
    text::{LayoutJob, TextFormat},
    vec2,
};
use glyphst_ui::theme::{self, Tokens};

use inline::Base;
use parse::{Block, Item};

/// Fonts, colours and spacing of rendered Markdown.
#[derive(Clone, Debug)]
pub struct MarkdownStyle {
    pub body: FontId,
    /// Bold text: the same size in the semibold family, which carries the
    /// bold CJK face.
    pub strong: FontId,
    /// Code spans.
    pub code: FontId,
    /// Code blocks.
    pub code_block: FontId,
    /// Line height as a multiple of the font size.
    pub line_height: f32,
    pub text: Color32,
    pub strong_text: Color32,
    pub muted: Color32,
    pub link: Color32,
    pub code_bg: Color32,
    pub block_bg: Color32,
    pub rule: Color32,
    pub rule_strong: Color32,
    pub radius: CornerRadius,
    /// Space between blocks.
    pub gap: f32,
    /// Formulas against the text size: KaTeX's fonts run small.
    pub math_scale: f32,
    pub dark: bool,
    pub accent: Color32,
    pub error: Color32,
}

impl MarkdownStyle {
    /// The design's answer prose (Inter `size`, line height 1.55) and code
    /// blocks (JetBrains Mono 11 on `bg` with a `rule` edge).
    pub fn new(tokens: &Tokens, size: f32) -> Self {
        Self {
            body: theme::regular(size),
            strong: theme::semibold(size),
            code: theme::mono((size * 0.86).round()),
            code_block: theme::mono(11.0),
            line_height: theme::PROSE_LINE_HEIGHT,
            text: tokens.text_2,
            strong_text: tokens.text,
            muted: tokens.muted,
            link: tokens.accent_text,
            code_bg: tokens.raised,
            block_bg: tokens.bg,
            rule: tokens.rule,
            rule_strong: tokens.rule_strong,
            radius: tokens.ctl_radius(),
            gap: 10.0,
            math_scale: 1.12,
            dark: tokens.dark(),
            accent: tokens.accent,
            error: tokens.err,
        }
    }

    fn base(&self) -> Base {
        Base {
            font: self.body.clone(),
            strong: self.strong.clone(),
            color: self.text,
            all_strong: false,
        }
    }
}

/// Parsed texts by hash, so a long chat isn't parsed again every frame.
#[derive(Default)]
pub struct MarkdownCache {
    docs: HashMap<u64, (Arc<Vec<Block>>, u64)>,
}

/// Texts kept before those not drawn lately are dropped.
const CACHE_SIZE: usize = 256;

impl MarkdownCache {
    fn blocks(&mut self, text: &str, frame: u64) -> Arc<Vec<Block>> {
        let mut hasher = std::hash::DefaultHasher::new();
        text.hash(&mut hasher);
        let key = hasher.finish();
        if let Some((blocks, used)) = self.docs.get_mut(&key) {
            *used = frame;
            return blocks.clone();
        }
        if self.docs.len() >= CACHE_SIZE {
            // A streaming answer leaves one stale entry per frame.
            self.docs
                .retain(|_, (_, used)| frame.saturating_sub(*used) < 2);
        }
        let blocks = Arc::new(parse::parse(text));
        self.docs.insert(key, (blocks.clone(), frame));
        blocks
    }
}

/// Draws `text` as Markdown in the available width.
pub fn show(ui: &mut Ui, cache: &mut MarkdownCache, style: &MarkdownStyle, text: &str) {
    let blocks = cache.blocks(text, ui.ctx().cumulative_frame_nr());
    ui.vertical(|ui| {
        // Nothing inside may push the column wider than it was given.
        ui.set_max_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        show_blocks(ui, style, &style.base(), &blocks, 0);
    });
}

fn show_blocks(ui: &mut Ui, style: &MarkdownStyle, base: &Base, blocks: &[Block], depth: usize) {
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            ui.add_space(style.gap);
        }
        show_block(ui, style, base, block, depth);
    }
}

fn show_block(ui: &mut Ui, style: &MarkdownStyle, base: &Base, block: &Block, depth: usize) {
    let width = ui.available_width();
    match block {
        Block::Paragraph(inlines) => {
            let laid = inline::layout(ui, style, base, inlines, width);
            inline::show(ui, laid);
        }
        Block::Heading(level, inlines) => {
            let scale = match level {
                1 => 1.3,
                2 => 1.18,
                3 => 1.07,
                _ => 1.0,
            };
            let font = FontId::new((base.font.size * scale).round(), base.strong.family.clone());
            let heading = Base {
                font: font.clone(),
                strong: font,
                color: style.strong_text,
                all_strong: true,
            };
            let laid = inline::layout(ui, style, &heading, inlines, width);
            inline::show(ui, laid);
        }
        Block::Quote(blocks) => {
            let quote = Base {
                color: style.muted,
                ..base.clone()
            };
            let inner = ui.horizontal_top(|ui| {
                ui.add_space(12.0);
                ui.vertical(|ui| show_blocks(ui, style, &quote, blocks, depth));
            });
            let rect = inner.response.rect;
            ui.painter().vline(
                rect.left() + 1.0,
                rect.y_range(),
                Stroke::new(2.0, style.rule_strong),
            );
        }
        Block::List { start, items } => list(ui, style, base, *start, items, depth),
        Block::Code {
            lang,
            text,
            open: false,
        } if lang.eq_ignore_ascii_case("mermaid") => {
            if !mermaid::show(ui, style, text) {
                code_block(ui, style, text);
            }
        }
        Block::Code { text, .. } => code_block(ui, style, text),
        Block::Math(tex) => display_math(ui, style, base, tex),
        Block::Table { align, head, rows } => table::show(ui, style, base, align, head, rows),
        Block::Rule => {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
            ui.painter().hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(1.0, style.rule),
            );
        }
    }
}

fn list(
    ui: &mut Ui,
    style: &MarkdownStyle,
    base: &Base,
    start: Option<u64>,
    items: &[Item],
    depth: usize,
) {
    let line_height = (base.font.size * style.line_height).round();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            ui.add_space((style.gap * 0.4).round());
        }
        let (marker, font) = match (item.task, start) {
            (Some(done), _) => (
                (if done { "[x]" } else { "[ ]" }).to_owned(),
                style.code.clone(),
            ),
            (None, Some(start)) => (format!("{}.", start + index as u64), base.font.clone()),
            (None, None) => (["•", "◦", "▪"][depth % 3].to_owned(), base.font.clone()),
        };
        let job = LayoutJob::single_section(
            marker,
            TextFormat {
                font_id: font,
                color: style.muted,
                line_height: Some(line_height),
                valign: egui::Align::Center,
                ..TextFormat::default()
            },
        );
        let marker = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        let indent = (marker.size().x + 8.0).max(18.0).ceil();
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(indent, line_height), Sense::hover());
            ui.painter().galley(rect.min, marker, style.muted);
            ui.vertical(|ui| show_blocks(ui, style, base, &item.blocks, depth + 1));
        });
    }
}

fn code_block(ui: &mut Ui, style: &MarkdownStyle, text: &str) {
    let job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: style.code_block.clone(),
            color: style.text,
            line_height: Some((style.code_block.size * 1.5).round()),
            ..TextFormat::default()
        },
    );
    Frame::new()
        .fill(style.block_bg)
        .stroke(Stroke::new(1.0, style.rule))
        .corner_radius(style.radius)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ScrollArea::horizontal()
                .id_salt(ui.next_auto_id())
                .show(ui, |ui| {
                    ui.add(Label::new(job).selectable(true).extend());
                });
        });
}

fn display_math(ui: &mut Ui, style: &MarkdownStyle, base: &Base, tex: &str) {
    let size = base.font.size * style.math_scale;
    let Ok(formula) = math::formula(ui.ctx(), tex, true, size, base.color) else {
        code_block(ui, style, &format!("$${tex}$$"));
        return;
    };
    let width = ui.available_width();
    let draw = |ui: &mut Ui| {
        let (rect, _) = ui.allocate_exact_size(
            vec2(width.max(formula.size.x), formula.size.y),
            Sense::hover(),
        );
        let image = Rect::from_center_size(rect.center(), formula.size);
        ui.painter().image(
            formula.texture.id(),
            image,
            Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    };
    if formula.size.x > width {
        ScrollArea::horizontal()
            .id_salt(ui.next_auto_id())
            .show(ui, draw);
    } else {
        draw(ui);
    }
}
