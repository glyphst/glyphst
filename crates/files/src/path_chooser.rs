use std::{
    collections::{BTreeSet, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use futures::channel::oneshot;
use glyphst_ui::{
    entry_selection::apply_entry_click,
    icons::{Icon, IconName},
    menu::{SelectNext, SelectPrev},
    scrollbar::WithScrollbar as _,
    settings::{PathChooserBackend, SettingsStore, ui_rems},
};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, KeystrokeEvent,
    Modifiers, MouseButton, MouseDownEvent, PathPromptOptions, Render, ScrollStrategy,
    SharedString, Subscription, Task, UniformListScrollHandle, Window, div, prelude::*, px,
    relative, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, WindowExt as _,
    input::{Input, InputEvent, InputState},
    skeleton::Skeleton,
};

const CARD_WIDTH: f32 = 680.0;
const BODY_HEIGHT: f32 = 330.0;
const HEADER_HEIGHT: f32 = 46.0;
const FOOTER_HEIGHT: f32 = 39.0;
const SAVE_ROW_HEIGHT: f32 = 44.0;
const PLACES_WIDTH: f32 = 196.0;
const ROW_HEIGHT: f32 = 32.0;
const VIEWPORT_MARGIN: f32 = 16.0;

pub type OpenPathReceiver = oneshot::Receiver<Result<Option<Vec<PathBuf>>>>;
pub type SavePathReceiver = oneshot::Receiver<Result<Option<PathBuf>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathFilter {
    pub label: SharedString,
    pub extensions: Vec<String>,
}

impl PathFilter {
    pub fn new(
        label: impl Into<SharedString>,
        extensions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            label: label.into(),
            extensions: extensions
                .into_iter()
                .map(Into::into)
                .map(|extension: String| extension.trim_start_matches('.').to_ascii_lowercase())
                .collect(),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                self.extensions
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(extension))
            })
    }
}

#[derive(Clone, Debug)]
pub struct OpenPathOptions {
    pub initial_directory: PathBuf,
    pub prompt: SharedString,
    pub files: bool,
    pub directories: bool,
    pub multiple: bool,
    pub filters: Vec<PathFilter>,
    pub allow_all_files: bool,
}

impl OpenPathOptions {
    pub fn files(initial_directory: impl Into<PathBuf>, prompt: impl Into<SharedString>) -> Self {
        Self {
            initial_directory: initial_directory.into(),
            prompt: prompt.into(),
            files: true,
            directories: false,
            multiple: false,
            filters: Vec::new(),
            allow_all_files: false,
        }
    }

    pub fn multiple(mut self, multiple: bool) -> Self {
        self.multiple = multiple;
        self
    }

    pub fn directories(mut self, directories: bool) -> Self {
        self.directories = directories;
        self
    }

    pub fn filter(mut self, filter: PathFilter) -> Self {
        self.filters.push(filter);
        self
    }

    pub fn allow_all_files(mut self, allow: bool) -> Self {
        self.allow_all_files = allow;
        self
    }
}

#[derive(Clone, Debug)]
pub struct SavePathOptions {
    pub initial_directory: PathBuf,
    pub prompt: SharedString,
    pub suggested_name: String,
    pub required_extension: Option<String>,
}

impl SavePathOptions {
    pub fn new(
        initial_directory: impl Into<PathBuf>,
        prompt: impl Into<SharedString>,
        suggested_name: impl Into<String>,
    ) -> Self {
        Self {
            initial_directory: initial_directory.into(),
            prompt: prompt.into(),
            suggested_name: suggested_name.into(),
            required_extension: None,
        }
    }

    pub fn required_extension(mut self, extension: impl Into<String>) -> Self {
        self.required_extension = Some(
            extension
                .into()
                .trim_start_matches('.')
                .to_ascii_lowercase(),
        );
        self
    }
}

#[derive(Clone, Debug)]
enum ChooserMode {
    Open(OpenPathOptions),
    Save(SavePathOptions),
}

impl ChooserMode {
    fn initial_directory(&self) -> &Path {
        match self {
            Self::Open(options) => &options.initial_directory,
            Self::Save(options) => &options.initial_directory,
        }
    }

    fn is_save(&self) -> bool {
        matches!(self, Self::Save(_))
    }

    fn card_height(&self) -> f32 {
        HEADER_HEIGHT
            + BODY_HEIGHT
            + FOOTER_HEIGHT
            + if self.is_save() { SAVE_ROW_HEIGHT } else { 0.0 }
    }
}

#[derive(Clone, Debug)]
struct ChooserEntry {
    path: PathBuf,
    name: SharedString,
    directory: bool,
}

#[derive(Clone)]
struct ChooserPlace {
    label: SharedString,
    path: PathBuf,
    icon: IconName,
}

#[derive(Clone, Debug)]
enum LoadState {
    Loading,
    Ready,
    Error(SharedString),
}

pub fn prompt_for_paths(
    window: &mut Window,
    cx: &mut App,
    options: OpenPathOptions,
) -> OpenPathReceiver {
    if cx.global::<SettingsStore>().settings.path_chooser == PathChooserBackend::System {
        return cx.prompt_for_paths(PathPromptOptions {
            files: options.files,
            directories: options.directories,
            multiple: options.multiple,
            prompt: Some(options.prompt),
        });
    }

    prompt_custom(window, cx, ChooserMode::Open(options))
}

pub fn prompt_for_new_path(
    window: &mut Window,
    cx: &mut App,
    options: SavePathOptions,
) -> SavePathReceiver {
    if cx.global::<SettingsStore>().settings.path_chooser == PathChooserBackend::System {
        let native = cx.prompt_for_new_path(
            &options.initial_directory,
            Some(options.suggested_name.as_str()),
        );
        let required_extension = options.required_extension;
        let (sender, receiver) = oneshot::channel();
        cx.spawn(async move |_| {
            let result = match native.await {
                Ok(result) => result.and_then(|path| {
                    path.map(|path| normalize_system_save_path(path, required_extension.as_deref()))
                        .transpose()
                }),
                Err(_) => Ok(None),
            };
            _ = sender.send(result);
        })
        .detach();
        return receiver;
    }

    let receiver = prompt_custom(window, cx, ChooserMode::Save(options));
    let (sender, save_receiver) = oneshot::channel();
    cx.spawn(async move |_| {
        let result = match receiver.await {
            Ok(result) => result.map(|paths| paths.and_then(|paths| paths.into_iter().next())),
            Err(_) => Ok(None),
        };
        _ = sender.send(result);
    })
    .detach();
    save_receiver
}

fn prompt_custom(window: &mut Window, cx: &mut App, mode: ChooserMode) -> OpenPathReceiver {
    let viewport = window.viewport_size();
    let card_height = mode.card_height();
    let card_width =
        CARD_WIDTH.min((viewport.width.as_f32() - VIEWPORT_MARGIN * 2.0).max(px(280.0).as_f32()));
    let margin_top = ((viewport.height.as_f32() - card_height) / 2.0).max(VIEWPORT_MARGIN);
    let (sender, receiver) = oneshot::channel();
    let chooser = cx.new(|cx| PathChooser::new(mode, sender, window, cx));
    let content_chooser = chooser.clone();
    let closed_chooser = chooser;
    window.open_dialog(cx, move |dialog, _, cx| {
        let content_chooser = content_chooser.clone();
        let closed_chooser = closed_chooser.clone();
        let theme = cx.theme().colors;
        dialog
            .width(px(card_width))
            .max_w(px(CARD_WIDTH))
            .margin_top(px(margin_top))
            .p_0()
            .border_0()
            .rounded(px(14.0))
            .bg(theme.popover)
            .close_button(false)
            .keyboard(false)
            .overlay_closable(true)
            .content(move |content, _, _| content.gap_0().child(content_chooser.clone()))
            .on_close(move |_, _, cx| {
                closed_chooser.update(cx, |chooser, _| chooser.finish(Ok(None)));
            })
    });
    receiver
}

