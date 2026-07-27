use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read as _},
    ops::Range,
    path::{Path, PathBuf},
};

use glyphst_ui::scrollbar::WithScrollbar as _;
use gpui::{
    App, Context, FocusHandle, Focusable, IntoElement, KeyBinding, Render, ScrollStrategy,
    UniformListScrollHandle, Window, div, prelude::*, px, relative, transparent_black,
    uniform_list,
};
use gpui_component::{ActiveTheme as _, ThemeColor};

const KEY_CONTEXT: &str = "Files";
const ROW_HEIGHT: f32 = 28.0;
const HEADER_HEIGHT: f32 = 40.0;
const STATUS_HEIGHT: f32 = 32.0;
const PREVIEW_MAX_BYTES: usize = 256 * 1024;
const PREVIEW_MAX_LINES: usize = 2_000;
const PREVIEW_MAX_LINE_CHARS: usize = 4_096;

gpui::actions!(
    glyphst_files,
    [
        SelectPrevious,
        SelectNext,
        LeaveDirectory,
        EnterDirectory,
        SelectFirst,
        SelectLast,
    ]
);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("k", SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(KEY_CONTEXT)),
        KeyBinding::new("j", SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(KEY_CONTEXT)),
        KeyBinding::new("h", LeaveDirectory, Some(KEY_CONTEXT)),
        KeyBinding::new("left", LeaveDirectory, Some(KEY_CONTEXT)),
        KeyBinding::new("l", EnterDirectory, Some(KEY_CONTEXT)),
        KeyBinding::new("right", EnterDirectory, Some(KEY_CONTEXT)),
        KeyBinding::new("g g", SelectFirst, Some(KEY_CONTEXT)),
        KeyBinding::new("home", SelectFirst, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-g", SelectLast, Some(KEY_CONTEXT)),
        KeyBinding::new("end", SelectLast, Some(KEY_CONTEXT)),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Other,
}

