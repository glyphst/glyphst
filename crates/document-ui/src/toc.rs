use std::ops::Range;

use crate::{
    document_view::DocumentView,
    icons::IconName,
    picker::{Picker, PickerDelegate, fuzzy_match, normalize_query},
    settings::ui_rems,
};
use glyphst_ui::{icons::Icon, tooltip::text_tooltip};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::ThemeColor;

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 40.0;

fn toc_match_id(entry_ix: usize) -> gpui::ElementId {
    ("toc-picker-entry", entry_ix).into()
}

pub type TocPalette = Picker<TocPaletteDelegate>;

#[derive(Clone)]
pub struct TocPaletteEntry {
    pub entry_ix: usize,
    pub title: SharedString,
    pub depth: usize,
    pub page_number: usize,
    pub bookmarked: bool,
}

struct TocMatch {
    entry_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct TocPaletteDelegate {
    document_view: WeakEntity<DocumentView>,
    entries: Vec<TocPaletteEntry>,
    matches: Vec<TocMatch>,
}

impl TocPaletteDelegate {
    pub fn new(document_view: WeakEntity<DocumentView>, entries: Vec<TocPaletteEntry>) -> Self {
        Self {
            document_view,
            entries,
            matches: Vec::new(),
        }
    }

    pub fn replace_entries(&mut self, entries: Vec<TocPaletteEntry>) {
        self.entries = entries;
    }
}

impl PickerDelegate for TocPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "Toc Menu"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("toc-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("toc-empty").into()
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

    fn match_id(&self, match_ix: usize) -> gpui::ElementId {
        self.matches
            .get(match_ix)
            .map(|entry_match| toc_match_id(entry_match.entry_ix))
            .unwrap_or_else(|| toc_match_id(match_ix))
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
        theme: ThemeColor,
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
            .child(div().flex_none().w(px(depth as f32 * 14.0)))
            .child(
                div()
                    .flex_none()
                    .w(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(entry.bookmarked, |this| {
                        this.child(
                            Icon::new(IconName::Bookmark)
                                .size(ui_rems(13.0))
                                .text_color(theme.primary),
                        )
                    }),
            )
            .child(
                div()
                    .id(("toc-picker-title", entry.entry_ix))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .tooltip(text_tooltip(title.clone()))
                    .child(StyledText::new(title).with_highlights(highlights)),
            )
            .child(
                div()
                    .flex_none()
                    .ml_3()
                    .text_color(theme.muted_foreground)
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

        if let Err(error) = self.document_view.update(cx, |document_view, cx| {
            document_view.navigate_to_toc_entry(entry_ix, cx);
        }) {
            eprintln!("failed to navigate to TOC entry: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::toc_match_id;

    #[test]
    fn toc_match_ids_follow_entries_across_reordering() {
        let before = [toc_match_id(2), toc_match_id(7), toc_match_id(11)];
        let after = [toc_match_id(11), toc_match_id(2)];

        assert_eq!(before[0], after[1]);
        assert_eq!(before[2], after[0]);
        assert_ne!(before[0], before[1]);
    }
}