fn normalize_extension(mut path: PathBuf, required_extension: Option<&str>) -> PathBuf {
    if let Some(required_extension) = required_extension
        && !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case(required_extension))
    {
        path.set_extension(required_extension);
    }
    path
}

fn normalize_system_save_path(path: PathBuf, required_extension: Option<&str>) -> Result<PathBuf> {
    let normalized = normalize_extension(path.clone(), required_extension);
    if normalized != path && normalized.exists() {
        Err(anyhow!(
            "{} already exists; include the required extension in the file name to confirm replacement",
            normalized.display()
        ))
    } else {
        Ok(normalized)
    }
}

struct PathChooser {
    mode: ChooserMode,
    current_dir: PathBuf,
    places: Vec<ChooserPlace>,
    entries: Vec<ChooserEntry>,
    visible: Vec<usize>,
    focused: usize,
    selected: BTreeSet<PathBuf>,
    anchor: Option<PathBuf>,
    show_hidden: bool,
    active_filter: Option<usize>,
    load_state: LoadState,
    load_generation: u64,
    error: Option<SharedString>,
    pending_overwrite: Option<PathBuf>,
    creating_folder: bool,
    search_input: Entity<InputState>,
    filename_input: Entity<InputState>,
    folder_input: Entity<InputState>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    load_task: Option<Task<()>>,
    completion: Option<oneshot::Sender<Result<Option<Vec<PathBuf>>>>>,
    _search_subscription: Subscription,
    _filename_subscription: Subscription,
    _key_interceptor: Subscription,
}

