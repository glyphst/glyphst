//! The assistant's text field, after Zed's message editor: `@` mentions and
//! pictures sit in the text as chips (an icon and a label on an outlined
//! plate) that the caret steps over and that delete as one.
//!
//! A chip is a run of the text: room for its icon, then its label, both
//! with non-breaking spaces so it never wraps. A layouter colours the run,
//! the plate is painted under the text and the icon over its room. An edit
//! that touches a chip removes all of it (Zed's `remove_invalid`), and a
//! caret that lands inside one moves to its edge.

use std::ops::Range;

use egui::{
    Color32, CornerRadius, FontId, Id, Rect, Response, Sense, Stroke, StrokeKind,
    text::{CCursor, CCursorRange, LayoutJob, TextFormat},
};

use crate::{
    ai::Attachment,
    ai_mentions::Mention,
    theme::{self, Tokens},
};

/// The room before a chip's label where its icon is painted.
const ICON_ROOM: &str = "\u{a0}\u{a0}\u{a0}\u{a0}\u{a0}";
const NBSP: char = '\u{a0}';

/// What a chip stands for.
#[derive(Clone, Debug)]
pub enum ChipKind {
    Mention(Mention),
    /// A pasted, dropped or chosen picture, or a region of a page.
    Image(Attachment),
}

impl ChipKind {
    pub fn label(&self) -> String {
        match self {
            Self::Mention(mention) => mention.label(),
            Self::Image(attachment) => attachment.label.clone(),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Self::Mention(mention) => mention.icon(),
            Self::Image(_) => "image",
        }
    }
}

/// A chip and the bytes of the text it takes.
#[derive(Clone, Debug)]
pub struct Chip {
    pub range: Range<usize>,
    pub kind: ChipKind,
}

/// The text a chip with `label` takes.
pub fn chip_text(label: &str) -> String {
    let label: String = label
        .chars()
        .map(|ch| if ch.is_whitespace() { NBSP } else { ch })
        .collect();
    format!("{ICON_ROOM}{label}{NBSP}")
}

/// A message as typed: its text and the chips in it.
#[derive(Clone, Debug, Default)]
pub struct Draft {
    pub text: String,
    /// In text order, not overlapping.
    pub chips: Vec<Chip>,
}

