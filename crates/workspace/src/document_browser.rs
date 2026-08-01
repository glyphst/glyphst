use std::{
    collections::HashSet,
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use glyphst_actions::{
    document_browser::{GoUp, OpenSelected},
    menu::{Cancel, Confirm, SelectNext, SelectPrev},
};
use glyphst_core::DocumentFormat;
use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, Render, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, relative, transparent_black,
    uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
};

use crate::{
    icons::{Icon, IconName},
    library::LibraryStore,
    scrollbar::WithScrollbar as _,
    settings::ui_rems,
};

const BROWSER_WIDTH: f32 = 720.0;
const BROWSER_HEIGHT: f32 = 520.0;
const PLACES_WIDTH: f32 = 172.0;
const ROW_HEIGHT: f32 = 38.0;
const MAX_RECENT_PLACES: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    Pdf,
    Epub,
}

impl EntryKind {
    fn label(self) -> String {
        match self {
            Self::Directory => crate::fl!("document-browser-folder-kind"),
            Self::Pdf => "PDF".to_string(),
            Self::Epub => "EPUB".to_string(),
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Directory => IconName::FolderOpen,
            Self::Pdf => IconName::FileText,
            Self::Epub => IconName::BookOpen,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserEntry {
    path: PathBuf,
    name: SharedString,
    kind: EntryKind,
}

#[derive(Clone)]
struct Place {
    label: SharedString,
    path: PathBuf,
}

#[derive(Clone, Debug)]
enum LoadState {
    Loading,
    Ready,
    Error(SharedString),
}

#[derive(Clone, Debug)]
pub enum DocumentBrowserEvent {
    Open(PathBuf),
    UseSystemPicker,
    Dismissed,
}

pub struct DocumentBrowser {
    input: gpui::Entity<InputState>,
    current_dir: PathBuf,
    places: Vec<Place>,
    entries: Vec<BrowserEntry>,
    filtered: Vec<usize>,
    selected: usize,
    show_hidden: bool,
    load_state: LoadState,
    load_generation: u64,
    scroll: UniformListScrollHandle,
    load_task: Option<Task<()>>,
    _input_subscription: Subscription,
}

impl DocumentBrowser {
    pub fn suggested_directory(active_document: Option<&Path>, cx: &App) -> PathBuf {
        active_document
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| {
                cx.global::<LibraryStore>()
                    .history_entries()
                    .first()
                    .and_then(|entry| entry.path.parent())
                    .map(Path::to_path_buf)
            })
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn new(initial_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::fl!("document-browser-filter-placeholder"))
                .submit_on_enter(true)
        });
        let _input_subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        let mut browser = Self {
            input: input.clone(),
            current_dir: initial_dir.clone(),
            places: Self::build_places(cx),
            entries: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            show_hidden: false,
            load_state: LoadState::Loading,
            load_generation: 0,
            scroll: UniformListScrollHandle::new(),
            load_task: None,
            _input_subscription,
        };
        browser.load_directory(initial_dir, cx);

        let focus = input.focus_handle(cx);
        window.defer(cx, move |window, cx| focus.focus(window, cx));
        browser
    }