impl PathChooser {
    fn new(
        mode: ChooserMode,
        completion: oneshot::Sender<Result<Option<Vec<PathBuf>>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(glyphst_ui::fl!("path-chooser-search-placeholder"))
        });
        let filename_input = cx.new(|cx| {
            let mut input = InputState::new(window, cx)
                .placeholder(glyphst_ui::fl!("path-chooser-name-placeholder"));
            if let ChooserMode::Save(options) = &mode {
                input.set_value(options.suggested_name.clone(), window, cx);
            }
            input
        });
        let folder_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(glyphst_ui::fl!("path-chooser-folder-placeholder"))
        });
        let _search_subscription = cx.subscribe_in(
            &search_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    if this.slash_descend(window, cx) {
                        return;
                    }
                    this.update_visible(cx);
                    cx.notify();
                }
            },
        );
        let _filename_subscription = cx.subscribe_in(
            &filename_input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.pending_overwrite = None;
                    this.error = None;
                    cx.notify();
                }
            },
        );
        let key_listener = cx.listener(Self::on_keystroke);
        let _key_interceptor = cx.intercept_keystrokes(key_listener);
        let initial_dir = existing_directory(mode.initial_directory());
        let active_filter = match &mode {
            ChooserMode::Open(options) if !options.filters.is_empty() => Some(0),
            ChooserMode::Open(_) | ChooserMode::Save(_) => None,
        };
        let mut chooser = Self {
            mode,
            current_dir: initial_dir.clone(),
            places: chooser_places(),
            entries: Vec::new(),
            visible: Vec::new(),
            focused: 0,
            selected: BTreeSet::new(),
            anchor: None,
            show_hidden: false,
            active_filter,
            load_state: LoadState::Loading,
            load_generation: 0,
            error: None,
            pending_overwrite: None,
            creating_folder: false,
            search_input: search_input.clone(),
            filename_input,
            folder_input,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            load_task: None,
            completion: Some(completion),
            _search_subscription,
            _filename_subscription,
            _key_interceptor,
        };
        chooser.load_directory(initial_dir, cx);
        let save_mode = matches!(&chooser.mode, ChooserMode::Save(_));
        let focus = if save_mode {
            chooser.filename_input.focus_handle(cx)
        } else {
            search_input.focus_handle(cx)
        };
        let filename_input = chooser.filename_input.clone();
        window.defer(cx, move |window, cx| {
            focus.focus(window, cx);
            if save_mode {
                filename_input.update(cx, |input, cx| input.select_all(window, cx));
            }
        });
        chooser
    }

    fn finish(&mut self, result: Result<Option<Vec<PathBuf>>>) {
        if let Some(completion) = self.completion.take() {
            _ = completion.send(result);
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish(Ok(None));
        window.close_dialog(cx);
    }

    fn load_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.current_dir = path.clone();
        self.entries.clear();
        self.visible.clear();
        self.selected.clear();
        self.anchor = None;
        self.focused = 0;
        self.error = None;
        self.pending_overwrite = None;
        self.creating_folder = false;
        self.load_state = LoadState::Loading;
        self.load_generation = self.load_generation.wrapping_add(1);
        let generation = self.load_generation;
        let show_hidden = self.show_hidden;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { read_directory(&path, show_hidden) })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.load_generation != generation {
                    return;
                }
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.load_state = LoadState::Ready;
                        this.update_visible(cx);
                    }
                    Err(error) => {
                        this.load_state = LoadState::Error(error.into());
                        this.entries.clear();
                        this.visible.clear();
                    }
                }
                this.load_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn active_filter(&self) -> Option<&PathFilter> {
        let ChooserMode::Open(options) = &self.mode else {
            return None;
        };
        self.active_filter
            .and_then(|index| options.filters.get(index))
    }

    fn entry_passes_filter(&self, entry: &ChooserEntry) -> bool {
        entry.directory
            || self
                .active_filter()
                .is_none_or(|filter| filter.matches(&entry.path))
    }

    fn update_visible(&mut self, cx: &App) {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let mut ranked = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.entry_passes_filter(entry))
            .filter_map(|(index, entry)| {
                if query.is_empty() {
                    return Some((1, index));
                }
                let name = entry.name.to_lowercase();
                if name.starts_with(&query) {
                    Some((0, index))
                } else if name.contains(&query) {
                    Some((1, index))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by_key(|&(rank, index)| (rank, index));
        self.visible = ranked.into_iter().map(|(_, index)| index).collect();
        self.focused = self.focused.min(self.visible.len().saturating_sub(1));
    }

    fn selectable_paths(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|entry| self.entry_passes_filter(entry) && self.is_selectable(entry))
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn visible_selectable_paths(&self) -> Vec<PathBuf> {
        self.visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .filter(|entry| self.is_selectable(entry))
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn is_selectable(&self, entry: &ChooserEntry) -> bool {
        match &self.mode {
            ChooserMode::Open(options) => {
                (entry.directory && options.directories) || (!entry.directory && options.files)
            }
            ChooserMode::Save(_) => false,
        }
    }

    fn navigate_to(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.load_directory(path, cx);
    }

    fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.current_dir.parent().map(Path::to_path_buf) {
            self.navigate_to(parent, window, cx);
        }
    }

    fn completion_prefix_len(name: &str, query: &str) -> Option<usize> {
        let mut len = 0;
        let mut name_chars = name.chars();
        for query_char in query.chars() {
            let name_char = name_chars.next()?;
            if !name_char.to_lowercase().eq(query_char.to_lowercase()) {
                return None;
            }
            len += name_char.len_utf8();
        }
        Some(len)
    }

    fn segment_target(names: &[&str], query: &str) -> Option<usize> {
        if let Some(index) = names.iter().position(|name| *name == query) {
            return Some(index);
        }
        if let Some(index) = names
            .iter()
            .position(|name| Self::completion_prefix_len(name, query) == Some(name.len()))
        {
            return Some(index);
        }
        let mut matches = names
            .iter()
            .enumerate()
            .filter(|(_, name)| Self::completion_prefix_len(name, query).is_some());
        let (index, _) = matches.next()?;
        matches.next().is_none().then_some(index)
    }

    fn slash_descend(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !matches!(self.load_state, LoadState::Ready) {
            return false;
        }
        let text = self.search_input.read(cx).value().to_string();
        let Some(query) = text.strip_suffix('/') else {
            return false;
        };
        if query.is_empty() || query.contains('/') {
            return false;
        }
        let directories = self
            .entries
            .iter()
            .filter(|entry| entry.directory)
            .collect::<Vec<_>>();
        let names = directories
            .iter()
            .map(|entry| entry.name.as_ref())
            .collect::<Vec<_>>();
        let Some(index) = Self::segment_target(&names, query) else {
            return false;
        };
        let path = directories[index].path.clone();
        self.navigate_to(path, window, cx);
        true
    }

    fn completion_target(&self, cx: &App) -> Option<String> {
        let query = self.search_input.read(cx).value().trim().to_string();
        if query.is_empty() || query.contains('/') {
            return None;
        }
        let active = self
            .visible
            .get(self.focused)
            .and_then(|index| self.entries.get(*index));
        active
            .filter(|entry| Self::completion_prefix_len(&entry.name, &query).is_some())
            .or_else(|| {
                self.visible
                    .iter()
                    .filter_map(|index| self.entries.get(*index))
                    .find(|entry| Self::completion_prefix_len(&entry.name, &query).is_some())
            })
            .filter(|entry| entry.name.as_ref() != query)
            .map(|entry| entry.name.to_string())
    }

    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self.completion_target(cx) else {
            return;
        };
        self.search_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
    }

    fn click_entry(
        &mut self,
        display_index: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self
            .visible
            .get(display_index)
            .and_then(|index| self.entries.get(*index))
            .cloned()
        else {
            return;
        };
        self.focused = display_index;
        self.error = None;
        self.pending_overwrite = None;
        if entry.directory && (!self.is_selectable(&entry) || event.click_count >= 2) {
            self.navigate_to(entry.path, window, cx);
            return;
        }
        if !entry.directory && matches!(&self.mode, ChooserMode::Save(_)) {
            self.filename_input.update(cx, |input, cx| {
                input.set_value(entry.name.to_string(), window, cx);
                input.select_all(window, cx);
            });
            self.filename_input.focus_handle(cx).focus(window, cx);
            if event.click_count >= 2 {
                self.confirm(window, cx);
                return;
            }
            cx.notify();
            return;
        }
        if self.is_selectable(&entry) {
            let ordered = self.visible_selectable_paths();
            let modifiers = match &self.mode {
                ChooserMode::Open(options) if options.multiple => event.modifiers,
                _ => Modifiers::default(),
            };
            apply_entry_click(
                &mut self.selected,
                &mut self.anchor,
                &ordered,
                entry.path,
                modifiers,
            );
            if event.click_count >= 2 && matches!(&self.mode, ChooserMode::Open(_)) {
                self.confirm(window, cx);
                return;
            }
        }
        cx.notify();
    }

    fn move_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        self.focused =
            (self.focused as isize + delta).rem_euclid(self.visible.len() as isize) as usize;
        self.scroll
            .scroll_to_item(self.focused, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn list_navigation_has_focus(&self, window: &Window, cx: &App) -> bool {
        if self.folder_input.focus_handle(cx).is_focused(window)
            || (matches!(self.mode, ChooserMode::Save(_))
                && self.filename_input.focus_handle(cx).is_focused(window))
        {
            return false;
        }

        self.focus_handle.contains_focused(window, cx)
            || self.search_input.focus_handle(cx).is_focused(window)
    }

    fn select_next_action(&mut self, _: &SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.list_navigation_has_focus(window, cx) {
            self.move_focus(1, cx);
        }
    }

    fn select_prev_action(&mut self, _: &SelectPrev, window: &mut Window, cx: &mut Context<Self>) {
        if self.list_navigation_has_focus(window, cx) {
            self.move_focus(-1, cx);
        }
    }

    fn activate_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self
            .visible
            .get(self.focused)
            .and_then(|index| self.entries.get(*index))
            .cloned()
        else {
            return;
        };
        if entry.directory {
            self.navigate_to(entry.path, window, cx);
        } else if matches!(self.mode, ChooserMode::Save(_)) {
            self.filename_input.update(cx, |input, cx| {
                input.set_value(entry.name.to_string(), window, cx);
                input.select_all(window, cx);
            });
            self.filename_input.focus_handle(cx).focus(window, cx);
        } else if self.is_selectable(&entry) {
            let multiple = matches!(&self.mode, ChooserMode::Open(options) if options.multiple);
            if !multiple {
                self.selected.clear();
            }
            self.selected.insert(entry.path.clone());
            self.anchor = Some(entry.path);
            self.confirm(window, cx);
        }
    }

    fn is_file_only_open(&self) -> bool {
        matches!(
            &self.mode,
            ChooserMode::Open(options) if options.files && !options.directories
        )
    }

    fn activate_file_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            self.confirm(window, cx);
            return;
        }
        self.activate_focused(window, cx);
    }

    fn toggle_focused(&mut self, cx: &mut Context<Self>) {
        if !matches!(&self.mode, ChooserMode::Open(options) if options.multiple) {
            return;
        }
        let Some(entry) = self
            .visible
            .get(self.focused)
            .and_then(|index| self.entries.get(*index))
            .filter(|entry| self.is_selectable(entry))
            .cloned()
        else {
            return;
        };
        if !self.selected.insert(entry.path.clone()) {
            self.selected.remove(&entry.path);
        }
        self.anchor = Some(entry.path);
        self.error = None;
        cx.notify();
    }

    fn select_focused_if_needed(&mut self) {
        if !self.selected.is_empty() {
            return;
        }
        let Some(entry) = self
            .visible
            .get(self.focused)
            .and_then(|index| self.entries.get(*index))
            .filter(|entry| self.is_selectable(entry))
        else {
            return;
        };
        self.selected.insert(entry.path.clone());
        self.anchor = Some(entry.path.clone());
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_filter = self.active_filter().cloned();
        let result = match &self.mode {
            ChooserMode::Open(options) => {
                let mut paths = self
                    .selectable_paths()
                    .into_iter()
                    .filter(|path| self.selected.contains(path))
                    .collect::<Vec<_>>();
                if paths.is_empty() && options.directories {
                    paths.push(self.current_dir.clone());
                }
                if paths.is_empty() {
                    self.error = Some(glyphst_ui::fl!("path-chooser-select-item").into());
                    cx.notify();
                    return;
                }
                if !options.multiple {
                    paths.truncate(1);
                }
                if let Some(filter) = active_filter.as_ref()
                    && let Some(invalid) = paths
                        .iter()
                        .find(|path| path.is_file() && !filter.matches(path))
                {
                    self.error = Some(
                        glyphst_ui::fl!(
                            "path-chooser-invalid-type",
                            name = invalid
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                            types = filter.label.to_string()
                        )
                        .into(),
                    );
                    cx.notify();
                    return;
                }
                paths
            }
            ChooserMode::Save(options) => {
                let name = self.filename_input.read(cx).value().trim().to_string();
                if !valid_child_name(&name) {
                    self.error = Some(glyphst_ui::fl!("path-chooser-invalid-name").into());
                    cx.notify();
                    return;
                }
                let path = normalize_extension(
                    self.current_dir.join(name),
                    options.required_extension.as_deref(),
                );
                if path.is_dir() {
                    self.error = Some(glyphst_ui::fl!("path-chooser-name-is-folder").into());
                    cx.notify();
                    return;
                }
                if path.exists() && self.pending_overwrite.as_ref() != Some(&path) {
                    self.pending_overwrite = Some(path.clone());
                    self.error = Some(
                        glyphst_ui::fl!(
                            "path-chooser-overwrite-confirm",
                            name = path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string()
                        )
                        .into(),
                    );
                    cx.notify();
                    return;
                }
                vec![path]
            }
        };
        self.finish(Ok(Some(result)));
        window.close_dialog(cx);
    }

    fn toggle_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.load_state, LoadState::Ready) {
            return;
        }
        self.creating_folder = !self.creating_folder;
        self.error = None;
        if self.creating_folder {
            let focus = self.folder_input.focus_handle(cx);
            window.defer(cx, move |window, cx| focus.focus(window, cx));
        }
        cx.notify();
    }

    fn create_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.folder_input.read(cx).value().trim().to_string();
        if !valid_child_name(&name) {
            self.error = Some(glyphst_ui::fl!("path-chooser-invalid-folder-name").into());
            cx.notify();
            return;
        }
        let path = self.current_dir.join(name);
        match fs::create_dir(&path) {
            Ok(()) => {
                self.creating_folder = false;
                self.folder_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.navigate_to(path, window, cx);
            }
            Err(error) => {
                self.error = Some(
                    glyphst_ui::fl!(
                        "path-chooser-create-folder-error",
                        error = error.to_string()
                    )
                    .into(),
                );
                cx.notify();
            }
        }
    }

    fn cycle_filter(&mut self, cx: &mut Context<Self>) {
        let ChooserMode::Open(options) = &self.mode else {
            return;
        };
        if options.filters.is_empty() {
            return;
        }
        self.active_filter = match self.active_filter {
            Some(index) if index + 1 < options.filters.len() => Some(index + 1),
            Some(_) if options.allow_all_files => None,
            Some(_) => Some(0),
            None => Some(0),
        };
        self.error = None;
        self.update_visible(cx);
        let selectable = self.selectable_paths().into_iter().collect::<HashSet<_>>();
        self.selected.retain(|path| selectable.contains(path));
        if self
            .anchor
            .as_ref()
            .is_some_and(|anchor| !selectable.contains(anchor))
        {
            self.anchor = None;
        }
        cx.notify();
    }

    fn filter_label(&self) -> Option<SharedString> {
        let ChooserMode::Open(options) = &self.mode else {
            return None;
        };
        if options.filters.is_empty() {
            return None;
        }
        Some(
            self.active_filter
                .and_then(|index| options.filters.get(index))
                .map(|filter| filter.label.clone())
                .unwrap_or_else(|| glyphst_ui::fl!("path-chooser-all-files").into()),
        )
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.load_directory(self.current_dir.clone(), cx);
    }

    fn on_keystroke(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.focus_handle.contains_focused(window, cx)
            && !self.search_input.focus_handle(cx).is_focused(window)
            && !self.filename_input.focus_handle(cx).is_focused(window)
            && !self.folder_input.focus_handle(cx).is_focused(window)
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            if self.creating_folder {
                self.creating_folder = false;
                cx.notify();
            } else {
                self.cancel(window, cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.folder_input.focus_handle(cx).is_focused(window) {
            if key == "enter" {
                self.create_folder(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.filename_input.focus_handle(cx).is_focused(window)
            && matches!(self.mode, ChooserMode::Save(_))
        {
            if key == "enter" {
                self.confirm(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let modifiers = event.keystroke.modifiers;
        let secondary = modifiers.secondary();
        if key == "enter" && secondary && !self.is_file_only_open() {
            self.select_focused_if_needed();
            self.confirm(window, cx);
            cx.stop_propagation();
            return;
        }
        let handled = match key {
            "left" => {
                self.go_up(window, cx);
                true
            }
            "right" => {
                self.activate_focused(window, cx);
                true
            }
            "enter" => {
                if self.is_file_only_open() {
                    self.activate_file_open(window, cx);
                } else {
                    self.activate_focused(window, cx);
                }
                true
            }
            "tab" => {
                self.accept_completion(window, cx);
                true
            }
            "space" if secondary || self.search_input.read(cx).value().is_empty() => {
                self.toggle_focused(cx);
                true
            }
            "backspace" if self.search_input.read(cx).value().is_empty() => {
                self.go_up(window, cx);
                true
            }
            "h" if event.keystroke.modifiers.control => {
                self.show_hidden = !self.show_hidden;
                self.load_directory(self.current_dir.clone(), cx);
                true
            }
            _ => false,
        };
        if handled {
            cx.stop_propagation();
        }
    }

    fn can_confirm(&self, cx: &App) -> bool {
        match &self.mode {
            ChooserMode::Open(options) => !self.selected.is_empty() || options.directories,
            ChooserMode::Save(_) => !self.filename_input.read(cx).value().trim().is_empty(),
        }
    }

    fn can_activate_file_open(&self) -> bool {
        if !matches!(self.load_state, LoadState::Ready) {
            return false;
        }
        if !self.selected.is_empty() {
            return true;
        }
        self.visible
            .get(self.focused)
            .and_then(|index| self.entries.get(*index))
            .is_some_and(|entry| entry.directory || self.is_selectable(entry))
    }

    fn breadcrumbs(&self) -> Vec<(SharedString, PathBuf)> {
        if let Some(home) = dirs::home_dir()
            && let Ok(relative_path) = self.current_dir.strip_prefix(&home)
        {
            let mut path = home.clone();
            let mut breadcrumbs = vec![(glyphst_ui::fl!("document-browser-home").into(), home)];
            for component in relative_path.components() {
                path.push(component.as_os_str());
                breadcrumbs.push((
                    component.as_os_str().to_string_lossy().into_owned().into(),
                    path.clone(),
                ));
            }
            return breadcrumbs;
        }

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

    fn active_place_index(&self) -> Option<usize> {
        self.places
            .iter()
            .enumerate()
            .filter(|(_, place)| self.current_dir.starts_with(&place.path))
            .max_by_key(|(_, place)| place.path.components().count())
            .map(|(index, _)| index)
    }

    fn key_cap(label: impl Into<SharedString>, theme: &gpui_component::ThemeColor) -> gpui::Div {
        div()
            .h(px(22.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.foreground.opacity(0.06))
            .text_size(ui_rems(10.5))
            .text_color(theme.muted_foreground.opacity(0.82))
            .child(label.into())
    }

    fn footer_hint(
        key: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &gpui_component::ThemeColor,
    ) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(Self::key_cap(key, theme))
            .child(
                div()
                    .text_size(ui_rems(10.5))
                    .text_color(theme.muted_foreground.opacity(0.7))
                    .child(label.into()),
            )
    }

    fn footer_hint_pair(
        first: impl Into<SharedString>,
        second: impl Into<SharedString>,
        label: impl Into<SharedString>,
        theme: &gpui_component::ThemeColor,
    ) -> gpui::Div {
        let cap = div()
            .h(px(22.0))
            .px(px(5.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(5.0))
            .bg(theme.foreground.opacity(0.06))
            .text_size(ui_rems(10.5))
            .text_color(theme.muted_foreground.opacity(0.82))
            .child(first.into())
            .child(
                div()
                    .w(px(1.0))
                    .h(px(11.0))
                    .bg(theme.foreground.opacity(0.1)),
            )
            .child(second.into());
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .child(cap)
            .child(
                div()
                    .text_size(ui_rems(10.5))
                    .text_color(theme.muted_foreground.opacity(0.7))
                    .child(label.into()),
            )
    }

    fn render_places(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let active_place = self.active_place_index();
        div()
            .id("path-chooser-places")
            .debug_selector(|| "path-chooser-places".to_string())
            .w(px(PLACES_WIDTH))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .px(px(8.0))
            .py(px(8.0))
            .border_r_1()
            .border_color(theme.border.opacity(0.72))
            .bg(theme.muted.opacity(0.16))
            .child(
                div()
                    .px(px(8.0))
                    .pt(px(2.0))
                    .pb(px(4.0))
                    .text_size(ui_rems(11.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.muted_foreground.opacity(0.74))
                    .child(glyphst_ui::fl!("document-browser-places")),
            )
            .children(self.places.iter().enumerate().map(|(index, place)| {
                let path = place.path.clone();
                let selected = active_place == Some(index);
                div()
                    .id(("path-chooser-place", index))
                    .h(px(28.0))
                    .w_full()
                    .min_w_0()
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .text_size(ui_rems(12.5))
                    .border_1()
                    .border_color(if selected {
                        theme.list_active_border
                    } else {
                        transparent_black()
                    })
                    .bg(if selected {
                        theme.list_active
                    } else {
                        transparent_black()
                    })
                    .text_color(if selected {
                        theme.foreground
                    } else {
                        theme.muted_foreground
                    })
                    .when(!selected, |row| {
                        row.hover(move |style| style.bg(theme.list_hover))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_to(path.clone(), window, cx);
                        cx.stop_propagation();
                    }))
                    .child(
                        Icon::new(place.icon)
                            .size(ui_rems(14.0))
                            .text_color(theme.muted_foreground.opacity(0.82)),
                    )
                    .child(div().min_w_0().truncate().child(place.label.clone()))
            }))
            .child(div().flex_1())
            .into_any_element()
    }

    fn render_entry(&self, display_index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let entry = self.entries[self.visible[display_index]].clone();
        let selected = self.selected.contains(&entry.path);
        let focused = self.focused == display_index;
        let kind = if entry.directory {
            glyphst_ui::fl!("document-browser-folder-kind")
        } else {
            entry
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_uppercase)
                .unwrap_or_else(|| glyphst_ui::fl!("path-chooser-file-kind"))
        };
        div()
            .h(px(ROW_HEIGHT))
            .w_full()
            .px(px(8.0))
            .py(px(2.0))
            .child(
                div()
                    .id(("path-chooser-entry", display_index))
                    .debug_selector(move || format!("path-chooser-entry-{display_index}"))
                    .size_full()
                    .min_w_0()
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .border_1()
                    .border_color(if focused {
                        theme.list_active_border
                    } else {
                        transparent_black()
                    })
                    .bg(if selected {
                        theme.list_active
                    } else if focused {
                        theme.list_hover
                    } else {
                        transparent_black()
                    })
                    .hover(move |style| style.bg(theme.list_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.click_entry(display_index, event, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        Icon::new(if entry.directory {
                            IconName::FolderOpen
                        } else {
                            IconName::FileText
                        })
                        .size(ui_rems(15.0))
                        .text_color(
                            theme.muted_foreground.opacity(if entry.directory {
                                0.86
                            } else {
                                0.72
                            }),
                        ),
                    )
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("path-chooser-entry-label-{display_index}")
                            })
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_color(theme.foreground)
                            .child(entry.name),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(10.5))
                            .text_color(theme.muted_foreground.opacity(0.78))
                            .child(kind),
                    ),
            )
            .into_any_element()
    }

    fn render_new_folder(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        div()
            .id("path-chooser-new-folder-row")
            .debug_selector(|| "path-chooser-new-folder-row".to_string())
            .h(px(36.0))
            .flex_none()
            .px(px(8.0))
            .py(px(4.0))
            .child(
                div()
                    .size_full()
                    .min_w_0()
                    .px(px(8.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.list_active_border)
                    .bg(theme.list_active)
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size(ui_rems(14.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.folder_input).appearance(false)),
                    )
                    .child(
                        div()
                            .id("path-chooser-create-folder")
                            .h(px(22.0))
                            .px(px(8.0))
                            .rounded(px(5.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .bg(theme.primary)
                            .text_size(ui_rems(11.5))
                            .text_color(theme.primary_foreground)
                            .hover(move |style| style.bg(theme.primary_hover))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create_folder(window, cx);
                                cx.stop_propagation();
                            }))
                            .child(glyphst_ui::fl!("path-chooser-create")),
                    ),
            )
            .into_any_element()
    }

    fn render_loading(_: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .px(px(16.0))
            .py(px(8.0))
            .children((0..7).map(|index| {
                Skeleton::new()
                    .h(px(24.0))
                    .w(relative(if index % 3 == 0 { 0.68 } else { 0.9 }))
                    .rounded(px(8.0))
            }))
            .into_any_element()
    }

    fn render_listing(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().colors;
        let body = match &self.load_state {
            LoadState::Loading => Self::render_loading(cx),
            LoadState::Error(error) => {
                let error = error.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(10.0))
                    .px(px(24.0))
                    .text_center()
                    .text_color(theme.muted_foreground)
                    .child(error)
                    .child(
                        div()
                            .id("path-chooser-retry")
                            .h(px(26.0))
                            .px(px(10.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .text_color(theme.foreground)
                            .hover(move |style| style.bg(theme.list_hover))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.retry(cx);
                                cx.stop_propagation();
                            }))
                            .child(glyphst_ui::fl!("path-chooser-retry")),
                    )
                    .into_any_element()
            }
            LoadState::Ready if self.visible.is_empty() => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(if self.search_input.read(cx).value().is_empty() {
                    glyphst_ui::fl!("path-chooser-empty-folder")
                } else {
                    glyphst_ui::fl!("path-chooser-no-results")
                })
                .into_any_element(),
            LoadState::Ready => {
                let count = self.visible.len();
                let scroll = self.scroll.clone();
                div()
                    .size_full()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "path-chooser-list",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|index| this.render_entry(index, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(
                        &self.scroll,
                        "path-chooser-scrollbar",
                        window,
                        cx,
                    )
                    .into_any_element()
            }
        };

        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .when(self.creating_folder, |column| {
                column.child(self.render_new_folder(cx))
            })
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

impl Focusable for PathChooser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PathChooser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let hairline = theme.border.opacity(0.72);
        let band = theme.muted.opacity(0.28);
        let save_confirm_label = if self.pending_overwrite.is_some() {
            glyphst_ui::fl!("path-chooser-replace")
        } else {
            glyphst_ui::fl!("path-chooser-save")
        };
        let open_mode = matches!(&self.mode, ChooserMode::Open(_));
        let file_only_open = self.is_file_only_open();
        let primary_modifier: SharedString = if cfg!(target_os = "macos") {
            "⌘".into()
        } else {
            "Ctrl".into()
        };
        let action_enabled = if file_only_open {
            self.can_activate_file_open()
        } else {
            self.can_confirm(cx)
        };
        let breadcrumbs = self.breadcrumbs();
        let last_crumb = breadcrumbs.len().saturating_sub(1);
        let filter_label = self.filter_label();
        let ready = matches!(self.load_state, LoadState::Ready);
        let show_selection_count = matches!(&self.mode, ChooserMode::Open(options) if options.multiple)
            || !self.selected.is_empty();

        let action = div()
            .id("path-chooser-confirm")
            .debug_selector(|| "path-chooser-confirm".to_string())
            .h(px(22.0))
            .px(px(9.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .bg(theme.primary)
            .text_size(ui_rems(11.5))
            .text_color(theme.primary_foreground)
            .when(!action_enabled, |button| button.opacity(0.55))
            .when(action_enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.primary_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            if this.is_file_only_open() {
                                this.activate_file_open(window, cx);
                            } else {
                                this.confirm(window, cx);
                            }
                            cx.stop_propagation();
                        }),
                    )
            })
            .when(file_only_open, |button| button.child("Enter"))
            .when(open_mode && !file_only_open, |button| {
                button
                    .child(
                        div()
                            .debug_selector(|| "path-chooser-confirm-modifier".to_string())
                            .text_size(ui_rems(10.5))
                            .text_color(theme.primary_foreground.opacity(0.82))
                            .child(primary_modifier),
                    )
                    .child("Enter")
            })
            .when(!open_mode, |button| button.child(save_confirm_label));

        let header = div()
            .id("path-chooser-header")
            .debug_selector(|| "path-chooser-header".to_string())
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .pl(px(12.0))
            .pr(px(10.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .bg(band)
            .border_b_1()
            .border_color(hairline)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(ui_rems(14.0))
                    .child(Input::new(&self.search_input).appearance(false)),
            )
            .child(action)
            .child(
                Self::key_cap("esc", &theme)
                    .id("path-chooser-cancel")
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.foreground.opacity(0.1)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.cancel(window, cx);
                        cx.stop_propagation();
                    })),
            );

        let new_folder = div()
            .id("path-chooser-new-folder")
            .debug_selector(|| "path-chooser-new-folder".to_string())
            .size(px(24.0))
            .flex_none()
            .rounded(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(if self.creating_folder {
                theme.list_active_border
            } else {
                transparent_black()
            })
            .bg(if self.creating_folder {
                theme.list_active
            } else {
                transparent_black()
            })
            .when(ready, |button| {
                button
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.list_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_new_folder(window, cx);
                            cx.stop_propagation();
                        }),
                    )
            })
            .when(!ready, |button| button.opacity(0.5))
            .tooltip(glyphst_ui::tooltip::text_tooltip(glyphst_ui::fl!(
                "path-chooser-new-folder"
            )))
            .child(
                Icon::new(IconName::Plus)
                    .size(ui_rems(14.0))
                    .text_color(theme.muted_foreground.opacity(0.82)),
            );

        let crumbs =
            div()
                .id("path-chooser-breadcrumbs")
                .debug_selector(|| "path-chooser-breadcrumbs".to_string())
                .h(px(38.0))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(6.0))
                .pl(px(10.0))
                .pr(px(8.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .text_size(ui_rems(11.0))
                        .font_family(cx.theme().mono_font_family.clone())
                        .children(breadcrumbs.into_iter().enumerate().map(
                            |(index, (label, path))| {
                                let current = index == last_crumb;
                                let crumb = div()
                                    .id(("path-chooser-crumb", index))
                                    .px(px(3.0))
                                    .rounded(px(4.0))
                                    .text_color(if current {
                                        theme.foreground.opacity(0.9)
                                    } else {
                                        theme.muted_foreground.opacity(0.72)
                                    })
                                    .child(label);
                                let crumb: AnyElement = if current {
                                    crumb.into_any_element()
                                } else {
                                    crumb
                                        .cursor_pointer()
                                        .hover(move |style| {
                                            style.text_color(theme.foreground).bg(theme.list_hover)
                                        })
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.navigate_to(path.clone(), window, cx);
                                            cx.stop_propagation();
                                        }))
                                        .into_any_element()
                                };
                                div()
                                    .flex()
                                    .items_center()
                                    .when(index > 0, |item| {
                                        item.child(
                                            div()
                                                .text_color(theme.muted_foreground.opacity(0.55))
                                                .child("/"),
                                        )
                                    })
                                    .child(crumb)
                            },
                        )),
                )
                .child(new_folder);

        let body = div()
            .id("path-chooser-body")
            .debug_selector(|| "path-chooser-body".to_string())
            .h(px(BODY_HEIGHT))
            .flex_none()
            .flex()
            .items_stretch()
            .child(self.render_places(cx))
            .child(
                div()
                    .id("path-chooser-browser")
                    .debug_selector(|| "path-chooser-browser".to_string())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .bg(theme.background)
                    .child(crumbs)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .child(self.render_listing(window, cx)),
                    ),
            );

        let save_row = matches!(&self.mode, ChooserMode::Save(_)).then(|| {
            div()
                .id("path-chooser-save-name")
                .debug_selector(|| "path-chooser-save-name".to_string())
                .h(px(SAVE_ROW_HEIGHT))
                .flex_none()
                .px(px(12.0))
                .flex()
                .items_center()
                .gap(px(10.0))
                .border_t_1()
                .border_color(hairline)
                .bg(theme.background)
                .child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(11.5))
                        .text_color(theme.muted_foreground)
                        .child(glyphst_ui::fl!("path-chooser-name")),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(px(28.0))
                        .px(px(8.0))
                        .rounded(px(7.0))
                        .bg(theme.foreground.opacity(0.04))
                        .flex()
                        .items_center()
                        .child(Input::new(&self.filename_input).appearance(false)),
                )
        });

        let footer_left: AnyElement = if let Some(error) = self.error.clone() {
            div()
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(6.0))
                .text_size(ui_rems(11.0))
                .text_color(theme.danger)
                .child(
                    Icon::new(IconName::TriangleAlert)
                        .size(ui_rems(12.0))
                        .flex_none(),
                )
                .child(div().min_w_0().truncate().child(error))
                .into_any_element()
        } else {
            div()
                .min_w_0()
                .flex_1()
                .overflow_hidden()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(Self::footer_hint_pair(
                    "↑",
                    "↓",
                    glyphst_ui::fl!("document-browser-navigate"),
                    &theme,
                ))
                .child(Self::footer_hint(
                    "←",
                    glyphst_ui::fl!("document-browser-up"),
                    &theme,
                ))
                .child(Self::footer_hint(
                    "→",
                    glyphst_ui::fl!("document-browser-open"),
                    &theme,
                ))
                .child(Self::footer_hint(
                    "tab",
                    glyphst_ui::fl!("path-chooser-complete"),
                    &theme,
                ))
                .into_any_element()
        };

        let footer = div()
            .id("path-chooser-footer")
            .debug_selector(|| "path-chooser-footer".to_string())
            .h(px(FOOTER_HEIGHT))
            .flex_none()
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .border_t_1()
            .border_color(hairline)
            .bg(band)
            .child(footer_left)
            .when(self.show_hidden, |footer| {
                footer.child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(10.5))
                        .text_color(theme.muted_foreground)
                        .child(glyphst_ui::fl!("path-chooser-hidden-visible")),
                )
            })
            .when(show_selection_count, |footer| {
                footer.child(
                    div()
                        .flex_none()
                        .text_size(ui_rems(10.5))
                        .text_color(theme.muted_foreground)
                        .child(glyphst_ui::fl!(
                            "path-chooser-selected-count",
                            count = self.selected.len() as i64
                        )),
                )
            })
            .when_some(filter_label, |footer, label| {
                footer.child(
                    div()
                        .id("path-chooser-filter")
                        .h(px(24.0))
                        .max_w(px(150.0))
                        .px(px(7.0))
                        .rounded(px(6.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .border_1()
                        .border_color(theme.border)
                        .cursor_pointer()
                        .text_size(ui_rems(10.5))
                        .text_color(theme.muted_foreground)
                        .hover(move |style| style.bg(theme.list_hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.cycle_filter(cx);
                            cx.stop_propagation();
                        }))
                        .child(div().min_w_0().truncate().child(label))
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(ui_rems(11.0))
                                .flex_none(),
                        ),
                )
            });

        div()
            .id("path-chooser")
            .debug_selector(|| "path-chooser".to_string())
            .key_context("PathChooser Menu")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next_action))
            .on_action(cx.listener(Self::select_prev_action))
            .w_full()
            .h(px(self.mode.card_height()))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(14.0))
            .border_1()
            .border_color(hairline)
            .bg(theme.popover)
            .text_size(ui_rems(12.5))
            .text_color(theme.popover_foreground)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.focus_handle.focus(window, cx);
                    }
                }),
            )
            .child(header)
            .child(body)
            .children(save_row)
            .child(footer)
    }
}

