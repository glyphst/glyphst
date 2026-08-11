use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Datelike as _, Local, Utc};
use glyphst_core::DocumentFormat;
use glyphst_ui::style::{SURFACE_GAP, SURFACE_INSET};
use gpui::{
    AnyElement, App, Context, Corners, DragMoveEvent, Entity, EventEmitter, ExternalPaths,
    FocusHandle, IntoElement, ListSizingBehavior, Modifiers, MouseButton, MouseDownEvent,
    MouseExitEvent, PathPromptOptions, Pixels, PromptButton, PromptLevel, Render, RenderImage,
    ScrollStrategy, SharedString, Subscription, Task, UniformListScrollHandle, Window, canvas, div,
    fill, point, prelude::*, px, rgb, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeColor,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::CancelDialog,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
};
use image::{Frame, ImageFormat, RgbaImage};
use mupdf::{Colorspace, Device, DisplayList, Matrix, Pixmap, Rect};
use smallvec::smallvec;

pub use glyphst_actions::new_tab::{
    ActivateFocused, CancelSelect, CursorDown, CursorFirst, CursorLast, CursorLeft, CursorRight,
    CursorUp, DeleteSelected, EnterSelect, ExtendSelectionDown, ExtendSelectionFirst,
    ExtendSelectionLast, ExtendSelectionLeft, ExtendSelectionRight, ExtendSelectionUp, SelectAll,
    ToggleFocused,
};

use crate::navigation_rail::{slim_navigation_button, slim_navigation_rail};
use crate::{
    document_view::{NextHalfPage, PreviousHalfPage, ScrollLineDown, ScrollLineUp},
    icons::{Icon, IconName},
    library::{
        BrowserPresentation, DocumentId, HistoryEntry, HomeDestination, LibraryCollectionId,
        LibraryCreator, LibraryCreatorRole, LibraryFilter, LibraryImportOutcome, LibraryItem,
        LibraryItemKind, LibraryItemMetadata, LibraryNodeId, LibrarySort, LibrarySortDirection,
        LibrarySortField, LibraryStore, LibraryTagId, LibraryTrashFilter, LibraryViewMode,
        QuickOpenSource, ReaderNewTabDestination, ReaderNewTabSession, ReadingStatus,
        prepare_managed_import,
    },
    scrollbar::{ScrollbarHandle as _, WithScrollbar as _},
    settings::{NewTabViewMode, SettingsStore, ui_rems},
};

const BOARD_WIDTH: f32 = 900.0;
const BOARD_HEIGHT: f32 = 700.0;
const CARD_WIDTH: f32 = 220.0;
const CARD_HEIGHT: f32 = 330.0;
const THUMBNAIL_MAX_WIDTH: f32 = 170.0;
const THUMBNAIL_MAX_HEIGHT: f32 = 240.0;
const GRID_GAP: f32 = 18.0;
const GRID_PADDING_X: f32 = 24.0;
const VIEWPORT_WIDTH_EPSILON: f32 = 0.5;
const DEFAULT_COLS: usize = 3;
const GRID_ROW_HEIGHT: f32 = CARD_HEIGHT + GRID_GAP;
const LIST_ROW_HEIGHT: f32 = 38.0;
const NEW_TAB_HEADER_HEIGHT: f32 = 88.0;
const NEW_TAB_NAV_WIDTH: f32 = 176.0;
const NEW_TAB_NARROW_BREAKPOINT: f32 = 520.0;
const NEW_TAB_EXPANDED_BREAKPOINT: f32 = 800.0;
const NEW_TAB_WIDE_PORTRAIT_BREAKPOINT: f32 = 1100.0;
const NEW_TAB_INSPECTOR_WIDTH: f32 = 320.0;
const THUMBNAIL_CACHE_VERSION: &str = "v1";
const MAX_PERSISTENT_THUMBNAILS: usize = 256;

#[derive(Clone)]
struct ThumbnailImage {
    image: Arc<RenderImage>,
    width: f32,
    height: f32,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ThumbnailCacheKey {
    path: PathBuf,
    size: u64,
    modified_nanos: u128,
}

impl ThumbnailCacheKey {
    fn for_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to read metadata for {}", path.display()))?;
        let modified_nanos = metadata
            .modified()
            .unwrap_or(UNIX_EPOCH)
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        Ok(Self {
            path: path.to_path_buf(),
            size: metadata.len(),
            modified_nanos,
        })
    }

    fn disk_path(&self) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        thumbnail_cache_dir().join(format!("{:016x}.png", hasher.finish()))
    }
}

pub struct ThumbnailCache {
    images: HashMap<ThumbnailCacheKey, ThumbnailImage>,
    order: VecDeque<ThumbnailCacheKey>,
}

impl gpui::Global for ThumbnailCache {}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self {
            images: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &ThumbnailCacheKey) -> Option<ThumbnailImage> {
        let image = self.images.get(key).cloned()?;
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
        Some(image)
    }

    fn insert(&mut self, key: ThumbnailCacheKey, image: ThumbnailImage) {
        self.images
            .retain(|existing, _| existing.path != key.path || existing == &key);
        self.order
            .retain(|existing| existing.path != key.path || existing == &key);
        self.images.insert(key.clone(), image);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key);
        while self.images.len() > MAX_PERSISTENT_THUMBNAILS {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.images.remove(&oldest);
        }
    }
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
enum ThumbnailState {
    Loading,
    Ready(ThumbnailImage),
    Failed,
}

struct NewTabItemState {
    item_ix: usize,
    focused: bool,
    selecting: bool,
    selected: bool,
}

struct NewTabCardState {
    thumbnail: ThumbnailState,
    item: NewTabItemState,
}

#[derive(Clone)]
struct NewTabEntry {
    item_id: Option<DocumentId>,
    path: PathBuf,
    file_name: SharedString,
    directory: SharedString,
    last_read: SharedString,
    progress: SharedString,
    format_size: SharedString,
    last_opened: SharedString,
    format: Option<DocumentFormat>,
    favorite: bool,
    status: Option<ReadingStatus>,
    missing: bool,
    title_sort: SharedString,
    creator_sort: SharedString,
    year: Option<i32>,
    date_added: Option<u64>,
    last_opened_timestamp: Option<u64>,
    search_text: SharedString,
}