impl Draft {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            chips: Vec::new(),
        }
    }

    /// A draft of `text` with `chips` at the `[label]` spans of `spans`
    /// (byte ranges in `text`, in order), as [`Self::prompt`] wrote them.
    pub fn from_prompt(text: &str, chips: Vec<(Range<usize>, ChipKind)>) -> Self {
        let mut draft = Self::default();
        let mut at = 0;
        for (span, kind) in chips {
            let Some(before) = text.get(at..span.start) else {
                continue;
            };
            draft.text.push_str(before);
            let start = draft.text.len();
            draft.text.push_str(&chip_text(&kind.label()));
            draft.chips.push(Chip {
                range: start..draft.text.len(),
                kind,
            });
            at = span.end;
        }
        draft.text.push_str(text.get(at..).unwrap_or_default());
        draft
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Adds `kind` as a chip at the end.
    pub fn push_chip(&mut self, kind: ChipKind) {
        let end = self.text.len();
        self.insert_chip(end..end, kind);
    }

    /// The text the model and the chat's title read: each chip as
    /// `[label]`; with the spans of the chips in it.
    pub fn prompt(&self) -> (String, Vec<Range<usize>>) {
        let mut out = String::new();
        let mut spans = Vec::new();
        let mut at = 0;
        for chip in &self.chips {
            out.push_str(&self.text[at..chip.range.start]);
            let start = out.len();
            out.push('[');
            out.push_str(&chip.kind.label());
            out.push(']');
            spans.push(start..out.len());
            at = chip.range.end;
        }
        out.push_str(&self.text[at..]);
        // Trimming moves the spans.
        let leading = out.len() - out.trim_start().len();
        let trimmed = out.trim().to_string();
        let spans = spans
            .into_iter()
            .map(|span| span.start - leading..span.end - leading)
            .collect();
        (trimmed, spans)
    }

    pub fn mentions(&self) -> Vec<Mention> {
        self.chips
            .iter()
            .filter_map(|chip| match &chip.kind {
                ChipKind::Mention(mention) => Some(mention.clone()),
                ChipKind::Image(_) => None,
            })
            .collect()
    }

    pub fn images(&self) -> Vec<Attachment> {
        self.chips
            .iter()
            .filter_map(|chip| match &chip.kind {
                ChipKind::Image(attachment) => Some(attachment.clone()),
                ChipKind::Mention(_) => None,
            })
            .collect()
    }

    /// The chips whose text is where it should be.
    fn valid_chips<'a>(&'a self, text: &'a str) -> impl Iterator<Item = (usize, &'a Chip)> + 'a {
        self.chips.iter().enumerate().filter(move |(_, chip)| {
            text.get(chip.range.clone()) == Some(chip_text(&chip.kind.label()).as_str())
        })
    }

    /// Inserts `kind` as a chip in place of `range`, followed by a space;
    /// returns the byte after the space.
    fn insert_chip(&mut self, range: Range<usize>, kind: ChipKind) -> usize {
        let range = clamp_range(&self.text, range);
        let text = chip_text(&kind.label());
        let needs_space = !self.text[range.end..].starts_with(' ');
        let inserted = format!("{text}{}", if needs_space { " " } else { "" });
        self.replace(range.clone(), &inserted);
        let index = self
            .chips
            .iter()
            .position(|chip| chip.range.start >= range.start)
            .unwrap_or(self.chips.len());
        self.chips.insert(
            index,
            Chip {
                range: range.start..range.start + text.len(),
                kind,
            },
        );
        range.start + text.len() + 1
    }

    /// Replaces `range` of the text with `with`, dropping the chips it
    /// touches and moving the ones after it.
    fn replace(&mut self, range: Range<usize>, with: &str) {
        let range = clamp_range(&self.text, range);
        self.text.replace_range(range.clone(), with);
        let delta = with.len() as isize - range.len() as isize;
        self.chips.retain_mut(|chip| {
            if chip.range.end <= range.start {
                true
            } else if chip.range.start >= range.end {
                chip.range = shift(chip.range.start, delta)..shift(chip.range.end, delta);
                true
            } else {
                false
            }
        });
    }

    /// Brings the chips in line with an edit from `old` to the current
    /// text: chips before and after it move with it, and a chip it touched
    /// is removed whole. Returns where removed text was, to move the caret.
    fn reconcile(&mut self, old: &str) -> Option<usize> {
        if old == self.text {
            return None;
        }
        let new = self.text.clone();
        let prefix = common_prefix(old, &new);
        let suffix = common_suffix(&old[prefix..], &new[prefix..]);
        let (old_end, new_end) = (old.len() - suffix, new.len() - suffix);
        let delta = new_end as isize - old_end as isize;
        let mut broken = Vec::new();
        self.chips.retain_mut(|chip| {
            if chip.range.end <= prefix {
                true
            } else if chip.range.start >= old_end {
                chip.range = shift(chip.range.start, delta)..shift(chip.range.end, delta);
                true
            } else {
                // What is left of it in the new text.
                if chip.range.start < prefix {
                    broken.push(chip.range.start..prefix);
                }
                if chip.range.end > old_end {
                    broken.push(new_end..shift(chip.range.end, delta));
                }
                false
            }
        });
        if broken.is_empty() {
            return None;
        }
        let mut caret = None;
        for range in broken.into_iter().rev() {
            caret = Some(range.start);
            self.replace(range, "");
        }
        caret
    }
}

fn shift(at: usize, delta: isize) -> usize {
    (at as isize + delta).max(0) as usize
}

fn clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let floor = |mut at: usize| {
        at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    let start = floor(range.start);
    start..floor(range.end).max(start)
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((at, _), _)| at)
        .min(b.len())
}

fn common_suffix(a: &str, b: &str) -> usize {
    let mut length = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        length += x.len_utf8();
    }
    length
}

fn char_index(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

fn byte_index(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(byte, _)| byte)
}

/// How the field looks.
pub struct EditorStyle<'a> {
    pub hint: &'a str,
    pub rows: usize,
    pub max_height: f32,
    pub font: FontId,
}

/// What the field did this frame.
pub struct EditorOutput {
    pub response: Response,
    /// The chip under the pointer: its index and rect.
    pub hovered: Option<(usize, Rect)>,
    /// Where each chip is.
    pub plates: Vec<(usize, Rect)>,
}

/// The text field with chips.
pub struct MentionEditor {
    pub id: Id,
    draft: Draft,
    /// The caret last frame, in chars, to leave a chip the way it moved.
    last_caret: usize,
    /// An input method is composing text.
    pub composing: bool,
}

