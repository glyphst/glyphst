use std::ops::Range;

use crate::pdf_view::PdfView;
use crate::picker::{Picker, PickerDelegate, fuzzy_match, normalize_query};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*, px, rgb,
};

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 40.0;

pub type TocPalette = Picker<TocPaletteDelegate>;

#[derive(Clone)]
pub struct TocPaletteEntry {
    pub entry_ix: usize,
    pub title: SharedString,
    pub depth: usize,
    pub page_number: usize,
}

struct TocMatch {
    entry_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct TocPaletteDelegate {
    pdf_view: WeakEntity<PdfView>,
    entries: Vec<TocPaletteEntry>,
    matches: Vec<TocMatch>,
}

impl TocPaletteDelegate {
    pub fn new(pdf_view: WeakEntity<PdfView>, entries: Vec<TocPaletteEntry>) -> Self {
        Self {
            pdf_view,
            entries,
            matches: Vec::new(),
        }
    }
}

impl PickerDelegate for TocPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "Toc"
    }

    fn placeholder(&self) -> SharedString {
        "Go to table of contents…".into()
    }

    fn empty_message(&self) -> SharedString {
        "No table of contents entries found".into()
    }

    fn width(&self) -> f32 {
        620.0
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

    fn update_matches(&mut self, query: &str) {
        let query = normalize_query(query);
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry_ix, entry)| {
                fuzzy_match(entry.title.as_ref(), &query).map(|(highlights, score)| TocMatch {
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
        theme: Theme,
        _: &FocusHandle,
        _: &Window,
    ) -> AnyElement {
        let Some(entry_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let entry = &self.entries[entry_match.entry_ix];
        let title = entry.title.clone();
        let page_number = entry.page_number;
        let depth = entry.depth;
        let highlights = entry_match
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
            .child(div().flex_none().w(px(depth as f32 * 14.0)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(StyledText::new(title).with_highlights(highlights)),
            )
            .child(
                div()
                    .flex_none()
                    .ml_3()
                    .text_color(rgb(theme.text_muted))
                    .child(format!("{}", page_number)),
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
        let Some(entry_ix) = self
            .matches
            .get(match_ix)
            .map(|entry_match| self.entries[entry_match.entry_ix].entry_ix)
        else {
            return;
        };

        if let Err(error) = self.pdf_view.update(cx, |pdf_view, cx| {
            pdf_view.navigate_to_toc_entry(entry_ix, cx);
        }) {
            eprintln!("failed to navigate to TOC entry: {error:#}");
        }
    }
}
