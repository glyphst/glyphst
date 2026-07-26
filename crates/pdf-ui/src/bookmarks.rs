use std::{ops::Range, path::PathBuf};

use crate::{
    library::{Bookmark, DocumentPosition, LibraryStore},
    pdf_view::PdfView,
    picker::{Picker, PickerDelegate, fuzzy_match, normalize_query},
    settings::ui_rems,
};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, HighlightStyle, SharedString, StyledText,
    WeakEntity, Window, div, prelude::*,
};
use gpui_component::ThemeColor;

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 46.0;

pub type BookmarkPalette = Picker<BookmarkPaletteDelegate>;
pub type BookmarkPrompt = Picker<BookmarkPromptDelegate>;

#[derive(Clone)]
pub struct BookmarkPaletteEntry {
    id: u64,
    path: PathBuf,
    label: SharedString,
    file_name: SharedString,
    page: usize,
    position: DocumentPosition,
    current_document: bool,
}

struct BookmarkMatch {
    entry_ix: usize,
    highlights: Vec<Range<usize>>,
    score: usize,
}

pub struct BookmarkPaletteDelegate {
    pdf_view: WeakEntity<PdfView>,
    entries: Vec<BookmarkPaletteEntry>,
    matches: Vec<BookmarkMatch>,
}

pub struct BookmarkPromptDelegate {
    path: PathBuf,
    position: DocumentPosition,
}

impl BookmarkPaletteDelegate {
    pub fn new(
        pdf_view: WeakEntity<PdfView>,
        bookmarks: Vec<Bookmark>,
        current_path: PathBuf,
    ) -> Self {
        let mut entries = bookmarks
            .into_iter()
            .filter(|bookmark| bookmark.path.exists())
            .map(|bookmark| {
                let file_name = bookmark
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToString::to_string)
                    .unwrap_or_else(|| crate::fl!("untitled"))
                    .into();
                BookmarkPaletteEntry {
                    id: bookmark.id,
                    current_document: bookmark.path == current_path,
                    path: bookmark.path,
                    label: bookmark.label.into(),
                    file_name,
                    page: bookmark.position.page,
                    position: bookmark.position,
                }
            })
            .collect::<Vec<_>>();

        entries.sort_by(|left, right| {
            right
                .current_document
                .cmp(&left.current_document)
                .then_with(|| left.file_name.as_ref().cmp(right.file_name.as_ref()))
                .then_with(|| left.page.cmp(&right.page))
                .then_with(|| left.id.cmp(&right.id))
        });

        Self {
            pdf_view,
            entries,
            matches: Vec::new(),
        }
    }

    pub fn replace_bookmarks(&mut self, bookmarks: Vec<Bookmark>, current_path: PathBuf) {
        let pdf_view = self.pdf_view.clone();
        *self = Self::new(pdf_view, bookmarks, current_path);
    }
}

impl BookmarkPromptDelegate {
    pub fn new(path: PathBuf, position: DocumentPosition) -> Self {
        Self { path, position }
    }
}

impl PickerDelegate for BookmarkPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "Bookmarks Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        crate::fl!("bookmarks-placeholder").into()
    }

    fn empty_message(&self) -> SharedString {
        crate::fl!("bookmarks-empty").into()
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
            .and_then(|bookmark_match| self.entries.get(bookmark_match.entry_ix))
            .map(|entry| format!("bookmark-picker-{}", entry.id).into())
            .unwrap_or_else(|| ("bookmark-picker", match_ix).into())
    }

    fn update_matches(&mut self, query: &str) {
        let query = normalize_query(query);
        let mut matches = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(entry_ix, entry)| {
                fuzzy_match(entry.label.as_ref(), &query)
                    .or_else(|| {
                        fuzzy_match(entry.file_name.as_ref(), &query)
                            .map(|(_, score)| (Vec::new(), score + entry.label.len()))
                    })
                    .map(|(highlights, score)| BookmarkMatch {
                        entry_ix,
                        highlights,
                        score,
                    })
            })
            .collect::<Vec<_>>();

        matches.sort_by(|left, right| {
            let left_entry = &self.entries[left.entry_ix];
            let right_entry = &self.entries[right.entry_ix];
            right_entry
                .current_document
                .cmp(&left_entry.current_document)
                .then_with(|| left.score.cmp(&right.score))
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
        let Some(bookmark_match) = self.matches.get(match_ix) else {
            return div().into_any_element();
        };
        let entry = &self.entries[bookmark_match.entry_ix];
        let highlights = bookmark_match
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
                    .child(StyledText::new(entry.label.clone()).with_highlights(highlights))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.muted_foreground)
                            .child(entry.file_name.clone()),
                    ),
            )
            .when(entry.current_document, |this| {
                this.child(
                    div()
                        .flex_none()
                        .px_1()
                        .bg(theme.list_active)
                        .text_color(theme.primary)
                        .child(crate::fl!("current")),
                )
            })
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
            .and_then(|bookmark_match| self.entries.get(bookmark_match.entry_ix))
            .cloned()
        else {
            return;
        };

        if let Err(error) = self.pdf_view.update(cx, |pdf_view, cx| {
            if pdf_view.path() == entry.path {
                pdf_view.navigate_to_document_position(entry.position, cx);
            } else {
                pdf_view.open_library_document(entry.path, Some(entry.position), cx);
            }
        }) {
            eprintln!("failed to open bookmark: {error:#}");
        }
    }

    fn remove_match(&mut self, match_ix: usize, cx: &mut Context<Picker<Self>>) {
        let Some(id) = self
            .matches
            .get(match_ix)
            .and_then(|bookmark_match| self.entries.get(bookmark_match.entry_ix))
            .map(|entry| entry.id)
        else {
            return;
        };

        match cx.update_global::<LibraryStore, _>(|store, _| store.remove_bookmark(id)) {
            Ok(true) => self.entries.retain(|entry| entry.id != id),
            Ok(false) => {}
            Err(error) => eprintln!("failed to remove bookmark: {error:#}"),
        }
    }
}

impl PickerDelegate for BookmarkPromptDelegate {
    fn key_context(&self) -> &'static str {
        "BookmarkPrompt Menu TextInput"
    }

    fn placeholder(&self) -> SharedString {
        "Bookmark label…".into()
    }

    fn empty_message(&self) -> SharedString {
        "Press Enter to save bookmark".into()
    }

    fn width(&self) -> f32 {
        460.0
    }

    fn row_height(&self) -> f32 {
        MATCH_ROW_HEIGHT
    }

    fn max_visible_matches(&self) -> usize {
        1
    }

    fn match_count(&self) -> usize {
        0
    }

    fn update_matches(&mut self, _: &str) {}

    fn render_match(&self, _: usize, _: ThemeColor, _: &FocusHandle, _: &Window) -> AnyElement {
        div().into_any_element()
    }

    fn confirm(
        &mut self,
        _: usize,
        _: &FocusHandle,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
    }

    fn confirm_input(
        &mut self,
        input: SharedString,
        _: &FocusHandle,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) {
        let label = input.as_ref().trim();
        if label.is_empty() {
            return;
        }

        if let Err(error) = cx.update_global::<LibraryStore, _>(|store, _| {
            store.add_bookmark(&self.path, label, self.position.clone())
        }) {
            eprintln!("failed to add bookmark: {error:#}");
        }
    }
}