impl NewTabEntry {
    fn icon(&self) -> IconName {
        match self.format {
            Some(DocumentFormat::Pdf) => IconName::FileText,
            Some(DocumentFormat::Epub) => IconName::BookOpen,
            None => IconName::File,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum NewTabItemKey {
    OpenFile,
    Document(PathBuf),
    Collection(LibraryCollectionId),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum NewTabLayoutClass {
    Narrow,
    #[default]
    Compact,
    Expanded,
}

impl NewTabLayoutClass {
    fn for_size(width: f32, height: f32, previous: Self) -> Self {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return previous;
        }
        match previous {
            Self::Narrow if width < NEW_TAB_NARROW_BREAKPOINT + 20.0 => Self::Narrow,
            Self::Expanded
                if width >= NEW_TAB_EXPANDED_BREAKPOINT - 40.0
                    && (width >= height * 0.95
                        || width >= NEW_TAB_WIDE_PORTRAIT_BREAKPOINT - 40.0) =>
            {
                Self::Expanded
            }
            _ if width < NEW_TAB_NARROW_BREAKPOINT => Self::Narrow,
            _ if width >= NEW_TAB_EXPANDED_BREAKPOINT
                && (width >= height || width >= NEW_TAB_WIDE_PORTRAIT_BREAKPOINT) =>
            {
                Self::Expanded
            }
            _ => Self::Compact,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CursorMove {
    Up,
    Down,
    Left,
    Right,
    First,
    Last,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NewTabEvent {
    OpenFile,
    OpenDocument(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryBrowserHost {
    QuickOpen,
    Home,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BrowserEmptyAction {
    None,
    AddFiles(Option<LibraryCollectionId>),
    OpenFile,
}

struct BrowserEmptyState {
    title: String,
    description: String,
    icon: IconName,
    action: BrowserEmptyAction,
}

#[derive(Clone, Debug)]
struct LibraryBrowserSession {
    destination: ReaderNewTabDestination,
    view_mode: LibraryViewMode,
    sort: LibrarySort,
}

#[derive(Clone, Debug)]
enum LibraryTreeRow {
    Collection {
        id: LibraryCollectionId,
        depth: usize,
    },
    Document {
        entry_ix: usize,
        depth: usize,
    },
}

impl LibraryTreeRow {
    fn node_id(&self, entries: &[NewTabEntry]) -> Option<LibraryNodeId> {
        match self {
            Self::Collection { id, .. } => Some(LibraryNodeId::Collection(*id)),
            Self::Document { entry_ix, .. } => entries
                .get(*entry_ix)
                .and_then(|entry| entry.item_id)
                .map(LibraryNodeId::Item),
        }
    }

    fn depth(&self) -> usize {
        match self {
            Self::Collection { depth, .. } | Self::Document { depth, .. } => *depth,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FolderEditor {
    collection_id: Option<LibraryCollectionId>,
    parent_id: Option<LibraryCollectionId>,
}

#[derive(Clone)]
struct DraggedLibrarySelection {
    active: LibraryNodeId,
    marked: Arc<[LibraryNodeId]>,
    label: SharedString,
}

impl DraggedLibrarySelection {
    fn items(&self) -> Vec<LibraryNodeId> {
        if self.marked.contains(&self.active) {
            self.marked.to_vec()
        } else {
            vec![self.active]
        }
    }
}

impl Render for DraggedLibrarySelection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.items().len();
        div()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .child(if count == 1 {
                self.label.to_string()
            } else {
                format!("{count} entries")
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryDropTarget {
    Entry {
        hovered: LibraryNodeId,
        parent: Option<LibraryCollectionId>,
    },
    Root,
}

#[derive(Clone, Debug, Default)]
struct LibraryImportProgress {
    completed: usize,
    total: usize,
    added: usize,
    duplicates: usize,
    skipped: usize,
    failed: usize,
    cancelled: bool,
}

#[derive(Clone, Debug)]
struct LibraryImportCandidate {
    path: PathBuf,
    folder_path: Vec<String>,
}

#[derive(Clone, Debug, Default)]
struct LibraryImportScan {
    candidates: Vec<LibraryImportCandidate>,
    folders: Vec<Vec<String>>,
    skipped: usize,
    failed: usize,
}

pub struct NewTab {
    host: LibraryBrowserHost,
    entries: Vec<NewTabEntry>,
    filtered_entries: Vec<usize>,
    thumbnails: HashMap<PathBuf, ThumbnailState>,
    search_input: Entity<InputState>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    cols: usize,
    viewport_width: f32,
    viewport_height: f32,
    browser_viewport_width: f32,
    layout_class: NewTabLayoutClass,
    session: LibraryBrowserSession,
    quick_session: Option<ReaderNewTabSession>,
    current_collection: Option<LibraryCollectionId>,
    expanded_collections: BTreeSet<LibraryCollectionId>,
    library_tree_rows: Vec<LibraryTreeRow>,
    tree_active: Option<LibraryNodeId>,
    tree_marked: BTreeSet<LibraryNodeId>,
    tree_anchor: Option<LibraryNodeId>,
    dragging_library_nodes: BTreeSet<LibraryNodeId>,
    library_drop_target: Option<LibraryDropTarget>,
    hover_expand_task: Option<Task<()>>,
    hover_scroll_task: Option<Task<()>>,
    import_cancel: Option<Arc<AtomicBool>>,
    import_progress: Option<LibraryImportProgress>,
    import_message: Option<SharedString>,
    folder_editor: Option<FolderEditor>,
    folder_delete_prompt_pending: bool,
    kind_filter: Option<LibraryItemKind>,
    status_filter: Option<ReadingStatus>,
    observed_catalog_revision: u64,
    nav_menu_open: bool,
    inspector_open: bool,
    note_expanded: bool,
    metadata_editor_open: bool,
    metadata_kind: LibraryItemKind,
    saved_view_editor_open: bool,
    tag_editor_id: Option<LibraryTagId>,
    permanent_delete_prompt_pending: bool,
    work_note_input: Entity<InputState>,
    tag_input: Entity<InputState>,
    metadata_title_input: Entity<InputState>,
    metadata_creators_input: Entity<InputState>,
    metadata_year_input: Entity<InputState>,
    metadata_publication_input: Entity<InputState>,
    metadata_publisher_input: Entity<InputState>,
    metadata_doi_input: Entity<InputState>,
    metadata_isbn_input: Entity<InputState>,
    metadata_arxiv_input: Entity<InputState>,
    metadata_subject_input: Entity<InputState>,
    metadata_language_input: Entity<InputState>,
    metadata_abstract_input: Entity<InputState>,
    saved_view_name_input: Entity<InputState>,
    tag_editor_name_input: Entity<InputState>,
    folder_name_input: Entity<InputState>,
    note_item_id: Option<DocumentId>,
    selecting: bool,
    selection: BTreeSet<PathBuf>,
    focused_item: NewTabItemKey,
    selection_anchor: Option<PathBuf>,
    _search_subscription: Subscription,
    _work_note_subscription: Subscription,
}

impl NewTab {
    pub const WINDOW_WIDTH: f32 = BOARD_WIDTH;
    pub const WINDOW_HEIGHT: f32 = BOARD_HEIGHT;

    fn search_placeholder(destination: &ReaderNewTabDestination) -> String {
        match destination {
            ReaderNewTabDestination::Home => crate::fl!("new-tab-home-search-placeholder"),
            ReaderNewTabDestination::Library => {
                crate::fl!("new-tab-library-search-placeholder")
            }
            ReaderNewTabDestination::Favorites => {
                crate::fl!("new-tab-favorites-search-placeholder")
            }
            ReaderNewTabDestination::History => crate::fl!("new-tab-search-placeholder"),
            ReaderNewTabDestination::Tags => crate::fl!("new-tab-tags-search-placeholder"),
            ReaderNewTabDestination::Trash => crate::fl!("new-tab-trash-search-placeholder"),
            ReaderNewTabDestination::Tag(_) => {
                crate::fl!("new-tab-tagged-search-placeholder")
            }
            ReaderNewTabDestination::SavedView(_) => {
                crate::fl!("new-tab-saved-view-search-placeholder")
            }
        }
    }

    fn empty_state(&self, filtering: bool) -> BrowserEmptyState {
        if filtering {
            let (title, description) = match &self.session.destination {
                ReaderNewTabDestination::Home => (
                    crate::fl!("new-tab-no-results-title"),
                    crate::fl!("new-tab-no-results-description"),
                ),
                ReaderNewTabDestination::Library => (
                    crate::fl!("new-tab-library-no-results-title"),
                    crate::fl!("new-tab-library-no-results-description"),
                ),
                ReaderNewTabDestination::Favorites => (
                    crate::fl!("new-tab-favorites-no-results-title"),
                    crate::fl!("new-tab-library-no-results-description"),
                ),
                ReaderNewTabDestination::History => (
                    crate::fl!("new-tab-no-results-title"),
                    crate::fl!("new-tab-no-results-description"),
                ),
                ReaderNewTabDestination::Tags => (
                    crate::fl!("new-tab-tags-no-results-title"),
                    crate::fl!("new-tab-tags-no-results-description"),
                ),
                ReaderNewTabDestination::Trash => (
                    crate::fl!("new-tab-trash-no-results-title"),
                    crate::fl!("new-tab-library-no-results-description"),
                ),
                ReaderNewTabDestination::Tag(_) => (
                    crate::fl!("new-tab-tagged-no-results-title"),
                    crate::fl!("new-tab-library-no-results-description"),
                ),
                ReaderNewTabDestination::SavedView(_) => (
                    crate::fl!("new-tab-saved-view-no-results-title"),
                    crate::fl!("new-tab-library-no-results-description"),
                ),
            };
            return BrowserEmptyState {
                title,
                description,
                icon: IconName::Search,
                action: BrowserEmptyAction::None,
            };
        }

        match &self.session.destination {
            ReaderNewTabDestination::Home => BrowserEmptyState {
                title: crate::fl!("new-tab-home-empty-title"),
                description: crate::fl!("new-tab-home-empty-description"),
                icon: IconName::FileText,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::Library => {
                let target_collection = if self.session.view_mode == LibraryViewMode::Table {
                    None
                } else {
                    self.current_collection
                };
                let (title, description) = if target_collection.is_some() {
                    (
                        crate::fl!("new-tab-library-folder-empty-title"),
                        crate::fl!("new-tab-library-folder-empty-description"),
                    )
                } else {
                    (
                        crate::fl!("new-tab-library-empty-title"),
                        if self.host == LibraryBrowserHost::Home {
                            crate::fl!("new-tab-library-empty-description")
                        } else {
                            crate::fl!("new-tab-library-empty-quick-description")
                        },
                    )
                };
                BrowserEmptyState {
                    title,
                    description,
                    icon: IconName::BookOpen,
                    action: if self.host == LibraryBrowserHost::Home {
                        BrowserEmptyAction::AddFiles(target_collection)
                    } else {
                        BrowserEmptyAction::OpenFile
                    },
                }
            }
            ReaderNewTabDestination::Favorites => BrowserEmptyState {
                title: crate::fl!("new-tab-favorites-empty-title"),
                description: crate::fl!("new-tab-favorites-empty-description"),
                icon: IconName::BookOpen,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::History => BrowserEmptyState {
                title: crate::fl!("new-tab-empty-title"),
                description: crate::fl!("new-tab-empty-description"),
                icon: IconName::RotateCcw,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::Tags => BrowserEmptyState {
                title: crate::fl!("new-tab-tags-empty-title"),
                description: crate::fl!("new-tab-tags-empty-description"),
                icon: IconName::Link2,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::Trash => BrowserEmptyState {
                title: crate::fl!("new-tab-trash-empty-title"),
                description: crate::fl!("new-tab-trash-empty-description"),
                icon: IconName::Trash2,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::Tag(_) => BrowserEmptyState {
                title: crate::fl!("new-tab-tagged-empty-title"),
                description: crate::fl!("new-tab-tagged-empty-description"),
                icon: IconName::Link2,
                action: BrowserEmptyAction::None,
            },
            ReaderNewTabDestination::SavedView(_) => BrowserEmptyState {
                title: crate::fl!("new-tab-saved-view-empty-title"),
                description: crate::fl!("new-tab-saved-view-empty-description"),
                icon: IconName::Table2,
                action: BrowserEmptyAction::None,
            },
        }
    }

    pub(crate) fn new(
        focus_handle: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_mode = match cx.global::<SettingsStore>().settings.new_tab.view_mode {
            NewTabViewMode::Grid => LibraryViewMode::Grid,
            NewTabViewMode::Grouped => LibraryViewMode::Grouped,
            NewTabViewMode::Table => LibraryViewMode::Table,
        };
        let mut session = ReaderNewTabSession::default();
        session.library.view_mode = view_mode;
        Self::new_with_session(focus_handle, session, window, cx)
    }

    pub(crate) fn new_with_session(
        focus_handle: FocusHandle,
        session: ReaderNewTabSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let presentation = session.presentation(session.source);
        Self::new_with_host(
            focus_handle,
            LibraryBrowserSession {
                destination: match session.source {
                    QuickOpenSource::History => ReaderNewTabDestination::History,
                    QuickOpenSource::Library => ReaderNewTabDestination::Library,
                },
                view_mode: presentation.view_mode,
                sort: presentation.sort,
            },
            Some(session),
            LibraryBrowserHost::QuickOpen,
            window,
            cx,
        )
    }

    pub(crate) fn new_home(
        focus_handle: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = cx.global::<LibraryStore>().home_preferences().clone();
        let presentation = preferences.presentation(preferences.destination);
        Self::new_with_host(
            focus_handle,
            LibraryBrowserSession {
                destination: match preferences.destination {
                    HomeDestination::Library => ReaderNewTabDestination::Library,
                    HomeDestination::History => ReaderNewTabDestination::History,
                    HomeDestination::Trash => ReaderNewTabDestination::Trash,
                },
                view_mode: presentation.view_mode,
                sort: presentation.sort,
            },
            None,
            LibraryBrowserHost::Home,
            window,
            cx,
        )
    }

    fn new_with_host(
        focus_handle: FocusHandle,
        session: LibraryBrowserSession,
        quick_session: Option<ReaderNewTabSession>,
        host: LibraryBrowserHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if !cx.has_global::<ThumbnailCache>() {
            cx.set_global(ThumbnailCache::new());
        }
        let store = cx.global::<LibraryStore>();
        let entries = Self::entries_for_session(store, &session, None, None);
        let observed_catalog_revision = store.catalog_revision();
        let filtered_entries = (0..entries.len()).collect::<Vec<_>>();
        let focused_item = if host == LibraryBrowserHost::QuickOpen {
            NewTabItemKey::OpenFile
        } else {
            entries
                .first()
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                .unwrap_or(NewTabItemKey::OpenFile)
        };
        let search_placeholder = Self::search_placeholder(&session.destination);
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(search_placeholder)
                .submit_on_enter(true)
        });
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let work_note_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(10)
                .soft_wrap(true)
                .placeholder("Write a Markdown research note…")
        });
        let _work_note_subscription =
            cx.subscribe_in(&work_note_input, window, Self::on_work_note_input_event);
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder("Add a tag…"));
        let metadata_title_input = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let metadata_creators_input = cx
            .new(|cx| InputState::new(window, cx).placeholder("Creators, separated by semicolons"));
        let metadata_year_input = cx.new(|cx| InputState::new(window, cx).placeholder("Year"));
        let metadata_publication_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Publication or venue"));
        let metadata_publisher_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Publisher"));
        let metadata_doi_input = cx.new(|cx| InputState::new(window, cx).placeholder("DOI"));
        let metadata_isbn_input = cx.new(|cx| InputState::new(window, cx).placeholder("ISBN"));
        let metadata_arxiv_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("arXiv identifier"));
        let metadata_subject_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Subject"));
        let metadata_language_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Language"));
        let metadata_abstract_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .rows(8)
                .soft_wrap(true)
                .placeholder("Abstract or summary")
        });
        let saved_view_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Saved view name"));
        let tag_editor_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Tag name"));
        let folder_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Folder name"));

        let mut this = Self {
            host,
            entries,
            filtered_entries,
            thumbnails: HashMap::new(),
            search_input,
            focus_handle,
            scroll: UniformListScrollHandle::new(),
            cols: DEFAULT_COLS,
            viewport_width: 0.0,
            viewport_height: 0.0,
            browser_viewport_width: 0.0,
            layout_class: NewTabLayoutClass::default(),
            session,
            quick_session,
            current_collection: None,
            expanded_collections: BTreeSet::new(),
            library_tree_rows: Vec::new(),
            tree_active: None,
            tree_marked: BTreeSet::new(),
            tree_anchor: None,
            dragging_library_nodes: BTreeSet::new(),
            library_drop_target: None,
            hover_expand_task: None,
            hover_scroll_task: None,
            import_cancel: None,
            import_progress: None,
            import_message: None,
            folder_editor: None,
            folder_delete_prompt_pending: false,
            kind_filter: None,
            status_filter: None,
            observed_catalog_revision,
            nav_menu_open: false,
            inspector_open: false,
            note_expanded: false,
            metadata_editor_open: false,
            metadata_kind: LibraryItemKind::Paper,
            saved_view_editor_open: false,
            tag_editor_id: None,
            permanent_delete_prompt_pending: false,
            work_note_input,
            tag_input,
            metadata_title_input,
            metadata_creators_input,
            metadata_year_input,
            metadata_publication_input,
            metadata_publisher_input,
            metadata_doi_input,
            metadata_isbn_input,
            metadata_arxiv_input,
            metadata_subject_input,
            metadata_language_input,
            metadata_abstract_input,
            saved_view_name_input,
            tag_editor_name_input,
            folder_name_input,
            note_item_id: None,
            selecting: false,
            selection: BTreeSet::new(),
            focused_item,
            selection_anchor: None,
            _search_subscription,
            _work_note_subscription,
        };
        this.refresh_entries(cx);
        this
    }

    pub(crate) fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub(crate) fn session_state(&self) -> ReaderNewTabSession {
        let mut state = self.quick_session.clone().unwrap_or_default();
        let source = match self.session.destination {
            ReaderNewTabDestination::History => QuickOpenSource::History,
            _ => QuickOpenSource::Library,
        };
        state.source = source;
        state.set_presentation(
            source,
            BrowserPresentation {
                view_mode: self.session.view_mode,
                sort: self.session.sort,
            },
        );
        state
    }

    fn current_presentation(&self) -> BrowserPresentation {
        BrowserPresentation {
            view_mode: self.session.view_mode,
            sort: self.session.sort,
        }
    }

    fn quick_source(destination: &ReaderNewTabDestination) -> QuickOpenSource {
        if *destination == ReaderNewTabDestination::History {
            QuickOpenSource::History
        } else {
            QuickOpenSource::Library
        }
    }

    fn home_destination(destination: &ReaderNewTabDestination) -> HomeDestination {
        match destination {
            ReaderNewTabDestination::History => HomeDestination::History,
            ReaderNewTabDestination::Trash => HomeDestination::Trash,
            ReaderNewTabDestination::Home
            | ReaderNewTabDestination::Library
            | ReaderNewTabDestination::Favorites
            | ReaderNewTabDestination::Tags
            | ReaderNewTabDestination::Tag(_)
            | ReaderNewTabDestination::SavedView(_) => HomeDestination::Library,
        }
    }

    fn persist_current_presentation(&mut self, cx: &mut Context<Self>) {
        let presentation = self.current_presentation();
        match self.host {
            LibraryBrowserHost::QuickOpen => {
                let source = Self::quick_source(&self.session.destination);
                if let Some(session) = self.quick_session.as_mut() {
                    session.source = source;
                    session.set_presentation(source, presentation);
                }
            }
            LibraryBrowserHost::Home => {
                let destination = Self::home_destination(&self.session.destination);
                let mut preferences = cx.global::<LibraryStore>().home_preferences().clone();
                preferences.destination = destination;
                preferences.set_presentation(destination, presentation);
                if let Err(error) = cx
                    .global_mut::<LibraryStore>()
                    .set_home_preferences(preferences)
                {
                    eprintln!("failed to persist Home preferences: {error:#}");
                }
            }
        }
    }

    fn filter_for_destination(
        store: &LibraryStore,
        destination: &ReaderNewTabDestination,
    ) -> LibraryFilter {
        let mut filter = LibraryFilter::default();
        match destination {
            ReaderNewTabDestination::Favorites => filter.favorite = Some(true),
            ReaderNewTabDestination::Trash => filter.trash = LibraryTrashFilter::Trashed,
            ReaderNewTabDestination::Tag(tag_id) => filter.tag_ids.push(*tag_id),
            ReaderNewTabDestination::SavedView(view_id) => {
                if let Some(view) = store
                    .library_saved_views()
                    .iter()
                    .find(|view| view.id == *view_id)
                {
                    filter = view.filter.clone();
                }
            }
            ReaderNewTabDestination::Home
            | ReaderNewTabDestination::Library
            | ReaderNewTabDestination::History
            | ReaderNewTabDestination::Tags => {}
        }
        filter
    }

    fn entries_for_session(
        store: &LibraryStore,
        session: &LibraryBrowserSession,
        kind_filter: Option<LibraryItemKind>,
        status_filter: Option<ReadingStatus>,
    ) -> Vec<NewTabEntry> {
        if session.destination == ReaderNewTabDestination::History {
            let mut entries = store
                .history_entries()
                .iter()
                .map(|history| {
                    let item = store.library_item_for_path(&history.path);
                    NewTabEntry::from_history_and_item(history, item)
                })
                .collect::<Vec<_>>();
            Self::sort_entries(&mut entries, session.sort);
            return entries;
        }
        if matches!(session.destination, ReaderNewTabDestination::Tags) {
            return Vec::new();
        }

        let mut filter = Self::filter_for_destination(store, &session.destination);
        if let Some(kind) = kind_filter {
            filter.kinds = vec![kind];
        }
        if let Some(status) = status_filter {
            filter.statuses = vec![status];
        }
        let mut ids = store.query_library_items(&filter, session.sort);
        if session.destination == ReaderNewTabDestination::Home {
            ids.sort_by_key(|id| {
                let item = store.library_item(*id).expect("queried item must exist");
                let rank = if item.status == ReadingStatus::Reading {
                    0
                } else if item.favorite {
                    1
                } else {
                    2
                };
                (rank, std::cmp::Reverse(item.updated))
            });
            ids.truncate(24);
        }
        let mut entries = ids
            .into_iter()
            .filter_map(|id| NewTabEntry::from_library(store, id))
            .collect::<Vec<_>>();
        if session.destination == ReaderNewTabDestination::Home {
            entries.extend(
                store
                    .history_entries()
                    .iter()
                    .filter(|history| store.library_item_for_path(&history.path).is_none())
                    .take(12)
                    .map(NewTabEntry::from_history),
            );
        }
        entries
    }

    fn sort_entries(entries: &mut [NewTabEntry], sort: LibrarySort) {
        entries.sort_by(|left, right| {
            let primary = match sort.field {
                LibrarySortField::Title => left.title_sort.cmp(&right.title_sort),
                LibrarySortField::Creator => left.creator_sort.cmp(&right.creator_sort),
                LibrarySortField::Year => left.year.cmp(&right.year),
                LibrarySortField::DateAdded => left.date_added.cmp(&right.date_added),
                LibrarySortField::LastOpened => {
                    left.last_opened_timestamp.cmp(&right.last_opened_timestamp)
                }
            };
            let ordering = primary
                .then_with(|| left.title_sort.cmp(&right.title_sort))
                .then_with(|| left.path.cmp(&right.path));
            match sort.direction {
                LibrarySortDirection::Ascending => ordering,
                LibrarySortDirection::Descending => ordering.reverse(),
            }
        });
    }

    fn start_thumbnail_render(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.thumbnails.contains_key(&path) {
            return;
        }
        let key = match ThumbnailCacheKey::for_path(&path) {
            Ok(key) => key,
            Err(error) => {
                eprintln!(
                    "failed to fingerprint new tab preview for {}: {error:#}",
                    path.display()
                );
                self.thumbnails.insert(path, ThumbnailState::Failed);
                return;
            }
        };
        if let Some(image) = cx.global_mut::<ThumbnailCache>().get(&key) {
            self.thumbnails.insert(path, ThumbnailState::Ready(image));
            return;
        }

        self.thumbnails
            .insert(path.clone(), ThumbnailState::Loading);

        cx.spawn(async move |this, cx| {
            let render_path = path.clone();
            let render_key = key.clone();
            let result = cx
                .background_spawn(async move {
                    load_or_render_cover_thumbnail(&render_path, &render_key)
                })
                .await;

            let state = match result {
                Ok(image) => ThumbnailState::Ready(image),
                Err(error) => {
                    eprintln!(
                        "failed to render new tab preview for {}: {error:#}",
                        path.display()
                    );
                    ThumbnailState::Failed
                }
            };

            let _ = this.update(cx, |this, cx| {
                if let ThumbnailState::Ready(image) = &state {
                    cx.global_mut::<ThumbnailCache>().insert(key, image.clone());
                }
                this.thumbnails.insert(path, state);
                cx.notify();
            });
        })
        .detach();
    }

    fn cols_for_width(viewport_width: f32) -> usize {
        if !viewport_width.is_finite() || viewport_width <= 0.0 {
            return DEFAULT_COLS;
        }

        let available_width = (viewport_width - GRID_PADDING_X * 2.0).max(1.0);
        (((available_width + GRID_GAP) / (CARD_WIDTH + GRID_GAP)).floor() as usize).max(1)
    }

    fn horizontal_gap(viewport_width: f32, cols: usize) -> f32 {
        if cols <= 1 || !viewport_width.is_finite() || viewport_width <= 0.0 {
            return GRID_GAP;
        }

        let available_width = (viewport_width - GRID_PADDING_X * 2.0).max(1.0);
        let cards_width = CARD_WIDTH * cols as f32;
        ((available_width - cards_width) / (cols - 1) as f32).max(GRID_GAP)
    }

    fn update_viewport(
        &mut self,
        viewport_width: f32,
        viewport_height: f32,
        cx: &mut Context<Self>,
    ) {
        if !viewport_width.is_finite()
            || !viewport_height.is_finite()
            || viewport_width <= 0.0
            || viewport_height <= 0.0
        {
            return;
        }

        let size_changed = (viewport_width - self.viewport_width).abs() > VIEWPORT_WIDTH_EPSILON
            || (viewport_height - self.viewport_height).abs() > VIEWPORT_WIDTH_EPSILON;
        self.viewport_width = viewport_width;
        self.viewport_height = viewport_height;
        let layout_class =
            NewTabLayoutClass::for_size(viewport_width, viewport_height, self.layout_class);
        let cols = Self::cols_for_width(if self.browser_viewport_width > 0.0 {
            self.browser_viewport_width
        } else {
            viewport_width
        });
        if cols == self.cols && layout_class == self.layout_class && !size_changed {
            return;
        }

        self.cols = cols;
        self.layout_class = layout_class;
        cx.notify();
    }

    fn update_browser_viewport(&mut self, width: f32, cx: &mut Context<Self>) {
        if !width.is_finite() || width <= 0.0 {
            return;
        }
        if (width - self.browser_viewport_width).abs() <= VIEWPORT_WIDTH_EPSILON {
            return;
        }

        self.browser_viewport_width = width;
        let cols = Self::cols_for_width(width);
        if cols != self.cols {
            self.cols = cols;
        }
        cx.notify();
    }

    fn view_mode(&self, _cx: &Context<Self>) -> NewTabViewMode {
        match self.session.view_mode {
            LibraryViewMode::Grid => NewTabViewMode::Grid,
            LibraryViewMode::Grouped => NewTabViewMode::Grouped,
            LibraryViewMode::Table => NewTabViewMode::Table,
        }
    }

    fn set_view_mode(&mut self, mode: NewTabViewMode, cx: &mut Context<Self>) {
        if self.view_mode(cx) == mode {
            return;
        }

        self.session.view_mode = match mode {
            NewTabViewMode::Grid => LibraryViewMode::Grid,
            NewTabViewMode::Grouped => LibraryViewMode::Grouped,
            NewTabViewMode::Table => LibraryViewMode::Table,
        };
        self.persist_current_presentation(cx);
        if self.host == LibraryBrowserHost::QuickOpen
            && self.session.destination == ReaderNewTabDestination::Library
        {
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.new_tab.view_mode = mode;
            if let Err(error) = cx
                .global_mut::<SettingsStore>()
                .set_settings(settings.clone())
            {
                eprintln!("failed to persist new tab view mode: {error:#}");
                cx.global_mut::<SettingsStore>()
                    .set_settings_in_memory(settings);
            }
        }

        self.update_filter(cx);
        self.scroll_focused(mode);
        cx.refresh_windows();
    }

    fn append_collection_tree_rows(
        store: &LibraryStore,
        parent_id: Option<LibraryCollectionId>,
        depth: usize,
        entries_by_id: &HashMap<DocumentId, usize>,
        visible_items: &HashSet<DocumentId>,
        included_collections: Option<&HashSet<LibraryCollectionId>>,
        expanded: &BTreeSet<LibraryCollectionId>,
        rows: &mut Vec<LibraryTreeRow>,
    ) {
        for collection in store.child_collections(parent_id) {
            if included_collections.is_some_and(|included| !included.contains(&collection.id)) {
                continue;
            }
            rows.push(LibraryTreeRow::Collection {
                id: collection.id,
                depth,
            });
            if included_collections.is_some() || expanded.contains(&collection.id) {
                Self::append_collection_tree_rows(
                    store,
                    Some(collection.id),
                    depth + 1,
                    entries_by_id,
                    visible_items,
                    included_collections,
                    expanded,
                    rows,
                );
            }
        }
        let mut entry_indices = store
            .direct_collection_items(parent_id)
            .into_iter()
            .filter(|item_id| visible_items.contains(item_id))
            .filter_map(|item_id| entries_by_id.get(&item_id).copied())
            .collect::<Vec<_>>();
        entry_indices.sort_unstable();
        for entry_ix in entry_indices {
            rows.push(LibraryTreeRow::Document { entry_ix, depth });
        }
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let mut filtered_entries = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.file_name.to_lowercase().contains(&query)
                    || entry.directory.to_lowercase().contains(&query)
                    || entry.search_text.to_lowercase().contains(&query)
            })
            .map(|(entry_ix, _)| entry_ix)
            .collect::<Vec<_>>();

        self.library_tree_rows.clear();
        if self.session.destination == ReaderNewTabDestination::Library {
            let store = cx.global::<LibraryStore>();
            if self
                .current_collection
                .is_some_and(|id| store.library_collection(id).is_none())
            {
                self.current_collection = None;
            }
            match self.session.view_mode {
                LibraryViewMode::Grid | LibraryViewMode::Grouped => {
                    let direct_ids = store
                        .direct_collection_items(self.current_collection)
                        .into_iter()
                        .collect::<HashSet<_>>();
                    filtered_entries.retain(|entry_ix| {
                        self.entries[*entry_ix]
                            .item_id
                            .is_some_and(|id| direct_ids.contains(&id))
                    });
                }
                LibraryViewMode::Table => {
                    let entries_by_id = self
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(entry_ix, entry)| entry.item_id.map(|id| (id, entry_ix)))
                        .collect::<HashMap<_, _>>();
                    let visible_items = filtered_entries
                        .iter()
                        .filter_map(|entry_ix| self.entries[*entry_ix].item_id)
                        .collect::<HashSet<_>>();
                    let included_collections = (!query.is_empty()).then(|| {
                        let mut included = HashSet::new();
                        for item_id in &visible_items {
                            if let Some(parent_id) = store.item_parent(*item_id) {
                                included.extend(
                                    store
                                        .collection_ancestors(parent_id)
                                        .into_iter()
                                        .map(|collection| collection.id),
                                );
                            }
                        }
                        for collection in store.library_collections() {
                            let path = store.collection_path_label(collection.id).to_lowercase();
                            if collection.normalized_name.contains(&query) || path.contains(&query)
                            {
                                included.extend(
                                    store
                                        .collection_ancestors(collection.id)
                                        .into_iter()
                                        .map(|ancestor| ancestor.id),
                                );
                            }
                        }
                        included
                    });
                    Self::append_collection_tree_rows(
                        store,
                        None,
                        0,
                        &entries_by_id,
                        &visible_items,
                        included_collections.as_ref(),
                        &self.expanded_collections,
                        &mut self.library_tree_rows,
                    );
                    filtered_entries = self
                        .library_tree_rows
                        .iter()
                        .filter_map(|row| match row {
                            LibraryTreeRow::Document { entry_ix, .. } => Some(*entry_ix),
                            LibraryTreeRow::Collection { .. } => None,
                        })
                        .collect();
                }
            }
        }
        self.filtered_entries = filtered_entries;

        if self.session.destination == ReaderNewTabDestination::Library
            && self.session.view_mode == LibraryViewMode::Table
        {
            let visible_nodes = self
                .library_tree_rows
                .iter()
                .filter_map(|row| row.node_id(&self.entries))
                .collect::<HashSet<_>>();
            self.tree_marked.retain(|node| visible_nodes.contains(node));
            self.tree_anchor = self.tree_anchor.filter(|node| visible_nodes.contains(node));
            if self
                .tree_active
                .is_none_or(|node| !visible_nodes.contains(&node))
            {
                self.tree_active = if self.host == LibraryBrowserHost::Home {
                    self.library_tree_rows
                        .first()
                        .and_then(|row| row.node_id(&self.entries))
                } else {
                    None
                };
            }
            if let Some(node) = self.tree_active {
                self.focused_item = self.new_tab_key_for_library_node(node);
            } else if self.host == LibraryBrowserHost::QuickOpen {
                self.focused_item = NewTabItemKey::OpenFile;
            }
        } else if self.item_index(&self.focused_item).is_none() {
            self.focused_item = if self.host == LibraryBrowserHost::Home {
                self.filtered_entries
                    .first()
                    .and_then(|entry_ix| self.entries.get(*entry_ix))
                    .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                    .unwrap_or(NewTabItemKey::OpenFile)
            } else {
                NewTabItemKey::OpenFile
            };
        }
        self.selection_anchor = self.selection_anchor.take().filter(|path| {
            self.item_index(&NewTabItemKey::Document(path.clone()))
                .is_some()
        });
        if !self.filtered_entries.is_empty() {
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
    }

    fn on_search_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.update_filter(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. }
                if self.session.destination != ReaderNewTabDestination::Tags =>
            {
                self.activate_focused(cx)
            }
            InputEvent::PressEnter { .. } => {}
            InputEvent::CompletionAccepted { .. }
            | InputEvent::SpanHoverChanged { .. }
            | InputEvent::Focus
            | InputEvent::Blur => {}
        }
    }

    fn item_count(&self) -> usize {
        self.filtered_entries.len()
    }

    fn item_key(&self, item_ix: usize) -> Option<NewTabItemKey> {
        self.entry_for_item(item_ix)
            .map(|entry| NewTabItemKey::Document(entry.path.clone()))
    }

    fn item_index(&self, key: &NewTabItemKey) -> Option<usize> {
        match key {
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => None,
            NewTabItemKey::Document(path) => self
                .filtered_entries
                .iter()
                .position(|entry_ix| self.entries[*entry_ix].path == *path),
        }
    }

    fn entry_for_item(&self, item_ix: usize) -> Option<&NewTabEntry> {
        self.filtered_entries
            .get(item_ix)
            .and_then(|entry_ix| self.entries.get(*entry_ix))
    }

    fn focused_index(&self) -> usize {
        self.item_index(&self.focused_item).unwrap_or(0)
    }

    fn set_focused_index(&mut self, item_ix: usize) {
        if let Some(key) = self.item_key(item_ix.min(self.item_count().saturating_sub(1))) {
            self.focused_item = key;
        }
    }

    fn row_for_item(&self, item_ix: usize, mode: NewTabViewMode) -> usize {
        match mode {
            NewTabViewMode::Grid => item_ix / self.cols.max(1),
            NewTabViewMode::Grouped | NewTabViewMode::Table => item_ix,
        }
    }

    fn scroll_focused(&self, mode: NewTabViewMode) {
        self.scroll.scroll_to_item(
            self.row_for_item(self.focused_index(), mode),
            ScrollStrategy::Center,
        );
    }

    fn row_height(mode: NewTabViewMode) -> f32 {
        match mode {
            NewTabViewMode::Grid => GRID_ROW_HEIGHT,
            NewTabViewMode::Grouped | NewTabViewMode::Table => LIST_ROW_HEIGHT,
        }
    }

    fn scroll_by(&mut self, delta_y: Pixels, cx: &mut Context<Self>) {
        let max_offset = self.scroll.0.borrow().base_handle.max_offset().y;
        if max_offset <= px(0.) {
            return;
        }

        let current = self.scroll.offset();
        let new_y = (current.y - delta_y).clamp(-max_offset, px(0.));
        self.scroll.set_offset(point(current.x, new_y));
        cx.notify();
    }

    fn enter_select(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.selecting = true;
        if self.is_library_tree() {
            if let Some(active) = self.tree_active {
                self.tree_marked.insert(active);
                self.tree_anchor = Some(active);
            }
            cx.notify();
            return;
        }
        if self.focused_item == NewTabItemKey::OpenFile
            && let Some(first) = self.entry_for_item(0)
        {
            self.focused_item = NewTabItemKey::Document(first.path.clone());
        }
        cx.notify();
    }

    fn cancel_select(&mut self, cx: &mut Context<Self>) {
        let changed = self.selecting || !self.selection.is_empty() || !self.tree_marked.is_empty();
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        self.tree_marked.clear();
        self.tree_anchor = self.tree_active;
        if changed {
            cx.notify();
        }
    }

    fn toggle_selection(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selecting = true;
        self.focused_item = NewTabItemKey::Document(path.clone());
        self.selection_anchor = Some(path.clone());
        if !self.selection.insert(path.clone()) {
            self.selection.remove(&path);
        }
        cx.notify();
    }

    fn selection_range(
        entries: &[NewTabEntry],
        filtered_entries: &[usize],
        anchor: &Path,
        target_item_ix: usize,
    ) -> BTreeSet<PathBuf> {
        if filtered_entries.is_empty() {
            return BTreeSet::new();
        }
        let Some(anchor_item_ix) = filtered_entries
            .iter()
            .position(|entry_ix| entries[*entry_ix].path == anchor)
        else {
            return BTreeSet::new();
        };
        let target_item_ix = target_item_ix.min(filtered_entries.len().saturating_sub(1));
        let (start, end) = if anchor_item_ix <= target_item_ix {
            (anchor_item_ix, target_item_ix)
        } else {
            (target_item_ix, anchor_item_ix)
        };
        filtered_entries[start..=end]
            .iter()
            .map(|entry_ix| entries[*entry_ix].path.clone())
            .collect()
    }

    fn extend_selection_to(&mut self, target_item_ix: usize, cx: &mut Context<Self>) {
        if self.filtered_entries.is_empty() {
            return;
        }

        let anchor = self.selection_anchor.clone().or_else(|| {
            self.entry_for_item(self.focused_index())
                .map(|entry| entry.path.clone())
        });
        let target_item_ix = target_item_ix.min(self.item_count() - 1);
        let Some(anchor) = anchor.or_else(|| {
            self.entry_for_item(target_item_ix)
                .map(|entry| entry.path.clone())
        }) else {
            return;
        };

        self.selecting = true;
        self.selection_anchor = Some(anchor.clone());
        self.selection = Self::selection_range(
            &self.entries,
            &self.filtered_entries,
            &anchor,
            target_item_ix,
        );
        self.set_focused_index(target_item_ix);
        cx.notify();
    }

    fn handle_document_pointer(
        &mut self,
        path: PathBuf,
        modifiers: Modifiers,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !modifiers.shift
            && !modifiers.secondary()
            && !self.selecting
            && click_count > 0
        {
            self.open_document(path, cx);
            return;
        }

        let target_item_ix = self
            .filtered_entries
            .iter()
            .position(|entry_ix| self.entries[*entry_ix].path == path)
            .unwrap_or(0);

        if modifiers.shift {
            self.selecting = true;
            self.extend_selection_to(target_item_ix, cx);
        } else if modifiers.secondary() || self.selecting {
            self.toggle_selection(path, cx);
        } else {
            self.focused_item = NewTabItemKey::Document(path.clone());
            self.selection_anchor = Some(path.clone());
            self.selection.clear();
            self.selection.insert(path.clone());
            if self.host == LibraryBrowserHost::Home {
                self.inspector_open = true;
            }
            if click_count >= 2 {
                self.open_document(path, cx);
            }
            cx.notify();
        }
    }

    fn tree_node_index(&self, node: LibraryNodeId) -> Option<usize> {
        self.library_tree_rows
            .iter()
            .position(|row| row.node_id(&self.entries) == Some(node))
    }

    fn handle_tree_pointer(
        &mut self,
        node: LibraryNodeId,
        modifiers: Modifiers,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !modifiers.shift
            && !modifiers.secondary()
            && !self.selecting
            && let LibraryNodeId::Item(id) = node
            && let Some(path) = self
                .entry_for_library_item(id)
                .map(|entry| entry.path.clone())
        {
            self.open_document(path, cx);
            return;
        }

        if modifiers.shift {
            self.selecting = true;
            let anchor = self.tree_anchor.or(self.tree_active).unwrap_or(node);
            if let Some((anchor_ix, target_ix)) =
                self.tree_node_index(anchor).zip(self.tree_node_index(node))
            {
                let start = anchor_ix.min(target_ix);
                let end = anchor_ix.max(target_ix);
                for row in &self.library_tree_rows[start..=end] {
                    if let Some(node) = row.node_id(&self.entries) {
                        self.tree_marked.insert(node);
                    }
                }
            }
            self.tree_active = Some(node);
        } else if modifiers.secondary() {
            self.selecting = true;
            self.tree_active = Some(node);
            if !self.tree_marked.remove(&node) {
                self.tree_marked.insert(node);
            }
            self.tree_anchor = Some(node);
        } else {
            self.selecting = false;
            self.tree_active = Some(node);
            self.tree_anchor = Some(node);
            self.tree_marked.clear();
        }

        self.focused_item = self.new_tab_key_for_library_node(node);
        match node {
            LibraryNodeId::Collection(id) => {
                self.inspector_open = false;
                if !modifiers.shift && !modifiers.secondary() {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
            }
            LibraryNodeId::Item(id) => {
                if let Some(entry) = self.entry_for_library_item(id) {
                    let path = entry.path.clone();
                    self.selection.clear();
                    self.selection.insert(path.clone());
                    self.selection_anchor = Some(path.clone());
                    if self.host == LibraryBrowserHost::Home {
                        self.inspector_open = true;
                    }
                    if click_count >= 2 {
                        self.open_document(path, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    fn dragged_library_selection(
        &self,
        active: LibraryNodeId,
        label: SharedString,
    ) -> DraggedLibrarySelection {
        DraggedLibrarySelection {
            active,
            marked: self.tree_marked.iter().copied().collect::<Vec<_>>().into(),
            label,
        }
    }

    fn target_index(&self, movement: CursorMove, mode: NewTabViewMode) -> usize {
        let current = self.focused_index();
        let last = self.item_count().saturating_sub(1);
        match movement {
            CursorMove::First => 0,
            CursorMove::Last => last,
            CursorMove::Left | CursorMove::Up if mode != NewTabViewMode::Grid => {
                current.saturating_sub(1)
            }
            CursorMove::Right | CursorMove::Down if mode != NewTabViewMode::Grid => {
                current.saturating_add(1).min(last)
            }
            CursorMove::Left => current.saturating_sub(1),
            CursorMove::Right => current.saturating_add(1).min(last),
            CursorMove::Up => current.saturating_sub(self.cols.max(1)),
            CursorMove::Down => current.saturating_add(self.cols.max(1)).min(last),
        }
    }

    fn is_library_tree(&self) -> bool {
        self.session.destination == ReaderNewTabDestination::Library
            && self.session.view_mode == LibraryViewMode::Table
    }

    fn effective_tree_selection(&self) -> Vec<LibraryNodeId> {
        let Some(active) = self.tree_active else {
            return Vec::new();
        };
        if self.tree_marked.contains(&active) {
            self.tree_marked.iter().copied().collect()
        } else {
            vec![active]
        }
    }

    fn move_tree_cursor(&mut self, movement: CursorMove, extend: bool, cx: &mut Context<Self>) {
        if self.library_tree_rows.is_empty() {
            return;
        }
        if self.host == LibraryBrowserHost::QuickOpen
            && self.tree_active.is_none()
            && matches!(
                movement,
                CursorMove::Up | CursorMove::Down | CursorMove::Left | CursorMove::Right
            )
        {
            let Some(node) = self.library_tree_rows[0].node_id(&self.entries) else {
                return;
            };
            self.tree_active = Some(node);
            self.tree_anchor = Some(node);
            self.focused_item = self.new_tab_key_for_library_node(node);
            if extend {
                self.selecting = true;
                self.tree_marked.insert(node);
            }
            self.scroll.scroll_to_item(0, ScrollStrategy::Center);
            cx.notify();
            return;
        }
        let current = self
            .tree_active
            .and_then(|node| self.tree_node_index(node))
            .unwrap_or(0);
        let current_row = self.library_tree_rows[current].clone();
        if let LibraryTreeRow::Collection { id, .. } = current_row {
            match movement {
                CursorMove::Left if self.expanded_collections.contains(&id) => {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
                CursorMove::Right if !self.expanded_collections.contains(&id) => {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
                _ => {}
            }
        }

        let last = self.library_tree_rows.len().saturating_sub(1);
        let target = match movement {
            CursorMove::First => 0,
            CursorMove::Last => last,
            CursorMove::Up => current.saturating_sub(1),
            CursorMove::Down => current.saturating_add(1).min(last),
            CursorMove::Left => {
                let depth = current_row.depth();
                if depth == 0 {
                    current
                } else {
                    (0..current)
                        .rev()
                        .find(|index| self.library_tree_rows[*index].depth() + 1 == depth)
                        .unwrap_or(current)
                }
            }
            CursorMove::Right => {
                let depth = current_row.depth();
                if current < last && self.library_tree_rows[current + 1].depth() > depth {
                    current + 1
                } else {
                    current
                }
            }
        };
        let Some(node) = self.library_tree_rows[target].node_id(&self.entries) else {
            return;
        };
        if extend {
            self.selecting = true;
            let anchor = self.tree_anchor.or(self.tree_active).unwrap_or(node);
            self.tree_marked.clear();
            if let Some(anchor_ix) = self.tree_node_index(anchor) {
                for row in &self.library_tree_rows[anchor_ix.min(target)..=anchor_ix.max(target)] {
                    if let Some(node) = row.node_id(&self.entries) {
                        self.tree_marked.insert(node);
                    }
                }
            }
        } else {
            self.tree_anchor = Some(node);
        }
        self.tree_active = Some(node);
        self.focused_item = self.new_tab_key_for_library_node(node);
        self.scroll.scroll_to_item(target, ScrollStrategy::Center);
        cx.notify();
    }

    fn move_cursor(&mut self, movement: CursorMove, extend: bool, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            self.move_tree_cursor(movement, extend, cx);
            return;
        }
        let mode = self.view_mode(cx);
        if self.host == LibraryBrowserHost::QuickOpen
            && self.item_index(&self.focused_item).is_none()
            && matches!(
                movement,
                CursorMove::Up | CursorMove::Down | CursorMove::Left | CursorMove::Right
            )
        {
            if self.item_count() == 0 {
                return;
            }
            if extend {
                let Some(path) = self.entry_for_item(0).map(|entry| entry.path.clone()) else {
                    return;
                };
                self.selection_anchor = Some(path.clone());
                self.selection.clear();
                self.selection.insert(path);
                self.selecting = true;
            }
            self.set_focused_index(0);
            self.scroll_focused(mode);
            cx.notify();
            return;
        }
        let target = self.target_index(movement, mode);
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = self
                    .entry_for_item(self.focused_index())
                    .map(|entry| entry.path.clone())
                    .or_else(|| self.entry_for_item(target).map(|entry| entry.path.clone()));
            }
            self.extend_selection_to(target, cx);
        } else {
            self.set_focused_index(target);
            cx.notify();
        }
        self.scroll_focused(mode);
    }

    fn activate_focused(&mut self, cx: &mut Context<Self>) {
        match self.focused_item.clone() {
            NewTabItemKey::OpenFile if self.host == LibraryBrowserHost::QuickOpen => {}
            NewTabItemKey::OpenFile => self.open_file(cx),
            NewTabItemKey::Document(path) => self.open_document(path, cx),
            NewTabItemKey::Collection(id) => self.toggle_collection_expanded(id, cx),
        }
    }

    fn toggle_focused(&mut self, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            let Some(node) = self.tree_active else {
                return;
            };
            self.selecting = true;
            if !self.tree_marked.remove(&node) {
                self.tree_marked.insert(node);
            }
            self.tree_anchor = Some(node);
            cx.notify();
            return;
        }
        let path = match self.focused_item.clone() {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::Collection(id) => {
                let node = LibraryNodeId::Collection(id);
                self.tree_active = Some(node);
                if !self.tree_marked.remove(&node) {
                    self.tree_marked.insert(node);
                }
                cx.notify();
                return;
            }
            NewTabItemKey::OpenFile => {
                let Some(first) = self.entry_for_item(0) else {
                    self.enter_select(cx);
                    return;
                };
                first.path.clone()
            }
        };
        self.toggle_selection(path, cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            self.selecting = true;
            self.tree_marked = self
                .library_tree_rows
                .iter()
                .filter_map(|row| row.node_id(&self.entries))
                .collect();
            if self.tree_active.is_none() {
                self.tree_active = self.tree_marked.iter().next().copied();
            }
            cx.notify();
            return;
        }
        if self.filtered_entries.is_empty() {
            return;
        }
        self.selecting = true;
        self.selection = self
            .filtered_entries
            .iter()
            .map(|entry_ix| self.entries[*entry_ix].path.clone())
            .collect();
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.entry_for_item(0).map(|entry| entry.path.clone());
        }
        if self.focused_item == NewTabItemKey::OpenFile
            && let Some(first) = self.entry_for_item(0)
        {
            self.focused_item = NewTabItemKey::Document(first.path.clone());
        }
        cx.notify();
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        if self.is_library_tree() {
            let nodes = cx
                .global::<LibraryStore>()
                .disjoint_library_nodes(&self.effective_tree_selection());
            if nodes.is_empty() {
                return;
            }
            if nodes
                .iter()
                .any(|node| matches!(node, LibraryNodeId::Collection(_)))
            {
                if self.folder_delete_prompt_pending {
                    return;
                }
                let confirmation = window.prompt(
                    PromptLevel::Warning,
                    "Move selected entries to Trash?",
                    Some(
                        "Selected folders and their subfolders will be removed. Papers inside them will move to Trash.",
                    ),
                    &[
                        PromptButton::ok("Move to Trash"),
                        PromptButton::cancel("Cancel"),
                    ],
                    cx,
                );
                self.folder_delete_prompt_pending = true;
                cx.spawn_in(window, async move |this, cx| {
                    let confirmed = confirmation.await == Ok(0);
                    _ = this.update_in(cx, |this, _, cx| {
                        this.folder_delete_prompt_pending = false;
                        if confirmed {
                            this.delete_tree_nodes_now(nodes, cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
                .detach();
            } else {
                self.delete_tree_nodes_now(nodes, cx);
            }
            return;
        }
        if self.selection.is_empty() {
            return;
        }

        if matches!(self.session.destination, ReaderNewTabDestination::Trash) {
            if self.permanent_delete_prompt_pending {
                return;
            }
            let count = self.selection.len();
            let detail = format!(
                "This permanently removes {count} catalog {} and its managed Library file. The original imported source is unaffected.",
                if count == 1 { "record" } else { "records" }
            );
            let confirmation = window.prompt(
                PromptLevel::Warning,
                "Delete permanently?",
                Some(&detail),
                &[
                    PromptButton::ok("Delete Permanently"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            );
            self.permanent_delete_prompt_pending = true;
            cx.spawn_in(window, async move |this, cx| {
                let confirmed = confirmation.await == Ok(0);
                _ = this.update_in(cx, |this, _, cx| {
                    this.permanent_delete_prompt_pending = false;
                    if confirmed {
                        this.delete_selected_now(cx);
                    } else {
                        cx.notify();
                    }
                });
            })
            .detach();
            return;
        }

        self.delete_selected_now(cx);
    }

    fn delete_tree_nodes_now(&mut self, nodes: Vec<LibraryNodeId>, cx: &mut Context<Self>) {
        let nodes = cx.global::<LibraryStore>().disjoint_library_nodes(&nodes);
        let collections = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Collection(id) => Some(*id),
                LibraryNodeId::Item(_) => None,
            })
            .collect::<Vec<_>>();
        let items = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Item(id) => Some(*id),
                LibraryNodeId::Collection(_) => None,
            })
            .collect::<Vec<_>>();

        let store = cx.global_mut::<LibraryStore>();
        let result = collections
            .into_iter()
            .try_for_each(|id| store.delete_library_collection(id).map(|_| ()))
            .and_then(|_| store.trash_library_items(&items).map(|_| ()));
        if let Err(error) = result {
            self.import_message =
                Some(format!("Could not delete Library entries: {error:#}").into());
        }
        self.tree_active = None;
        self.tree_marked.clear();
        self.tree_anchor = None;
        self.selection.clear();
        self.selection_anchor = None;
        self.selecting = false;
        self.inspector_open = false;
        self.focused_item = NewTabItemKey::OpenFile;
        self.expanded_collections.retain(|collection_id| {
            cx.global::<LibraryStore>()
                .library_collection(*collection_id)
                .is_some()
        });
        self.refresh_entries(cx);
        cx.notify();
    }

    fn delete_selected_now(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }

        let paths = self.selection.iter().cloned().collect::<Vec<_>>();
        let item_ids = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path))
            .filter_map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        let history_only_paths = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path) && entry.item_id.is_none())
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let focused_ix = self.focused_index();

        {
            let store = cx.global_mut::<LibraryStore>();
            let result = match &self.session.destination {
                ReaderNewTabDestination::History => {
                    store.remove_history_entries(&paths).map(|_| ())
                }
                ReaderNewTabDestination::Trash => store
                    .permanently_delete_library_items(&item_ids)
                    .map(|_| ()),
                _ => store.trash_library_items(&item_ids).and_then(|_| {
                    store
                        .remove_history_entries(&history_only_paths)
                        .map(|_| ())
                }),
            };
            if let Err(error) = result {
                eprintln!("failed to remove New Tab items: {error:#}");
            }
        }

        for path in &paths {
            self.thumbnails.remove(path);
        }

        self.refresh_entries(cx);
        if self.item_index(&self.focused_item).is_none() {
            self.set_focused_index(focused_ix.min(self.item_count().saturating_sub(1)));
        }
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        cx.notify();
    }

    fn refresh_entries(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.entries =
            Self::entries_for_session(store, &self.session, self.kind_filter, self.status_filter);
        self.observed_catalog_revision = store.catalog_revision();
        self.update_filter(cx);
        let existing = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.selection.retain(|path| existing.contains(path));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !existing.contains(path))
        {
            self.selection_anchor = None;
        }
    }

    fn set_destination(
        &mut self,
        destination: ReaderNewTabDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !matches!(
                destination,
                ReaderNewTabDestination::History | ReaderNewTabDestination::Library
            )
        {
            return;
        }
        if self.session.destination == destination {
            self.nav_menu_open = false;
            cx.notify();
            return;
        }
        self.persist_current_presentation(cx);
        let presentation = match self.host {
            LibraryBrowserHost::QuickOpen => {
                let source = Self::quick_source(&destination);
                let session = self.quick_session.get_or_insert_with(Default::default);
                session.source = source;
                session.presentation(source)
            }
            LibraryBrowserHost::Home => {
                let home_destination = Self::home_destination(&destination);
                cx.global::<LibraryStore>()
                    .home_preferences()
                    .presentation(home_destination)
            }
        };
        self.session.sort = presentation.sort;
        self.session.view_mode = presentation.view_mode;
        if let ReaderNewTabDestination::SavedView(view_id) = destination
            && let Some(view) = cx
                .global::<LibraryStore>()
                .library_saved_views()
                .iter()
                .find(|view| view.id == view_id)
        {
            self.session.sort = view.sort;
            self.session.view_mode = view.view_mode;
        }
        self.session.destination = destination;
        self.persist_current_presentation(cx);
        self.nav_menu_open = false;
        self.inspector_open = false;
        self.note_expanded = false;
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        if self.host == LibraryBrowserHost::QuickOpen {
            self.focused_item = NewTabItemKey::OpenFile;
            self.tree_active = None;
            self.tree_marked.clear();
            self.tree_anchor = None;
        }
        let search_placeholder = Self::search_placeholder(&self.session.destination);
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(search_placeholder, window, cx);
        });
        self.refresh_entries(cx);
        if self.host == LibraryBrowserHost::Home {
            self.focused_item = self
                .entries
                .first()
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                .unwrap_or(NewTabItemKey::OpenFile);
        }
        cx.notify();
    }

    fn sort_field_label(field: LibrarySortField) -> &'static str {
        match field {
            LibrarySortField::Title => "Title",
            LibrarySortField::Creator => "Creator",
            LibrarySortField::Year => "Year",
            LibrarySortField::DateAdded => "Added",
            LibrarySortField::LastOpened => "Opened",
        }
    }

    fn cycle_sort_field(&mut self, cx: &mut Context<Self>) {
        self.session.sort.field = match self.session.sort.field {
            LibrarySortField::Title => LibrarySortField::Creator,
            LibrarySortField::Creator => LibrarySortField::Year,
            LibrarySortField::Year => LibrarySortField::DateAdded,
            LibrarySortField::DateAdded => LibrarySortField::LastOpened,
            LibrarySortField::LastOpened => LibrarySortField::Title,
        };
        self.persist_current_presentation(cx);
        self.refresh_entries(cx);
        cx.notify();
    }

    fn toggle_sort_direction(&mut self, cx: &mut Context<Self>) {
        self.session.sort.direction = match self.session.sort.direction {
            LibrarySortDirection::Ascending => LibrarySortDirection::Descending,
            LibrarySortDirection::Descending => LibrarySortDirection::Ascending,
        };
        self.persist_current_presentation(cx);
        self.refresh_entries(cx);
        cx.notify();
    }

    fn cycle_kind_filter(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.kind_filter = match self.kind_filter {
            None => Some(LibraryItemKind::Paper),
            Some(LibraryItemKind::Paper) => Some(LibraryItemKind::Book),
            Some(LibraryItemKind::Book) => Some(LibraryItemKind::Other),
            Some(LibraryItemKind::Other) => None,
        };
        self.refresh_entries(cx);
        cx.notify();
    }

    fn cycle_status_filter(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.status_filter = match self.status_filter {
            None => Some(ReadingStatus::ToRead),
            Some(ReadingStatus::ToRead) => Some(ReadingStatus::Reading),
            Some(ReadingStatus::Reading) => Some(ReadingStatus::Read),
            Some(ReadingStatus::Read) => None,
        };
        self.refresh_entries(cx);
        cx.notify();
    }

    fn selected_item_id(&self) -> Option<DocumentId> {
        let path = match &self.focused_item {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => return None,
        };
        self.entries
            .iter()
            .find(|entry| entry.path == *path)
            .and_then(|entry| entry.item_id)
    }

    fn selected_entry(&self) -> Option<&NewTabEntry> {
        let path = match &self.focused_item {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => return None,
        };
        self.entries.iter().find(|entry| entry.path == *path)
    }

    fn entry_for_library_item(&self, id: DocumentId) -> Option<&NewTabEntry> {
        self.entries.iter().find(|entry| entry.item_id == Some(id))
    }

    fn new_tab_key_for_library_node(&self, node: LibraryNodeId) -> NewTabItemKey {
        match node {
            LibraryNodeId::Collection(id) => NewTabItemKey::Collection(id),
            LibraryNodeId::Item(id) => self
                .entry_for_library_item(id)
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                .unwrap_or(NewTabItemKey::OpenFile),
        }
    }

    fn sync_work_note_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item_id = self.selected_item_id();
        if item_id == self.note_item_id {
            return;
        }
        self.note_item_id = item_id;
        let note = item_id
            .and_then(|id| cx.global::<LibraryStore>().library_item(id))
            .map(|item| item.work_note.clone())
            .unwrap_or_default();
        self.work_note_input
            .update(cx, |input, cx| input.set_value(note, window, cx));
    }

    fn on_work_note_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(item_id) = self.note_item_id else {
            return;
        };
        let note = input.read(cx).value().to_string();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_note(item_id, note)
        {
            eprintln!("failed to save library work note: {error:#}");
        }
        self.observed_catalog_revision = cx.global::<LibraryStore>().catalog_revision();
        cx.notify();
    }

    fn enter_collection(&mut self, id: LibraryCollectionId, cx: &mut Context<Self>) {
        if cx.global::<LibraryStore>().library_collection(id).is_none() {
            return;
        }
        self.current_collection = Some(id);
        self.selection.clear();
        self.selection_anchor = None;
        self.inspector_open = false;
        self.update_filter(cx);
        cx.notify();
    }

    fn enter_collection_parent(&mut self, id: Option<LibraryCollectionId>, cx: &mut Context<Self>) {
        self.current_collection = id.filter(|id| {
            cx.global::<LibraryStore>()
                .library_collection(*id)
                .is_some()
        });
        self.selection.clear();
        self.selection_anchor = None;
        self.inspector_open = false;
        self.update_filter(cx);
        cx.notify();
    }

    fn toggle_collection_expanded(&mut self, id: LibraryCollectionId, cx: &mut Context<Self>) {
        if !self.expanded_collections.insert(id) {
            self.expanded_collections.remove(&id);
        }
        self.update_filter(cx);
        cx.notify();
    }

    fn open_new_folder_editor(
        &mut self,
        parent_id: Option<LibraryCollectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.folder_name_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.folder_editor = Some(FolderEditor {
            collection_id: None,
            parent_id,
        });
        let input = self.folder_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn open_rename_folder_editor(
        &mut self,
        id: LibraryCollectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return;
        };
        let name = collection.name.clone();
        let parent_id = collection.parent_id;
        self.folder_name_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.folder_editor = Some(FolderEditor {
            collection_id: Some(id),
            parent_id,
        });
        let input = self.folder_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn save_folder_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.folder_editor else {
            return;
        };
        let name = self.folder_name_input.read(cx).value().to_string();
        let result = if let Some(id) = editor.collection_id {
            cx.global_mut::<LibraryStore>()
                .rename_library_collection(id, &name)
                .map(|_| id)
        } else {
            cx.global_mut::<LibraryStore>()
                .create_library_collection(editor.parent_id, &name)
        };
        match result {
            Ok(id) => {
                self.folder_editor = None;
                if editor.parent_id.is_some() {
                    self.expanded_collections.insert(editor.parent_id.unwrap());
                }
                if editor.collection_id.is_none() {
                    self.expanded_collections.insert(id);
                }
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to save library folder: {error:#}"),
        }
        cx.notify();
    }

    fn request_delete_folder(
        &mut self,
        id: LibraryCollectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home || self.folder_delete_prompt_pending {
            return;
        }
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return;
        };
        let name = collection.name.clone();
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete folder?",
            Some(&format!(
                "Delete {name:?} and its subfolders? Papers inside will move to Trash; managed files remain until Trash is emptied."
            )),
            &[
                PromptButton::ok("Delete Folder"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        self.folder_delete_prompt_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = confirmation.await == Ok(0);
            _ = this.update_in(cx, |this, _, cx| {
                this.folder_delete_prompt_pending = false;
                if confirmed {
                    match cx
                        .global_mut::<LibraryStore>()
                        .delete_library_collection(id)
                    {
                        Ok(_) => {
                            if this.current_collection == Some(id)
                                || this.current_collection.is_some_and(|current| {
                                    cx.global::<LibraryStore>()
                                        .library_collection(current)
                                        .is_none()
                                })
                            {
                                this.current_collection = None;
                            }
                            this.expanded_collections.retain(|collection_id| {
                                cx.global::<LibraryStore>()
                                    .library_collection(*collection_id)
                                    .is_some()
                            });
                            this.refresh_entries(cx);
                        }
                        Err(error) => eprintln!("failed to delete library folder: {error:#}"),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn move_library_nodes(
        &mut self,
        nodes: &[LibraryNodeId],
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        match cx
            .global_mut::<LibraryStore>()
            .move_library_nodes(nodes, parent_id)
        {
            Ok(_) => self.import_message = None,
            Err(error) => {
                self.import_message =
                    Some(format!("Could not move Library entries: {error:#}").into());
            }
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    fn drop_library_selection(
        &mut self,
        dragged: &DraggedLibrarySelection,
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        self.clear_library_drag_state(cx);
        self.move_library_nodes(&dragged.items(), parent_id, cx);
    }

    fn begin_library_drag(&mut self, dragged: &DraggedLibrarySelection, cx: &mut Context<Self>) {
        self.dragging_library_nodes = dragged.items().into_iter().collect();
        cx.notify();
    }

    fn clear_library_drag_state(&mut self, cx: &mut Context<Self>) {
        let changed = self.library_drop_target.take().is_some()
            | self.hover_expand_task.take().is_some()
            | self.hover_scroll_task.take().is_some()
            | !self.dragging_library_nodes.is_empty();
        self.dragging_library_nodes.clear();
        if changed {
            cx.notify();
        }
    }

    fn clear_library_drop_feedback(&mut self, cx: &mut Context<Self>) {
        let changed = self.library_drop_target.take().is_some()
            | self.hover_expand_task.take().is_some()
            | self.hover_scroll_task.take().is_some();
        if changed {
            cx.notify();
        }
    }

    fn set_library_drag_target(
        &mut self,
        node: LibraryNodeId,
        dragged: &DraggedLibrarySelection,
        bounds_contains_pointer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !bounds_contains_pointer {
            if matches!(
                self.library_drop_target,
                Some(LibraryDropTarget::Entry { hovered, .. }) if hovered == node
            ) {
                self.library_drop_target = None;
                self.hover_expand_task.take();
                cx.notify();
            }
            return;
        }
        let parent = match node {
            LibraryNodeId::Collection(id) => Some(id),
            LibraryNodeId::Item(id) => cx.global::<LibraryStore>().item_parent(id),
        };
        let target = LibraryDropTarget::Entry {
            hovered: node,
            parent,
        };
        if self.library_drop_target == Some(target) {
            return;
        }
        let valid = cx
            .global::<LibraryStore>()
            .validate_library_move(&dragged.items(), parent)
            .unwrap_or(false);
        if !valid {
            self.library_drop_target = None;
            self.hover_expand_task.take();
            cx.notify();
            return;
        }
        self.library_drop_target = Some(target);
        self.hover_expand_task.take();
        if let LibraryNodeId::Collection(id) = node
            && !self.expanded_collections.contains(&id)
        {
            self.hover_expand_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                _ = this.update(cx, |this, cx| {
                    this.hover_expand_task.take();
                    if matches!(
                        this.library_drop_target,
                        Some(LibraryDropTarget::Entry { hovered, .. })
                            if hovered == LibraryNodeId::Collection(id)
                    ) {
                        this.expanded_collections.insert(id);
                        this.update_filter(cx);
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    fn library_node_is_drop_highlighted(&self, node: LibraryNodeId, cx: &Context<Self>) -> bool {
        let parent = match self.library_drop_target {
            Some(LibraryDropTarget::Root) => return true,
            Some(LibraryDropTarget::Entry { parent, .. }) => parent,
            None => return false,
        };
        let Some(parent_id) = parent else {
            return true;
        };
        match node {
            LibraryNodeId::Collection(id) => cx
                .global::<LibraryStore>()
                .collection_ancestors(id)
                .iter()
                .any(|ancestor| ancestor.id == parent_id),
            LibraryNodeId::Item(id) => {
                cx.global::<LibraryStore>()
                    .item_parent(id)
                    .is_some_and(|id| {
                        cx.global::<LibraryStore>()
                            .collection_ancestors(id)
                            .iter()
                            .any(|ancestor| ancestor.id == parent_id)
                    })
            }
        }
    }

    fn set_library_root_drag_target(
        &mut self,
        dragged: &DraggedLibrarySelection,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if !active {
            if self.library_drop_target == Some(LibraryDropTarget::Root) {
                self.library_drop_target = None;
                cx.notify();
            }
            return;
        }
        if self.library_drop_target == Some(LibraryDropTarget::Root) {
            return;
        }
        if cx
            .global::<LibraryStore>()
            .validate_library_move(&dragged.items(), None)
            .unwrap_or(false)
        {
            self.library_drop_target = Some(LibraryDropTarget::Root);
        } else {
            self.library_drop_target = None;
        }
        self.hover_expand_task.take();
        cx.notify();
    }

    fn set_external_library_drag_target(
        &mut self,
        node: LibraryNodeId,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active {
            if matches!(
                self.library_drop_target,
                Some(LibraryDropTarget::Entry { hovered, .. }) if hovered == node
            ) {
                self.library_drop_target = None;
                self.hover_expand_task.take();
                cx.notify();
            }
            return;
        }
        let parent = match node {
            LibraryNodeId::Collection(id) => Some(id),
            LibraryNodeId::Item(id) => cx.global::<LibraryStore>().item_parent(id),
        };
        let target = LibraryDropTarget::Entry {
            hovered: node,
            parent,
        };
        if self.library_drop_target == Some(target) {
            return;
        }
        self.library_drop_target = Some(target);
        self.hover_expand_task.take();
        if let LibraryNodeId::Collection(id) = node
            && !self.expanded_collections.contains(&id)
        {
            self.hover_expand_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                _ = this.update(cx, |this, cx| {
                    this.hover_expand_task.take();
                    if matches!(
                        this.library_drop_target,
                        Some(LibraryDropTarget::Entry { hovered, .. })
                            if hovered == LibraryNodeId::Collection(id)
                    ) {
                        this.expanded_collections.insert(id);
                        this.update_filter(cx);
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    fn set_external_library_root_drag_target(&mut self, active: bool, cx: &mut Context<Self>) {
        if active {
            self.library_drop_target = Some(LibraryDropTarget::Root);
            self.hover_expand_task.take();
        } else if self.library_drop_target == Some(LibraryDropTarget::Root) {
            self.library_drop_target = None;
        }
        cx.notify();
    }

    fn drop_external_library_paths(
        &mut self,
        paths: &ExternalPaths,
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        let paths = paths.paths().to_vec();
        self.clear_library_drag_state(cx);
        self.start_library_import(paths, parent_id, cx);
    }

    fn handle_library_drag_move<T: 'static>(
        &mut self,
        event: &DragMoveEvent<T>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            self.clear_library_drop_feedback(cx);
            return;
        }
        self.hover_scroll_task.take();
        let height = event.bounds.size.height;
        if height <= px(0.) {
            return;
        }
        let fraction = (event.event.position.y - event.bounds.origin.y) / height;
        let delta = if fraction <= 0.05 {
            8.0
        } else if fraction <= 0.15 {
            5.0
        } else if fraction >= 0.95 {
            -8.0
        } else if fraction >= 0.85 {
            -5.0
        } else {
            return;
        };
        self.hover_scroll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let continuing = this
                    .update(cx, |this, cx| {
                        if this.hover_scroll_task.is_none() {
                            return false;
                        }
                        let handle = this.scroll.0.borrow_mut();
                        let offset = handle.base_handle.offset();
                        handle
                            .base_handle
                            .set_offset(offset + point(px(0.0), px(delta)));
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !continuing {
                    break;
                }
            }
        }));
    }

    fn library_import_target(&self, cx: &Context<Self>) -> Option<LibraryCollectionId> {
        if self.is_library_tree() {
            return match self.tree_active {
                Some(LibraryNodeId::Collection(id)) => Some(id),
                Some(LibraryNodeId::Item(id)) => cx.global::<LibraryStore>().item_parent(id),
                None => None,
            };
        }
        self.current_collection
    }

    fn cancel_library_import(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = &self.import_cancel else {
            return;
        };
        cancel.store(true, AtomicOrdering::Release);
        if let Some(progress) = &mut self.import_progress {
            progress.cancelled = true;
        }
        self.import_message = Some("Cancelling import…".into());
        cx.notify();
    }

    fn start_library_import(
        &mut self,
        paths: Vec<PathBuf>,
        target_collection: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home || paths.is_empty() {
            return;
        }
        if self.import_cancel.is_some() {
            self.import_message = Some("A Library import is already running.".into());
            cx.notify();
            return;
        }
        let Some(managed_root) = cx
            .global::<LibraryStore>()
            .managed_library_root()
            .map(Path::to_path_buf)
        else {
            self.import_message = Some("Managed Library storage is unavailable.".into());
            cx.notify();
            return;
        };

        let cancel = Arc::new(AtomicBool::new(false));
        self.import_cancel = Some(cancel.clone());
        self.import_progress = Some(LibraryImportProgress::default());
        self.import_message = Some("Scanning files…".into());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let scan = cx
                .background_spawn(async move { scan_library_import_paths(paths) })
                .await;
            let folders = scan.folders.clone();
            let candidates = scan.candidates;
            let skipped = scan.skipped;
            let scan_failed = scan.failed;
            let total = candidates.len();

            if !cancel.load(AtomicOrdering::Acquire)
                && this
                    .update(cx, |this, cx| {
                        if let Some(progress) = &mut this.import_progress {
                            progress.total = total;
                            progress.skipped = skipped;
                            progress.failed = scan_failed;
                        }
                        for folder in &folders {
                            if let Err(error) = cx
                                .global_mut::<LibraryStore>()
                                .ensure_library_collection_path(target_collection, folder)
                            {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.failed = progress.failed.saturating_add(1);
                                }
                                this.import_message = Some(
                                    format!("Could not create imported folder: {error:#}").into(),
                                );
                            }
                        }
                        this.refresh_entries(cx);
                        cx.notify();
                    })
                    .is_err()
            {
                return;
            }

            for candidate in candidates {
                if cancel.load(AtomicOrdering::Acquire) {
                    break;
                }
                let folder_path = candidate.folder_path.clone();
                let parent_result = this.update(cx, |_, cx| {
                    cx.global_mut::<LibraryStore>()
                        .ensure_library_collection_path(target_collection, &folder_path)
                });
                let parent_id = match parent_result {
                    Ok(Ok(parent_id)) => parent_id,
                    Ok(Err(error)) => {
                        _ = this.update(cx, |this, cx| {
                            if let Some(progress) = &mut this.import_progress {
                                progress.completed = progress.completed.saturating_add(1);
                                progress.failed = progress.failed.saturating_add(1);
                            }
                            this.import_message =
                                Some(format!("Could not create import folder: {error:#}").into());
                            cx.notify();
                        });
                        continue;
                    }
                    Err(_) => return,
                };

                let source_path = candidate.path;
                let work_path = source_path.clone();
                let work_root = managed_root.clone();
                let work_cancel = cancel.clone();
                let prepared = cx
                    .background_spawn(async move {
                        let metadata = extract_library_metadata(&work_path)?;
                        let prepared =
                            prepare_managed_import(&work_root, &work_path, &work_cancel)?;
                        Ok::<_, anyhow::Error>((prepared, metadata))
                    })
                    .await;
                let was_cancelled = cancel.load(AtomicOrdering::Acquire);
                if this
                    .update(cx, |this, cx| {
                        if let Some(progress) = &mut this.import_progress {
                            progress.completed = progress.completed.saturating_add(1);
                        }
                        match prepared {
                            Ok((prepared, metadata)) => match cx
                                .global_mut::<LibraryStore>()
                                .finalize_managed_import(prepared, metadata, parent_id)
                            {
                                Ok(LibraryImportOutcome::Added(_)) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.added = progress.added.saturating_add(1);
                                    }
                                }
                                Ok(
                                    LibraryImportOutcome::Duplicate(_)
                                    | LibraryImportOutcome::DuplicateInTrash(_),
                                ) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.duplicates = progress.duplicates.saturating_add(1);
                                    }
                                }
                                Err(error) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.failed = progress.failed.saturating_add(1);
                                    }
                                    this.import_message = Some(
                                        format!(
                                            "Could not import {}: {error:#}",
                                            source_path.display()
                                        )
                                        .into(),
                                    );
                                }
                            },
                            Err(_error) if was_cancelled => {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.cancelled = true;
                                }
                                this.import_message = Some("Cancelling import…".into());
                            }
                            Err(error) => {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.failed = progress.failed.saturating_add(1);
                                }
                                this.import_message = Some(
                                    format!(
                                        "Could not import {}: {error:#}",
                                        source_path.display()
                                    )
                                    .into(),
                                );
                            }
                        }
                        this.refresh_entries(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }

            _ = this.update(cx, |this, cx| {
                let progress = this.import_progress.take().unwrap_or_default();
                let cancelled = cancel.load(AtomicOrdering::Acquire) || progress.cancelled;
                this.import_cancel = None;
                this.import_message = Some(
                    format!(
                        "{} {}. {} already present, {} skipped, {} failed{}.",
                        if cancelled {
                            "Import stopped after"
                        } else {
                            "Imported"
                        },
                        progress.added,
                        progress.duplicates,
                        progress.skipped,
                        progress.failed,
                        if cancelled { " before completion" } else { "" }
                    )
                    .into(),
                );
                this.refresh_entries(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn add_files(&mut self, cx: &mut Context<Self>) {
        let target_collection = self.library_import_target(cx);
        self.add_files_to(target_collection, cx);
    }

    fn add_files_to(
        &mut self,
        target_collection: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add books and papers".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = dialog.await else {
                return;
            };
            _ = this.update(cx, |this, cx| {
                this.start_library_import(paths, target_collection, cx);
            });
        })
        .detach();
    }

    fn add_selected_history_item(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if entry.item_id.is_some() {
            return;
        }
        self.start_library_import(vec![entry.path], None, cx);
    }

    fn toggle_selected_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_item_id() else {
            return;
        };
        let favorite = cx
            .global::<LibraryStore>()
            .library_item(id)
            .is_some_and(|item| !item.favorite);
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_favorite(id, favorite)
        {
            eprintln!("failed to update favorite: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    fn cycle_selected_status(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_item_id() else {
            return;
        };
        let status = match cx
            .global::<LibraryStore>()
            .library_item(id)
            .map(|item| item.status)
        {
            Some(ReadingStatus::ToRead) => ReadingStatus::Reading,
            Some(ReadingStatus::Reading) => ReadingStatus::Read,
            Some(ReadingStatus::Read) | None => ReadingStatus::ToRead,
        };
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_status(id, status)
        {
            eprintln!("failed to update reading status: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    fn restore_selected_items(&mut self, cx: &mut Context<Self>) {
        let ids = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path))
            .filter_map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        if let Err(error) = cx.global_mut::<LibraryStore>().restore_library_items(&ids) {
            eprintln!("failed to restore library items: {error:#}");
        }
        self.refresh_entries(cx);
        self.selection.clear();
        cx.notify();
    }

    fn add_tag_to_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let name = self.tag_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .add_tag_to_items(&[item_id], &name)
        {
            eprintln!("failed to add library tag: {error:#}");
            return;
        }
        self.tag_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.refresh_entries(cx);
        cx.notify();
    }

    fn remove_tag_from_selected(&mut self, tag_id: LibraryTagId, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let retained = cx
            .global::<LibraryStore>()
            .tags_for_item(item_id)
            .into_iter()
            .map(|tag| tag.id)
            .filter(|candidate| *candidate != tag_id)
            .collect::<Vec<_>>();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_item_tags(item_id, &retained)
        {
            eprintln!("failed to remove library tag: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    fn set_selected_collection(
        &mut self,
        collection_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let collection_ids = collection_id.into_iter().collect::<Vec<_>>();
        let result = cx
            .global_mut::<LibraryStore>()
            .set_item_collections(item_id, &collection_ids);
        if let Err(error) = result {
            eprintln!("failed to move Library item: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    fn open_saved_view_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggested = match &self.session.destination {
            ReaderNewTabDestination::Favorites => "Favorite works",
            ReaderNewTabDestination::Tag(_) => "Tagged works",
            ReaderNewTabDestination::SavedView(_) => "Copy of saved view",
            _ => "Research view",
        };
        self.saved_view_name_input
            .update(cx, |input, cx| input.set_value(suggested, window, cx));
        self.saved_view_editor_open = true;
        let input = self.saved_view_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn save_current_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .saved_view_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        let mut filter =
            Self::filter_for_destination(cx.global::<LibraryStore>(), &self.session.destination);
        let search = self.search_input.read(cx).value().trim().to_string();
        if !search.is_empty() {
            filter.text = search;
        }
        let result = cx.global_mut::<LibraryStore>().save_library_view(
            &name,
            filter,
            self.session.sort,
            self.session.view_mode,
        );
        match result {
            Ok(view_id) => {
                self.saved_view_editor_open = false;
                self.set_destination(ReaderNewTabDestination::SavedView(view_id), window, cx);
            }
            Err(error) => eprintln!("failed to save library view: {error:#}"),
        }
        cx.notify();
    }

    fn delete_current_saved_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ReaderNewTabDestination::SavedView(view_id) = self.session.destination else {
            return;
        };
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete saved view?",
            Some("The books and papers matched by this view remain in your library."),
            &[
                PromptButton::ok("Delete View"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = cx.global_mut::<LibraryStore>().delete_library_view(view_id) {
                    eprintln!("failed to delete saved library view: {error:#}");
                    return;
                }
                this.set_destination(ReaderNewTabDestination::Library, window, cx);
            });
        })
        .detach();
    }

    fn open_tag_editor(
        &mut self,
        tag_id: LibraryTagId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tag_editor_id = Some(tag_id);
        self.tag_editor_name_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
        let input = self.tag_editor_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn save_tag_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tag_id) = self.tag_editor_id else {
            return;
        };
        let name = self
            .tag_editor_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        match cx
            .global_mut::<LibraryStore>()
            .rename_library_tag(tag_id, &name)
        {
            Ok(result_id) => {
                if self.session.destination == ReaderNewTabDestination::Tag(tag_id) {
                    self.session.destination = ReaderNewTabDestination::Tag(result_id);
                }
                self.tag_editor_id = None;
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to rename library tag: {error:#}"),
        }
        cx.notify();
    }

    fn delete_edited_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag_id) = self.tag_editor_id else {
            return;
        };
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete tag?",
            Some("This removes the tag from every work. It does not delete any library item."),
            &[
                PromptButton::ok("Delete Tag"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = cx.global_mut::<LibraryStore>().delete_library_tag(tag_id) {
                    eprintln!("failed to delete library tag: {error:#}");
                    return;
                }
                this.tag_editor_id = None;
                if this.session.destination == ReaderNewTabDestination::Tag(tag_id) {
                    this.set_destination(ReaderNewTabDestination::Tags, window, cx);
                } else {
                    this.refresh_entries(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_metadata_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let Some(item) = cx.global::<LibraryStore>().library_item(item_id).cloned() else {
            return;
        };
        let creators = item
            .creators
            .iter()
            .map(|creator| creator.name.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let mut set = |input: &Entity<InputState>, value: String| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        self.metadata_kind = item.kind;
        set(&self.metadata_title_input, item.title);
        set(&self.metadata_creators_input, creators);
        set(
            &self.metadata_year_input,
            item.year.map(|year| year.to_string()).unwrap_or_default(),
        );
        set(&self.metadata_publication_input, item.publication);
        set(&self.metadata_publisher_input, item.publisher);
        set(
            &self.metadata_doi_input,
            item.identifiers.doi.unwrap_or_default(),
        );
        set(
            &self.metadata_isbn_input,
            item.identifiers.isbn.unwrap_or_default(),
        );
        set(
            &self.metadata_arxiv_input,
            item.identifiers.arxiv.unwrap_or_default(),
        );
        set(&self.metadata_subject_input, item.subject);
        set(&self.metadata_language_input, item.language);
        set(&self.metadata_abstract_input, item.abstract_text);
        self.metadata_editor_open = true;
        let input = self.metadata_title_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    fn save_metadata_editor(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let Some(mut item) = cx.global::<LibraryStore>().library_item(item_id).cloned() else {
            return;
        };
        item.kind = self.metadata_kind;
        item.title = self.metadata_title_input.read(cx).value().to_string();
        item.creators = self
            .metadata_creators_input
            .read(cx)
            .value()
            .split(';')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| LibraryCreator {
                name: name.to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
        item.year = self
            .metadata_year_input
            .read(cx)
            .value()
            .trim()
            .parse::<i32>()
            .ok();
        item.publication = self.metadata_publication_input.read(cx).value().to_string();
        item.publisher = self.metadata_publisher_input.read(cx).value().to_string();
        let optional_value = |input: &Entity<InputState>, cx: &App| {
            let value = input.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        item.identifiers.doi = optional_value(&self.metadata_doi_input, cx);
        item.identifiers.isbn = optional_value(&self.metadata_isbn_input, cx);
        item.identifiers.arxiv = optional_value(&self.metadata_arxiv_input, cx);
        item.subject = self.metadata_subject_input.read(cx).value().to_string();
        item.language = self.metadata_language_input.read(cx).value().to_string();
        item.abstract_text = self.metadata_abstract_input.read(cx).value().to_string();
        match cx.global_mut::<LibraryStore>().update_library_item(item) {
            Ok(()) => {
                self.metadata_editor_open = false;
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to update library metadata: {error:#}"),
        }
        cx.notify();
    }

    fn locate_selected_attachment(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let Some(managed_root) = cx
            .global::<LibraryStore>()
            .managed_library_root()
            .map(Path::to_path_buf)
        else {
            self.import_message = Some("Managed Library storage is unavailable.".into());
            cx.notify();
            return;
        };
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Locate linked book or paper".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = dialog.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let prepared = cx
                .background_spawn(async move {
                    prepare_managed_import(&managed_root, &path, &AtomicBool::new(false))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                let result = prepared.and_then(|prepared| {
                    cx.global_mut::<LibraryStore>()
                        .finalize_managed_replacement(item_id, prepared)
                });
                match result {
                    Ok(()) => {
                        this.import_message = Some("Managed Library file replaced.".into());
                        this.refresh_entries(cx);
                    }
                    Err(error) => {
                        this.import_message =
                            Some(format!("Could not replace Library file: {error:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn trash_selected_item(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .trash_library_items(&[item_id])
        {
            eprintln!("failed to move library item to Trash: {error:#}");
        }
        self.refresh_entries(cx);
        self.selection.clear();
        cx.notify();
    }

    fn destination_label(destination: &ReaderNewTabDestination, store: &LibraryStore) -> String {
        match destination {
            ReaderNewTabDestination::Home => "Home".to_string(),
            ReaderNewTabDestination::Library => "Library".to_string(),
            ReaderNewTabDestination::Favorites => "Favorites".to_string(),
            ReaderNewTabDestination::History => "History".to_string(),
            ReaderNewTabDestination::Tags => "Tags".to_string(),
            ReaderNewTabDestination::Trash => "Trash".to_string(),
            ReaderNewTabDestination::Tag(id) => store
                .library_tags()
                .iter()
                .find(|tag| tag.id == *id)
                .map(|tag| format!("#{}", tag.name))
                .unwrap_or_else(|| "Tag".to_string()),
            ReaderNewTabDestination::SavedView(id) => store
                .library_saved_views()
                .iter()
                .find(|view| view.id == *id)
                .map(|view| view.name.clone())
                .unwrap_or_else(|| "Saved View".to_string()),
        }
    }

    fn render_destination_button(
        destination: ReaderNewTabDestination,
        label: String,
        icon: IconName,
        selected: bool,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = format!("new-tab-nav-{destination:?}");
        let debug_label = label.clone();
        let mut button = Button::new(id)
            .debug_selector(move || format!("new-tab-nav-{debug_label}"))
            .ghost()
            .selected(selected)
            .icon(icon)
            .label(label);
        if compact {
            button = button.xsmall().compact();
        } else {
            button = button.small().w_full();
        }
        button
            .on_click(cx.listener(move |this, _, window, cx| {
                this.set_destination(destination.clone(), window, cx);
                cx.stop_propagation();
            }))
            .into_any_element()
    }

    fn primary_destinations() -> [(ReaderNewTabDestination, &'static str, IconName); 6] {
        [
            (ReaderNewTabDestination::Home, "Home", IconName::AppWindow),
            (
                ReaderNewTabDestination::Library,
                "Library",
                IconName::BookOpen,
            ),
            (
                ReaderNewTabDestination::Favorites,
                "Favorites",
                IconName::Circle,
            ),
            (
                ReaderNewTabDestination::History,
                "History",
                IconName::RotateCcw,
            ),
            (ReaderNewTabDestination::Tags, "Tags", IconName::Link2),
            (ReaderNewTabDestination::Trash, "Trash", IconName::Trash2),
        ]
    }

    fn home_destinations() -> [(ReaderNewTabDestination, &'static str, IconName); 3] {
        [
            (
                ReaderNewTabDestination::Library,
                "Library",
                IconName::BookOpen,
            ),
            (
                ReaderNewTabDestination::History,
                "History",
                IconName::RotateCcw,
            ),
            (ReaderNewTabDestination::Trash, "Trash", IconName::Trash2),
        ]
    }

    fn render_quick_source_selector(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.session.destination.clone();
        let label = Self::destination_label(&selected, cx.global::<LibraryStore>());
        let mut selector = div().relative().child(
            Button::new("new-tab-source-selector")
                .debug_selector(|| "new-tab-source-selector".to_string())
                .small()
                .ghost()
                .label(label)
                .icon(IconName::ChevronDown)
                .selected(self.nav_menu_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.nav_menu_open = !this.nav_menu_open;
                    cx.stop_propagation();
                    cx.notify();
                })),
        );
        if self.nav_menu_open {
            let menu = div()
                .id("new-tab-source-menu")
                .debug_selector(|| "new-tab-source-menu".to_string())
                .absolute()
                .top(px(30.0))
                .left_0()
                .w(px(180.0))
                .p_1()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .children([
                    Self::render_destination_button(
                        ReaderNewTabDestination::History,
                        "History".to_string(),
                        IconName::RotateCcw,
                        selected == ReaderNewTabDestination::History,
                        false,
                        cx,
                    ),
                    Self::render_destination_button(
                        ReaderNewTabDestination::Library,
                        "Library".to_string(),
                        IconName::BookOpen,
                        selected == ReaderNewTabDestination::Library,
                        false,
                        cx,
                    ),
                ]);
            selector = selector.child(menu);
        }
        selector.into_any_element()
    }

    fn render_library_filter_menu(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.session.destination.clone();
        let tags = cx
            .global::<LibraryStore>()
            .library_tags()
            .iter()
            .map(|tag| (tag.id, tag.name.clone()))
            .collect::<Vec<_>>();
        let saved_views = cx
            .global::<LibraryStore>()
            .library_saved_views()
            .iter()
            .map(|view| (view.id, view.name.clone()))
            .collect::<Vec<_>>();
        let active_filter = !matches!(selected, ReaderNewTabDestination::Library);
        let mut control = div().relative().child(
            Button::new("home-library-filter-menu")
                .debug_selector(|| "home-library-filter-menu".to_string())
                .xsmall()
                .outline()
                .icon(IconName::Link2)
                .label("Filters")
                .selected(active_filter || self.nav_menu_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.nav_menu_open = !this.nav_menu_open;
                    cx.stop_propagation();
                    cx.notify();
                })),
        );
        if self.nav_menu_open {
            let mut menu = div()
                .id("home-library-filter-popover")
                .debug_selector(|| "home-library-filter-popover".to_string())
                .absolute()
                .top(px(28.0))
                .right_0()
                .w(px(240.0))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg()
                .child(Self::render_destination_button(
                    ReaderNewTabDestination::Library,
                    "All Library Items".to_string(),
                    IconName::BookOpen,
                    selected == ReaderNewTabDestination::Library,
                    false,
                    cx,
                ))
                .child(Self::render_destination_button(
                    ReaderNewTabDestination::Favorites,
                    "Favorites".to_string(),
                    IconName::Circle,
                    selected == ReaderNewTabDestination::Favorites,
                    false,
                    cx,
                ));
            if !saved_views.is_empty() {
                menu = menu.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("SAVED VIEWS"),
                );
                for (id, name) in saved_views {
                    menu = menu.child(Self::render_destination_button(
                        ReaderNewTabDestination::SavedView(id),
                        name,
                        IconName::Table2,
                        selected == ReaderNewTabDestination::SavedView(id),
                        false,
                        cx,
                    ));
                }
            }
            menu = menu.child(Self::render_destination_button(
                ReaderNewTabDestination::Tags,
                "Manage Tags".to_string(),
                IconName::Pencil,
                selected == ReaderNewTabDestination::Tags,
                false,
                cx,
            ));
            if !tags.is_empty() {
                menu = menu.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("TAGS"),
                );
                for (id, name) in tags {
                    menu = menu.child(Self::render_destination_button(
                        ReaderNewTabDestination::Tag(id),
                        format!("#{name}"),
                        IconName::Link2,
                        selected == ReaderNewTabDestination::Tag(id),
                        false,
                        cx,
                    ));
                }
            }
            control = control.child(menu);
        }
        control.into_any_element()
    }

    fn render_navigation(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let selected = self.session.destination.clone();
        if self.host == LibraryBrowserHost::Home {
            let destinations = Self::home_destinations();
            let mut navigation = slim_navigation_rail("home-navigation-rail", theme);
            for (destination, label, icon) in destinations.iter().take(2) {
                let destination = destination.clone();
                navigation = navigation.child(
                    slim_navigation_button(
                        format!("home-nav-{destination:?}"),
                        *icon,
                        selected == destination,
                        *label,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_destination(destination.clone(), window, cx);
                        cx.stop_propagation();
                    })),
                );
            }
            let (destination, label, icon) = &destinations[2];
            let destination = destination.clone();
            return navigation
                .child(div().flex_1())
                .child(
                    slim_navigation_button(
                        format!("home-nav-{destination:?}"),
                        *icon,
                        selected == destination,
                        *label,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_destination(destination.clone(), window, cx);
                        cx.stop_propagation();
                    })),
                )
                .into_any_element();
        }
        let store = cx.global::<LibraryStore>();
        let tag_destinations = store
            .library_tags()
            .iter()
            .map(|tag| {
                (
                    ReaderNewTabDestination::Tag(tag.id),
                    format!("#{}", tag.name),
                )
            })
            .collect::<Vec<_>>();
        let saved_destinations = store
            .library_saved_views()
            .iter()
            .map(|view| {
                (
                    ReaderNewTabDestination::SavedView(view.id),
                    view.name.clone(),
                )
            })
            .collect::<Vec<_>>();
        let current_label = Self::destination_label(&selected, store);

        if self.layout_class == NewTabLayoutClass::Expanded {
            let mut navigation = div()
                .id("new-tab-navigation-rail")
                .debug_selector(|| "new-tab-navigation-rail".to_string())
                .w(px(NEW_TAB_NAV_WIDTH))
                .h_full()
                .flex_none()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .border_r(px(1.0))
                .border_color(theme.border)
                .bg(theme.sidebar);
            for (destination, label, icon) in Self::primary_destinations() {
                navigation = navigation.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            if !saved_destinations.is_empty() {
                navigation = navigation.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("SAVED VIEWS"),
                );
                for (destination, label) in saved_destinations {
                    navigation = navigation.child(Self::render_destination_button(
                        destination.clone(),
                        label,
                        IconName::Table2,
                        selected == destination,
                        false,
                        cx,
                    ));
                }
            }
            if !tag_destinations.is_empty() {
                navigation = navigation.child(
                    div()
                        .mt_2()
                        .px_2()
                        .text_size(ui_rems(10.0))
                        .text_color(theme.muted_foreground)
                        .child("TAGS"),
                );
                for (destination, label) in tag_destinations.into_iter().take(12) {
                    navigation = navigation.child(Self::render_destination_button(
                        destination.clone(),
                        label,
                        IconName::Link2,
                        selected == destination,
                        false,
                        cx,
                    ));
                }
            }
            return navigation.into_any_element();
        }

        let mut bar = div()
            .id("new-tab-navigation-top")
            .debug_selector(|| "new-tab-navigation-top".to_string())
            .relative()
            .h(px(40.0))
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .border_b(px(1.0))
            .border_color(theme.border)
            .bg(theme.sidebar);
        if self.layout_class == NewTabLayoutClass::Narrow {
            bar = bar
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui_rems(13.0))
                        .child(current_label),
                )
                .child(
                    Button::new("new-tab-nav-menu")
                        .xsmall()
                        .compact()
                        .ghost()
                        .icon(IconName::Menu)
                        .label("Browse")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_menu_open = !this.nav_menu_open;
                            cx.stop_propagation();
                            cx.notify();
                        })),
                );
        } else {
            for (destination, label, icon) in Self::primary_destinations() {
                bar = bar.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    true,
                    cx,
                ));
            }
            if !saved_destinations.is_empty() || !tag_destinations.is_empty() {
                bar = bar.child(
                    Button::new("new-tab-nav-more")
                        .xsmall()
                        .compact()
                        .ghost()
                        .icon(IconName::Menu)
                        .label("More")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_menu_open = !this.nav_menu_open;
                            cx.stop_propagation();
                            cx.notify();
                        })),
                );
            }
        }
        if self.nav_menu_open {
            let mut menu = div()
                .id("new-tab-nav-overflow")
                .debug_selector(|| "new-tab-nav-overflow".to_string())
                .absolute()
                .top(px(38.0))
                .right(px(8.0))
                .w(px(220.0))
                .max_h(px(420.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .shadow_lg();
            for (destination, label, icon) in Self::primary_destinations() {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label.to_string(),
                    icon,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            for (destination, label) in saved_destinations {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label,
                    IconName::Table2,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            for (destination, label) in tag_destinations {
                menu = menu.child(Self::render_destination_button(
                    destination.clone(),
                    label,
                    IconName::Link2,
                    selected == destination,
                    false,
                    cx,
                ));
            }
            bar = bar.child(menu);
        }
        bar.into_any_element()
    }

    fn tag_result_counts(&self, cx: &Context<Self>) -> (i64, i64) {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let tags = cx.global::<LibraryStore>().library_tags();
        let total = tags.len() as i64;
        let visible = tags
            .iter()
            .filter(|tag| query.is_empty() || tag.name.to_lowercase().contains(&query))
            .count() as i64;
        (visible, total)
    }

    fn render_tags_content(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let tags = cx
            .global::<LibraryStore>()
            .library_tags()
            .iter()
            .filter(|tag| query.is_empty() || tag.name.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        let relations = cx
            .global::<LibraryStore>()
            .library
            .library_item_tags
            .clone();
        if tags.is_empty() {
            return div()
                .id("new-tab-tags-content")
                .debug_selector(|| "new-tab-tags-content".to_string())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.render_empty_state(self.empty_state(!query.is_empty()), theme, cx))
                .into_any_element();
        }
        div()
            .id("new-tab-tags-content")
            .debug_selector(|| "new-tab-tags-content".to_string())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .children(tags.into_iter().map(|tag| {
                let count = relations
                    .iter()
                    .filter(|relation| relation.tag_id == tag.id)
                    .count();
                let destination = ReaderNewTabDestination::Tag(tag.id);
                let tag_id = tag.id;
                let tag_name = tag.name.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(("new-tab-tag", tag.id.0 as usize))
                            .small()
                            .outline()
                            .flex_1()
                            .label(format!("#{}  ({count})", tag.name))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_destination(destination.clone(), window, cx);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        Button::new(("new-tab-tag-edit", tag_id.0 as usize))
                            .xsmall()
                            .ghost()
                            .icon(IconName::Pencil)
                            .tooltip("Rename, merge, or delete tag")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_tag_editor(tag_id, tag_name.clone(), window, cx);
                                cx.stop_propagation();
                            })),
                    )
            }))
            .into_any_element()
    }

    fn render_inspector(&self, theme: ThemeColor, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.selected_entry()?.clone();
        let item = entry
            .item_id
            .and_then(|id| cx.global::<LibraryStore>().library_item(id).cloned());
        let item_tags = item
            .as_ref()
            .map(|item| {
                cx.global::<LibraryStore>()
                    .tags_for_item(item.id)
                    .into_iter()
                    .map(|tag| (tag.id, tag.name.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (assigned_collection_id, collection_options) = item
            .as_ref()
            .map(|item| {
                let store = cx.global::<LibraryStore>();
                let assigned = store.item_parent(item.id);
                let mut options = store
                    .library_collections()
                    .iter()
                    .map(|collection| {
                        (
                            collection.id,
                            store.collection_path_label(collection.id),
                            store
                                .collection_ancestors(collection.id)
                                .len()
                                .saturating_sub(1),
                        )
                    })
                    .collect::<Vec<_>>();
                options.sort_by(|left, right| left.1.cmp(&right.1));
                (assigned, options)
            })
            .unwrap_or_default();
        let mut inspector = div()
            .id("new-tab-item-inspector")
            .debug_selector(|| "new-tab-item-inspector".to_string())
            .w(px(NEW_TAB_INSPECTOR_WIDTH))
            .h_full()
            .flex_none()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .border_l(px(1.0))
            .border_color(theme.border)
            .bg(theme.sidebar)
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_size(ui_rems(15.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(entry.file_name.clone()),
                    )
                    .child(
                        Button::new("new-tab-inspector-close")
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_open = false;
                                cx.stop_propagation();
                                cx.notify();
                            })),
                    ),
            );
        if let Some(item) = item {
            let creators = item
                .creators
                .iter()
                .map(|creator| creator.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let status = match item.status {
                ReadingStatus::ToRead => "To Read",
                ReadingStatus::Reading => "Reading",
                ReadingStatus::Read => "Read",
            };
            inspector = inspector
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(if creators.is_empty() {
                            "Unknown creator".to_string()
                        } else {
                            creators
                        }),
                )
                .when_some(item.year, |this, year| this.child(format!("Year: {year}")))
                .when(!item.publication.is_empty(), |this| {
                    this.child(format!("Publication: {}", item.publication))
                })
                .when(!item_tags.is_empty(), |this| {
                    this.child(div().flex().flex_wrap().gap_1().children(
                        item_tags.clone().into_iter().map(|(tag_id, name)| {
                            Button::new(("new-tab-inspector-tag", tag_id.0 as usize))
                                .xsmall()
                                .outline()
                                .label(format!("#{name} ×"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_tag_from_selected(tag_id, cx);
                                    cx.stop_propagation();
                                }))
                        }),
                    ))
                })
                .when(entry.missing, |this| {
                    this.child(
                        div()
                            .text_color(theme.danger)
                            .child("Linked file is missing. Use Locate/Relink from item actions."),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .child(
                            Button::new("new-tab-inspector-favorite")
                                .xsmall()
                                .outline()
                                .label(if item.favorite {
                                    "Unfavorite"
                                } else {
                                    "Favorite"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toggle_selected_favorite(cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-status")
                                .xsmall()
                                .outline()
                                .label(status)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.cycle_selected_status(cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-edit-metadata")
                                .xsmall()
                                .outline()
                                .icon(IconName::Pencil)
                                .label("Edit Metadata")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_metadata_editor(window, cx);
                                    cx.stop_propagation();
                                })),
                        )
                        .child(
                            Button::new("new-tab-inspector-locate")
                                .xsmall()
                                .outline()
                                .icon(IconName::LocateFixed)
                                .label(if entry.missing { "Locate" } else { "Relink" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.locate_selected_attachment(cx);
                                    cx.stop_propagation();
                                })),
                        ),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .child("FOLDER"),
                )
                .child(
                    div()
                        .id("new-tab-inspector-library-root")
                        .h(px(30.0))
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .hover(move |style| style.bg(theme.list_hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_selected_collection(None, cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            Checkbox::new("new-tab-inspector-library-root-check")
                                .xsmall()
                                .checked(assigned_collection_id.is_none())
                                .tab_stop(false),
                        )
                        .child("Library root"),
                )
                .children(collection_options.into_iter().map(|(id, path, depth)| {
                    let checked = assigned_collection_id == Some(id);
                    div()
                        .id(("new-tab-inspector-folder", id.0 as usize))
                        .debug_selector(|| "new-tab-inspector-folder".to_string())
                        .h(px(30.0))
                        .w_full()
                        .pl(px(depth as f32 * 12.0))
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .hover(move |style| style.bg(theme.list_hover))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_selected_collection(Some(id), cx);
                                cx.stop_propagation();
                            }),
                        )
                        .child(
                            Checkbox::new(("new-tab-inspector-folder-check", id.0 as usize))
                                .xsmall()
                                .checked(checked)
                                .tab_stop(false),
                        )
                        .child(div().min_w_0().truncate().child(path))
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .h(px(30.0))
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .child(Input::new(&self.tag_input).small().appearance(false)),
                        )
                        .child(
                            Button::new("new-tab-inspector-add-tag")
                                .xsmall()
                                .primary()
                                .icon(IconName::Plus)
                                .label("Tag")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.add_tag_to_selected(window, cx);
                                    cx.stop_propagation();
                                })),
                        ),
                )
                .child(
                    div()
                        .mt_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child("Research note")
                        .child(
                            Button::new("new-tab-note-expand")
                                .xsmall()
                                .ghost()
                                .icon(IconName::ArrowUpRight)
                                .label("Expand")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.note_expanded = true;
                                    let input = this.work_note_input.clone();
                                    window.defer(cx, move |window, cx| {
                                        input.update(cx, |input, cx| input.focus(window, cx));
                                    });
                                    cx.stop_propagation();
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    div()
                        .min_h(px(180.0))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Input::new(&self.work_note_input).appearance(false)),
                )
                .when(item.trashed_at.is_none(), |this| {
                    this.child(
                        Button::new("new-tab-inspector-trash")
                            .small()
                            .danger()
                            .icon(IconName::Trash2)
                            .label("Move to Trash")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.trash_selected_item(cx);
                                cx.stop_propagation();
                            })),
                    )
                });
        } else {
            inspector = inspector
                .child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child("This file is in History but not in your library."),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_size(ui_rems(11.0))
                        .child(format!("Path: {}", entry.path.display()))
                        .child(format!("Format: {}", entry.format_size))
                        .child(format!("Progress: {}", entry.progress))
                        .child(format!("Last opened: {}", entry.last_opened)),
                )
                .child(
                    Button::new("new-tab-add-selected-to-library")
                        .small()
                        .primary()
                        .icon(IconName::Plus)
                        .label("Add to Library")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.add_selected_history_item(cx);
                            cx.stop_propagation();
                        })),
                );
        }
        Some(inspector.into_any_element())
    }

    fn cancel_topmost_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let cancelled = if self.folder_editor.is_some() {
            self.folder_editor = None;
            true
        } else if self.tag_editor_id.is_some() {
            self.tag_editor_id = None;
            true
        } else if self.saved_view_editor_open {
            self.saved_view_editor_open = false;
            true
        } else if self.metadata_editor_open {
            self.metadata_editor_open = false;
            true
        } else if self.note_expanded {
            self.note_expanded = false;
            true
        } else {
            false
        };
        if cancelled {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
        cancelled
    }

    fn render_note_overlay(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("new-tab-note-overlay")
            .debug_selector(|| "new-tab-note-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .bg(theme.overlay)
            .child(
                div()
                    .id("new-tab-metadata-card")
                    .size_full()
                    .max_w(px(900.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .child(
                        div()
                            .h(px(44.0))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_3()
                            .border_b(px(1.0))
                            .border_color(theme.border)
                            .child("Research note (Markdown)")
                            .child(
                                Button::new("new-tab-note-overlay-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .p_3()
                            .child(Input::new(&self.work_note_input).appearance(false)),
                    ),
            )
            .into_any_element()
    }

    fn render_metadata_overlay(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        let field = |label: &'static str, input: &Entity<InputState>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(ui_rems(11.0))
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .min_h(px(32.0))
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.background)
                        .child(Input::new(input).small().appearance(false)),
                )
        };
        div()
            .id("new-tab-metadata-overlay")
            .debug_selector(|| "new-tab-metadata-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .bg(theme.overlay)
            .child(
                div()
                    .id("new-tab-metadata-card")
                    .size_full()
                    .max_w(px(760.0))
                    .mx_auto()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(ui_rems(16.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Edit academic metadata"),
                            )
                            .child(
                                Button::new("new-tab-metadata-close")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::X)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(theme.muted_foreground)
                                    .child("Item type"),
                            )
                            .child(
                                div().flex().gap_1().children(
                                    [
                                        (LibraryItemKind::Paper, "Paper"),
                                        (LibraryItemKind::Book, "Book"),
                                        (LibraryItemKind::Other, "Other"),
                                    ]
                                    .into_iter()
                                    .enumerate()
                                    .map(
                                        |(index, (kind, label))| {
                                            Button::new(("new-tab-metadata-kind", index))
                                                .small()
                                                .outline()
                                                .selected(self.metadata_kind == kind)
                                                .label(label)
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.metadata_kind = kind;
                                                    cx.stop_propagation();
                                                    cx.notify();
                                                }))
                                        },
                                    ),
                                ),
                            ),
                    )
                    .child(field("Title", &self.metadata_title_input))
                    .child(field("Creators", &self.metadata_creators_input))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("Year", &self.metadata_year_input)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("Language", &self.metadata_language_input)),
                            ),
                    )
                    .child(field(
                        "Publication or venue",
                        &self.metadata_publication_input,
                    ))
                    .child(field("Publisher", &self.metadata_publisher_input))
                    .child(field("DOI", &self.metadata_doi_input))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("ISBN", &self.metadata_isbn_input)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .child(field("arXiv", &self.metadata_arxiv_input)),
                            ),
                    )
                    .child(field("Subject", &self.metadata_subject_input))
                    .child(field("Abstract or summary", &self.metadata_abstract_input))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("new-tab-metadata-cancel")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                Button::new("new-tab-metadata-save")
                                    .small()
                                    .primary()
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_metadata_editor(cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_saved_view_overlay(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("new-tab-saved-view-overlay")
            .debug_selector(|| "new-tab-saved-view-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Save current library view"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("The current destination, search, sort, and layout will be reusable from the navigation panel."),
                    )
                    .child(
                        div()
                            .h(px(34.0))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                Input::new(&self.saved_view_name_input)
                                    .small()
                                    .appearance(false),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("new-tab-saved-view-cancel")
                                    .small()
                                    .outline()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel_topmost_editor(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                Button::new("new-tab-saved-view-save")
                                    .small()
                                    .primary()
                                    .label("Save View")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_current_view(window, cx);
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_tag_editor_overlay(&self, theme: ThemeColor, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("new-tab-tag-editor-overlay")
            .debug_selector(|| "new-tab-tag-editor-overlay".to_string())
            .key_context("NewTabModal Dialog")
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .p_6()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .child(
                div()
                    .w_full()
                    .max_w(px(420.0))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.popover)
                    .shadow_lg()
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Edit tag"),
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child("Renaming to an existing tag merges their assignments."),
                    )
                    .child(
                        div()
                            .h(px(34.0))
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .child(
                                Input::new(&self.tag_editor_name_input)
                                    .small()
                                    .appearance(false),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                Button::new("new-tab-tag-editor-delete")
                                    .small()
                                    .danger()
                                    .icon(IconName::Trash2)
                                    .label("Delete Tag")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.delete_edited_tag(window, cx);
                                        cx.stop_propagation();
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(
                                        Button::new("new-tab-tag-editor-cancel")
                                            .small()
                                            .outline()
                                            .label("Cancel")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_topmost_editor(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    )
                                    .child(
                                        Button::new("new-tab-tag-editor-save")
                                            .small()
                                            .primary()
                                            .label("Save")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.save_tag_editor(cx);
                                                cx.stop_propagation();
                                            })),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_toolbar_button(
        label: String,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(label.clone())
            .small()
            .outline()
            .label(label)
            .on_click(cx.listener(move |this, _, window, cx| {
                handler(this, window, cx);
                cx.stop_propagation();
            }))
    }

    fn render_destination_heading(
        &self,
        label: String,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_root_target = self.host == LibraryBrowserHost::Home && self.is_library_tree();
        let highlighted = self.library_drop_target == Some(LibraryDropTarget::Root);
        let mut heading = div()
            .id("library-destination-heading")
            .px_1()
            .rounded(px(3.0))
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .child(label);
        if is_root_target {
            heading = heading
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    |this, event: &DragMoveEvent<DraggedLibrarySelection>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_root_drag_target(&dragged, active, cx);
                    },
                ))
                .on_drop(
                    cx.listener(|this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, None, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_root_drag_target(active, cx);
                    },
                ))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, None, cx);
                    cx.stop_propagation();
                }));
            let view = cx.entity();
            return heading
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, None, view.clone())
                })
                .into_any_element();
        }
        heading.into_any_element()
    }

    fn render_view_button(
        mode: NewTabViewMode,
        current_mode: NewTabViewMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, icon, label) = match mode {
            NewTabViewMode::Grid => (
                "new-tab-view-grid",
                IconName::LayoutGrid,
                crate::fl!("new-tab-view-grid"),
            ),
            NewTabViewMode::Grouped => (
                "new-tab-view-grouped",
                IconName::LayoutList,
                crate::fl!("new-tab-view-grouped"),
            ),
            NewTabViewMode::Table => (
                "new-tab-view-tree",
                IconName::ListTree,
                crate::fl!("new-tab-view-tree"),
            ),
        };

        Button::new(id)
            .debug_selector(move || id.to_string())
            .xsmall()
            .compact()
            .ghost()
            .selected(mode == current_mode)
            .icon(icon)
            .tooltip(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_view_mode(mode, cx);
                cx.stop_propagation();
            }))
    }

    fn visible_child_collections(
        &self,
        cx: &Context<Self>,
    ) -> Vec<(LibraryCollectionId, SharedString)> {
        if self.session.destination != ReaderNewTabDestination::Library
            || !self.search_input.read(cx).value().trim().is_empty()
        {
            return Vec::new();
        }
        cx.global::<LibraryStore>()
            .child_collections(self.current_collection)
            .into_iter()
            .map(|collection| (collection.id, collection.name.clone().into()))
            .collect()
    }

    fn collection_context_menu(
        menu: PopupMenu,
        id: Option<LibraryCollectionId>,
        view: Entity<Self>,
    ) -> PopupMenu {
        let new_folder_view = view.clone();
        let mut menu = menu.item(
            PopupMenuItem::new(if id.is_some() {
                "New Subfolder"
            } else {
                "New Folder"
            })
            .icon(IconName::Plus)
            .on_click(move |_, window, cx| {
                new_folder_view.update(cx, |this, cx| {
                    this.open_new_folder_editor(id, window, cx);
                });
            }),
        );
        if let Some(id) = id {
            let rename_view = view.clone();
            let delete_view = view;
            menu = menu
                .item(
                    PopupMenuItem::new("Rename Folder")
                        .icon(IconName::Pencil)
                        .on_click(move |_, window, cx| {
                            rename_view.update(cx, |this, cx| {
                                this.open_rename_folder_editor(id, window, cx);
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Delete Folder")
                        .icon(IconName::Trash2)
                        .on_click(move |_, window, cx| {
                            delete_view.update(cx, |this, cx| {
                                this.request_delete_folder(id, window, cx);
                            });
                        }),
                );
        }
        menu
    }

    fn render_collection_item(
        &self,
        id: LibraryCollectionId,
        name: SharedString,
        grouped: bool,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let management = self.host == LibraryBrowserHost::Home;
        let enter_id = id;
        let drag_name = name.clone();
        let mut item = div()
            .id(("library-folder-item", id.0 as usize))
            .debug_selector(|| "library-folder-item".to_string())
            .relative()
            .flex_none()
            .max_w_full()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .when(grouped, |item| item.h(px(40.0)).w_auto())
            .when(!grouped, |item| {
                item.w(px(CARD_WIDTH))
                    .h(px(92.0))
                    .flex_col()
                    .justify_center()
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.enter_collection(enter_id, cx);
                cx.stop_propagation();
            }));
        if management {
            let dragged = DraggedLibrarySelection {
                active: LibraryNodeId::Collection(id),
                marked: Arc::from([LibraryNodeId::Collection(id)]),
                label: drag_name,
            };
            item = item
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedLibrarySelection>(move |style, _, _, _| {
                    style.border_color(theme.primary).bg(theme.list_hover)
                })
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, Some(id), cx);
                    }),
                );
        }

        let label = div()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Icon::new(IconName::FolderOpen)
                    .size(ui_rems(if grouped { 16.0 } else { 22.0 }))
                    .text_color(theme.muted_foreground),
            )
            .child(div().min_w_0().truncate().child(name));
        let item = item.child(label);
        if management {
            let view = cx.entity();
            return item
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, Some(id), view.clone())
                })
                .into_any_element();
        }
        item.into_any_element()
    }

    fn render_collection_breadcrumb(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.session.destination != ReaderNewTabDestination::Library
            || !self.search_input.read(cx).value().trim().is_empty()
            || self.session.view_mode == LibraryViewMode::Table
        {
            return None;
        }
        let mut crumbs = vec![(None, "Library".to_string())];
        if let Some(id) = self.current_collection {
            crumbs.extend(
                cx.global::<LibraryStore>()
                    .collection_ancestors(id)
                    .into_iter()
                    .map(|collection| (Some(collection.id), collection.name.clone())),
            );
        }
        let management = self.host == LibraryBrowserHost::Home;
        let mut breadcrumb = div()
            .id("library-folder-breadcrumb")
            .debug_selector(|| "library-folder-breadcrumb".to_string())
            .h(px(34.0))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .gap_0p5()
            .px_4()
            .border_b(px(1.0))
            .border_color(theme.border);
        for (ix, (id, label)) in crumbs.into_iter().enumerate() {
            if ix > 0 {
                breadcrumb = breadcrumb.child(
                    Icon::new(IconName::ChevronRight)
                        .size(ui_rems(12.0))
                        .text_color(theme.muted_foreground),
                );
            }
            let mut crumb = Button::new(("library-folder-crumb", ix))
                .xsmall()
                .ghost()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.enter_collection_parent(id, cx);
                    cx.stop_propagation();
                }));
            if management {
                crumb = crumb
                    .drag_over::<DraggedLibrarySelection>(move |style, _, _, _| {
                        style.bg(theme.list_hover)
                    })
                    .on_drop(
                        cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                            this.drop_library_selection(dragged, id, cx);
                        }),
                    );
            }
            breadcrumb = breadcrumb.child(crumb);
        }
        Some(breadcrumb.into_any_element())
    }

    fn render_folder_editor_overlay(
        &self,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let editor = self.folder_editor?;
        let title = if editor.collection_id.is_some() {
            "Rename Folder"
        } else {
            "New Folder"
        };
        Some(
            div()
                .id("library-folder-editor-overlay")
                .debug_selector(|| "library-folder-editor-overlay".to_string())
                .key_context("NewTabModal Dialog")
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(transparent_black().opacity(0.35))
                .child(
                    div()
                        .w(px(360.0))
                        .max_w_full()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popover)
                        .shadow_lg()
                        .child(div().text_size(ui_rems(14.0)).child(title))
                        .child(Input::new(&self.folder_name_input).small())
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("library-folder-editor-cancel")
                                        .small()
                                        .outline()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.cancel_topmost_editor(window, cx);
                                            cx.stop_propagation();
                                        })),
                                )
                                .child(
                                    Button::new("library-folder-editor-save")
                                        .small()
                                        .primary()
                                        .label("Save")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_folder_editor(cx);
                                            cx.stop_propagation();
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn item_state(&self, item_ix: usize) -> NewTabItemState {
        let key = self.item_key(item_ix);
        let selected = key.as_ref().is_some_and(|key| match key {
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => false,
            NewTabItemKey::Document(path) => self.selection.contains(path),
        });
        NewTabItemState {
            item_ix,
            focused: key.as_ref() == Some(&self.focused_item),
            selecting: self.selecting,
            selected,
        }
    }

    fn drag_ids_for_entry(&self, entry: &NewTabEntry) -> Vec<DocumentId> {
        if self.host != LibraryBrowserHost::Home {
            return Vec::new();
        }
        if self.selection.contains(&entry.path) {
            let ids = self
                .entries
                .iter()
                .filter(|candidate| self.selection.contains(&candidate.path))
                .filter_map(|candidate| candidate.item_id)
                .collect::<HashSet<_>>();
            if !ids.is_empty() {
                return ids.into_iter().collect();
            }
        }
        entry.item_id.into_iter().collect()
    }

    fn render_document_card(
        entry: &NewTabEntry,
        state: NewTabCardState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        page_color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let NewTabCardState { thumbnail, item } = state;
        let item_ix = item.item_ix;
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let directory = entry.directory.clone();
        let last_read = entry.last_read.clone();
        let border_color = if item.focused {
            theme.ring
        } else if item.selected {
            theme.primary
        } else {
            theme.border
        };
        let card_bg = if item.selected {
            theme.primary.opacity(0x22 as f32 / 255.0)
        } else {
            theme.popover
        };

        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: file_name.clone(),
        };
        div()
            .id(("new-tab-card", item_ix))
            .debug_selector(move || format!("new-tab-card-{item_ix}"))
            .flex()
            .flex_none()
            .flex_col()
            .items_center()
            .relative()
            .w(px(CARD_WIDTH))
            .h(px(CARD_HEIGHT))
            .p_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(border_color)
            .bg(card_bg)
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |card| {
                card.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            })
            .when(item.selecting, |card| {
                card.child(
                    Checkbox::new(("new-tab-card-selection", item.item_ix))
                        .xsmall()
                        .checked(item.selected)
                        .tab_stop(false)
                        .absolute()
                        .top(px(6.0))
                        .right(px(6.0))
                        .p_1()
                        .bg(theme.popover.opacity(0xe8 as f32 / 255.0))
                        .rounded(cx.theme().radius),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(THUMBNAIL_MAX_WIDTH))
                    .h(px(THUMBNAIL_MAX_HEIGHT))
                    .child(Self::render_thumbnail(thumbnail, theme, page_color)),
            )
            .child(
                div()
                    .mt_3()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(13.0))
                    .child(file_name),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(directory),
            )
            .child(
                div()
                    .mt_1()
                    .w_full()
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(last_read),
            )
    }

    fn render_tree_document_row(
        entry: &NewTabEntry,
        state: NewTabItemState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = entry.path.clone();
        let file_name = entry.file_name.clone();
        let icon = entry.icon();

        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: file_name.clone(),
        };
        let mut row = div()
            .id(("new-tab-tree-document-row", state.item_ix))
            .debug_selector(|| "new-tab-tree-document-row".to_string())
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else if state.selected {
                theme.primary
            } else {
                transparent_black()
            })
            .bg(if state.selected {
                theme.primary.opacity(0x18 as f32 / 255.0)
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |row| {
                row.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            });

        if state.selecting {
            row = row.child(
                Checkbox::new(("new-tab-row-selection", state.item_ix))
                    .xsmall()
                    .checked(state.selected)
                    .tab_stop(false),
            );
        }

        row = row
            .child(
                Icon::new(icon)
                    .size(ui_rems(16.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .debug_selector(|| "new-tab-tree-document-name".to_string())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(file_name),
            );

        div()
            .debug_selector(|| "new-tab-tree-row-slot".to_string())
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .px_1()
            .py_0p5()
            .child(row)
            .into_any_element()
    }

    fn render_document_span(
        entry: &NewTabEntry,
        state: NewTabItemState,
        drag_ids: Vec<DocumentId>,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = entry.path.clone();
        let draggable = !drag_ids.is_empty();
        let dragged = DraggedLibrarySelection {
            active: LibraryNodeId::Item(entry.item_id.unwrap_or(DocumentId(0))),
            marked: drag_ids
                .into_iter()
                .map(LibraryNodeId::Item)
                .collect::<Vec<_>>()
                .into(),
            label: entry.file_name.clone(),
        };
        let mut span = div()
            .id(("new-tab-grouped-item", state.item_ix))
            .debug_selector(|| "new-tab-grouped-item".to_string())
            .h(px(40.0))
            .w_auto()
            .max_w_full()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if state.focused {
                theme.ring
            } else if state.selected {
                theme.primary
            } else {
                theme.border
            })
            .bg(if state.selected {
                theme.primary.opacity(0x18 as f32 / 255.0)
            } else {
                theme.popover
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.handle_document_pointer(
                        path.clone(),
                        event.modifiers,
                        event.click_count,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .when(draggable, |span| {
                span.on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            });
        if state.selecting {
            span = span.child(
                Checkbox::new(("new-tab-grouped-selection", state.item_ix))
                    .xsmall()
                    .checked(state.selected)
                    .tab_stop(false),
            );
        }
        span.child(
            Icon::new(entry.icon())
                .size(ui_rems(16.0))
                .text_color(if entry.missing {
                    theme.danger
                } else {
                    theme.muted_foreground
                }),
        )
        .child(div().min_w_0().truncate().child(entry.file_name.clone()))
        .into_any_element()
    }

    fn grouped_time_label(timestamp: Option<u64>, unknown: &'static str) -> String {
        let Some(timestamp) = timestamp
            .and_then(|timestamp| i64::try_from(timestamp).ok())
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
        else {
            return unknown.to_string();
        };
        let date = timestamp.with_timezone(&Local).date_naive();
        let today = Local::now().date_naive();
        if date == today {
            "Today".to_string()
        } else if date.iso_week() == today.iso_week() {
            "This Week".to_string()
        } else if date.year() == today.year() && date.month() == today.month() {
            "This Month".to_string()
        } else {
            "Earlier".to_string()
        }
    }

    fn grouped_alpha_label(value: &str, empty: &'static str) -> String {
        let value = value.trim();
        if value.is_empty() {
            return empty.to_string();
        }
        value
            .chars()
            .next()
            .filter(|character| character.is_alphabetic())
            .map(|character| character.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "#".to_string())
    }

    fn grouped_sections(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups = HashMap::<String, Vec<usize>>::new();
        for (item_ix, entry_ix) in self.filtered_entries.iter().copied().enumerate() {
            let entry = &self.entries[entry_ix];
            let label = match self.session.sort.field {
                LibrarySortField::DateAdded => {
                    Self::grouped_time_label(entry.date_added, "Unknown")
                }
                LibrarySortField::LastOpened => {
                    Self::grouped_time_label(entry.last_opened_timestamp, "Never Opened")
                }
                LibrarySortField::Year => entry
                    .year
                    .map(|year| year.to_string())
                    .unwrap_or_else(|| "Unknown".to_string()),
                LibrarySortField::Title => Self::grouped_alpha_label(&entry.title_sort, "#"),
                LibrarySortField::Creator => {
                    Self::grouped_alpha_label(&entry.creator_sort, "Unknown")
                }
            };
            groups.entry(label).or_default().push(item_ix);
        }

        let direction = self.session.sort.direction;
        let field = self.session.sort.field;
        let mut labels = groups.keys().cloned().collect::<Vec<_>>();
        labels.sort_by(|left, right| {
            let ordering = match field {
                LibrarySortField::DateAdded | LibrarySortField::LastOpened => {
                    let rank = |label: &str| match (direction, label) {
                        (_, "Never Opened" | "Unknown") => 4,
                        (LibrarySortDirection::Descending, "Today") => 0,
                        (LibrarySortDirection::Descending, "This Week") => 1,
                        (LibrarySortDirection::Descending, "This Month") => 2,
                        (LibrarySortDirection::Descending, "Earlier") => 3,
                        (LibrarySortDirection::Ascending, "Earlier") => 0,
                        (LibrarySortDirection::Ascending, "This Month") => 1,
                        (LibrarySortDirection::Ascending, "This Week") => 2,
                        (LibrarySortDirection::Ascending, "Today") => 3,
                        _ => 4,
                    };
                    rank(left).cmp(&rank(right))
                }
                LibrarySortField::Year => {
                    if left == "Unknown" {
                        std::cmp::Ordering::Greater
                    } else if right == "Unknown" {
                        std::cmp::Ordering::Less
                    } else {
                        let ordering = left
                            .parse::<i32>()
                            .unwrap_or_default()
                            .cmp(&right.parse::<i32>().unwrap_or_default());
                        if direction == LibrarySortDirection::Ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        }
                    }
                }
                LibrarySortField::Title | LibrarySortField::Creator => {
                    let left_last = left == "#" || left == "Unknown";
                    let right_last = right == "#" || right == "Unknown";
                    match (left_last, right_last) {
                        (true, false) => std::cmp::Ordering::Greater,
                        (false, true) => std::cmp::Ordering::Less,
                        _ => {
                            let ordering = left.cmp(right);
                            if direction == LibrarySortDirection::Ascending {
                                ordering
                            } else {
                                ordering.reverse()
                            }
                        }
                    }
                }
            };
            ordering.then_with(|| left.cmp(right))
        });
        labels
            .into_iter()
            .map(|label| {
                let items = groups.remove(&label).unwrap_or_default();
                (label, items)
            })
            .collect()
    }

    fn render_empty_state(
        &self,
        state: BrowserEmptyState,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let BrowserEmptyState {
            title,
            description,
            icon,
            action,
        } = state;
        let highlighted = matches!(action, BrowserEmptyAction::AddFiles(_))
            && self.library_drop_target == Some(LibraryDropTarget::Root);
        let mut empty = div()
            .id("new-tab-empty-state")
            .debug_selector(|| "new-tab-empty-state".to_string())
            .flex_1()
            .min_h(px(180.0))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .rounded(cx.theme().radius)
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .child(
                Icon::new(icon)
                    .size(ui_rems(28.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .text_size(ui_rems(15.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.foreground)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(460.0))
                    .text_center()
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .child(description),
            );

        match action {
            BrowserEmptyAction::None => {}
            BrowserEmptyAction::AddFiles(target_collection) => {
                empty = empty.child(
                    Button::new("library-empty-add-files")
                        .debug_selector(|| "library-empty-add-files".to_string())
                        .small()
                        .primary()
                        .icon(IconName::Plus)
                        .label(crate::fl!("new-tab-add-files"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_files_to(target_collection, cx);
                            cx.stop_propagation();
                        })),
                );
            }
            BrowserEmptyAction::OpenFile => {
                empty = empty.child(
                    Button::new("new-tab-empty-open-file")
                        .debug_selector(|| "new-tab-empty-open-file".to_string())
                        .small()
                        .primary()
                        .icon(IconName::FolderOpen)
                        .label(crate::fl!("new-tab-open-file"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_file(cx);
                            cx.stop_propagation();
                        })),
                );
            }
        }

        if let BrowserEmptyAction::AddFiles(target_collection) = action {
            empty = empty
                .on_drag_move::<ExternalPaths>(cx.listener(
                    |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        this.set_external_library_root_drag_target(
                            event.bounds.contains(&event.event.position),
                            cx,
                        );
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, target_collection, cx);
                    cx.stop_propagation();
                }))
                .on_mouse_exit(cx.listener(|this, _: &MouseExitEvent, _, cx| {
                    this.set_external_library_root_drag_target(false, cx);
                }));
        }

        empty.into_any_element()
    }

    fn render_thumbnail(
        thumbnail: ThumbnailState,
        theme: ThemeColor,
        page_color: u32,
    ) -> AnyElement {
        match thumbnail {
            ThumbnailState::Ready(thumbnail) => {
                let image = thumbnail.image.clone();
                canvas(
                    move |_, _, _| image.clone(),
                    move |bounds, image, window, _| {
                        window.paint_quad(fill(bounds, rgb(page_color)));
                        if let Err(error) =
                            window.paint_image(bounds, bounds, Corners::default(), image, 0, false)
                        {
                            eprintln!("failed to paint new tab preview: {error:#}");
                        }
                    },
                )
                .w(px(thumbnail.width))
                .h(px(thumbnail.height))
                .border_1()
                .border_color(theme.border)
                .into_any_element()
            }
            ThumbnailState::Loading => div()
                .flex()
                .items_center()
                .justify_center()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .border_1()
                .border_color(theme.border)
                .bg(rgb(page_color))
                .text_size(ui_rems(11.0))
                .text_color(theme.muted_foreground)
                .child(crate::fl!("new-tab-rendering-preview"))
                .into_any_element(),
            ThumbnailState::Failed => div()
                .w(px(THUMBNAIL_MAX_WIDTH))
                .h(px(THUMBNAIL_MAX_HEIGHT))
                .child(
                    Alert::error(
                        "new-tab-preview-unavailable",
                        crate::fl!("new-tab-preview-unavailable"),
                    )
                    .xsmall(),
                )
                .into_any_element(),
        }
    }

    fn render_grid(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let cols = self.cols.max(1);
        let item_count = self.item_count();
        let item_rows = item_count.div_ceil(cols);
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let grid_width = if self.browser_viewport_width > 0.0 {
            self.browser_viewport_width
        } else {
            self.viewport_width
        };
        let horizontal_gap = Self::horizontal_gap(grid_width, cols);
        let scroll = self.scroll.clone();
        let breadcrumb = self.render_collection_breadcrumb(cx.theme().colors, cx);
        let folders = self.visible_child_collections(cx);
        let has_folders = !folders.is_empty();
        let folder_grid = (!folders.is_empty()).then(|| {
            let mut grid = div()
                .id("library-folder-grid")
                .debug_selector(|| "library-folder-grid".to_string())
                .flex_none()
                .max_h(px(220.0))
                .overflow_y_scroll()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_3()
                .px(px(GRID_PADDING_X))
                .py_3();
            for (id, name) in folders {
                grid =
                    grid.child(self.render_collection_item(id, name, false, cx.theme().colors, cx));
            }
            grid.into_any_element()
        });

        if item_count == 0 {
            let show_empty = filtering || !has_folders || self.entries.is_empty();
            let mut content = div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(breadcrumb)
                .children(folder_grid);
            if show_empty {
                content = content.child(self.render_empty_state(
                    self.empty_state(filtering),
                    cx.theme().colors,
                    cx,
                ));
            }
            return content.into_any_element();
        }

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(breadcrumb)
            .children(folder_grid)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        uniform_list(
                            "new-tab-grid",
                            item_rows,
                            cx.processor(move |this, visible_range: Range<usize>, _window, cx| {
                                let page_color = cx.global::<SettingsStore>().resolved_theme.page;
                                let theme = cx.theme().colors;
                                let prewarm_range = visible_range.start.saturating_sub(1)
                                    ..(visible_range.end + 1).min(item_rows);
                                let visible_paths = prewarm_range
                                    .flat_map(|row_ix| {
                                        let first_item_ix = row_ix * cols;
                                        first_item_ix..first_item_ix + cols
                                    })
                                    .filter_map(|item_ix| {
                                        this.entry_for_item(item_ix).map(|entry| entry.path.clone())
                                    })
                                    .collect::<Vec<_>>();

                                for path in visible_paths {
                                    this.start_thumbnail_render(path, cx);
                                }

                                visible_range
                                    .map(|row_ix| {
                                        let mut row = div()
                                            .flex()
                                            .flex_row()
                                            .items_start()
                                            .justify_center()
                                            .gap(px(horizontal_gap))
                                            .w_full()
                                            .h(px(GRID_ROW_HEIGHT))
                                            .px(px(GRID_PADDING_X))
                                            .py_2();

                                        for col_ix in 0..cols {
                                            let item_ix = row_ix * cols + col_ix;
                                            if item_ix < item_count {
                                                let entry = this
                                                    .entry_for_item(item_ix)
                                                    .expect("document item must have an entry")
                                                    .clone();
                                                let thumbnail = this
                                                    .thumbnails
                                                    .get(&entry.path)
                                                    .cloned()
                                                    .unwrap_or(ThumbnailState::Loading);
                                                row = row.child(Self::render_document_card(
                                                    &entry,
                                                    NewTabCardState {
                                                        thumbnail,
                                                        item: this.item_state(item_ix),
                                                    },
                                                    this.drag_ids_for_entry(&entry),
                                                    theme,
                                                    page_color,
                                                    cx,
                                                ));
                                            } else {
                                                row = row.child(
                                                    div()
                                                        .flex_none()
                                                        .w(px(CARD_WIDTH))
                                                        .h(px(CARD_HEIGHT)),
                                                );
                                            }
                                        }

                                        row.into_any_element()
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    fn render_list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let sections = self.grouped_sections();
        let theme = cx.theme().colors;
        let _ = window;
        let breadcrumb = self.render_collection_breadcrumb(theme, cx);
        let folders = self.visible_child_collections(cx);
        let sections_empty = sections.is_empty();
        let show_empty_below_folders = sections_empty && self.entries.is_empty();

        if sections_empty && folders.is_empty() {
            return div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .children(breadcrumb)
                .child(self.render_empty_state(self.empty_state(filtering), theme, cx))
                .into_any_element();
        }

        let mut content = div()
            .id("new-tab-grouped-list")
            .debug_selector(|| "new-tab-grouped-list".to_string())
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_4();
        if !folders.is_empty() {
            let mut folder_items = div()
                .id("library-folder-spans")
                .debug_selector(|| "library-folder-spans".to_string())
                .w_full()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_2();
            for (id, name) in folders {
                folder_items =
                    folder_items.child(self.render_collection_item(id, name, true, theme, cx));
            }
            content = content.child(folder_items);
        }
        if show_empty_below_folders {
            content = content.child(self.render_empty_state(self.empty_state(false), theme, cx));
        }
        for (section_ix, (label, item_indices)) in sections.into_iter().enumerate() {
            let mut items = div()
                .id(("new-tab-grouped-section-items", section_ix))
                .w_full()
                .flex()
                .flex_wrap()
                .items_start()
                .gap_2();
            for item_ix in item_indices {
                let entry = self
                    .entry_for_item(item_ix)
                    .expect("grouped document item must have an entry")
                    .clone();
                items = items.child(Self::render_document_span(
                    &entry,
                    self.item_state(item_ix),
                    self.drag_ids_for_entry(&entry),
                    theme,
                    cx,
                ));
            }
            content = content.child(
                div()
                    .id(("new-tab-grouped-section", section_ix))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(items),
            );
        }

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .children(breadcrumb)
            .child(content)
            .into_any_element()
    }

    fn render_library_tree_document_row(
        &self,
        entry_ix: usize,
        depth: usize,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(entry) = self.entries.get(entry_ix) else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let Some(item_id) = entry.item_id else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let node = LibraryNodeId::Item(item_id);
        let label = entry.file_name.clone();
        let icon = entry.icon();
        let selected = self.tree_active == Some(node) || self.tree_marked.contains(&node);
        let drop_highlighted = self.library_node_is_drop_highlighted(node, cx);
        let drag_dimmed = cx.has_active_drag() && self.dragging_library_nodes.contains(&node);
        let dragged = self.dragged_library_selection(node, label.clone());
        let mut row = div()
            .id(("library-document-tree-row", item_id.0 as usize))
            .debug_selector(move || format!("library-document-tree-row-{}", item_id.0))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .pl(px(34.0 + depth as f32 * 20.0))
            .border_b(px(1.0))
            .border_color(theme.border.opacity(0x70 as f32 / 255.0))
            .opacity(if drag_dimmed { 0.45 } else { 1.0 })
            .bg(if drop_highlighted {
                theme.drop_target
            } else if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                this.handle_tree_pointer(node, event.modifiers(), event.click_count(), cx);
                cx.stop_propagation();
            }));
        if self.host == LibraryBrowserHost::Home {
            let view = cx.entity();
            row = row
                .on_drag(dragged, move |dragged, _, _, cx| {
                    view.update(cx, |this, cx| this.begin_library_drag(dragged, cx));
                    cx.new(|_| dragged.clone())
                })
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_drag_target(node, &dragged, active, window, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        let parent = cx.global::<LibraryStore>().item_parent(item_id);
                        this.drop_library_selection(dragged, parent, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_drag_target(node, active, window, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    let parent = cx.global::<LibraryStore>().item_parent(item_id);
                    this.drop_external_library_paths(paths, parent, cx);
                    cx.stop_propagation();
                }));
        }
        row.child(
            Icon::new(icon)
                .size(ui_rems(16.0))
                .text_color(theme.muted_foreground),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label),
        )
        .into_any_element()
    }

    fn render_collection_tree_row(
        &self,
        id: LibraryCollectionId,
        depth: usize,
        theme: ThemeColor,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let management = self.host == LibraryBrowserHost::Home;
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return div().h(px(LIST_ROW_HEIGHT)).into_any_element();
        };
        let name = collection.name.clone();
        let expanded = self.expanded_collections.contains(&id);
        let node = LibraryNodeId::Collection(id);
        let selected = self.tree_active == Some(node) || self.tree_marked.contains(&node);
        let drop_highlighted = self.library_node_is_drop_highlighted(node, cx);
        let drag_dimmed = cx.has_active_drag() && self.dragging_library_nodes.contains(&node);
        let mut row = div()
            .id(("library-folder-tree-row", id.0 as usize))
            .debug_selector(move || format!("library-folder-tree-row-{}", id.0))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .pr_3()
            .pl(px(14.0 + depth as f32 * 20.0))
            .border_b(px(1.0))
            .border_color(theme.border.opacity(0x70 as f32 / 255.0))
            .opacity(if drag_dimmed { 0.45 } else { 1.0 })
            .bg(if drop_highlighted {
                theme.drop_target
            } else if selected {
                theme.list_active
            } else {
                transparent_black()
            })
            .hover(move |style| style.bg(theme.list_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                if !event.standard_click() {
                    return;
                }
                this.handle_tree_pointer(node, event.modifiers(), event.click_count(), cx);
                cx.stop_propagation();
            }));
        if management {
            let dragged = self.dragged_library_selection(node, name.clone().into());
            let view = cx.entity();
            row = row
                .on_drag(dragged, move |dragged, _, _, cx| {
                    view.update(cx, |this, cx| this.begin_library_drag(dragged, cx));
                    cx.new(|_| dragged.clone())
                })
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_drag_target(node, &dragged, active, window, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, Some(id), cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, window, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_drag_target(node, active, window, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, Some(id), cx);
                    cx.stop_propagation();
                }));
        }

        row = row
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(ui_rems(12.0))
                .text_color(theme.muted_foreground),
            )
            .child(
                Icon::new(IconName::FolderOpen)
                    .size(ui_rems(16.0))
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            );
        if management {
            let view = cx.entity();
            return row
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, Some(id), view.clone())
                })
                .into_any_element();
        }
        row.into_any_element()
    }

    fn render_tree_root_area(
        &self,
        theme: ThemeColor,
        empty_state: Option<BrowserEmptyState>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = self.library_drop_target == Some(LibraryDropTarget::Root);
        let mut area = div()
            .id("library-tree-root-area")
            .debug_selector(|| "library-tree-root-area".to_string())
            .flex_grow_1()
            .min_h_0()
            .w_full()
            .bg(if highlighted {
                theme.drop_target
            } else {
                transparent_black()
            })
            .children(empty_state.map(|state| self.render_empty_state(state, theme, cx)));
        if self.host == LibraryBrowserHost::Home {
            area = area
                .on_drag_move::<DraggedLibrarySelection>(cx.listener(
                    move |this, event: &DragMoveEvent<DraggedLibrarySelection>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        let dragged = event.drag(cx).clone();
                        this.set_library_root_drag_target(&dragged, active, cx);
                    },
                ))
                .on_drop(
                    cx.listener(move |this, dragged: &DraggedLibrarySelection, _, cx| {
                        this.drop_library_selection(dragged, None, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_drag_move::<ExternalPaths>(cx.listener(
                    move |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let active = event.bounds.contains(&event.event.position);
                        this.set_external_library_root_drag_target(active, cx);
                    },
                ))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                    this.drop_external_library_paths(paths, None, cx);
                    cx.stop_propagation();
                }));
            let view = cx.entity();
            return area
                .context_menu(move |menu, _, _| {
                    Self::collection_context_menu(menu, None, view.clone())
                })
                .into_any_element();
        }
        area.into_any_element()
    }

    fn render_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !cx.has_active_drag() {
            self.dragging_library_nodes.clear();
        }
        let filtering = !self.search_input.read(cx).value().trim().is_empty();
        let tree_mode = self.session.destination == ReaderNewTabDestination::Library;
        let item_count = if tree_mode {
            self.library_tree_rows.len()
        } else {
            self.item_count()
        };
        if !tree_mode && item_count == 0 {
            return div()
                .id("new-tab-content")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(self.render_empty_state(self.empty_state(filtering), cx.theme().colors, cx))
                .into_any_element();
        }
        let root_empty_state = tree_mode
            .then(|| {
                if filtering && item_count == 0 {
                    Some(self.empty_state(true))
                } else if !filtering && self.entries.is_empty() {
                    Some(self.empty_state(false))
                } else {
                    None
                }
            })
            .flatten();
        let scroll = self.scroll.clone();
        let list = uniform_list(
            "new-tab-tree-list",
            item_count,
            cx.processor(move |this, visible_range: Range<usize>, _, cx| {
                let theme = cx.theme().colors;
                visible_range
                    .map(|item_ix| {
                        if tree_mode {
                            match this.library_tree_rows[item_ix].clone() {
                                LibraryTreeRow::Collection { id, depth } => {
                                    this.render_collection_tree_row(id, depth, theme, cx)
                                }
                                LibraryTreeRow::Document { entry_ix, depth } => this
                                    .render_library_tree_document_row(entry_ix, depth, theme, cx),
                            }
                        } else {
                            let entry = this
                                .entry_for_item(item_ix)
                                .expect("document item must have an entry")
                                .clone();
                            Self::render_tree_document_row(
                                &entry,
                                this.item_state(item_ix),
                                this.drag_ids_for_entry(&entry),
                                theme,
                                cx,
                            )
                        }
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&scroll)
        .when(tree_mode, |list| {
            list.with_sizing_behavior(ListSizingBehavior::Infer)
        })
        .when(!tree_mode, |list| list.size_full());
        let root_area =
            tree_mode.then(|| self.render_tree_root_area(cx.theme().colors, root_empty_state, cx));

        div()
            .id("new-tab-content")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("new-tab-tree-viewport")
                    .debug_selector(|| "new-tab-tree-viewport".to_string())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .when(tree_mode && self.host == LibraryBrowserHost::Home, |view| {
                        view.on_drag_move::<DraggedLibrarySelection>(
                            cx.listener(Self::handle_library_drag_move::<DraggedLibrarySelection>),
                        )
                        .on_drag_move::<ExternalPaths>(
                            cx.listener(Self::handle_library_drag_move::<ExternalPaths>),
                        )
                        .on_mouse_exit(cx.listener(
                            |this, _: &MouseExitEvent, _, cx| this.clear_library_drop_feedback(cx),
                        ))
                    })
                    .child(list)
                    .children(root_area)
                    .vertical_scrollbar_for_with_id(&scroll, "new-tab-scrollbar", window, cx),
            )
            .into_any_element()
    }

    fn open_file(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenFile);
    }

    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenDocument(path));
    }
}

