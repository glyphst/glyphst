use std::{ops::Range, path::PathBuf};

use crate::{
    library::{DocumentPosition, HistoryEntry},
    pdf_view::PdfView,
    picker::{Picker, PickerDelegate, fuzzy_match, normalize_query},
};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*,
};
use gpui_component::ThemeColor;

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 44.0;

pub type HistoryPalette = Picker<HistoryPaletteDelegate>;

#[derive(Clone)]
pub struct HistoryPaletteEntry {
    path: PathBuf,
    file_name: SharedString,
    directory: SharedString,
    page: usize,
    position: DocumentPosition,
}

struct HistoryMatch {
    entry_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct HistoryPaletteDelegate {
    pdf_view: WeakEntity<PdfView>,
    entries: Vec<HistoryPaletteEntry>,
    matches: Vec<HistoryMatch>,
}

impl HistoryPaletteDelegate {
    pub fn new(pdf_view: WeakEntity<PdfView>, entries: Vec<HistoryEntry>) -> Self {
        Self {
            pdf_view,
            entries: entries
                .into_iter()
                .filter(|entry| entry.path.exists())
                .map(|entry| {
                    let file_name = entry
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(ToString::to_string)
                        .unwrap_or_else(|| crate::fl!("untitled"))
                        .into();
                    let directory = entry
                        .path
                        .parent()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                        .into();
                    HistoryPaletteEntry {
                        path: entry.path,
                        file_name,
                        directory,
                        page: entry.position.page,
                        position: entry.position,
                    }
                })
                .collect(),
            matches: Vec::new(),
        }
    }
}

impl PickerDelegate for HistoryPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "History Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("history-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("history-empty").into()
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
            .and_then(|entry_match| self.entries.get(entry_match.entry_ix))
            .map(|entry| format!("history-picker-{}", entry.path.display()).into())
            .unwrap_or_else(|| ("history-picker", match_ix).into())
    }

    fn update_matches(&mut self, query: &str) {
        let query = normalize_query(query);
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry_ix, entry)| {
                fuzzy_match(entry.file_name.as_ref(), &query)
                    .or_else(|| {
                        fuzzy_match(&entry.path.display().to_string(), &query)
                            .map(|(_, score)| (Vec::new(), score + entry.file_name.len()))
                    })
                    .map(|(highlights, score)| HistoryMatch {
                        entry_ix,
                        highlights,
                        score,
                    })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| left.entry_ix.cmp(&right.entry_ix))
        });

        self.matches = matches;
    }

    fn render_match(
        &self,
        match_ix: usize,
        theme: ThemeColor,
        _: &FocusHandle,
        _: &Window,
    ) -> AnyElement {
        let Some(entry_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let entry = &self.entries[entry_match.entry_ix];
        let highlights = entry_match
            .highlights
            .iter()
            .cloned()
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        color: Some(theme.warning),
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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(StyledText::new(entry.file_name.clone()).with_highlights(highlights))
                    .child(
                        div()
                            .truncate()
                            .text_size(gpui::px(11.0))
                            .text_color(theme.muted_foreground)
                            .child(entry.directory.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
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
        let Some(entry) = self
            .matches
            .get(match_ix)
            .and_then(|entry_match| self.entries.get(entry_match.entry_ix))
            .cloned()
        else {
            return;
        };

        if let Err(error) = self.pdf_view.update(cx, |pdf_view, cx| {
            pdf_view.open_library_document(entry.path, Some(entry.position), cx);
        }) {
            eprintln!("failed to open recent PDF: {error:#}");
        }
    }
}