impl EntryKind {
    fn label(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileEntry {
    path: PathBuf,
    display_name: String,
    kind: EntryKind,
    is_symlink: bool,
}

impl FileEntry {
    fn from_dir_entry(entry: fs::DirEntry) -> io::Result<Self> {
        let path = entry.path();
        let file_type = entry.file_type()?;
        let is_symlink = file_type.is_symlink();
        let followed_type = if is_symlink {
            fs::metadata(&path)
                .ok()
                .map(|metadata| metadata.file_type())
        } else {
            Some(file_type)
        };
        let kind = match followed_type {
            Some(file_type) if file_type.is_dir() => EntryKind::Directory,
            Some(file_type) if file_type.is_file() => EntryKind::File,
            _ => EntryKind::Other,
        };

        Ok(Self {
            path,
            display_name: entry.file_name().to_string_lossy().into_owned(),
            kind,
            is_symlink,
        })
    }

    fn is_directory(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    fn display_label(&self) -> String {
        match (self.kind, self.is_symlink) {
            (EntryKind::Directory, true) => format!("{}@/", self.display_name),
            (EntryKind::Directory, false) => format!("{}/", self.display_name),
            (_, true) => format!("{}@", self.display_name),
            _ => self.display_name.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TextPreview {
    lines: Vec<String>,
    truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Preview {
    None,
    Directory(Vec<FileEntry>),
    Text(TextPreview),
    Message(String),
}

impl Preview {
    fn row_count(&self) -> usize {
        match self {
            Self::Directory(entries) => entries.len(),
            Self::Text(preview) => preview.lines.len() + usize::from(preview.truncated),
            Self::None | Self::Message(_) => 0,
        }
    }

    fn row(&self, index: usize) -> Option<(String, bool)> {
        match self {
            Self::Directory(entries) => entries
                .get(index)
                .map(|entry| (entry.display_label(), false)),
            Self::Text(preview) => {
                if let Some(line) = preview.lines.get(index) {
                    Some((line.clone(), false))
                } else if preview.truncated && index == preview.lines.len() {
                    Some(("… preview truncated".to_owned(), true))
                } else {
                    None
                }
            }
            Self::None | Self::Message(_) => None,
        }
    }

    fn empty_message(&self) -> Option<&str> {
        match self {
            Self::None => Some("No item selected"),
            Self::Directory(entries) if entries.is_empty() => Some("Empty directory"),
            Self::Text(preview) if preview.lines.is_empty() && !preview.truncated => {
                Some("Empty file")
            }
            Self::Message(message) => Some(message),
            Self::Directory(_) | Self::Text(_) => None,
        }
    }
}

#[derive(Debug)]
struct LoadedLocation {
    current_dir: PathBuf,
    parent_entries: Vec<FileEntry>,
    parent_selected: Option<usize>,
    parent_error: Option<String>,
    entries: Vec<FileEntry>,
    selected: Option<usize>,
    preview: Preview,
}

impl LoadedLocation {
    fn load(current_dir: PathBuf, selection: Option<&Path>) -> io::Result<Self> {
        let entries = load_entries(&current_dir, selection)?;
        let selected = if entries.is_empty() {
            None
        } else {
            selection
                .and_then(|selection| entries.iter().position(|entry| entry.path == selection))
                .or(Some(0))
        };
        let preview = selected
            .and_then(|index| entries.get(index))
            .map(load_preview)
            .unwrap_or(Preview::None);

        let (parent_entries, parent_selected, parent_error) =
            if let Some(parent) = current_dir.parent() {
                match load_entries(parent, Some(&current_dir)) {
                    Ok(parent_entries) => {
                        let parent_selected = parent_entries
                            .iter()
                            .position(|entry| entry.path == current_dir);
                        (parent_entries, parent_selected, None)
                    }
                    Err(error) => (
                        Vec::new(),
                        None,
                        Some(format!("Failed to read parent: {error}")),
                    ),
                }
            } else {
                (Vec::new(), None, None)
            };

        Ok(Self {
            current_dir,
            parent_entries,
            parent_selected,
            parent_error,
            entries,
            selected,
            preview,
        })
    }
}

#[derive(Debug)]
struct StartupTarget {
    directory: PathBuf,
    selection: Option<PathBuf>,
    fallback_directory: PathBuf,
    explicit: bool,
    warning: Option<String>,
}

impl StartupTarget {
    fn resolve(argument: Option<&OsStr>, cwd: &Path) -> Self {
        let fallback_directory = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
        let Some(argument) = argument else {
            return Self {
                directory: fallback_directory.clone(),
                selection: None,
                fallback_directory,
                explicit: false,
                warning: None,
            };
        };

        let requested = PathBuf::from(argument);
        let requested = if requested.is_absolute() {
            requested
        } else {
            cwd.join(requested)
        };

        match fs::canonicalize(&requested) {
            Ok(path) if path.is_dir() => Self {
                directory: path,
                selection: None,
                fallback_directory,
                explicit: true,
                warning: None,
            },
            Ok(path) => {
                let directory = path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| fallback_directory.clone());
                Self {
                    directory,
                    selection: Some(path),
                    fallback_directory,
                    explicit: true,
                    warning: None,
                }
            }
            Err(error) => Self {
                directory: fallback_directory.clone(),
                selection: None,
                fallback_directory,
                explicit: true,
                warning: Some(format!(
                    "Could not open {}: {error}; showing the working directory",
                    requested.display()
                )),
            },
        }
    }
}

#[derive(Debug)]
struct Navigator {
    current_dir: PathBuf,
    parent_entries: Vec<FileEntry>,
    parent_selected: Option<usize>,
    parent_error: Option<String>,
    entries: Vec<FileEntry>,
    selected: Option<usize>,
    preview: Preview,
    current_error: Option<String>,
    notice: Option<String>,
}

impl Navigator {
    fn from_argument(argument: Option<OsString>) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::from_argument_and_cwd(argument.as_deref(), &cwd)
    }

    fn from_argument_and_cwd(argument: Option<&OsStr>, cwd: &Path) -> Self {
        let startup = StartupTarget::resolve(argument, cwd);
        match LoadedLocation::load(startup.directory.clone(), startup.selection.as_deref()) {
            Ok(location) => Self::from_location(location, startup.warning),
            Err(error) if startup.explicit && startup.directory != startup.fallback_directory => {
                let warning = format!(
                    "Could not read {}: {error}; showing the working directory",
                    startup.directory.display()
                );
                match LoadedLocation::load(startup.fallback_directory.clone(), None) {
                    Ok(location) => Self::from_location(location, Some(warning)),
                    Err(fallback_error) => Self::failed(
                        startup.fallback_directory,
                        format!("{warning}; fallback failed: {fallback_error}"),
                    ),
                }
            }
            Err(error) => Self::failed(
                startup.directory.clone(),
                startup.warning.unwrap_or_else(|| {
                    format!("Could not read {}: {error}", startup.directory.display())
                }),
            ),
        }
    }

    fn from_location(location: LoadedLocation, notice: Option<String>) -> Self {
        Self {
            current_dir: location.current_dir,
            parent_entries: location.parent_entries,
            parent_selected: location.parent_selected,
            parent_error: location.parent_error,
            entries: location.entries,
            selected: location.selected,
            preview: location.preview,
            current_error: None,
            notice,
        }
    }

    fn failed(current_dir: PathBuf, message: String) -> Self {
        Self {
            current_dir,
            parent_entries: Vec::new(),
            parent_selected: None,
            parent_error: None,
            entries: Vec::new(),
            selected: None,
            preview: Preview::None,
            current_error: Some(message),
            notice: None,
        }
    }

    fn selected_entry(&self) -> Option<&FileEntry> {
        self.selected.and_then(|index| self.entries.get(index))
    }

    fn select_relative(&mut self, delta: isize) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        let last = self.entries.len().saturating_sub(1);
        let next = if delta < 0 {
            selected.saturating_sub(delta.unsigned_abs())
        } else {
            selected.saturating_add(delta as usize).min(last)
        };
        self.set_selected(next)
    }

    fn select_first(&mut self) -> bool {
        self.set_selected(0)
    }

    fn select_last(&mut self) -> bool {
        self.set_selected(self.entries.len().saturating_sub(1))
    }

    fn set_selected(&mut self, index: usize) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let index = index.min(self.entries.len() - 1);
        if self.selected == Some(index) {
            return false;
        }
        self.selected = Some(index);
        self.preview = load_preview(&self.entries[index]);
        self.notice = None;
        true
    }

    fn enter_selected(&mut self) -> bool {
        let Some(entry) = self.selected_entry().cloned() else {
            return false;
        };
        if !entry.is_directory() {
            return false;
        }
        self.transition(entry.path, None)
    }

    fn leave(&mut self) -> bool {
        let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        let selection = self.current_dir.clone();
        self.transition(parent, Some(selection))
    }

    fn transition(&mut self, directory: PathBuf, selection: Option<PathBuf>) -> bool {
        match LoadedLocation::load(directory.clone(), selection.as_deref()) {
            Ok(location) => {
                *self = Self::from_location(location, None);
                true
            }
            Err(error) => {
                self.notice = Some(format!("Could not read {}: {error}", directory.display()));
                false
            }
        }
    }

    fn parent_message(&self) -> Option<&str> {
        if let Some(error) = self.parent_error.as_deref() {
            Some(error)
        } else if self.current_dir.parent().is_none() {
            Some("Filesystem root")
        } else if self.parent_entries.is_empty() {
            Some("Empty parent directory")
        } else {
            None
        }
    }

    fn current_message(&self) -> Option<&str> {
        if let Some(error) = self.current_error.as_deref() {
            Some(error)
        } else if self.entries.is_empty() {
            Some("Empty directory")
        } else {
            None
        }
    }

    fn status(&self) -> String {
        if let Some(notice) = self.notice.as_deref() {
            return notice.to_owned();
        }
        let Some(entry) = self.selected_entry() else {
            return format!("0/0  {}", self.current_dir.display());
        };
        format!(
            "{}/{}  {}  {}",
            self.selected.unwrap_or(0) + 1,
            self.entries.len(),
            entry.kind.label(),
            entry.display_label()
        )
    }
}

pub struct FileManagerView {
    navigator: Navigator,
    focus_handle: FocusHandle,
    parent_scroll: UniformListScrollHandle,
    current_scroll: UniformListScrollHandle,
    preview_scroll: UniformListScrollHandle,
}

impl FileManagerView {
    pub fn new(start_path: Option<OsString>, cx: &mut Context<Self>) -> Self {
        let view = Self {
            navigator: Navigator::from_argument(start_path),
            focus_handle: cx.focus_handle(),
            parent_scroll: UniformListScrollHandle::new(),
            current_scroll: UniformListScrollHandle::new(),
            preview_scroll: UniformListScrollHandle::new(),
        };
        if let Some(selected) = view.navigator.selected {
            view.current_scroll
                .scroll_to_item(selected, ScrollStrategy::Center);
        }
        if let Some(selected) = view.navigator.parent_selected {
            view.parent_scroll
                .scroll_to_item(selected, ScrollStrategy::Center);
        }
        view
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
    }

    fn after_selection_change(&mut self, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        if let Some(selected) = self.navigator.selected {
            self.current_scroll.scroll_to_item(selected, strategy);
        }
        self.preview_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn after_directory_change(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.navigator.selected {
            self.current_scroll
                .scroll_to_item(selected, ScrollStrategy::Center);
        }
        if let Some(selected) = self.navigator.parent_selected {
            self.parent_scroll
                .scroll_to_item(selected, ScrollStrategy::Center);
        }
        self.preview_scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn select_previous(&mut self, cx: &mut Context<Self>) {
        if self.navigator.select_relative(-1) {
            self.after_selection_change(ScrollStrategy::Center, cx);
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        if self.navigator.select_relative(1) {
            self.after_selection_change(ScrollStrategy::Center, cx);
        }
    }

    fn select_first(&mut self, cx: &mut Context<Self>) {
        if self.navigator.select_first() {
            self.after_selection_change(ScrollStrategy::Top, cx);
        }
    }

    fn select_last(&mut self, cx: &mut Context<Self>) {
        if self.navigator.select_last() {
            self.after_selection_change(ScrollStrategy::Bottom, cx);
        }
    }

    fn enter_directory(&mut self, cx: &mut Context<Self>) {
        if self.navigator.enter_selected() {
            self.after_directory_change(cx);
        } else if self.navigator.notice.is_some() {
            cx.notify();
        }
    }

    fn leave_directory(&mut self, cx: &mut Context<Self>) {
        if self.navigator.leave() {
            self.after_directory_change(cx);
        } else if self.navigator.notice.is_some() {
            cx.notify();
        }
    }

    fn render_message(message: &str, theme: ThemeColor) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px_3()
            .text_color(theme.muted_foreground)
            .child(message.to_owned())
    }

    fn render_row(
        text: String,
        selected: bool,
        muted: bool,
        theme: ThemeColor,
    ) -> impl IntoElement {
        div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .px_2()
            .bg(if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .text_color(if selected {
                theme.primary
            } else if muted {
                theme.muted_foreground
            } else {
                theme.foreground
            })
            .child(div().min_w_0().truncate().child(text))
    }

    fn render_parent(
        &self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.navigator.parent_entries.len();
        let scroll = self.parent_scroll.clone();
        let message = self.navigator.parent_message().map(str::to_owned);

        div()
            .h_full()
            .w(relative(1.0 / 8.0))
            .min_w_0()
            .overflow_hidden()
            .bg(theme.sidebar)
            .when_some(message, |this, message| {
                this.child(Self::render_message(&message, theme))
            })
            .when(count > 0, |this| {
                this.child(
                    uniform_list(
                        "files-parent-list",
                        count,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let theme = cx.theme().colors;
                            range
                                .filter_map(|index| {
                                    let entry = this.navigator.parent_entries.get(index)?;
                                    Some(Self::render_row(
                                        entry.display_label(),
                                        this.navigator.parent_selected == Some(index),
                                        false,
                                        theme,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll,
                    "files-parent-scrollbar",
                    window,
                    cx,
                )
            })
    }

    fn render_current(
        &self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.navigator.entries.len();
        let scroll = self.current_scroll.clone();
        let message = self.navigator.current_message().map(str::to_owned);

        div()
            .h_full()
            .w(relative(4.0 / 8.0))
            .min_w_0()
            .overflow_hidden()
            .border_l(px(1.0))
            .border_r(px(1.0))
            .border_color(theme.border)
            .bg(theme.background)
            .when_some(message, |this, message| {
                this.child(Self::render_message(&message, theme))
            })
            .when(count > 0, |this| {
                this.child(
                    uniform_list(
                        "files-current-list",
                        count,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let theme = cx.theme().colors;
                            range
                                .filter_map(|index| {
                                    let entry = this.navigator.entries.get(index)?;
                                    Some(Self::render_row(
                                        entry.display_label(),
                                        this.navigator.selected == Some(index),
                                        false,
                                        theme,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll,
                    "files-current-scrollbar",
                    window,
                    cx,
                )
            })
    }

    fn render_preview(
        &self,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = self.navigator.preview.row_count();
        let scroll = self.preview_scroll.clone();
        let message = self.navigator.preview.empty_message().map(str::to_owned);

        div()
            .h_full()
            .w(relative(3.0 / 8.0))
            .min_w_0()
            .overflow_hidden()
            .bg(theme.tab_active)
            .when_some(message, |this, message| {
                this.child(Self::render_message(&message, theme))
            })
            .when(count > 0, |this| {
                this.child(
                    uniform_list(
                        "files-preview-list",
                        count,
                        cx.processor(|this, range: Range<usize>, _, cx| {
                            let theme = cx.theme().colors;
                            range
                                .filter_map(|index| {
                                    let (text, muted) = this.navigator.preview.row(index)?;
                                    Some(Self::render_row(text, false, muted, theme))
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&scroll)
                    .size_full(),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll,
                    "files-preview-scrollbar",
                    window,
                    cx,
                )
            })
    }
}

impl Focusable for FileManagerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileManagerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let mono_font = cx.theme().mono_font_family.clone();
        let mono_size = cx.theme().mono_font_size;
        let current_path = self.navigator.current_dir.display().to_string();
        let status = self.navigator.status();
        let status_color = if self.navigator.notice.is_some() {
            theme.warning_foreground
        } else {
            theme.muted_foreground
        };

        div()
            .id("glyphst-files")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                this.select_previous(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                this.select_next(cx);
            }))
            .on_action(cx.listener(|this, _: &LeaveDirectory, _, cx| {
                this.leave_directory(cx);
            }))
            .on_action(cx.listener(|this, _: &EnterDirectory, _, cx| {
                this.enter_directory(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                this.select_first(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                this.select_last(cx);
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(mono_font)
            .text_size(mono_size)
            .child(
                div()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border)
                    .bg(theme.title_bar)
                    .child(div().min_w_0().truncate().child(current_path)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .child(self.render_parent(theme, window, cx))
                    .child(self.render_current(theme, window, cx))
                    .child(self.render_preview(theme, window, cx)),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(STATUS_HEIGHT))
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .px_3()
                    .border_t(px(1.0))
                    .border_color(theme.status_bar_border)
                    .bg(theme.status_bar)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(status_color)
                            .child(status),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child("h/l parent/enter  j/k move  gg/G first/last"),
                    ),
            )
    }
}

fn load_entries(directory: &Path, explicit_include: Option<&Path>) -> io::Result<Vec<FileEntry>> {
    let mut entries = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| FileEntry::from_dir_entry(entry).ok())
        .filter(|entry| {
            !entry.display_name.starts_with('.')
                || explicit_include.is_some_and(|path| path == entry.path)
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        right
            .is_directory()
            .cmp(&left.is_directory())
            .then_with(|| {
                left.display_name
                    .to_lowercase()
                    .cmp(&right.display_name.to_lowercase())
            })
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    Ok(entries)
}

fn load_preview(entry: &FileEntry) -> Preview {
    match entry.kind {
        EntryKind::Directory => match load_entries(&entry.path, None) {
            Ok(entries) => Preview::Directory(entries),
            Err(error) => Preview::Message(format!("Failed to preview directory: {error}")),
        },
        EntryKind::File => match read_text_preview(&entry.path) {
            Ok(Some(preview)) => Preview::Text(preview),
            Ok(None) => Preview::Message("Binary or non-UTF-8 file — no preview".to_owned()),
            Err(error) => Preview::Message(format!("Failed to preview file: {error}")),
        },
        EntryKind::Other => Preview::Message("Preview unavailable for this file type".to_owned()),
    }
}

fn read_text_preview(path: &Path) -> io::Result<Option<TextPreview>> {
    let mut bytes = Vec::with_capacity(PREVIEW_MAX_BYTES + 1);
    File::open(path)?
        .take((PREVIEW_MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;

    let byte_truncated = bytes.len() > PREVIEW_MAX_BYTES;
    bytes.truncate(PREVIEW_MAX_BYTES);
    if bytes.contains(&0) {
        return Ok(None);
    }

    let valid_len = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if byte_truncated && error.error_len().is_none() => error.valid_up_to(),
        Err(_) => return Ok(None),
    };
    let text = std::str::from_utf8(&bytes[..valid_len]).expect("validated UTF-8 prefix");
    let mut source_lines = text.lines();
    let mut lines = Vec::new();
    let mut truncated = byte_truncated;

    for _ in 0..PREVIEW_MAX_LINES {
        let Some(line) = source_lines.next() else {
            break;
        };
        let (line, line_truncated) = truncate_preview_line(line);
        truncated |= line_truncated;
        lines.push(line);
    }
    if source_lines.next().is_some() {
        truncated = true;
    }

    Ok(Some(TextPreview { lines, truncated }))
}

fn truncate_preview_line(line: &str) -> (String, bool) {
    let mut chars = line.chars();
    let mut rendered = chars
        .by_ref()
        .take(PREVIEW_MAX_LINE_CHARS)
        .collect::<String>();
    let truncated = chars.next().is_some();
    if truncated {
        rendered.push('…');
    }
    (rendered.replace('\t', "    "), truncated)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsStr,
        fs,
        io::Write as _,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        EntryKind, Navigator, PREVIEW_MAX_BYTES, PREVIEW_MAX_LINES, Preview, StartupTarget,
        load_entries, load_preview, read_text_preview,
    };

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let unique = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "glyphst-files-{label}-{}-{nanos}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn select_named(navigator: &mut Navigator, name: &str) {
        let index = navigator
            .entries
            .iter()
            .position(|entry| entry.display_name == name)
            .unwrap();
        navigator.set_selected(index);
    }

    #[test]
    fn entries_hide_dotfiles_and_sort_directories_first() {
        let directory = TestDirectory::new("sorting");
        fs::create_dir(directory.path().join("beta")).unwrap();
        fs::create_dir(directory.path().join("Alpha")).unwrap();
        fs::write(directory.path().join("gamma.txt"), b"gamma").unwrap();
        fs::write(directory.path().join("Bravo.txt"), b"bravo").unwrap();
        fs::write(directory.path().join(".secret"), b"secret").unwrap();

        let entries = load_entries(directory.path(), None).unwrap();
        let labels = entries
            .iter()
            .map(|entry| entry.display_label())
            .collect::<Vec<_>>();

        assert_eq!(labels, ["Alpha/", "beta/", "Bravo.txt", "gamma.txt"]);
    }

    #[test]
    fn explicit_hidden_startup_file_remains_selectable() {
        let directory = TestDirectory::new("hidden-startup");
        let hidden = directory.path().join(".config");
        fs::write(&hidden, b"value").unwrap();

        let navigator =
            Navigator::from_argument_and_cwd(Some(hidden.as_os_str()), directory.path());

        assert_eq!(
            navigator.selected_entry().map(|entry| entry.path.as_path()),
            Some(hidden.as_path())
        );
    }

    #[test]
    fn startup_target_accepts_relative_directory_and_file_paths() {
        let directory = TestDirectory::new("startup");
        let child = directory.path().join("child");
        fs::create_dir(&child).unwrap();
        let file = directory.path().join("notes.txt");
        fs::write(&file, b"notes").unwrap();

        let directory_target = StartupTarget::resolve(Some(OsStr::new("child")), directory.path());
        assert_eq!(
            directory_target.directory,
            fs::canonicalize(&child).unwrap()
        );
        assert_eq!(directory_target.selection, None);

        let file_target = StartupTarget::resolve(Some(OsStr::new("notes.txt")), directory.path());
        assert_eq!(
            file_target.directory,
            fs::canonicalize(directory.path()).unwrap()
        );
        assert_eq!(file_target.selection, Some(fs::canonicalize(file).unwrap()));
    }

    #[test]
    fn entering_and_leaving_restore_the_directory_selection() {
        let directory = TestDirectory::new("navigation");
        let child = directory.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("inside.txt"), b"inside").unwrap();
        fs::write(directory.path().join("outside.txt"), b"outside").unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();

        let mut navigator = Navigator::from_argument_and_cwd(None, directory.path());
        select_named(&mut navigator, "child");
        assert!(navigator.enter_selected());
        assert_eq!(navigator.current_dir, child);
        assert_eq!(
            navigator
                .selected_entry()
                .map(|entry| entry.display_name.as_str()),
            Some("inside.txt")
        );

        assert!(navigator.leave());
        assert_eq!(navigator.current_dir, root);
        assert_eq!(
            navigator
                .selected_entry()
                .map(|entry| entry.display_name.as_str()),
            Some("child")
        );
    }

    #[test]
    fn navigation_clamps_and_jumps_to_boundaries() {
        let directory = TestDirectory::new("boundaries");
        for name in ["a.txt", "b.txt", "c.txt"] {
            fs::write(directory.path().join(name), name).unwrap();
        }
        let mut navigator = Navigator::from_argument_and_cwd(None, directory.path());

        assert_eq!(navigator.selected, Some(0));
        assert!(!navigator.select_relative(-1));
        assert!(navigator.select_last());
        assert_eq!(navigator.selected, Some(2));
        assert!(!navigator.select_relative(1));
        assert!(navigator.select_first());
        assert_eq!(navigator.selected, Some(0));
    }

    #[test]
    fn previews_directories_utf8_binary_and_missing_files() {
        let directory = TestDirectory::new("previews");
        let child = directory.path().join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("nested.txt"), b"nested").unwrap();
        fs::write(directory.path().join("text.txt"), "first\nsecond\n").unwrap();
        fs::write(directory.path().join("binary.bin"), b"\0binary").unwrap();

        let entries = load_entries(directory.path(), None).unwrap();
        let child_entry = entries
            .iter()
            .find(|entry| entry.display_name == "child")
            .unwrap();
        assert!(matches!(
            load_preview(child_entry),
            Preview::Directory(ref entries)
                if entries.len() == 1 && entries[0].display_name == "nested.txt"
        ));

        let text_entry = entries
            .iter()
            .find(|entry| entry.display_name == "text.txt")
            .unwrap();
        assert!(matches!(
            load_preview(text_entry),
            Preview::Text(ref preview)
                if preview.lines == ["first", "second"] && !preview.truncated
        ));

        let binary_entry = entries
            .iter()
            .find(|entry| entry.display_name == "binary.bin")
            .unwrap();
        assert!(matches!(
            load_preview(binary_entry),
            Preview::Message(ref message) if message.contains("non-UTF-8")
        ));

        fs::remove_file(&text_entry.path).unwrap();
        assert!(matches!(
            load_preview(text_entry),
            Preview::Message(ref message) if message.contains("Failed to preview file")
        ));
    }

    #[test]
    fn text_preview_is_bounded_by_bytes_and_lines() {
        let directory = TestDirectory::new("preview-bounds");
        let bytes_path = directory.path().join("bytes.txt");
        let mut bytes_file = fs::File::create(&bytes_path).unwrap();
        bytes_file
            .write_all(&vec![b'a'; PREVIEW_MAX_BYTES + 32])
            .unwrap();
        let byte_preview = read_text_preview(&bytes_path).unwrap().unwrap();
        assert!(byte_preview.truncated);

        let lines_path = directory.path().join("lines.txt");
        let contents = (0..PREVIEW_MAX_LINES + 2)
            .map(|index| format!("line {index}\n"))
            .collect::<String>();
        fs::write(&lines_path, contents).unwrap();
        let line_preview = read_text_preview(&lines_path).unwrap().unwrap();
        assert_eq!(line_preview.lines.len(), PREVIEW_MAX_LINES);
        assert!(line_preview.truncated);
    }

    #[test]
    fn failed_directory_entry_keeps_the_current_location() {
        let directory = TestDirectory::new("failed-enter");
        let child = directory.path().join("child");
        fs::create_dir(&child).unwrap();
        let mut navigator = Navigator::from_argument_and_cwd(None, directory.path());
        select_named(&mut navigator, "child");
        let before = navigator.current_dir.clone();
        fs::remove_dir(&child).unwrap();

        assert!(!navigator.enter_selected());
        assert_eq!(navigator.current_dir, before);
        assert!(
            navigator
                .notice
                .as_deref()
                .unwrap()
                .contains("Could not read")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_directories_are_navigable() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new("symlink");
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside.txt"), b"inside").unwrap();
        let link = directory.path().join("link");
        symlink(&target, &link).unwrap();

        let entries = load_entries(directory.path(), None).unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.display_name == "link")
            .unwrap();
        assert_eq!(entry.kind, EntryKind::Directory);
        assert!(entry.is_symlink);
        assert_eq!(entry.display_label(), "link@/");
    }
}