impl NewTabEntry {
    fn from_history(entry: &HistoryEntry) -> Self {
        let file_name: SharedString = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();
        let directory = entry
            .path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into();
        let format = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension);
        let format_label = match format {
            Some(DocumentFormat::Pdf) => "PDF",
            Some(DocumentFormat::Epub) => "EPUB",
            None => "—",
        };
        let file_size = fs::metadata(&entry.path)
            .ok()
            .map(|metadata| format_file_size(metadata.len()))
            .unwrap_or_else(|| "—".to_string());
        let format_size = format!("{format_label} · {file_size}").into();
        let last_opened = i64::try_from(entry.opened)
            .ok()
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
            .map(|timestamp| {
                timestamp
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "—".to_string())
            .into();
        let (last_read, progress) = entry
            .position
            .semantic
            .as_ref()
            .map(|position| {
                let progress = (position.progress.clamp(0.0, 1.0) * 100.0).round() as i64;
                (
                    crate::fl!("new-tab-last-read-progress", progress = progress).into(),
                    format!("{progress}%").into(),
                )
            })
            .unwrap_or_else(|| {
                (
                    crate::fl!("new-tab-last-read", page = entry.position.page as i64).into(),
                    crate::fl!("new-tab-progress-page", page = entry.position.page as i64).into(),
                )
            });

        Self {
            item_id: None,
            path: entry.path.clone(),
            file_name: file_name.clone(),
            directory,
            last_read,
            progress,
            format_size,
            last_opened,
            format,
            favorite: false,
            status: None,
            missing: !entry.path.exists(),
            title_sort: file_name.to_lowercase().into(),
            creator_sort: "".into(),
            year: None,
            date_added: None,
            last_opened_timestamp: Some(entry.opened),
            search_text: String::new().into(),
        }
    }