fn existing_directory(path: &Path) -> PathBuf {
    let mut candidate = path.to_path_buf();
    if candidate.is_file() {
        candidate.pop();
    }
    while !candidate.is_dir() && candidate.pop() {}
    if candidate.is_dir() {
        candidate
    } else {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }
}

fn chooser_places() -> Vec<ChooserPlace> {
    fn add_place(
        places: &mut Vec<ChooserPlace>,
        seen: &mut HashSet<PathBuf>,
        label: impl Into<SharedString>,
        path: Option<PathBuf>,
        icon: IconName,
    ) {
        let Some(path) = path.filter(|path| path.is_dir()) else {
            return;
        };
        if seen.insert(path.clone()) {
            places.push(ChooserPlace {
                label: label.into(),
                path,
                icon,
            });
        }
    }

    let mut places = Vec::new();
    let mut seen = HashSet::new();
    add_place(
        &mut places,
        &mut seen,
        glyphst_ui::fl!("document-browser-home"),
        dirs::home_dir(),
        IconName::Home,
    );
    add_place(
        &mut places,
        &mut seen,
        glyphst_ui::fl!("document-browser-documents"),
        dirs::document_dir(),
        IconName::FileText,
    );
    add_place(
        &mut places,
        &mut seen,
        glyphst_ui::fl!("document-browser-downloads"),
        dirs::download_dir(),
        IconName::Download,
    );
    places
}