impl MentionEditor {
    pub fn new(id: Id) -> Self {
        Self {
            id,
            draft: Draft::default(),
            last_caret: 0,
            composing: false,
        }
    }

    #[cfg(test)]
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    pub fn text(&self) -> &str {
        &self.draft.text
    }

    pub fn is_empty(&self) -> bool {
        self.draft.is_empty()
    }

    pub fn chips(&self) -> &[Chip] {
        &self.draft.chips
    }

    pub fn take(&mut self) -> Draft {
        std::mem::take(&mut self.draft)
    }

    /// Replaces the contents, with the caret at the end.
    pub fn set(&mut self, ctx: &egui::Context, draft: Draft) {
        self.draft = draft;
        let end = self.draft.text.len();
        self.set_caret(ctx, end);
    }

    /// The caret, as a byte of the text (its end without one).
    pub fn caret(&self, ctx: &egui::Context) -> usize {
        egui::TextEdit::load_state(ctx, self.id)
            .and_then(|state| state.cursor.char_range())
            .map_or(self.draft.text.len(), |range| {
                byte_index(&self.draft.text, range.primary.index.0)
            })
    }

    pub fn set_caret(&mut self, ctx: &egui::Context, byte: usize) {
        let chars = char_index(&self.draft.text, byte);
        self.last_caret = chars;
        let mut state = egui::TextEdit::load_state(ctx, self.id).unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(chars))));
        state.store(ctx, self.id);
    }

    /// The `@query` before the caret, outside chips.
    pub fn active_query(&self, ctx: &egui::Context) -> Option<Range<usize>> {
        let range = crate::ai_mentions::active_query(&self.draft.text, self.caret(ctx))?;
        let inside = self
            .draft
            .chips
            .iter()
            .any(|chip| chip.range.start < range.end && range.start < chip.range.end);
        (!inside).then_some(range)
    }

    /// Puts `kind` as a chip in place of `range` (at the caret without
    /// one), with the caret after it.
    pub fn insert_chip(
        &mut self,
        ctx: &egui::Context,
        range: Option<Range<usize>>,
        kind: ChipKind,
    ) {
        let range = range.unwrap_or_else(|| {
            let caret = self.caret(ctx);
            caret..caret
        });
        let caret = self.draft.insert_chip(range, kind);
        self.set_caret(ctx, caret);
    }

    /// Replaces `range` of the text with `text`, the caret after it.
    pub fn replace_text(&mut self, ctx: &egui::Context, range: Range<usize>, text: &str) {
        let start = clamp_range(&self.draft.text, range.clone()).start;
        self.draft.replace(range, text);
        self.set_caret(ctx, start + text.len());
    }

    /// Follows the input method, so keys go to it while it composes. Call
    /// before handling keys.
    pub fn track_input_method(&mut self, ui: &egui::Ui) {
        ui.input(|i| {
            for event in &i.events {
                match event {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        self.composing = !text.is_empty();
                    }
                    egui::Event::Ime(_) => self.composing = false,
                    _ => {}
                }
            }
        });
    }

    /// The field. Keys it should not see (Enter to send, the menu's) must
    /// be consumed before.
    pub fn show(&mut self, ui: &mut egui::Ui, style: &EditorStyle<'_>) -> EditorOutput {
        let tokens = Tokens::get(ui.ctx());
        let old = self.draft.text.clone();
        let plates = ui.painter().add(egui::Shape::Noop);
        let selection = egui::TextEdit::load_state(ui.ctx(), self.id)
            .and_then(|state| state.cursor.char_range());
        let Draft { text, chips } = &mut self.draft;
        let chips_now = chips.clone();
        let font = style.font.clone();
        let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap: f32| {
            let job = layout_job(buffer.as_str(), &chips_now, &font, wrap, &tokens);
            ui.painter().layout_job(job)
        };
        let output = egui::ScrollArea::vertical()
            .id_salt(self.id.with("scroll"))
            .max_height(style.max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                glyphst_ui::text::fitted(ui, |ui| {
                    egui::TextEdit::multiline(text)
                        .id(self.id)
                        .hint_text(egui::RichText::new(style.hint).color(tokens.faint))
                        .font(style.font.clone())
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::ZERO)
                        .desired_rows(style.rows)
                        .desired_width(f32::INFINITY)
                        .layouter(&mut layouter)
                        .show(ui)
                })
            })
            .inner;
        if let Some(caret) = self.draft.reconcile(&old) {
            self.set_caret(ui.ctx(), caret);
        } else {
            self.snap_caret(ui.ctx());
        }
        // The plates and icons of the chips as laid out.
        let galley = output.galley.clone();
        let origin = output.galley_pos;
        let mut shapes = Vec::new();
        let mut hovered = None;
        let mut plates_at = Vec::new();
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let text = galley.job.text.clone();
        let draft = Draft {
            text: text.clone(),
            chips: self.draft.chips.clone(),
        };
        for (index, chip) in draft.valid_chips(&text) {
            let selected = selection.is_some_and(|range| {
                let (from, to) = (
                    range.primary.index.0.min(range.secondary.index.0),
                    range.primary.index.0.max(range.secondary.index.0),
                );
                let (start, end) = (
                    char_index(&text, chip.range.start),
                    char_index(&text, chip.range.end),
                );
                from < to && from < end && start < to
            });
            for rect in chip_rects(&galley, origin, &text, &chip.range) {
                if pointer.is_some_and(|pointer| rect.contains(pointer)) {
                    hovered = Some((index, rect));
                }
                plates_at.push((index, rect));
                shapes.push(plate(rect, selected, &tokens));
            }
            if let Some(icon) = icon_rect(&galley, origin, &text, &chip.range) {
                crate::icons::paint(ui, icon, chip.kind.icon(), 12.0, tokens.muted);
            }
        }
        ui.painter().set(plates, egui::Shape::Vec(shapes));
        EditorOutput {
            response: output.response.response,
            hovered,
            plates: plates_at,
        }
    }

    /// Moves a caret that landed inside a chip to the edge it came from
    /// (to the nearer edge when clicked).
    fn snap_caret(&mut self, ctx: &egui::Context) {
        let Some(range) =
            egui::TextEdit::load_state(ctx, self.id).and_then(|state| state.cursor.char_range())
        else {
            return;
        };
        let caret = range.primary.index.0;
        let text = &self.draft.text;
        let inside = self.draft.chips.iter().find_map(|chip| {
            let (start, end) = (
                char_index(text, chip.range.start),
                char_index(text, chip.range.end),
            );
            (start < caret && caret < end).then_some((start, end))
        });
        if let Some((start, end)) = inside {
            // Moving right leaves by the end, moving left by the start, and
            // a click by the nearer edge.
            let rightwards = if caret > self.last_caret && self.last_caret <= start {
                true
            } else if caret < self.last_caret && self.last_caret >= end {
                false
            } else {
                caret - start > end - caret
            };
            let to = if rightwards { end } else { start };
            let mut state = egui::TextEdit::load_state(ctx, self.id).unwrap_or_default();
            let secondary = if range.is_empty() {
                CCursor::new(to)
            } else {
                range.secondary
            };
            state
                .cursor
                .set_char_range(Some(CCursorRange::two(secondary, CCursor::new(to))));
            state.store(ctx, self.id);
            self.last_caret = to;
        } else {
            self.last_caret = caret;
        }
    }
}