    fn from_history_and_item(entry: &HistoryEntry, item: Option<&LibraryItem>) -> Self {
        let mut result = Self::from_history(entry);
        if let Some(item) = item {
            result.apply_library_item(item, &[]);
        }
        result
    }

    fn from_library(store: &LibraryStore, item_id: DocumentId) -> Option<Self> {
        let item = store.library_item(item_id)?;
        let attachment = store.primary_attachment(item_id)?;
        let history = store
            .history_entries()
            .iter()
            .find(|history| history.path == attachment.path);
        let mut result = if let Some(history) = history {
            Self::from_history(history)
        } else {
            let format_label = match attachment.format {
                DocumentFormat::Pdf => "PDF",
                DocumentFormat::Epub => "EPUB",
            };
            Self {
                item_id: None,
                path: attachment.path.clone(),
                file_name: item.title.clone().into(),
                directory: attachment
                    .path
                    .parent()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
                    .into(),
                last_read: "Not opened yet".into(),
                progress: "—".into(),
                format_size: format!(
                    "{format_label} · {}",
                    format_file_size(attachment.file_size)
                )
                .into(),
                last_opened: "—".into(),
                format: Some(attachment.format),
                favorite: false,
                status: None,
                missing: attachment.is_missing(),
                title_sort: item.title.to_lowercase().into(),
                creator_sort: "".into(),
                year: item.year,
                date_added: Some(item.created),
                last_opened_timestamp: None,
                search_text: String::new().into(),
            }
        };
        let tags = store
            .tags_for_item(item_id)
            .into_iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        result.apply_library_item(item, &tags);
        let collection_paths = store
            .collections_for_item(item_id)
            .into_iter()
            .map(|collection| store.collection_path_label(collection.id))
            .collect::<Vec<_>>();
        if !collection_paths.is_empty() {
            let collection_paths = collection_paths.join(" · ");
            let secondary = result.directory.to_string();
            result.directory = if secondary.is_empty() {
                collection_paths.clone().into()
            } else {
                format!("{secondary} · {collection_paths}").into()
            };
            result.search_text = format!("{} {collection_paths}", result.search_text).into();
        }
        Some(result)
    }

