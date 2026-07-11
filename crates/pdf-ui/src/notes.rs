use std::ops::Range;

use crate::{
    library::{HighlightColor, Note},
    pdf_view::PdfView,
    picker::{Picker, PickerDelegate, fuzzy_match, normalize_query},
    settings::ui_rems,
    theme::Theme,
};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*, px, rgb, rgba,
};

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 52.0;

fn note_match_id(note_id: u64) -> gpui::ElementId {
    format!("notes-picker-entry-{note_id}").into()
}

pub type NotesPalette = Picker<NotesPaletteDelegate>;

#[derive(Clone)]
pub struct NotePaletteEntry {
    pub id: u64,
    pub note_ix: usize,
    pub body: SharedString,
    pub text: SharedString,
    pub page: usize,
    pub color: HighlightColor,
    page_idx: usize,
    y0: f32,
    x0: f32,
}

struct NoteMatch {
    entry_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct NotesPaletteDelegate {
    pdf_view: WeakEntity<PdfView>,
    entries: Vec<NotePaletteEntry>,
    matches: Vec<NoteMatch>,
}

impl NotePaletteEntry {
    pub fn for_notes(notes: &[Note]) -> Vec<Self> {
        let mut entries = notes
            .iter()
            .enumerate()
            .filter_map(|(note_ix, note)| Self::new(note_ix, note))
            .collect::<Vec<_>>();

        entries.sort_by(|left, right| {
            left.page_idx
                .cmp(&right.page_idx)
                .then_with(|| left.y0.total_cmp(&right.y0))
                .then_with(|| left.x0.total_cmp(&right.x0))
                .then_with(|| left.note_ix.cmp(&right.note_ix))
        });

        entries
    }

    fn new(note_ix: usize, note: &Note) -> Option<Self> {
        let rect = note.rects.iter().min_by(|left, right| {
            left.page_idx
                .cmp(&right.page_idx)
                .then_with(|| left.y0.total_cmp(&right.y0))
                .then_with(|| left.x0.total_cmp(&right.x0))
        })?;

        Some(Self {
            id: note.id,
            note_ix,
            body: compact_text(&note.body, &crate::fl!("untitled-note")),
            text: compact_text(&note.text, &crate::fl!("no-selected-text")),
            page: rect.page_idx + 1,
            color: note.color,
            page_idx: rect.page_idx,
            y0: rect.y0,
            x0: rect.x0,
        })
    }
}

impl NotesPaletteDelegate {
    pub fn new(pdf_view: WeakEntity<PdfView>, entries: Vec<NotePaletteEntry>) -> Self {
        Self {
            pdf_view,
            entries,
            matches: Vec::new(),
        }
    }
}

impl PickerDelegate for NotesPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "Notes Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("notes-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("notes-empty").into()
    }

    fn width(&self) -> f32 {
        660.0
    }

    fn row_height(&self) -> f32 {
        MATCH_ROW_HEIGHT
    }

    fn max_visible_matches(&self) -> usize {
        MAX_VISIBLE_MATCHES
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn match_id(&self, match_ix: usize) -> gpui::ElementId {
        self.matches
            .get(match_ix)
            .and_then(|note_match| self.entries.get(note_match.entry_ix))
            .map(|entry| note_match_id(entry.id))
            .unwrap_or_else(|| note_match_id(match_ix as u64))
    }

    fn update_matches(&mut self, query: &str) {
        let query = normalize_query(query);
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry_ix, entry)| {
                fuzzy_match(entry.body.as_ref(), &query)
                    .or_else(|| {
                        fuzzy_match(entry.text.as_ref(), &query)
                            .map(|(_, score)| (Vec::new(), score + entry.body.len()))
                    })
                    .map(|(highlights, score)| NoteMatch {
                        entry_ix,
                        highlights,
                        score,
                    })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            let left_entry = &self.entries[left.entry_ix];
            let right_entry = &self.entries[right.entry_ix];
            left.score
                .cmp(&right.score)
                .then_with(|| left_entry.page_idx.cmp(&right_entry.page_idx))
                .then_with(|| left_entry.y0.total_cmp(&right_entry.y0))
                .then_with(|| left_entry.x0.total_cmp(&right_entry.x0))
                .then_with(|| left.entry_ix.cmp(&right.entry_ix))
        });

        self.matches = matches;
    }

    fn render_match(
        &self,
        match_ix: usize,
        theme: Theme,
        _: &FocusHandle,
        _: &Window,
    ) -> AnyElement {
        let Some(note_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let entry = &self.entries[note_match.entry_ix];
        let highlights = note_match
            .highlights
            .iter()
            .cloned()
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        color: Some(rgb(theme.match_highlight).into()),
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .w_full()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(8.0))
                    .h(px(8.0))
                    .bg(rgba(theme.note_color(entry.color))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(StyledText::new(entry.body.clone()).with_highlights(highlights))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(entry.text.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("p. {}", entry.page)),
            )
            .into_any_element()
    }

    fn confirm(
        &mut self,
        match_ix: usize,
        _: &FocusHandle,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let Some(note_ix) = self
            .matches
            .get(match_ix)
            .and_then(|note_match| self.entries.get(note_match.entry_ix))
            .map(|entry| entry.note_ix)
        else {
            return;
        };

        if let Err(error) = self.pdf_view.update(cx, |pdf_view, cx| {
            pdf_view.navigate_to_note(note_ix, cx);
        }) {
            eprintln!("failed to navigate to note: {error:#}");
        }
    }
}

fn compact_text(text: &str, fallback: &str) -> SharedString {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        fallback.into()
    } else {
        text.into()
    }
}

#[cfg(test)]
mod tests {
    use super::note_match_id;

    #[test]
    fn note_match_ids_follow_notes_across_reordering() {
        let before = [note_match_id(41), note_match_id(88), note_match_id(103)];
        let after = [note_match_id(103), note_match_id(41)];

        assert_eq!(before[0], after[1]);
        assert_eq!(before[2], after[0]);
        assert_ne!(before[0], before[1]);
    }
}