/// The text laid out with its chips' runs coloured.
fn layout_job(text: &str, chips: &[Chip], font: &FontId, wrap: f32, tokens: &Tokens) -> LayoutJob {
    let mut job = LayoutJob {
        text: text.to_string(),
        ..LayoutJob::default()
    };
    job.wrap.max_width = wrap;
    let plain = TextFormat::simple(font.clone(), tokens.text);
    let chip = TextFormat::simple(font.clone(), tokens.text);
    let draft = Draft {
        text: text.to_string(),
        chips: chips.to_vec(),
    };
    let mut at = 0;
    for (_, valid) in draft.valid_chips(text) {
        if valid.range.start > at {
            job.sections.push(section(at..valid.range.start, &plain));
        }
        job.sections.push(section(valid.range.clone(), &chip));
        at = valid.range.end;
    }
    if at < text.len() || job.sections.is_empty() {
        job.sections.push(section(at..text.len(), &plain));
    }
    job
}

fn section(range: Range<usize>, format: &TextFormat) -> egui::text::LayoutSection {
    egui::text::LayoutSection {
        leading_space: 0.0,
        byte_range: egui::text::ByteIndex(range.start)..egui::text::ByteIndex(range.end),
        format: format.clone(),
    }
}

/// Where the characters `from..to` of a chip are, on one row or two. A
/// position at a row's boundary belongs to both rows, so each edge is
/// measured from a character inside the chip.
fn char_span(galley: &egui::Galley, from: usize, to: usize) -> Vec<Rect> {
    let at = |index: usize| galley.pos_from_cursor(CCursor::new(index));
    // The left edge of the first character, and the right edge of the last.
    let first = at(from + 1);
    let width = (at(from + 2).min.x - first.min.x).max(0.0);
    let start = Rect::from_min_max(
        egui::pos2(first.min.x - width, first.min.y),
        egui::pos2(first.min.x - width, first.max.y),
    );
    let last = at(to.saturating_sub(1));
    let end = Rect::from_min_max(
        egui::pos2(last.min.x + width, last.min.y),
        egui::pos2(last.min.x + width, last.max.y),
    );
    if (start.min.y - end.min.y).abs() < 1.0 {
        vec![Rect::from_min_max(start.min, end.max)]
    } else {
        vec![
            Rect::from_min_max(start.min, egui::pos2(galley.rect.right(), start.max.y)),
            Rect::from_min_max(egui::pos2(galley.rect.left(), end.min.y), end.max),
        ]
    }
}