fn valid_child_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && Path::new(name)
            .file_name()
            .is_some_and(|file_name| file_name == name)
}

fn read_directory(path: &Path, show_hidden: bool) -> Result<Vec<ChooserEntry>, String> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let metadata = fs::metadata(entry.path()).ok()?;
            (metadata.is_dir() || metadata.is_file()).then_some(ChooserEntry {
                path: entry.path(),
                name: name.into(),
                directory: metadata.is_dir(),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right.directory.cmp(&left.directory).then_with(|| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        })
    });
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt as _;
    use glyphst_ui::settings::sync_component_theme;
    use gpui::{KeyBinding, TestAppContext};
    use gpui_component::Root;

    struct TestHost;

    impl Render for TestHost {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn temporary_chooser_directory(label: &str, names: &[&str]) -> (PathBuf, Vec<PathBuf>) {
        let directory = std::env::temp_dir().join(format!(
            "glyphst-path-chooser-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let paths = names
            .iter()
            .map(|name| {
                let path = directory.join(name);
                fs::write(&path, b"test").unwrap();
                path
            })
            .collect();
        (directory, paths)
    }

    fn resolved_paths(receiver: OpenPathReceiver) -> Vec<PathBuf> {
        receiver
            .now_or_never()
            .expect("chooser receiver should be ready")
            .expect("chooser sender should remain connected")
            .expect("chooser should succeed")
            .expect("chooser should return selected paths")
    }

    fn remove_chooser_directory(directory: &Path, paths: &[PathBuf]) {
        for path in paths {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn required_extension_is_normalized() {
        assert_eq!(
            normalize_extension(PathBuf::from("paper.txt"), Some("pdf")),
            PathBuf::from("paper.pdf")
        );
        assert_eq!(
            normalize_extension(PathBuf::from("paper.PDF"), Some("pdf")),
            PathBuf::from("paper.PDF")
        );
    }

    #[test]
    fn filter_matching_is_case_insensitive() {
        let filter = PathFilter::new("Documents", ["pdf", ".epub"]);
        assert!(filter.matches(Path::new("paper.PDF")));
        assert!(filter.matches(Path::new("book.epub")));
        assert!(!filter.matches(Path::new("notes.txt")));
    }

    #[test]
    fn completion_prefixes_are_case_insensitive_and_unicode_safe() {
        assert_eq!(
            PathChooser::completion_prefix_len("Documents", "doc"),
            Some(3)
        );
        assert_eq!(
            PathChooser::completion_prefix_len("Documents", "DOC"),
            Some(3)
        );
        assert_eq!(PathChooser::completion_prefix_len("héllo", "HÉ"), Some(3));
        assert_eq!(PathChooser::completion_prefix_len("dev", "devel"), None);
        assert_eq!(
            PathChooser::completion_prefix_len("Documents", "notes"),
            None
        );
    }

    #[test]
    fn slash_descent_requires_an_exact_or_unique_prefix() {
        let names = ["Documents", "Downloads", "Pictures"];
        assert_eq!(PathChooser::segment_target(&names, "Documents"), Some(0));
        assert_eq!(PathChooser::segment_target(&names, "documents"), Some(0));
        assert_eq!(PathChooser::segment_target(&names, "pic"), Some(2));
        assert_eq!(PathChooser::segment_target(&names, "do"), None);
    }

    #[test]
    fn save_palette_reserves_the_filename_row() {
        let open = ChooserMode::Open(OpenPathOptions::files(".", "Open"));
        let save = ChooserMode::Save(SavePathOptions::new(".", "Save", "report.pdf"));
        assert_eq!(
            open.card_height(),
            HEADER_HEIGHT + BODY_HEIGHT + FOOTER_HEIGHT
        );
        assert_eq!(save.card_height(), open.card_height() + SAVE_ROW_HEIGHT);
    }

    #[test]
    fn system_extension_normalization_does_not_bypass_overwrite_confirmation() {
        let directory = std::env::temp_dir().join(format!(
            "glyphst-path-chooser-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let existing = directory.join("report.pdf");
        fs::write(&existing, b"existing").unwrap();

        assert!(normalize_system_save_path(directory.join("report.txt"), Some("pdf")).is_err());
        assert_eq!(
            normalize_system_save_path(existing.clone(), Some("pdf")).unwrap(),
            existing
        );

        fs::remove_file(directory.join("report.pdf")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn child_names_cannot_escape_the_current_directory() {
        assert!(valid_child_name("Papers"));
        assert!(!valid_child_name(""));
        assert!(!valid_child_name(".."));
        assert!(!valid_child_name("a/b"));
    }

    #[gpui::test]
    fn shared_item_navigation_actions_move_custom_dialog_focus(cx: &mut TestAppContext) {
        let (directory, paths) =
            temporary_chooser_directory("item-navigation", &["a.pdf", "b.pdf"]);
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-n", SelectNext, Some("Menu")),
                KeyBinding::new("ctrl-p", SelectPrev, Some("Menu")),
            ]);
            let mut settings = SettingsStore::memory();
            settings.settings.path_chooser = PathChooserBackend::Custom;
            settings.install_theme_catalog(cx).unwrap();
            cx.set_global(settings);
            sync_component_theme(cx);
        });
        let (sender, _receiver) = oneshot::channel();
        let mode = ChooserMode::Open(OpenPathOptions::files(directory.clone(), "Open a document"));
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let chooser = cx.new(|cx| PathChooser::new(mode, sender, window, cx));
            Root::new(chooser, window, cx)
        });
        let chooser = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<PathChooser>().unwrap()
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        chooser.read_with(cx, |chooser, _| {
            assert_eq!(chooser.visible.len(), 2);
            assert_eq!(chooser.focused, 0);
        });

        cx.simulate_keystrokes("ctrl-n");
        chooser.read_with(cx, |chooser, _| assert_eq!(chooser.focused, 1));

        cx.simulate_keystrokes("ctrl-p");
        chooser.read_with(cx, |chooser, _| assert_eq!(chooser.focused, 0));

        chooser.update_in(cx, |chooser, window, cx| {
            chooser.creating_folder = true;
            chooser.folder_input.focus_handle(cx).focus(window, cx);
            cx.notify();
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_keystrokes("ctrl-n");
        chooser.read_with(cx, |chooser, _| assert_eq!(chooser.focused, 0));

        remove_chooser_directory(&directory, &paths);
    }

    #[gpui::test]
    fn custom_dialog_uses_the_comet_palette_geometry(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.settings.path_chooser = PathChooserBackend::Custom;
            settings.install_theme_catalog(cx).unwrap();
            cx.set_global(settings);
            sync_component_theme(cx);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| TestHost);
            Root::new(host, window, cx)
        });
        cx.update(|window, cx| {
            let _receiver =
                prompt_for_paths(window, cx, OpenPathOptions::files(".", "Open a document"));
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let card = cx
            .debug_bounds("path-chooser")
            .expect("chooser card bounds");
        let header = cx
            .debug_bounds("path-chooser-header")
            .expect("chooser header bounds");
        let body = cx
            .debug_bounds("path-chooser-body")
            .expect("chooser body bounds");
        let places = cx
            .debug_bounds("path-chooser-places")
            .expect("chooser places bounds");
        let browser = cx
            .debug_bounds("path-chooser-browser")
            .expect("chooser browser bounds");
        let breadcrumbs = cx
            .debug_bounds("path-chooser-breadcrumbs")
            .expect("chooser breadcrumb bounds");
        let new_folder = cx
            .debug_bounds("path-chooser-new-folder")
            .expect("new folder button bounds");
        let footer = cx
            .debug_bounds("path-chooser-footer")
            .expect("chooser footer bounds");

        assert_eq!(card.size.width, px(CARD_WIDTH));
        assert_eq!(
            card.size.height,
            px(HEADER_HEIGHT + BODY_HEIGHT + FOOTER_HEIGHT)
        );
        assert_eq!(header.size.height, px(HEADER_HEIGHT));
        assert_eq!(body.size.height, px(BODY_HEIGHT));
        assert_eq!(places.size.width, px(PLACES_WIDTH));
        assert_eq!(footer.size.height, px(FOOTER_HEIGHT));
        assert_eq!(header.bottom(), body.top());
        assert_eq!(body.bottom(), footer.top());
        assert_eq!(places.left(), body.left());
        assert_eq!(places.right(), browser.left());
        assert_eq!(browser.right(), body.right());
        assert!(new_folder.left() >= breadcrumbs.left());
        assert!(new_folder.right() <= breadcrumbs.right());
        assert!(new_folder.top() >= breadcrumbs.top());
        assert!(new_folder.bottom() <= breadcrumbs.bottom());
        assert!(cx.debug_bounds("path-chooser-confirm-modifier").is_none());

        cx.simulate_click(new_folder.center(), Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("path-chooser-new-folder-row").is_some());
    }

    #[gpui::test]
    fn open_action_activates_a_focused_file_without_prior_selection(cx: &mut TestAppContext) {
        let (directory, paths) = temporary_chooser_directory("focused-open", &["paper.pdf"]);
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.settings.path_chooser = PathChooserBackend::Custom;
            settings.install_theme_catalog(cx).unwrap();
            cx.set_global(settings);
            sync_component_theme(cx);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|_| TestHost);
            Root::new(host, window, cx)
        });
        let receiver = cx.update(|window, cx| {
            prompt_for_paths(
                window,
                cx,
                OpenPathOptions::files(directory.clone(), "Open a document").multiple(true),
            )
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let action = cx
            .debug_bounds("path-chooser-confirm")
            .expect("open action bounds");
        cx.simulate_click(action.center(), Modifiers::default());
        cx.run_until_parked();

        assert_eq!(resolved_paths(receiver), paths);
        remove_chooser_directory(&directory, &paths);
    }

    #[gpui::test]
    fn modified_pointer_selection_is_confirmed_as_a_set(cx: &mut TestAppContext) {
        let (directory, paths) =
            temporary_chooser_directory("multi-open", &["a.pdf", "b.pdf", "c.pdf"]);
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.settings.path_chooser = PathChooserBackend::Custom;
            settings.install_theme_catalog(cx).unwrap();
            cx.set_global(settings);
            sync_component_theme(cx);
        });
        let (sender, receiver) = oneshot::channel();
        let mode = ChooserMode::Open(
            OpenPathOptions::files(directory.clone(), "Open documents").multiple(true),
        );
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let chooser = cx.new(|cx| PathChooser::new(mode, sender, window, cx));
            Root::new(chooser, window, cx)
        });
        let chooser = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<PathChooser>().unwrap()
        });
        cx.run_until_parked();
        chooser.update_in(cx, |chooser, window, cx| {
            let event = |modifiers| MouseDownEvent {
                position: Default::default(),
                modifiers,
                button: MouseButton::Left,
                click_count: 1,
                first_mouse: false,
            };
            chooser.click_entry(0, &event(Modifiers::default()), window, cx);
            chooser.click_entry(2, &event(Modifiers::secondary_key()), window, cx);
            chooser.click_entry(
                1,
                &event(Modifiers {
                    shift: true,
                    ..Modifiers::secondary_key()
                }),
                window,
                cx,
            );
            chooser.click_entry(1, &event(Modifiers::secondary_key()), window, cx);
            assert_eq!(
                chooser.selected,
                BTreeSet::from([paths[0].clone(), paths[2].clone()])
            );
            chooser.activate_file_open(window, cx);
        });
        cx.run_until_parked();

        assert_eq!(
            resolved_paths(receiver),
            vec![paths[0].clone(), paths[2].clone()]
        );
        remove_chooser_directory(&directory, &paths);
    }

    #[gpui::test]
    fn system_backend_routes_save_requests_to_the_native_dialog(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.settings.path_chooser = PathChooserBackend::System;
            cx.set_global(settings);
        });
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            let _receiver = prompt_for_new_path(
                window,
                cx,
                SavePathOptions::new(".", "Save", "report.pdf").required_extension("pdf"),
            );
        });
        assert!(cx.did_prompt_for_new_path());
    }
}
