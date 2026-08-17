use std::{ops::Range, path::PathBuf};

use crate::{
    document_view::DocumentView,
    library::{Bookmark, DocumentPosition, LibraryStore},
    picker::{Picker, PickerDelegate, fuzzy_match, normalize_query},
    settings::ui_rems,
};
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, MouseButton, Render, SharedString, StyledText, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_component::{
    Sizable as _, ThemeColor, ThemeStyled as _,
    button::{Button, ButtonVariants as _},
    input::{Escape as ComponentInputEscape, Input, InputEvent, InputState},
};

const MAX_VISIBLE_MATCHES: usize = 12;
const MATCH_ROW_HEIGHT: f32 = 46.0;

pub type BookmarkPalette = Picker<BookmarkPaletteDelegate>;

pub enum BookmarkTitleEditorEvent {
    Save(SharedString),
    Cancel,
}

pub struct BookmarkTitleEditor {
    input: Entity<InputState>,
}

#[derive(Clone)]
pub struct BookmarkPaletteEntry {
    id: u64,
    path: PathBuf,
    title: SharedString,
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
    document_view: WeakEntity<DocumentView>,
    entries: Vec<BookmarkPaletteEntry>,
    matches: Vec<BookmarkMatch>,
}

impl BookmarkPaletteDelegate {
    pub fn new(
        document_view: WeakEntity<DocumentView>,
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
                let title = bookmark.title.unwrap_or_else(|| {
                    crate::fl!("bookmark-page-label", page = bookmark.position.page as i64)
                });
                BookmarkPaletteEntry {
                    id: bookmark.id,
                    current_document: bookmark.path == current_path,
                    path: bookmark.path,
                    title: title.into(),
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
            document_view,
            entries,
            matches: Vec::new(),
        }
    }

    pub fn replace_bookmarks(&mut self, bookmarks: Vec<Bookmark>, current_path: PathBuf) {
        let document_view = self.document_view.clone();
        *self = Self::new(document_view, bookmarks, current_path);
    }
}

impl BookmarkTitleEditor {
    pub fn new(content: SharedString, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let content_len = content.len();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(content)
                .placeholder(crate::fl!("bookmark-title-placeholder"))
        });
        input.update(cx, |input, cx| {
            input.set_selected_range(0..content_len, cx);
        });
        cx.subscribe(&input, |this, _, event, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save(cx);
            }
        })
        .detach();

        let focus = input.read(cx).focus_handle(cx);
        window.on_next_frame(move |window, cx| {
            focus.focus(window, cx);
        });

        Self { input }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        cx.emit(BookmarkTitleEditorEvent::Save(self.input.read(cx).value()));
    }

    fn cancel(&mut self, _: &ComponentInputEscape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(BookmarkTitleEditorEvent::Cancel);
    }
}

impl EventEmitter<BookmarkTitleEditorEvent> for BookmarkTitleEditor {}

impl Focusable for BookmarkTitleEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for BookmarkTitleEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("bookmark-title-editor")
            .debug_selector(|| "bookmark-title-editor".to_string())
            .key_context("BookmarkTitle")
            .track_focus(&self.focus_handle(cx))
            .flex()
            .items_center()
            .gap_2()
            .w(px(420.0))
            .max_w_full()
            .p_2()
            .popover_style(cx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::cancel))
            .child(Input::new(&self.input).small().flex_1().min_w_0())
            .child(
                Button::new("bookmark-title-cancel")
                    .small()
                    .label(crate::fl!("cancel"))
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(BookmarkTitleEditorEvent::Cancel);
                    })),
            )
            .child(
                Button::new("bookmark-title-save")
                    .small()
                    .primary()
                    .label(crate::fl!("save"))
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
    }
}

impl PickerDelegate for BookmarkPaletteDelegate {
    fn key_context(&self) -> &'static str {
        "Bookmarks Menu"
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
                fuzzy_match(entry.title.as_ref(), &query)
                    .or_else(|| {
                        fuzzy_match(entry.file_name.as_ref(), &query)
                            .map(|(_, score)| (Vec::new(), score + entry.title.len()))
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
                    .child(StyledText::new(entry.title.clone()).with_highlights(highlights))
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
                        .rounded_full()
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

        if let Err(error) = self.document_view.update(cx, |document_view, cx| {
            if document_view.path() == entry.path {
                document_view.navigate_to_document_position(entry.position, cx);
            } else {
                document_view.open_library_document(entry.path, Some(entry.position), cx);
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
