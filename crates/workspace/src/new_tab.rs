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
use glyphst_actions::menu::{SelectNext, SelectPrev};
use glyphst_core::DocumentFormat;
use glyphst_files::path_chooser::{OpenPathOptions, PathFilter, prompt_for_paths};
use glyphst_ui::{
    entry_selection::apply_entry_click,
    format_user_float,
    style::{SURFACE_GAP, SURFACE_INSET},
};
use gpui::{
    AnyElement, App, Context, Corners, DragMoveEvent, Entity, EventEmitter, ExternalPaths,
    FocusHandle, IntoElement, ListSizingBehavior, Modifiers, MouseButton, MouseDownEvent,
    MouseExitEvent, Pixels, PromptButton, PromptLevel, Render, RenderImage, ScrollStrategy,
    SharedString, Subscription, Task, UniformListScrollHandle, Window, canvas, deferred, div, fill,
    point, prelude::*, px, rgb, transparent_black, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeColor,
    alert::Alert,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::Cancel,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
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
    external_drop::{
        ExternalDocumentDropFeedback, classify_external_document_paths,
        render_external_document_drop_overlay,
    },
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

mod catalog;
mod rendering;
mod selection;
#[cfg(test)]
mod tests;
mod thumbnails;

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

pub struct ThumbnailCache {
    images: HashMap<ThumbnailCacheKey, ThumbnailImage>,
    order: VecDeque<ThumbnailCacheKey>,
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

#[derive(Default)]
struct LibraryTreeState {
    current_collection: Option<LibraryCollectionId>,
    expanded_collections: BTreeSet<LibraryCollectionId>,
    rows: Vec<LibraryTreeRow>,
    active: Option<LibraryNodeId>,
    marked: BTreeSet<LibraryNodeId>,
    anchor: Option<LibraryNodeId>,
    dragging_nodes: BTreeSet<LibraryNodeId>,
    drop_target: Option<LibraryDropTarget>,
    hover_expand_task: Option<Task<()>>,
    hover_scroll_task: Option<Task<()>>,
}

struct LibraryEditorState {
    folder: Option<FolderEditor>,
    folder_delete_prompt_pending: bool,
    note_expanded: bool,
    metadata_open: bool,
    metadata_kind: LibraryItemKind,
    saved_view_open: bool,
    tag_id: Option<LibraryTagId>,
    permanent_delete_prompt_pending: bool,
    work_note_input: Entity<TextareaState>,
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
    metadata_abstract_input: Entity<TextareaState>,
    saved_view_name_input: Entity<InputState>,
    tag_name_input: Entity<InputState>,
    folder_name_input: Entity<InputState>,
    note_item_id: Option<DocumentId>,
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
    tree: LibraryTreeState,
    external_document_drop_feedback: Option<ExternalDocumentDropFeedback>,
    import_cancel: Option<Arc<AtomicBool>>,
    import_progress: Option<LibraryImportProgress>,
    import_message: Option<SharedString>,
    editors: LibraryEditorState,
    kind_filter: Option<LibraryItemKind>,
    status_filter: Option<ReadingStatus>,
    observed_catalog_revision: u64,
    nav_menu_open: bool,
    inspector_open: bool,
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
                    self.tree.current_collection
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
        let search_placeholder = Self::search_placeholder(&session.destination);
        let search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(search_placeholder)
                .submit_on_enter(true)
        });
        let _search_subscription =
            cx.subscribe_in(&search_input, window, Self::on_search_input_event);
        let work_note_input = cx.new(|cx| {
            TextareaState::new(window, cx)
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
            TextareaState::new(window, cx)
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
            tree: LibraryTreeState::default(),
            external_document_drop_feedback: None,
            import_cancel: None,
            import_progress: None,
            import_message: None,
            editors: LibraryEditorState {
                folder: None,
                folder_delete_prompt_pending: false,
                note_expanded: false,
                metadata_open: false,
                metadata_kind: LibraryItemKind::Paper,
                saved_view_open: false,
                tag_id: None,
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
                tag_name_input: tag_editor_name_input,
                folder_name_input,
                note_item_id: None,
            },
            kind_filter: None,
            status_filter: None,
            observed_catalog_revision,
            nav_menu_open: false,
            inspector_open: false,
            selecting: false,
            selection: BTreeSet::new(),
            focused_item: NewTabItemKey::OpenFile,
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

    fn open_file(&mut self, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenFile);
    }

    fn open_document(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        cx.emit(NewTabEvent::OpenDocument(path));
    }
}

impl EventEmitter<NewTabEvent> for NewTab {}