    fn build_places(cx: &App) -> Vec<Place> {
        fn add_place(
            places: &mut Vec<Place>,
            seen: &mut HashSet<PathBuf>,
            label: impl Into<SharedString>,
            path: Option<PathBuf>,
        ) {
            let Some(path) = path else { return };
            if seen.insert(path.clone()) {
                places.push(Place {
                    label: label.into(),
                    path,
                });
            }
        }

        let mut places = Vec::new();
        let mut seen = HashSet::new();
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-home"),
            dirs::home_dir(),
        );
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-documents"),
            dirs::document_dir(),
        );
        add_place(
            &mut places,
            &mut seen,
            crate::fl!("document-browser-downloads"),
            dirs::download_dir(),
        );

        for entry in cx
            .global::<LibraryStore>()
            .history_entries()
            .iter()
            .take(MAX_RECENT_PLACES * 2)
        {
            let Some(parent) = entry.path.parent().map(Path::to_path_buf) else {
                continue;
            };
            let label = parent
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| parent.display().to_string());
            add_place(&mut places, &mut seen, label, Some(parent));
            if places.len() >= 3 + MAX_RECENT_PLACES {
                break;
            }
        }
        places
    }

    fn read_directory(path: &Path, show_hidden: bool) -> Result<Vec<BrowserEntry>, String> {
        let read_dir = fs::read_dir(path).map_err(|error| {
            crate::fl!(
                "document-browser-read-error",
                path = path.display().to_string(),
                error = error.to_string()
            )
        })?;
        let mut entries = Vec::new();
        for entry in read_dir {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let entry_path = entry.path();
            let Ok(metadata) = fs::metadata(&entry_path) else {
                continue;
            };
            let kind = if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                let format = entry_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .and_then(DocumentFormat::from_extension);
                match format {
                    Some(DocumentFormat::Pdf) => EntryKind::Pdf,
                    Some(DocumentFormat::Epub) => EntryKind::Epub,
                    None => continue,
                }
            } else {
                continue;
            };
            entries.push(BrowserEntry {
                path: entry_path,
                name: name.into(),
                kind,
            });
        }
        Self::sort_entries(&mut entries);
        Ok(entries)
    }

    fn sort_entries(entries: &mut [BrowserEntry]) {
        entries.sort_by(|left, right| {
            let left_group = usize::from(left.kind != EntryKind::Directory);
            let right_group = usize::from(right.kind != EntryKind::Directory);
            left_group.cmp(&right_group).then_with(|| {
                left.name
                    .to_lowercase()
                    .cmp(&right.name.to_lowercase())
                    .then_with(|| left.name.cmp(&right.name))
            })
        });
    }

    fn load_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path.clone();
        self.entries.clear();
        self.filtered.clear();
        self.selected = 0;
        self.load_state = LoadState::Loading;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let show_hidden = self.show_hidden;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { Self::read_directory(&path, show_hidden) })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.load_state = LoadState::Ready;
                        this.update_filter(cx);
                    }
                    Err(error) => {
                        this.load_state = LoadState::Error(error.into());
                        this.entries.clear();
                        this.filtered.clear();
                    }
                }
                this.load_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| query.is_empty() || entry.name.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        if !self.filtered.is_empty() {
            self.scroll
                .scroll_to_item(self.selected, ScrollStrategy::Center);
        }
    }

    fn on_input_event(
        &mut self,
        _: &gpui::Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.update_filter(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.open_selected(window, cx),
            InputEvent::CompletionAccepted { .. }
            | InputEvent::SpanHoverChanged { .. }
            | InputEvent::Focus
            | InputEvent::Blur => {}
        }
    }

    fn clear_query(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn navigate_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_query(window, cx);
        self.load_directory(path, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return;
        };
        self.navigate_to(parent, window, cx);
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .filtered
            .get(self.selected)
            .and_then(|index| self.entries.get(*index))
            .cloned()
        else {
            return;
        };
        if entry.kind == EntryKind::Directory {
            self.navigate_to(entry.path, window, cx);
        } else {
            cx.emit(DocumentBrowserEvent::Open(entry.path));
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.open_selected(window, cx);
    }

    fn open_selected_action(
        &mut self,
        _: &OpenSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_selected(window, cx);
    }

    fn go_up_action(&mut self, _: &GoUp, window: &mut Window, cx: &mut Context<Self>) {
        self.go_up(window, cx);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DocumentBrowserEvent::Dismissed);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or(self.filtered.len() - 1);
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key.as_str() == "backspace" && self.input.read(cx).value().is_empty() {
            self.go_up(window, cx);
            cx.stop_propagation();
        }
    }

    fn breadcrumbs(&self) -> Vec<(SharedString, PathBuf)> {
        let mut ancestors = self.current_dir.ancestors().collect::<Vec<_>>();
        ancestors.reverse();
        ancestors
            .into_iter()
            .map(|path| {
                let label = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| path.display().to_string());
                (label.into(), path.to_path_buf())
            })
            .collect()
    }

    fn render_entry(
        &self,
        display_index: usize,
        entry: BrowserEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().colors;
        let selected = self.selected == display_index;
        let path = entry.path.clone();
        div()
            .id(("document-browser-entry", display_index))
            .h(px(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .cursor_pointer()
            .bg(if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.selected = display_index;
                    if event.click_count >= 2 {
                        if let Some(index) = this.filtered.get(display_index).copied()
                            && let Some(entry) = this.entries.get(index).cloned()
                        {
                            if entry.kind == EntryKind::Directory {
                                this.navigate_to(path.clone(), window, cx);
                            } else {
                                cx.emit(DocumentBrowserEvent::Open(path.clone()));
                            }
                        }
                    } else {
                        cx.notify();
                    }
                    cx.stop_propagation();
                }),
            )
            .child(Icon::new(entry.kind.icon()).size(ui_rems(15.0)).text_color(
                if entry.kind == EntryKind::Directory {
                    theme.primary
                } else {
                    theme.muted_foreground
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(theme.foreground)
                    .child(entry.name),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(entry.kind.label()),
            )
            .into_any_element()
    }

    fn render_places(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .w(px(PLACES_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .border_r(px(1.0))
            .border_color(theme.sidebar_border)
            .bg(theme.sidebar)
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(crate::fl!("document-browser-places")),
            )
            .children(self.places.iter().enumerate().map(|(index, place)| {
                let path = place.path.clone();
                Button::new(("document-browser-place", index))
                    .small()
                    .ghost()
                    .selected(self.current_dir == place.path)
                    .icon(IconName::FolderOpen)
                    .label(place.label.clone())
                    .justify_start()
                    .w_full()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_to(path.clone(), window, cx);
                        cx.stop_propagation();
                    }))
            }))
            .into_any_element()
    }

    fn render_loading(cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .children((0..8).map(|index| {
                Skeleton::new()
                    .h(px(18.0))
                    .w(relative(if index % 3 == 0 { 0.68 } else { 0.9 }))
                    .rounded(cx.theme().radius)
            }))
            .into_any_element()
    }

    fn render_listing(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        match &self.load_state {
            LoadState::Loading => Self::render_loading(cx),
            LoadState::Error(error) => {
                let message = error.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .px_6()
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(message)
                    .child(
                        Button::new("document-browser-retry")
                            .small()
                            .outline()
                            .icon(IconName::RotateCcw)
                            .label(crate::fl!("document-browser-retry"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_directory(this.current_dir.clone(), cx);
                                cx.stop_propagation();
                            })),
                    )
                    .into_any_element()
            }
            LoadState::Ready if self.filtered.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(if self.input.read(cx).value().is_empty() {
                    crate::fl!("document-browser-empty-folder")
                } else {
                    crate::fl!("document-browser-no-results")
                })
                .into_any_element(),
            LoadState::Ready => {
                let count = self.filtered.len();
                let scroll = self.scroll.clone();
                div()
                    .size_full()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "document-browser-list",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .filter_map(|display_index| {
                                        let entry_index = *this.filtered.get(display_index)?;
                                        let entry = this.entries.get(entry_index)?.clone();
                                        Some(this.render_entry(display_index, entry, cx))
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(
                        &self.scroll,
                        "document-browser-scrollbar",
                        window,
                        cx,
                    )
                    .into_any_element()
            }
        }
    }
}