/// The plate of a chip: one rect per row it spans.
fn chip_rects(
    galley: &egui::Galley,
    origin: egui::Pos2,
    text: &str,
    range: &Range<usize>,
) -> Vec<Rect> {
    let (start, end) = (char_index(text, range.start), char_index(text, range.end));
    char_span(galley, start, end)
        .into_iter()
        .map(|rect| {
            Rect::from_min_max(
                rect.min + egui::vec2(0.0, 1.0),
                rect.max - egui::vec2(0.0, 0.5),
            )
            .translate(origin.to_vec2())
        })
        .collect()
}

/// Where a chip's icon goes: its room before the label.
fn icon_rect(
    galley: &egui::Galley,
    origin: egui::Pos2,
    text: &str,
    range: &Range<usize>,
) -> Option<Rect> {
    let start = char_index(text, range.start);
    let room = char_span(galley, start, start + ICON_ROOM.chars().count());
    (room.len() == 1).then(|| room[0].translate(origin.to_vec2() + egui::vec2(1.0, 0.0)))
}

fn plate(rect: Rect, selected: bool, tokens: &Tokens) -> egui::Shape {
    let (fill, stroke) = if selected {
        (tokens.accent_soft, tokens.accent)
    } else {
        (tokens.ink(0.05), tokens.rule)
    };
    egui::Shape::Rect(egui::epaint::RectShape::new(
        rect,
        CornerRadius::same(4),
        fill,
        Stroke::new(1.0, stroke),
        StrokeKind::Inside,
    ))
}

/// What [`show_static`] drew.
pub struct Shown {
    /// The chip under the pointer: its index and rect.
    pub hovered: Option<(usize, Rect)>,
    /// The text went past the height limit and was cut.
    pub clipped: bool,
}