    fn apply_library_item(&mut self, item: &LibraryItem, tags: &[String]) {
        self.item_id = Some(item.id);
        self.file_name = item.title.clone().into();
        self.title_sort = item.title.to_lowercase().into();
        let creators = item
            .creators
            .iter()
            .map(|creator| creator.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !creators.is_empty() {
            self.directory = creators.clone().into();
        }
        self.creator_sort = creators.to_lowercase().into();
        self.year = item.year;
        self.date_added = Some(item.created);
        self.favorite = item.favorite;
        self.status = Some(item.status);
        self.last_read = match item.status {
            ReadingStatus::ToRead => "To Read".into(),
            ReadingStatus::Reading => "Reading".into(),
            ReadingStatus::Read => "Read".into(),
        };
        self.search_text = [
            creators,
            item.year.map(|year| year.to_string()).unwrap_or_default(),
            item.publication.clone(),
            item.publisher.clone(),
            item.abstract_text.clone(),
            item.subject.clone(),
            item.language.clone(),
            item.identifiers.doi.clone().unwrap_or_default(),
            item.identifiers.isbn.clone().unwrap_or_default(),
            item.identifiers.arxiv.clone().unwrap_or_default(),
            item.embedded_keywords.join(" "),
            tags.join(" "),
        ]
        .join(" ")
        .into();
    }
}

fn format_file_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

impl EventEmitter<NewTabEvent> for NewTab {}

impl Render for NewTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if cx.global::<LibraryStore>().catalog_revision() != self.observed_catalog_revision {
            self.refresh_entries(cx);
        }
        self.sync_work_note_input(window, cx);
        let theme = cx.theme().colors;
        let view_mode = self.view_mode(cx);
        let measure_view = cx.entity();