impl EventEmitter<DocumentBrowserEvent> for DocumentBrowser {}

impl Focusable for DocumentBrowser {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for DocumentBrowser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let breadcrumbs = self.breadcrumbs();
        let show_hidden = self.show_hidden;

        div()
            .id("document-browser")
            .debug_selector(|| "document-browser".to_string())
            .key_context("DocumentBrowser")
            .track_focus(&self.input.focus_handle(cx))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::open_selected_action))
            .on_action(cx.listener(Self::go_up_action))
            .on_key_down(cx.listener(Self::on_key_down))
            .w(px(BROWSER_WIDTH))
            .h(px(BROWSER_HEIGHT))
            .max_w(relative(0.92))
            .max_h(relative(0.86))
            .flex()
            .flex_col()
            .overflow_hidden()
            .popover_style(cx)
            .rounded(cx.theme().radius_lg)
            .shadow_xl()
            .text_size(ui_rems(13.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .h(px(48.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border.opacity(0.7))
                    .bg(theme.muted.opacity(0.35))
                    .child(
                        Icon::new(IconName::Search)
                            .size(ui_rems(15.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.input).appearance(false)),
                    )
                    .child(
                        Button::new("document-browser-hidden")
                            .xsmall()
                            .ghost()
                            .selected(show_hidden)
                            .label(if show_hidden {
                                crate::fl!("document-browser-hide-dotfiles")
                            } else {
                                crate::fl!("document-browser-show-dotfiles")
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_hidden = !this.show_hidden;
                                this.load_directory(this.current_dir.clone(), cx);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        Button::new("document-browser-close")
                            .xsmall()
                            .compact()
                            .ghost()
                            .icon(IconName::X)
                            .tooltip(crate::fl!("close"))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(DocumentBrowserEvent::Dismissed);
                                cx.stop_propagation();
                            })),
                    ),
            )
            .child(
                div()
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border.opacity(0.7))
                    .overflow_hidden()
                    .children(
                        breadcrumbs
                            .into_iter()
                            .enumerate()
                            .map(|(index, (label, path))| {
                                div()
                                    .flex()
                                    .items_center()
                                    .min_w_0()
                                    .when(index > 0, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .text_color(theme.muted_foreground)
                                                .child("/"),
                                        )
                                    })
                                    .child(
                                        Button::new(("document-browser-crumb", index))
                                            .xsmall()
                                            .compact()
                                            .ghost()
                                            .label(label)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.navigate_to(path.clone(), window, cx);
                                                cx.stop_propagation();
                                            })),
                                    )
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_places(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .bg(theme.background)
                            .child(self.render_listing(window, cx)),
                    ),
            )
            .child(
                div()
                    .h(px(42.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .border_t(px(1.0))
                    .border_color(theme.border.opacity(0.7))
                    .bg(theme.muted.opacity(0.35))
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(crate::fl!("document-browser-keyboard-hint"))
                    .child(
                        Button::new("document-browser-system-picker")
                            .xsmall()
                            .outline()
                            .label(crate::fl!("document-browser-system-picker"))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(DocumentBrowserEvent::UseSystemPicker);
                                cx.stop_propagation();
                            })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> BrowserEntry {
        BrowserEntry {
            path: PathBuf::from(name),
            name: name.into(),
            kind,
        }
    }

    #[test]
    fn folders_sort_before_documents_case_insensitively() {
        let mut entries = vec![
            entry("z.pdf", EntryKind::Pdf),
            entry("beta", EntryKind::Directory),
            entry("Alpha.epub", EntryKind::Epub),
            entry("alpha", EntryKind::Directory),
        ];
        DocumentBrowser::sort_entries(&mut entries);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.name.as_ref())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "Alpha.epub", "z.pdf"]
        );
    }

    #[test]
    fn directory_reader_filters_unsupported_and_hidden_files() {
        let root = std::env::temp_dir().join(format!(
            "glyphst-document-browser-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("folder")).unwrap();
        fs::write(root.join("paper.PDF"), b"").unwrap();
        fs::write(root.join("book.epub"), b"").unwrap();
        fs::write(root.join("notes.txt"), b"").unwrap();
        fs::write(root.join(".hidden.pdf"), b"").unwrap();

        let entries = DocumentBrowser::read_directory(&root, false).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|entry| !entry.name.starts_with('.')));

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn directory_reader_treats_symlinked_directories_as_folders() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "glyphst-document-browser-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let target = root.join("target");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("linked-folder")).unwrap();

        let entries = DocumentBrowser::read_directory(&root, false).unwrap();
        assert!(entries.iter().any(|entry| {
            entry.name.as_ref() == "linked-folder" && entry.kind == EntryKind::Directory
        }));

        fs::remove_dir_all(root).ok();
    }
}