/// A draft drawn as text with its chips, wrapped at `width` and cut at
/// `limit` high, for sent messages.
pub fn show_static(
    ui: &mut egui::Ui,
    draft: &Draft,
    width: f32,
    font: &FontId,
    color: Color32,
    limit: Option<f32>,
) -> Shown {
    let tokens = Tokens::get(ui.ctx());
    let mut job = layout_job(&draft.text, &draft.chips, font, width, &tokens);
    for section in &mut job.sections {
        section.format.color = color;
    }
    let galley = ui.painter().layout_job(job);
    let height = limit.map_or(galley.size().y, |limit| galley.size().y.min(limit));
    let clipped = height < galley.size().y - 0.5;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(galley.size().x, height), Sense::hover());
    let pointer = response.hover_pos();
    drop(response);
    let mut hovered = None;
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    for (index, chip) in draft.valid_chips(&draft.text) {
        for plate_rect in chip_rects(&galley, rect.min, &draft.text, &chip.range) {
            if pointer.is_some_and(|pointer| plate_rect.contains(pointer)) {
                hovered = Some((index, plate_rect));
            }
            painter.add(plate(plate_rect, false, &tokens));
        }
    }
    painter.galley(rect.min, galley.clone(), color);
    if clipped {
        // The last line fades out.
        let band = Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 14.0), rect.max);
        let mut mesh = egui::Mesh::default();
        let clear = Color32::from_rgba_premultiplied(0, 0, 0, 0);
        let fill = tokens.raised;
        mesh.colored_vertex(band.left_top(), clear);
        mesh.colored_vertex(band.right_top(), clear);
        mesh.colored_vertex(band.left_bottom(), fill);
        mesh.colored_vertex(band.right_bottom(), fill);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(1, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }
    for (_, chip) in draft.valid_chips(&draft.text) {
        if let Some(icon) = icon_rect(&galley, rect.min, &draft.text, &chip.range)
            && rect.contains_rect(icon)
        {
            crate::icons::paint(
                ui,
                icon,
                chip.kind.icon(),
                12.0,
                theme::mix(color, tokens.raised, 0.3),
            );
        }
    }
    Shown { hovered, clipped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_mention(label: &str) -> ChipKind {
        ChipKind::Mention(Mention::Text {
            kind: "selection",
            label: label.into(),
            text: "selected words".into(),
        })
    }

    #[test]
    fn chips_move_with_edits_around_them_and_go_whole_when_touched() {
        let mut draft = Draft::plain("Explain  please");
        let after = draft.insert_chip(8..8, text_mention("Selection"));
        assert_eq!(
            &draft.text[after..],
            "please",
            "the caret goes past the space"
        );
        let chip = draft.chips[0].range.clone();
        assert_eq!(&draft.text[chip.clone()], chip_text("Selection"));

        // Typing before the chip moves it.
        let old = draft.text.clone();
        draft.text.insert_str(0, "Please ");
        assert_eq!(draft.reconcile(&old), None);
        assert_eq!(draft.chips[0].range, chip.start + 7..chip.end + 7);

        // Typing after it leaves it.
        let old = draft.text.clone();
        draft.text.push('!');
        assert_eq!(draft.reconcile(&old), None);
        assert_eq!(draft.chips.len(), 1);

        // Backspace at its end takes it all.
        let chip = draft.chips[0].range.clone();
        let old = draft.text.clone();
        draft
            .text
            .replace_range(chip.end - NBSP.len_utf8()..chip.end, "");
        assert_eq!(draft.reconcile(&old), Some(chip.start));
        assert!(draft.chips.is_empty());
        assert_eq!(draft.text, "Please Explain  please!");
    }

    #[test]
    fn prompts_name_chips_and_drafts_come_back_from_them() {
        let mut draft = Draft::plain("Compare  with ");
        draft.insert_chip(8..8, text_mention("p. 3"));
        let end = draft.text.len();
        draft.insert_chip(end..end, text_mention("Selection"));
        let (prompt, spans) = draft.prompt();
        assert_eq!(prompt, "Compare [p. 3] with [Selection]");
        assert_eq!(&prompt[spans[1].clone()], "[Selection]");
        let back = Draft::from_prompt(
            &prompt,
            spans
                .into_iter()
                .zip(draft.chips.iter().map(|chip| chip.kind.clone()))
                .collect(),
        );
        assert_eq!(back.chips.len(), 2);
        assert_eq!(back.prompt().0, prompt);
    }

    #[test]
    fn the_caret_steps_over_chips_and_backspace_removes_them() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut editor = MentionEditor::new(Id::unique("test-editor"));
        let frame = |editor: &mut MentionEditor, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 300.0),
                )),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                editor.show(
                    ui,
                    &EditorStyle {
                        hint: "",
                        rows: 2,
                        max_height: 200.0,
                        font: theme::regular(13.5),
                    },
                );
            });
            output.textures_delta.clear();
        };
        ctx.memory_mut(|memory| memory.request_focus(editor.id));
        frame(&mut editor, Vec::new());
        frame(&mut editor, vec![egui::Event::Text("Read ".into())]);
        editor.insert_chip(&ctx, None, text_mention("p. 3"));
        frame(&mut editor, vec![egui::Event::Text("now".into())]);
        assert_eq!(editor.draft().prompt().0, "Read [p. 3] now");

        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        // Left from after the space lands before the space, then a step
        // left skips the whole chip.
        for _ in 0..4 {
            frame(&mut editor, vec![key(egui::Key::ArrowLeft)]);
        }
        frame(&mut editor, vec![key(egui::Key::ArrowLeft)]);
        assert_eq!(editor.caret(&ctx), "Read ".len(), "before the chip");
        // Right steps over it again; Backspace then removes it.
        frame(&mut editor, vec![key(egui::Key::ArrowRight)]);
        assert_eq!(editor.caret(&ctx), editor.chips()[0].range.end);
        frame(&mut editor, vec![key(egui::Key::Backspace)]);
        assert!(editor.chips().is_empty());
        assert_eq!(editor.text(), "Read  now");
        assert_eq!(editor.caret(&ctx), "Read ".len());
    }
}