        let measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    let width = bounds.size.width.as_f32();
                    let height = bounds.size.height.as_f32();
                    measure_view.update(cx, |this, cx| this.update_viewport(width, height, cx));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let content = match &self.session.destination {
            ReaderNewTabDestination::Tags => self.render_tags_content(theme, cx),
            _ => match view_mode {
                NewTabViewMode::Grid => self.render_grid(window, cx),
                NewTabViewMode::Grouped => self.render_list(window, cx),
                NewTabViewMode::Table => self.render_tree(window, cx),
            },
        };
        let management_host = self.host == LibraryBrowserHost::Home;
        let wide_inspector = management_host && self.viewport_width >= 960.0;
        let inspector = (wide_inspector && self.inspector_open)
            .then(|| self.render_inspector(theme, cx))
            .flatten();
        let inspector_overlay = (management_host && !wide_inspector && self.inspector_open)
            .then(|| self.render_inspector(theme, cx))
            .flatten()
            .map(|inspector| {
                div()
                    .id("new-tab-item-inspector-overlay")
                    .absolute()
                    .top(px(NEW_TAB_HEADER_HEIGHT))
                    .right_0()
                    .bottom_0()
                    .w(px(NEW_TAB_INSPECTOR_WIDTH))
                    .shadow_lg()
                    .child(inspector)
                    .into_any_element()
            });
        let browser_measure = canvas(
            {
                let measure_view = measure_view.clone();
                move |bounds, _window, cx| {
                    measure_view.update(cx, |this, cx| {
                        this.update_browser_viewport(bounds.size.width.as_f32(), cx);
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let browser_content = div()
            .id("new-tab-browser-viewport")
            .debug_selector(|| "new-tab-browser-viewport".to_string())
            .relative()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(browser_measure)
            .child(content);
        let content = div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .child(browser_content)
            .children(inspector);

        let tree_selection = self.effective_tree_selection();
        let selecting = if self.is_library_tree() {
            self.selecting || !self.tree_marked.is_empty()
        } else {
            self.selecting
        };
        let has_selection = if self.is_library_tree() {
            !tree_selection.is_empty()
        } else {
            !self.selection.is_empty()
        };
        let selected_count = if self.is_library_tree() {
            tree_selection.len() as i64
        } else {
            self.selection.len() as i64
        };
        let (filtered_count, total_count) =
            if self.session.destination == ReaderNewTabDestination::Tags {
                self.tag_result_counts(cx)
            } else {
                (
                    self.filtered_entries.len() as i64,
                    self.entries.len() as i64,
                )
            };
        let library_error = cx
            .global::<LibraryStore>()
            .startup_error()
            .map(|error| format!("Library unavailable. Changes will not be saved: {error}"));
        let import_progress = self.import_progress.clone();
        let import_message = self.import_message.clone();
        let destination_title =
            Self::destination_label(&self.session.destination, cx.global::<LibraryStore>());
        let in_trash = matches!(self.session.destination, ReaderNewTabDestination::Trash);
        let in_history = matches!(self.session.destination, ReaderNewTabDestination::History);
        // Retain the persisted advanced presentation state for compatibility,
        // but keep its controls out of the current clean browser UI.
        let can_save_view = false;
        let is_saved_view = false;
        let can_sort = false;
        let library_management = false;
        let sort_label = Self::sort_field_label(self.session.sort.field);
        let sort_ascending = self.session.sort.direction == LibrarySortDirection::Ascending;
        let kind_filter_label = "All Types";
        let status_filter_label = "All Statuses";
        let header =
            div()
                .flex()
                .flex_col()
                .flex_none()
                .h(px(NEW_TAB_HEADER_HEIGHT))
                .gap_1()
                .px_4()
                .py_2()
                .border_b(px(1.0))
                .border_color(theme.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(ui_rems(14.0))
                                .child(if management_host {
                                    self.render_destination_heading(destination_title, theme, cx)
                                } else {
                                    self.render_quick_source_selector(theme, cx)
                                })
                                .child(
                                    div()
                                        .text_size(ui_rems(11.0))
                                        .text_color(theme.muted_foreground)
                                        .child(crate::fl!(
                                            "new-tab-results-count",
                                            visible = filtered_count,
                                            total = total_count
                                        )),
                                )
                                .when(selecting, |this| {
                                    this.child(
                                        div()
                                            .text_size(ui_rems(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(crate::fl!(
                                                "selected-count",
                                                count = selected_count
                                            )),
                                    )
                                }),
                        )
                        .child({
                            let mut actions = div().flex().items_center().gap_1();
                            if let Some(progress) = import_progress {
                                let label = if progress.total == 0 {
                                    "Scanning…".to_string()
                                } else {
                                    format!(
                                        "Importing {} of {}",
                                        progress.completed.min(progress.total),
                                        progress.total
                                    )
                                };
                                actions = actions
                                    .child(
                                        div()
                                            .text_size(ui_rems(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(label),
                                    )
                                    .child(
                                        Button::new("new-tab-cancel-import")
                                            .xsmall()
                                            .outline()
                                            .label("Cancel")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.cancel_library_import(cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                            }
                            actions = actions.child(
                                Button::new("new-tab-open-file")
                                    .debug_selector(|| "new-tab-open-file".to_string())
                                    .small()
                                    .outline()
                                    .icon(IconName::FolderOpen)
                                    .label(crate::fl!("new-tab-open-file"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.open_file(cx);
                                        cx.stop_propagation();
                                    })),
                            );
                            if management_host {
                                actions = actions.child(
                                    Button::new("new-tab-add-files")
                                        .debug_selector(|| "new-tab-add-files".to_string())
                                        .small()
                                        .primary()
                                        .icon(IconName::Plus)
                                        .label(crate::fl!("new-tab-add-files"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.add_files(cx);
                                            cx.stop_propagation();
                                        })),
                                );
                            }
                            if management_host
                                && self.session.destination == ReaderNewTabDestination::Library
                            {
                                let parent_id = if view_mode == NewTabViewMode::Table {
                                    self.library_import_target(cx)
                                } else {
                                    self.current_collection
                                };
                                actions = actions.child(
                                    Button::new("home-library-new-folder")
                                        .debug_selector(|| "home-library-new-folder".to_string())
                                        .small()
                                        .outline()
                                        .icon(IconName::Plus)
                                        .label("New Folder")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.open_new_folder_editor(parent_id, window, cx);
                                            cx.stop_propagation();
                                        })),
                                );
                            }
                            actions = actions
                                .child(Self::render_view_button(
                                    NewTabViewMode::Grid,
                                    view_mode,
                                    cx,
                                ))
                                .child(Self::render_view_button(
                                    NewTabViewMode::Table,
                                    view_mode,
                                    cx,
                                ))
                                .child(Self::render_view_button(
                                    NewTabViewMode::Grouped,
                                    view_mode,
                                    cx,
                                ));
                            if management_host && has_selection && self.viewport_width < 960.0 {
                                actions = actions.child(Self::render_toolbar_button(
                                    "Details".to_string(),
                                    |this, _, cx| {
                                        this.inspector_open = true;
                                        cx.notify();
                                    },
                                    cx,
                                ));
                            }
                            if management_host && selecting && has_selection {
                                if in_trash {
                                    actions = actions.child(Self::render_toolbar_button(
                                        "Restore".to_string(),
                                        |this, _, cx| this.restore_selected_items(cx),
                                        cx,
                                    ));
                                }
                                actions = actions
                                    .child(Self::render_toolbar_button(
                                        if in_trash {
                                            "Delete Permanently".to_string()
                                        } else if in_history {
                                            crate::fl!("new-tab-remove-from-recents")
                                        } else {
                                            "Move to Trash".to_string()
                                        },
                                        |this, window, cx| this.delete_selected(window, cx),
                                        cx,
                                    ))
                                    .child(Self::render_toolbar_button(
                                        crate::fl!("cancel-selection"),
                                        |this, _, cx| this.cancel_select(cx),
                                        cx,
                                    ));
                            } else if management_host && selecting {
                                actions = actions.child(Self::render_toolbar_button(
                                    crate::fl!("cancel"),
                                    |this, _, cx| this.cancel_select(cx),
                                    cx,
                                ));
                            } else if management_host {
                                actions = actions.child(Self::render_toolbar_button(
                                    crate::fl!("select"),
                                    |this, _, cx| this.enter_select(cx),
                                    cx,
                                ));
                            }
                            actions
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id("new-tab-search")
                                .debug_selector(|| "new-tab-search".to_string())
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .h(px(30.0))
                                .items_center()
                                .gap_1()
                                .px_2()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(theme.border)
                                .bg(theme.background)
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(ui_rems(14.0))
                                        .text_color(theme.muted_foreground),
                                )
                                .child(div().flex_1().min_w_0().child(
                                    Input::new(&self.search_input).small().appearance(false),
                                )),
                        )
                        .when(false, |search_row| {
                            search_row.child({
                                let mut toolbar = div().flex().flex_none().items_center().gap_1();
                                if library_management {
                                    toolbar = toolbar
                                        .child(self.render_library_filter_menu(theme, cx))
                                        .child(
                                            Button::new("home-library-kind-filter")
                                                .xsmall()
                                                .outline()
                                                .label(kind_filter_label)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_kind_filter(cx);
                                                    cx.stop_propagation();
                                                })),
                                        )
                                        .child(
                                            Button::new("home-library-status-filter")
                                                .xsmall()
                                                .outline()
                                                .label(status_filter_label)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_status_filter(cx);
                                                    cx.stop_propagation();
                                                })),
                                        );
                                }
                                if can_sort {
                                    toolbar = toolbar
                                        .child(
                                            Button::new("new-tab-sort-field")
                                                .xsmall()
                                                .outline()
                                                .label(sort_label)
                                                .tooltip("Change sort field")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cycle_sort_field(cx);
                                                    cx.stop_propagation();
                                                })),
                                        )
                                        .child(
                                            Button::new("new-tab-sort-direction")
                                                .xsmall()
                                                .ghost()
                                                .icon(if sort_ascending {
                                                    IconName::ChevronUp
                                                } else {
                                                    IconName::ChevronDown
                                                })
                                                .tooltip(if sort_ascending {
                                                    "Ascending"
                                                } else {
                                                    "Descending"
                                                })
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle_sort_direction(cx);
                                                    cx.stop_propagation();
                                                })),
                                        );
                                }
                                toolbar = toolbar
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Grid,
                                        view_mode,
                                        cx,
                                    ))
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Table,
                                        view_mode,
                                        cx,
                                    ))
                                    .child(Self::render_view_button(
                                        NewTabViewMode::Grouped,
                                        view_mode,
                                        cx,
                                    ));
                                if can_save_view {
                                    toolbar = toolbar.child(
                                        Button::new("new-tab-save-current-view")
                                            .xsmall()
                                            .ghost()
                                            .icon(IconName::Table2)
                                            .tooltip("Save current view")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.open_saved_view_editor(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                                }
                                if is_saved_view {
                                    toolbar = toolbar.child(
                                        Button::new("new-tab-delete-current-view")
                                            .xsmall()
                                            .ghost()
                                            .icon(IconName::Trash2)
                                            .tooltip("Delete saved view")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.delete_current_saved_view(window, cx);
                                                cx.stop_propagation();
                                            })),
                                    );
                                }
                                toolbar = toolbar
                                    .child(div().mx_1().h(px(18.0)).w(px(1.0)).bg(theme.border));
                                if management_host && has_selection && self.viewport_width < 960.0 {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        "Details".to_string(),
                                        |this, _, cx| {
                                            this.inspector_open = true;
                                            cx.notify();
                                        },
                                        cx,
                                    ));
                                }
                                if management_host && selecting && has_selection {
                                    if in_trash {
                                        toolbar = toolbar.child(Self::render_toolbar_button(
                                            "Restore".to_string(),
                                            |this, _, cx| this.restore_selected_items(cx),
                                            cx,
                                        ));
                                    }
                                    toolbar = toolbar
                                        .child(Self::render_toolbar_button(
                                            if in_trash {
                                                "Delete Permanently".to_string()
                                            } else if in_history {
                                                crate::fl!("new-tab-remove-from-recents")
                                            } else {
                                                "Move to Trash".to_string()
                                            },
                                            |this, window, cx| this.delete_selected(window, cx),
                                            cx,
                                        ))
                                        .child(Self::render_toolbar_button(
                                            crate::fl!("cancel-selection"),
                                            |this, _, cx| this.cancel_select(cx),
                                            cx,
                                        ));
                                } else if management_host && selecting {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        crate::fl!("cancel"),
                                        |this, _, cx| this.cancel_select(cx),
                                        cx,
                                    ));
                                } else if management_host {
                                    toolbar = toolbar.child(Self::render_toolbar_button(
                                        crate::fl!("select"),
                                        |this, _, cx| this.enter_select(cx),
                                        cx,
                                    ));
                                }
                                toolbar
                            })
                        }),
                );

        let banners = library_error
            .map(SharedString::from)
            .into_iter()
            .chain(import_message)
            .map(|message| {
                div()
                    .mx_3()
                    .mt_3()
                    .p_2()
                    .rounded(px(4.0))
                    .bg(theme.list_hover)
                    .text_color(theme.primary)
                    .child(message)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let mut main_column = div().flex_1().min_w_0().min_h_0().flex().flex_col();
        main_column = main_column.children(banners).child(header).child(content);
        let workspace_content = if management_host {
            div()
                .size_full()
                .min_w_0()
                .min_h_0()
                .flex()
                .gap(px(SURFACE_GAP))
                .px(px(SURFACE_INSET))
                .pb(px(SURFACE_INSET))
                .child(self.render_navigation(theme, cx))
                .child(main_column)
                .into_any_element()
        } else {
            main_column.into_any_element()
        };
        let note_overlay =
            (management_host && self.note_expanded).then(|| self.render_note_overlay(theme, cx));
        let metadata_overlay = (management_host && self.metadata_editor_open)
            .then(|| self.render_metadata_overlay(theme, cx));
        let saved_view_overlay = (management_host && self.saved_view_editor_open)
            .then(|| self.render_saved_view_overlay(theme, cx));
        let tag_editor_overlay = (management_host && self.tag_editor_id.is_some())
            .then(|| self.render_tag_editor_overlay(theme, cx));
        let folder_editor_overlay = management_host
            .then(|| self.render_folder_editor_overlay(theme, cx))
            .flatten();
        let editor_overlay_open = management_host
            && (self.note_expanded
                || self.metadata_editor_open
                || self.saved_view_editor_open
                || self.tag_editor_id.is_some()
                || self.folder_editor.is_some());

        div()
            .id(if management_host {
                "home-view"
            } else {
                "new-tab"
            })
            .debug_selector(move || {
                if management_host {
                    "home-view".to_string()
                } else {
                    "new-tab".to_string()
                }
            })
            .relative()
            .size_full()
            .key_context(if editor_overlay_open {
                "NewTab Dialog"
            } else {
                "NewTab"
            })
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &CancelDialog, window, cx| {
                if this.cancel_topmost_editor(window, cx) {
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                let mode = this.view_mode(cx);
                this.scroll_by(-px(Self::row_height(mode)), cx);
            }))
            .on_action(cx.listener(|this, _: &NextHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(viewport_height * 0.5, cx);
            }))
            .on_action(cx.listener(|this, _: &PreviousHalfPage, _, cx| {
                let viewport_height = this.scroll.0.borrow().base_handle.bounds().size.height;
                this.scroll_by(-(viewport_height * 0.5), cx);
            }))
            .on_action(cx.listener(|this, _: &EnterSelect, _, cx| {
                this.enter_select(cx);
            }))
            .on_action(cx.listener(|this, _: &CancelSelect, _, cx| {
                this.cancel_select(cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| {
                this.delete_selected(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorUp, _, cx| {
                this.move_cursor(CursorMove::Up, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorDown, _, cx| {
                this.move_cursor(CursorMove::Down, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLeft, _, cx| {
                this.move_cursor(CursorMove::Left, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorRight, _, cx| {
                this.move_cursor(CursorMove::Right, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorFirst, _, cx| {
                this.move_cursor(CursorMove::First, false, cx);
            }))
            .on_action(cx.listener(|this, _: &CursorLast, _, cx| {
                this.move_cursor(CursorMove::Last, false, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionUp, _, cx| {
                this.move_cursor(CursorMove::Up, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionDown, _, cx| {
                this.move_cursor(CursorMove::Down, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLeft, _, cx| {
                this.move_cursor(CursorMove::Left, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionRight, _, cx| {
                this.move_cursor(CursorMove::Right, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionFirst, _, cx| {
                this.move_cursor(CursorMove::First, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ExtendSelectionLast, _, cx| {
                this.move_cursor(CursorMove::Last, true, cx);
            }))
            .on_action(cx.listener(|this, _: &ActivateFocused, _, cx| {
                this.activate_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFocused, _, cx| {
                this.toggle_focused(cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.select_all(cx);
            }))
            .flex()
            .flex_col()
            .bg(theme.tab_active)
            .text_size(ui_rems(12.0))
            .text_color(theme.foreground)
            .child(measure)
            .child(workspace_content)
            .children(inspector_overlay)
            .children(note_overlay)
            .children(metadata_overlay)
            .children(saved_view_overlay)
            .children(tag_editor_overlay)
            .children(folder_editor_overlay)
    }
}

fn extract_library_metadata(path: &Path) -> Result<LibraryItemMetadata> {
    let mut metadata = LibraryItemMetadata::for_path(path);
    let document = match mupdf::Document::open(path) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "failed to extract embedded metadata from {}; using the filename instead: {error:#}",
                path.display()
            );
            return Ok(metadata);
        }
    };
    let read = |name| {
        document
            .metadata(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(title) = read(mupdf::MetadataName::Title) {
        metadata.title = title;
    }
    if let Some(authors) = read(mupdf::MetadataName::Author) {
        metadata.creators = authors
            .split([';', '\n'])
            .map(str::trim)
            .filter(|author| !author.is_empty())
            .map(|name| LibraryCreator {
                name: name.to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
    }
    metadata.subject = read(mupdf::MetadataName::Subject).unwrap_or_default();
    if let Some(keywords) = read(mupdf::MetadataName::Keywords) {
        metadata.embedded_keywords = keywords
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|keyword| !keyword.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Some(date) = read(mupdf::MetadataName::CreationDate) {
        metadata.year = date
            .as_bytes()
            .windows(4)
            .filter_map(|digits| std::str::from_utf8(digits).ok())
            .filter_map(|digits| digits.parse::<i32>().ok())
            .find(|year| (1000..=3000).contains(year));
    }
    Ok(metadata)
}

fn scan_library_import_paths(paths: Vec<PathBuf>) -> LibraryImportScan {
    fn visit(path: PathBuf, logical_path: Vec<String>, scan: &mut LibraryImportScan) {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            scan.failed += 1;
            return;
        };
        if metadata.file_type().is_symlink() {
            scan.skipped += 1;
            return;
        }
        if metadata.is_file() {
            let supported = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(DocumentFormat::from_extension)
                .is_some();
            if supported {
                scan.candidates.push(LibraryImportCandidate {
                    path,
                    folder_path: logical_path,
                });
            } else {
                scan.skipped += 1;
            }
            return;
        }
        if !metadata.is_dir() {
            scan.skipped += 1;
            return;
        }

        scan.folders.push(logical_path.clone());
        let Ok(entries) = fs::read_dir(&path) else {
            scan.failed += 1;
            return;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
        for entry in entries {
            let child_path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                scan.failed += 1;
                continue;
            };
            if file_type.is_symlink() {
                scan.skipped += 1;
                continue;
            }
            if file_type.is_dir() {
                let mut child_logical_path = logical_path.clone();
                child_logical_path.push(entry.file_name().to_string_lossy().into_owned());
                visit(child_path, child_logical_path, scan);
            } else {
                visit(child_path, logical_path.clone(), scan);
            }
        }
    }

    let mut scan = LibraryImportScan::default();
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            scan.failed += 1;
            continue;
        };
        if metadata.file_type().is_symlink() {
            scan.skipped += 1;
        } else if metadata.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Imported".to_string());
            visit(path, vec![name], &mut scan);
        } else {
            visit(path, Vec::new(), &mut scan);
        }
    }
    scan.folders
        .sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    scan.folders.dedup();
    scan
}

fn thumbnail_cache_dir() -> PathBuf {
    glyphst_core::paths::state_dir()
        .join("thumbnails")
        .join(THUMBNAIL_CACHE_VERSION)
}

fn load_or_render_cover_thumbnail(path: &Path, key: &ThumbnailCacheKey) -> Result<ThumbnailImage> {
    let cache_path = key.disk_path();
    if cache_path.is_file()
        && let Ok(image) = image::open(&cache_path)
    {
        let rgba = image.into_rgba8();
        let width = rgba.width() as f32;
        let height = rgba.height() as f32;
        return Ok(ThumbnailImage {
            image: Arc::new(RenderImage::new(smallvec![Frame::new(rgba)])),
            width,
            height,
        });
    }
    render_cover_thumbnail(path, Some(&cache_path))
}

fn persist_cover_thumbnail(path: &Path, image: &RgbaImage) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("thumbnail cache path has no parent");
    };
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create thumbnail cache {}", parent.display()))?;
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("failed to save thumbnail cache {}", path.display()))?;

    let mut entries = fs::read_dir(parent)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata
                .is_file()
                .then(|| (metadata.modified().unwrap_or(UNIX_EPOCH), entry.path()))
        })
        .collect::<Vec<_>>();
    if entries.len() > MAX_PERSISTENT_THUMBNAILS {
        entries.sort_by_key(|(modified, _)| *modified);
        let remove_count = entries.len() - MAX_PERSISTENT_THUMBNAILS;
        for (_, path) in entries.into_iter().take(remove_count) {
            if let Err(error) = fs::remove_file(&path) {
                eprintln!(
                    "failed to prune cached new tab preview {}: {error:#}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

fn render_cover_thumbnail(path: &Path, cache_path: Option<&Path>) -> Result<ThumbnailImage> {
    let mut document = mupdf::Document::open(path)?;
    if document.is_reflowable()? {
        document.layout(480.0, 640.0, 16.0)?;
    }
    let page_count = document.page_count()?;
    if page_count <= 0 {
        bail!("document has no readable content");
    }

    let page = document.load_page(0)?;
    let page_bounds = page.bounds()?;
    let page_width = page_bounds.x1 - page_bounds.x0;
    let page_height = page_bounds.y1 - page_bounds.y0;
    if page_width <= 0.0 || page_height <= 0.0 {
        bail!("PDF first page has invalid bounds");
    }

    let scale = (THUMBNAIL_MAX_WIDTH / page_width)
        .min(THUMBNAIL_MAX_HEIGHT / page_height)
        .max(0.001);
    let logical_width = page_width * scale;
    let logical_height = page_height * scale;
    let physical_width = logical_width.ceil().max(1.0) as i32;
    let physical_height = logical_height.ceil().max(1.0) as i32;

    let display_list = DisplayList::new(page_bounds)?;
    let device = Device::from_display_list(&display_list)?;
    page.run(&device, &Matrix::IDENTITY)?;

    let mut pixmap = Pixmap::new_with_w_h(
        &Colorspace::device_rgb(),
        physical_width,
        physical_height,
        true,
    )?;
    pixmap.samples_mut().fill(255);

    let zoom_x = physical_width as f32 / page_width;
    let zoom_y = physical_height as f32 / page_height;
    let matrix = Matrix::new(
        zoom_x,
        0.0,
        0.0,
        zoom_y,
        -page_bounds.x0 * zoom_x,
        -page_bounds.y0 * zoom_y,
    );

    {
        let device = Device::from_pixmap(&pixmap)?;
        display_list
            .run(&device, &matrix, Rect::INF)
            .context("failed to render PDF preview")?;
    }

    let width = pixmap.width();
    let height = pixmap.height();
    let samples = pixmap.samples();
    let expected_len = width as usize * height as usize * 4;
    if samples.len() != expected_len {
        bail!(
            "unexpected pixmap sample length: got {}, expected {}",
            samples.len(),
            expected_len
        );
    }

    let mut bgra = Vec::with_capacity(samples.len());
    for pixel in samples.chunks_exact(4) {
        bgra.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }

    let rgba = RgbaImage::from_raw(width, height, bgra)
        .context("failed to create preview image from pixmap")?;
    if let Some(cache_path) = cache_path
        && let Err(error) = persist_cover_thumbnail(cache_path, &rgba)
    {
        eprintln!(
            "failed to cache new tab preview for {}: {error:#}",
            path.display()
        );
    }
    let image = Arc::new(RenderImage::new(smallvec![Frame::new(rgba)]));

    Ok(ThumbnailImage {
        image,
        width: logical_width,
        height: logical_height,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::BTreeSet,
        fs::{self, OpenOptions},
        io::Write as _,
        path::PathBuf,
        rc::Rc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use gpui::{AppContext as _, TestAppContext};
    use gpui_component::Root;
    use image::Rgba;

    use super::{
        BrowserEmptyAction, CursorMove, NewTab, NewTabEntry, NewTabEvent, NewTabItemKey,
        NewTabLayoutClass, ThumbnailCacheKey, format_file_size, persist_cover_thumbnail,
        scan_library_import_paths,
    };
    use crate::{
        library::{
            DocumentPosition, HistoryEntry, LibraryAddOutcome, LibraryItemMetadata, LibraryNodeId,
            LibrarySavedViewId, LibrarySort, LibrarySortDirection, LibrarySortField, LibraryStore,
            LibraryTagId, LibraryViewMode, QuickOpenSource, ReaderNewTabDestination,
            ReaderNewTabSession,
        },
        settings::{NewTabViewMode, SettingsStore},
    };

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn test_entry(path: &str) -> NewTabEntry {
        NewTabEntry {
            item_id: None,
            path: PathBuf::from(path),
            file_name: path.into(),
            directory: "/tmp".into(),
            last_read: "Last read".into(),
            progress: "50%".into(),
            format_size: "PDF · 1 KiB".into(),
            last_opened: "2026-07-28 12:00".into(),
            format: Some(glyphst_core::DocumentFormat::Pdf),
            favorite: false,
            status: None,
            missing: false,
            title_sort: path.to_lowercase().into(),
            creator_sort: "".into(),
            year: None,
            date_added: None,
            last_opened_timestamp: None,
            search_text: String::new().into(),
        }
    }

    fn history_entry(path: PathBuf, opened: u64) -> HistoryEntry {
        HistoryEntry {
            path,
            opened,
            position: DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
        }
    }

    #[test]
    fn responsive_layout_uses_orientation_and_hysteresis() {
        assert_eq!(
            NewTabLayoutClass::for_size(480.0, 700.0, NewTabLayoutClass::Compact),
            NewTabLayoutClass::Narrow
        );
        assert_eq!(
            NewTabLayoutClass::for_size(530.0, 700.0, NewTabLayoutClass::Narrow),
            NewTabLayoutClass::Narrow
        );
        assert_eq!(
            NewTabLayoutClass::for_size(600.0, 700.0, NewTabLayoutClass::Narrow),
            NewTabLayoutClass::Compact
        );
        assert_eq!(
            NewTabLayoutClass::for_size(900.0, 700.0, NewTabLayoutClass::Compact),
            NewTabLayoutClass::Expanded
        );
        assert_eq!(
            NewTabLayoutClass::for_size(900.0, 1_200.0, NewTabLayoutClass::Compact),
            NewTabLayoutClass::Compact
        );
        assert_eq!(
            NewTabLayoutClass::for_size(1_120.0, 1_400.0, NewTabLayoutClass::Compact),
            NewTabLayoutClass::Expanded
        );
        assert_eq!(
            NewTabLayoutClass::for_size(770.0, 700.0, NewTabLayoutClass::Expanded),
            NewTabLayoutClass::Expanded
        );
    }

    #[gpui::test]
    fn new_tab_keeps_its_own_navigation_view_and_sort_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
        });
        let expected = ReaderNewTabSession {
            source: crate::library::QuickOpenSource::Library,
            history: crate::library::BrowserPresentation::history_default(),
            library: crate::library::BrowserPresentation {
                view_mode: LibraryViewMode::Grid,
                sort: LibrarySort {
                    field: LibrarySortField::Title,
                    direction: LibrarySortDirection::Ascending,
                },
            },
        };
        let restored = expected.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view =
                cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), restored, window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        assert_eq!(view.read_with(cx, |view, _| view.session_state()), expected);
    }

    #[gpui::test]
    fn quick_open_starts_without_a_cursor_and_first_direction_focuses_item_one(
        cx: &mut TestAppContext,
    ) {
        let directory = temporary_directory("quick-open-initial-cursor");
        fs::create_dir(&directory).unwrap();
        let paths = ["a.pdf", "b.pdf", "c.pdf"]
            .into_iter()
            .map(|name| directory.join(name))
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, b"document").unwrap();
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            library.library.history = paths
                .iter()
                .enumerate()
                .map(|(index, path)| history_entry(path.clone(), (10 - index) as u64))
                .collect();
            cx.set_global(library);
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
                recorded_events.borrow_mut().push(event.clone());
            })
        });
        cx.run_until_parked();

        view.update_in(cx, |this, window, cx| {
            assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
            assert!(this.selection.is_empty());
            assert!(!this.item_state(0).focused);
            this.activate_focused(cx);

            for mode in [
                NewTabViewMode::Grid,
                NewTabViewMode::Grouped,
                NewTabViewMode::Table,
            ] {
                this.focused_item = NewTabItemKey::OpenFile;
                this.set_view_mode(mode, cx);
                assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
                for movement in [
                    CursorMove::Up,
                    CursorMove::Down,
                    CursorMove::Left,
                    CursorMove::Right,
                ] {
                    this.focused_item = NewTabItemKey::OpenFile;
                    this.selection.clear();
                    this.selection_anchor = None;
                    this.move_cursor(movement, false, cx);
                    assert_eq!(this.focused_item, NewTabItemKey::Document(paths[0].clone()));
                }
            }

            this.focused_item = NewTabItemKey::OpenFile;
            this.search_input.update(cx, |input, cx| {
                input.set_value("a", window, cx);
            });
            this.update_filter(cx);
            assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
            this.set_destination(ReaderNewTabDestination::Library, window, cx);
            assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
            this.set_destination(ReaderNewTabDestination::History, window, cx);
            assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
        });
        assert!(events.borrow().is_empty());

        view.update_in(cx, |this, _, cx| {
            this.focused_item = NewTabItemKey::OpenFile;
            this.handle_document_pointer(paths[1].clone(), gpui::Modifiers::default(), 1, cx);
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[NewTabEvent::OpenDocument(paths[1].clone())]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn home_single_click_keeps_selection_and_double_click_opening(cx: &mut TestAppContext) {
        let directory = temporary_directory("home-click-behavior");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("paper.pdf");
        fs::write(&path, b"document").unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            library.library.history = vec![history_entry(path.clone(), 1)];
            cx.set_global(library);
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
                recorded_events.borrow_mut().push(event.clone());
            })
        });
        cx.run_until_parked();

        view.update_in(cx, |this, window, cx| {
            this.set_destination(ReaderNewTabDestination::History, window, cx);
            this.handle_document_pointer(path.clone(), gpui::Modifiers::default(), 1, cx);
            assert_eq!(this.focused_item, NewTabItemKey::Document(path.clone()));
            assert_eq!(this.selection, BTreeSet::from([path.clone()]));
            assert!(this.inspector_open);
        });
        assert!(events.borrow().is_empty());

        view.update_in(cx, |this, _, cx| {
            this.handle_document_pointer(path.clone(), gpui::Modifiers::default(), 2, cx);
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[NewTabEvent::OpenDocument(path.clone())]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn quick_open_library_tree_starts_inactive_and_opens_items_on_one_click(
        cx: &mut TestAppContext,
    ) {
        let directory = temporary_directory("quick-open-library-tree");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("paper.pdf");
        fs::write(&path, b"document").unwrap();

        let item_id = cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings_store = SettingsStore::memory();
            let mut settings = settings_store.settings.clone();
            settings.new_tab.view_mode = NewTabViewMode::Table;
            settings_store.set_settings_in_memory(settings);
            cx.set_global(settings_store);
            let mut library = LibraryStore::memory();
            let outcome = library
                .add_linked_item(&path, LibraryItemMetadata::for_path(&path))
                .unwrap();
            let item_id = match outcome {
                LibraryAddOutcome::Added(id)
                | LibraryAddOutcome::Existing(id)
                | LibraryAddOutcome::ExistingInTrash(id) => id,
            };
            cx.set_global(library);
            item_id
        });
        let session = ReaderNewTabSession {
            source: QuickOpenSource::Library,
            ..ReaderNewTabSession::default()
        };
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let view =
                cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), session, window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded_events = events.clone();
        let _subscription = cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event: &NewTabEvent, _| {
                recorded_events.borrow_mut().push(event.clone());
            })
        });
        cx.run_until_parked();

        view.update_in(cx, |this, _, cx| {
            assert!(this.is_library_tree());
            assert_eq!(this.tree_active, None);
            assert_eq!(this.focused_item, NewTabItemKey::OpenFile);
            for movement in [
                CursorMove::Up,
                CursorMove::Down,
                CursorMove::Left,
                CursorMove::Right,
            ] {
                this.tree_active = None;
                this.focused_item = NewTabItemKey::OpenFile;
                this.move_cursor(movement, false, cx);
                assert_eq!(this.tree_active, Some(LibraryNodeId::Item(item_id)));
                assert_eq!(this.focused_item, NewTabItemKey::Document(path.clone()));
            }
            this.tree_active = None;
            this.focused_item = NewTabItemKey::OpenFile;
            this.handle_tree_pointer(
                LibraryNodeId::Item(item_id),
                gpui::Modifiers::default(),
                1,
                cx,
            );
        });
        assert_eq!(
            events.borrow().as_slice(),
            &[NewTabEvent::OpenDocument(path.clone())]
        );

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn selection_ranges_follow_the_visible_document_order() {
        let entries = vec![
            test_entry("/tmp/a.pdf"),
            test_entry("/tmp/b.pdf"),
            test_entry("/tmp/c.pdf"),
        ];
        let filtered = vec![0, 1, 2];

        assert_eq!(
            NewTab::selection_range(
                &entries,
                &filtered,
                PathBuf::from("/tmp/b.pdf").as_path(),
                0,
            ),
            BTreeSet::from([PathBuf::from("/tmp/a.pdf"), PathBuf::from("/tmp/b.pdf"),])
        );
        assert_eq!(
            NewTab::selection_range(
                &entries,
                &filtered,
                PathBuf::from("/tmp/a.pdf").as_path(),
                2,
            ),
            BTreeSet::from([
                PathBuf::from("/tmp/a.pdf"),
                PathBuf::from("/tmp/b.pdf"),
                PathBuf::from("/tmp/c.pdf"),
            ])
        );
        assert!(
            NewTab::selection_range(&entries, &[], PathBuf::from("/tmp/a.pdf").as_path(), 0,)
                .is_empty()
        );
    }

    #[test]
    fn file_sizes_use_binary_units() {
        assert_eq!(format_file_size(42), "42 B");
        assert_eq!(format_file_size(1536), "1.5 KiB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn grouped_labels_cover_timeline_and_alphabetic_fallbacks() {
        let now = u64::try_from(chrono::Utc::now().timestamp()).unwrap();
        assert_eq!(NewTab::grouped_time_label(Some(now), "Unknown"), "Today");
        assert_eq!(
            NewTab::grouped_time_label(None, "Never Opened"),
            "Never Opened"
        );
        assert_eq!(NewTab::grouped_alpha_label("Ada Lovelace", "Unknown"), "A");
        assert_eq!(NewTab::grouped_alpha_label("123 notes", "Unknown"), "#");
        assert_eq!(NewTab::grouped_alpha_label("", "Unknown"), "Unknown");
    }

    #[test]
    fn search_copy_is_specific_to_each_browser_destination() {
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Home),
            "Search documents…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::History),
            "Search recent documents…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Library),
            "Search your library…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Favorites),
            "Search favorites…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Trash),
            "Search trash…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Tags),
            "Search tags…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::Tag(LibraryTagId(1))),
            "Search tagged items…"
        );
        assert_eq!(
            NewTab::search_placeholder(&ReaderNewTabDestination::SavedView(LibrarySavedViewId(1))),
            "Search this view…"
        );
    }

    #[gpui::test]
    fn empty_home_library_has_a_full_import_state_in_every_view(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        assert_eq!(
            view.read_with(cx, |this, _| this.empty_state(false).title),
            "Your library is empty"
        );

        for mode in [
            NewTabViewMode::Grid,
            NewTabViewMode::Grouped,
            NewTabViewMode::Table,
        ] {
            view.update_in(cx, |this, _, cx| this.set_view_mode(mode, cx));
            cx.update(|window, cx| {
                _ = window.draw(cx);
                _ = window.draw(cx);
            });
            let empty = cx
                .debug_bounds("new-tab-empty-state")
                .expect("empty Library state");
            assert!(empty.size.height >= gpui::px(180.0));
            assert!(cx.debug_bounds("library-empty-add-files").is_some());
            assert!(cx.debug_bounds("new-tab-empty-open-file").is_none());
        }
    }

    #[gpui::test]
    fn empty_library_actions_respect_host_and_current_folder(cx: &mut TestAppContext) {
        let collection_id = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            let collection_id = library.create_library_collection(None, "Papers").unwrap();
            cx.set_global(library);
            collection_id
        });

        let (home_root, home_cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let home = home_root.read_with(home_cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        home.update_in(home_cx, |this, _, cx| {
            this.set_view_mode(NewTabViewMode::Grid, cx);
            this.enter_collection(collection_id, cx);
            assert!(matches!(
                this.empty_state(false).action,
                BrowserEmptyAction::AddFiles(Some(id)) if id == collection_id
            ));
        });

        let session = ReaderNewTabSession {
            source: crate::library::QuickOpenSource::Library,
            ..ReaderNewTabSession::default()
        };
        let (quick_root, quick_cx) = cx.add_window_view(move |window, cx| {
            let view =
                cx.new(|cx| NewTab::new_with_session(cx.focus_handle(), session, window, cx));
            Root::new(view, window, cx)
        });
        let quick = quick_root.read_with(quick_cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        quick_cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(matches!(
            quick.read_with(quick_cx, |this, _| this.empty_state(false).action),
            BrowserEmptyAction::OpenFile
        ));
        assert!(quick_cx.debug_bounds("new-tab-empty-open-file").is_some());
        assert!(quick_cx.debug_bounds("library-empty-add-files").is_none());
    }

    #[gpui::test]
    fn tag_search_filters_names_and_uses_its_own_empty_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            library.ensure_library_tag("Methods").unwrap();
            library.ensure_library_tag("Theory").unwrap();
            cx.set_global(library);
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        view.update_in(cx, |this, window, cx| {
            this.set_destination(ReaderNewTabDestination::Tags, window, cx);
            this.search_input.update(cx, |input, cx| {
                input.set_value("method", window, cx);
            });
            assert_eq!(this.tag_result_counts(cx), (1, 2));
            this.search_input.update(cx, |input, cx| {
                input.set_value("missing", window, cx);
            });
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-empty-state").is_some());
        assert!(cx.debug_bounds("library-empty-add-files").is_none());
    }

    #[gpui::test]
    fn open_file_and_search_stay_in_the_header_across_view_modes(cx: &mut TestAppContext) {
        let directory = temporary_directory("new-tab-view-modes");
        fs::create_dir(&directory).unwrap();
        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings = SettingsStore::memory();
            settings.state_path = directory.join("settings.kdl");
            cx.set_global(settings);
            cx.set_global(LibraryStore::memory());
        });

        let (_root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("new-tab-open-file").is_some());
        assert!(cx.debug_bounds("new-tab-search").is_some());
        assert!(cx.debug_bounds("new-tab-view-grid").is_some());
        assert!(cx.debug_bounds("new-tab-view-grouped").is_some());
        assert!(cx.debug_bounds("new-tab-view-tree").is_some());
        let grid = cx.debug_bounds("new-tab-view-grid").unwrap();
        let tree = cx.debug_bounds("new-tab-view-tree").unwrap();
        let grouped = cx.debug_bounds("new-tab-view-grouped").unwrap();
        assert!(grid.right() <= tree.left());
        assert!(tree.right() <= grouped.left());

        cx.simulate_click(grouped.center(), gpui::Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-open-file").is_some());

        let tree = cx.debug_bounds("new-tab-view-tree").unwrap().center();
        cx.simulate_click(tree, gpui::Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
            assert_eq!(
                cx.global::<SettingsStore>().settings.new_tab.view_mode,
                NewTabViewMode::Table
            );
        });
        assert!(cx.debug_bounds("new-tab-open-file").is_some());

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn quick_open_hides_management_while_home_exposes_it(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
        });

        let (_quick_root, quick_cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        quick_cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(quick_cx.debug_bounds("new-tab-source-selector").is_some());
        assert!(quick_cx.debug_bounds("home-navigation-rail").is_none());
        assert!(quick_cx.debug_bounds("new-tab-add-files").is_none());
        assert!(quick_cx.debug_bounds("home-library-filter-menu").is_none());

        let (_home_root, home_cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        home_cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(home_cx.debug_bounds("home-navigation-rail").is_some());
        assert!(home_cx.debug_bounds("new-tab-add-files").is_some());
        assert!(home_cx.debug_bounds("home-library-new-folder").is_some());
        assert!(home_cx.debug_bounds("home-library-filter-menu").is_none());
        assert!(home_cx.debug_bounds("home-library-kind-filter").is_none());
        assert!(home_cx.debug_bounds("home-library-status-filter").is_none());
        assert!(home_cx.debug_bounds("new-tab-sort-field").is_none());
        assert!(home_cx.debug_bounds("new-tab-sort-direction").is_none());
        assert!(home_cx.debug_bounds("new-tab-source-selector").is_none());
    }

    #[gpui::test]
    fn home_grid_cards_stay_inside_the_browser_with_an_inspector(cx: &mut TestAppContext) {
        let directory = temporary_directory("home-grid-browser-width");
        fs::create_dir(&directory).unwrap();
        let paths = (0..4)
            .map(|ix| directory.join(format!("paper-{ix}.pdf")))
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, b"document").unwrap();
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            library.library.history = paths
                .iter()
                .enumerate()
                .map(|(ix, path)| history_entry(path.clone(), ix as u64 + 1))
                .collect();
            cx.set_global(library);
        });
        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        cx.simulate_resize(gpui::size(gpui::px(1_200.0), gpui::px(800.0)));
        view.update_in(cx, |this, window, cx| {
            this.set_destination(ReaderNewTabDestination::History, window, cx);
            this.set_view_mode(NewTabViewMode::Grid, cx);
        });
        for _ in 0..3 {
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });
        }

        let browser = cx
            .debug_bounds("new-tab-browser-viewport")
            .expect("browser viewport");
        let (window_width, browser_width, cols) = view.read_with(cx, |this, _| {
            (this.viewport_width, this.browser_viewport_width, this.cols)
        });
        assert!(browser_width < window_width);
        assert_eq!(cols, NewTab::cols_for_width(browser_width));
        assert_eq!(cols, 4);
        for selector in [
            "new-tab-card-0",
            "new-tab-card-1",
            "new-tab-card-2",
            "new-tab-card-3",
        ] {
            let card = cx.debug_bounds(selector).expect("visible grid card");
            assert!(card.left() >= browser.left());
            assert!(card.right() <= browser.right());
        }

        let first_card = cx.debug_bounds("new-tab-card-0").unwrap();
        cx.simulate_click(first_card.center(), gpui::Modifiers::default());
        for _ in 0..3 {
            cx.update(|window, cx| {
                _ = window.draw(cx);
            });
        }
        assert!(cx.debug_bounds("new-tab-item-inspector").is_some());
        let narrowed_browser = cx
            .debug_bounds("new-tab-browser-viewport")
            .expect("narrowed browser viewport");
        let narrowed_cols = view.read_with(cx, |this, _| this.cols);
        assert!(narrowed_browser.size.width < browser.size.width);
        assert_eq!(narrowed_cols, 3);
        for selector in ["new-tab-card-0", "new-tab-card-1", "new-tab-card-2"] {
            let card = cx
                .debug_bounds(selector)
                .expect("visible narrowed grid card");
            assert!(card.left() >= narrowed_browser.left());
            assert!(card.right() <= narrowed_browser.right());
        }

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn folder_editor_input_receives_vim_navigation_keys(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::keybindings::capture_framework_bindings(cx);
            crate::keybindings::apply(cx, glyphst_settings::KeymapPreset::Vim, &[]);
            cx.set_global(SettingsStore::memory());
            cx.set_global(LibraryStore::memory());
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        view.update_in(cx, |this, window, cx| {
            this.open_new_folder_editor(None, window, cx);
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());

        cx.simulate_keystrokes("k");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            view.read_with(cx, |this, cx| this
                .folder_name_input
                .read(cx)
                .value()
                .to_string()),
            "k"
        );

        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("library-folder-editor-overlay").is_none());
        assert!(cx.update(|_, cx| cx.global::<LibraryStore>().library_collections().is_empty()));
        assert!(cx.update(|window, cx| view.read(cx).focus_handle.is_focused(window)));

        view.update_in(cx, |this, window, cx| {
            this.open_saved_view_editor(window, cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-saved-view-overlay").is_some());
        cx.simulate_keystrokes("escape");
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-saved-view-overlay").is_none());
    }

    #[gpui::test]
    fn library_folders_browse_as_cards_and_expand_as_a_tree(cx: &mut TestAppContext) {
        let (papers_id, theory_id) = cx.update(|cx| {
            gpui_component::init(cx);
            cx.set_global(SettingsStore::memory());
            let mut library = LibraryStore::memory();
            let papers_id = library.create_library_collection(None, "Papers").unwrap();
            let theory_id = library
                .create_library_collection(Some(papers_id), "Type Theory")
                .unwrap();
            cx.set_global(library);
            (papers_id, theory_id)
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new_home(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });
        view.update_in(cx, |this, _, cx| {
            this.set_view_mode(NewTabViewMode::Grid, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        assert!(cx.debug_bounds("library-folder-grid").is_some());
        assert!(cx.debug_bounds("library-folder-item").is_some());
        assert!(cx.debug_bounds("library-folder-breadcrumb").is_some());

        let papers = cx.debug_bounds("library-folder-item").unwrap().center();
        cx.simulate_mouse_down(papers, gpui::MouseButton::Right, gpui::Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(view.read_with(cx, |this, _| this.current_collection), None);
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());
        view.update_in(cx, |this, _, cx| {
            this.folder_editor = None;
            cx.notify();
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        cx.simulate_click(papers, gpui::Modifiers::default());
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert_eq!(
            view.read_with(cx, |this, _| this.current_collection),
            Some(papers_id)
        );
        assert!(cx.debug_bounds("library-folder-item").is_some());

        view.update_in(cx, |this, window, cx| {
            this.search_input.update(cx, |input, cx| {
                input.set_value("type", window, cx);
            });
            this.update_filter(cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("library-folder-item").is_none());
        assert_eq!(
            view.read_with(cx, |this, _| this.current_collection),
            Some(papers_id)
        );

        view.update_in(cx, |this, window, cx| {
            this.search_input.update(cx, |input, cx| {
                input.set_value("", window, cx);
            });
            this.update_filter(cx);
            this.set_view_mode(NewTabViewMode::Table, cx);
            this.toggle_collection_expanded(papers_id, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let papers_row_selector: &'static str =
            Box::leak(format!("library-folder-tree-row-{}", papers_id.0).into_boxed_str());
        let theory_row_selector: &'static str =
            Box::leak(format!("library-folder-tree-row-{}", theory_id.0).into_boxed_str());
        assert!(cx.debug_bounds(papers_row_selector).is_some());
        let tree_viewport = cx
            .debug_bounds("new-tab-tree-viewport")
            .expect("library tree viewport");
        let papers_row = cx
            .debug_bounds(papers_row_selector)
            .expect("top-level library folder row");
        let theory_row = cx
            .debug_bounds(theory_row_selector)
            .expect("nested library folder row");
        let root_area = cx
            .debug_bounds("library-tree-root-area")
            .expect("library tree root area");
        assert!(tree_viewport.size.height > gpui::px(0.0));
        assert!(papers_row.top() >= tree_viewport.top());
        assert!(theory_row.bottom() <= tree_viewport.bottom());
        assert!(root_area.size.height > gpui::px(0.0));
        assert!(root_area.top() >= theory_row.bottom());
        assert!(root_area.bottom() <= tree_viewport.bottom());
        assert!(view.read_with(cx, |this, _| {
            this.library_tree_rows.iter().any(|row| {
                matches!(
                    row,
                    super::LibraryTreeRow::Collection { id, depth: 0 } if *id == papers_id
                )
            }) && this.library_tree_rows.iter().any(|row| {
                matches!(
                    row,
                    super::LibraryTreeRow::Collection { id, depth: 1 } if *id == theory_id
                )
            })
        }));

        cx.simulate_mouse_down(
            theory_row.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.simulate_mouse_move(
            gpui::point(theory_row.center().x + gpui::px(8.0), theory_row.center().y),
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        assert!(view.read_with(cx, |this, _| {
            this.dragging_library_nodes
                .contains(&crate::library::LibraryNodeId::Collection(theory_id))
        }));
        cx.simulate_mouse_move(
            root_area.center(),
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_mouse_up(
            root_area.center(),
            gpui::MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| {
            _ = window.draw(cx);
            _ = window.draw(cx);
        });
        assert_eq!(
            cx.update(|_, cx| cx
                .global::<LibraryStore>()
                .library_collection(theory_id)
                .and_then(|collection| collection.parent_id)),
            None
        );

        cx.simulate_mouse_down(
            root_area.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("library-folder-editor-overlay").is_some());
    }

    #[test]
    fn recursive_import_scan_preserves_directories_and_skips_unsupported_files() {
        let directory = temporary_directory("library-import-scan");
        let dropped = directory.join("Reading List");
        let nested = dropped.join("Papers");
        let empty = dropped.join("Empty");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&empty).unwrap();
        fs::write(dropped.join("book.epub"), b"book").unwrap();
        fs::write(nested.join("paper.pdf"), b"paper").unwrap();
        fs::write(nested.join("notes.txt"), b"notes").unwrap();

        let scan = scan_library_import_paths(vec![dropped]);
        assert_eq!(scan.candidates.len(), 2);
        assert_eq!(scan.skipped, 1);
        assert_eq!(scan.failed, 0);
        assert!(scan.folders.contains(&vec!["Reading List".to_string()]));
        assert!(
            scan.folders
                .contains(&vec!["Reading List".to_string(), "Empty".to_string(),])
        );
        assert!(
            scan.folders
                .contains(&vec!["Reading List".to_string(), "Papers".to_string(),])
        );
        assert!(scan.candidates.iter().any(|candidate| {
            candidate.path.ends_with("paper.pdf")
                && candidate.folder_path == ["Reading List", "Papers"]
        }));

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn history_supports_grouped_spans_and_headerless_tree_rows(cx: &mut TestAppContext) {
        let directory = temporary_directory("new-tab-rounded-rows");
        fs::create_dir(&directory).unwrap();
        let path = directory.join(
            "a-very-long-academic-paper-title-that-remains-visible-until-the-span-reaches-its-parent.pdf",
        );
        fs::write(&path, b"document").unwrap();

        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings_store = SettingsStore::memory();
            settings_store.state_path = directory.join("settings.kdl");
            let mut settings = settings_store.settings.clone();
            settings.new_tab.view_mode = NewTabViewMode::Grouped;
            settings_store.set_settings_in_memory(settings);
            cx.set_global(settings_store);

            let mut library = LibraryStore::memory();
            library.library.history = vec![history_entry(path.clone(), 1)];
            cx.set_global(library);
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });

        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        assert!(cx.debug_bounds("new-tab-grouped-list").is_some());
        let grouped_item = cx
            .debug_bounds("new-tab-grouped-item")
            .expect("grouped item");
        assert!(grouped_item.size.width > gpui::px(320.0));

        view.update_in(cx, |this, _, cx| {
            this.set_view_mode(NewTabViewMode::Table, cx);
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let row = cx
            .debug_bounds("new-tab-tree-document-row")
            .expect("tree document row");
        let viewport = cx
            .debug_bounds("new-tab-tree-viewport")
            .expect("history tree viewport");
        let name = cx
            .debug_bounds("new-tab-tree-document-name")
            .expect("tree document name");
        assert!(cx.debug_bounds("new-tab-details-name-header").is_none());
        assert!(viewport.size.height > gpui::px(0.0));
        assert!(row.top() >= viewport.top());
        assert!(row.bottom() <= viewport.bottom());
        assert!(name.left() > row.left());
        assert!(name.right() <= row.right());

        fs::remove_dir_all(directory).unwrap();
    }

    #[gpui::test]
    fn keyboard_selection_supports_ranges_and_select_all(cx: &mut TestAppContext) {
        let directory = temporary_directory("new-tab-selection");
        fs::create_dir(&directory).unwrap();
        let paths = ["a.pdf", "b.pdf", "c.pdf"]
            .into_iter()
            .map(|name| directory.join(name))
            .collect::<Vec<_>>();
        for path in &paths {
            fs::write(path, b"document").unwrap();
        }

        cx.update(|cx| {
            gpui_component::init(cx);
            let mut settings_store = SettingsStore::memory();
            let mut settings = settings_store.settings.clone();
            settings.new_tab.view_mode = NewTabViewMode::Grouped;
            settings_store.set_settings_in_memory(settings);
            cx.set_global(settings_store);

            let mut library = LibraryStore::memory();
            library.library.history = paths
                .iter()
                .enumerate()
                .map(|(ix, path)| history_entry(path.clone(), (10 - ix) as u64))
                .collect();
            cx.set_global(library);
        });

        let (root, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| NewTab::new(cx.focus_handle(), window, cx));
            Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<NewTab>().unwrap()
        });

        view.update_in(cx, |this, _, cx| {
            assert_eq!(
                this.item_key(0),
                Some(NewTabItemKey::Document(paths[0].clone()))
            );

            this.handle_document_pointer(paths[0].clone(), gpui::Modifiers::secondary_key(), 1, cx);
            assert_eq!(this.selection, BTreeSet::from([paths[0].clone()]));
            this.handle_document_pointer(
                paths[2].clone(),
                gpui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
                1,
                cx,
            );
            assert_eq!(this.selection.len(), 3);
            this.cancel_select(cx);
            this.focused_item = NewTabItemKey::Document(paths[0].clone());
            this.toggle_focused(cx);
            assert_eq!(this.selection.len(), 1);

            this.move_cursor(CursorMove::Down, true, cx);
            assert_eq!(
                this.selection,
                BTreeSet::from([paths[0].clone(), paths[1].clone()])
            );

            this.select_all(cx);
            assert_eq!(this.selection.len(), 3);
            this.cancel_select(cx);
            assert!(this.selection.is_empty());
            assert!(!this.selecting);
            assert_eq!(cx.global::<LibraryStore>().history_entries().len(), 3);
        });

        for path in &paths {
            assert!(path.is_file(), "removing history must not delete {path:?}");
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn thumbnail_fingerprint_changes_when_the_document_changes() {
        let directory = temporary_directory("thumbnail-fingerprint");
        fs::create_dir(&directory).unwrap();
        let document = directory.join("document.pdf");
        fs::write(&document, b"first").unwrap();
        let first = ThumbnailCacheKey::for_path(&document).unwrap();

        OpenOptions::new()
            .append(true)
            .open(&document)
            .unwrap()
            .write_all(b"-changed")
            .unwrap();
        let changed = ThumbnailCacheKey::for_path(&document).unwrap();

        assert_ne!(first, changed);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn persistent_thumbnail_round_trips_png_pixels() {
        let directory = temporary_directory("thumbnail-persistence");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("thumbnail.png");
        let image = image::RgbaImage::from_pixel(3, 2, Rgba([10, 20, 30, 255]));

        persist_cover_thumbnail(&path, &image).unwrap();
        let restored = image::open(&path).unwrap().into_rgba8();

        assert_eq!(restored, image);
        fs::remove_dir_all(directory).unwrap();
    }
}
