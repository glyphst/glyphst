//! The reader window: tabs (home and documents), chrome, the document view
//! with its effects, preferences and reading positions.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui_wgpu;
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_core::{
    DocumentFormat, DocumentLayout, DocumentPageLayout, DocumentRotation, EpubLayout,
};
use glyphst_effects::EffectDef;
use glyphst_library::{ActivityKind, DocumentPosition, HighlightColor, LibraryStore};
use glyphst_pdf::text::LinkTarget;
use glyphst_ui::Tip;
use glyphst_ui::craft::docking::Zone;

use crate::{
    activity_view::{ActivityAction, ActivityView},
    ai::{AskContext, Assistant, PanelAction},
    annotations::{self, AnnotationId, Annotations},
    commands::{Command, Needs},
    desktop_theme::DesktopTheme,
    document::{Arrangement, FrameTimes, THUMBNAIL_SCALE},
    document_tab::{DocumentTab, ViewAction, ViewInput},
    find::{self, FindBarAction},
    generation::{Generations, PageState},
    gpu::{
        Capture as GpuCapture, EffectUse, PAGE_BUDGET_BYTES, PageKey, ReaderGpu, TransitionUse,
        UPLOAD_BYTES_PER_FRAME, ViewCallback, pixel_size,
    },
    harness::Harness,
    home::{Home, HomeAction},
    icons,
    keymap_page::KeymapPage,
    keys::{self, Keymap, KeymapPreset},
    launcher::{Launcher, LauncherAction, LauncherContext},
    overview::Overview,
    pad::{
        GamepadInput, Pads,
        route::{self, RouteState, Router},
    },
    pages::Page,
    palette::{self, Item, Palette},
    panes::{self, Panes},
    pickers::{Pickers, is_document},
    prefs::{Colours, LastSession, Prefs, SessionFile},
    quick_ask::{self, QuickAsk},
    region::{self, Region},
    settings_window::{Section, SettingsContext, SettingsWindow},
    sidebar::{self, OtherBookmark, SidebarAction, SidebarInfo, SidebarState, SidebarTab},
    text_overlay::{PageMap, Selection, TextPos, fill_rects, nearest_page, page_at},
    theme::{self, ThemeKind, ThemePreference, Tokens},
    transition::{Capture, Rng, ViewTransition},
    translation::{CardState, Translator},
    user_settings::UserSettings,
    window_blur::WindowBlur,
};

pub(crate) mod agent;
mod agent_library;
mod chrome;
mod couch;
mod page_chrome;
mod pages;
mod present;
mod ring;
mod status_bar;
mod switcher;

pub(crate) use ring::fits_reading_ring;
mod tab_bar;

/// How often the effects folder is looked at for changes.
const EFFECTS_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// Positions are written to the library at most this often while reading.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Thumbnails the GPU draws a frame, each one small render pass.
const GPU_THUMBNAILS_PER_FRAME: usize = 2;
/// Longest side of a GPU thumbnail in pixels (a large drawing's page is
/// thumbnailed no larger).
const GPU_THUMBNAIL_MAX_SIDE: u32 = 512;
const TAB_MIN_WIDTH: f32 = 90.0;
const TAB_MAX_LABEL: usize = 32;
const TAB_OVERFLOW_WIDTH: f32 = 28.0;
/// The Keyboard shortcuts tab's label.
/// Margin on each side of a reflowed column, in points.
const REFLOW_MARGIN: f32 = 24.0;
/// Height of a reflowed page, in points (as the GPUI Reader lays out).
const REFLOW_PAGE_HEIGHT: f32 = 960.0;
/// The view must keep a new width this long before a reflow.
const REFLOW_SETTLE: Duration = Duration::from_millis(300);
/// Width the sidebar is assumed to take before it is first shown.
const SIDEBAR_WIDTH: f32 = 280.0;
/// The most times a count repeats a command (`999j`).
const MAX_REPEAT: u32 = 999;
/// Hit slop around glyphs, in egui points.
const GLYPH_SLOP: f32 = 2.0;
/// Pixels per frame the view scrolls while a selection is dragged past
/// its edge.
const SELECT_SCROLL: f64 = 14.0;

/// How the app was started.
pub struct Launch {
    pub files: Vec<PathBuf>,
    /// `glyphst://` links to open once the library is up.
    pub urls: Vec<String>,
    pub harness: Option<Harness>,
    pub background: Option<String>,
    /// Book switches' and page turns' transition.
    pub transition: Option<String>,
    pub reduce_motion: bool,
    pub switcher_layout: Option<crate::switcher::layout::Layout>,
    /// Backgrounds for the documents opened, by their place from 1.
    pub book_backgrounds: Vec<(usize, String)>,
    /// The default page tint (an overlay effect id, or `none`).
    pub overlay: Option<String>,
    /// The wallpaper image.
    pub wallpaper: Option<String>,
    /// The background's parameters (`--background-params`).
    pub background_params: Option<[f32; glyphst_effects::MAX_PARAMS]>,
    /// A transparent window at this opacity, in memory (`--opacity`, for
    /// screenshots of glass).
    pub opacity: Option<f32>,
    /// Where the theme's colours come from (`--colours`).
    pub colours: Option<crate::prefs::Colours>,
    pub theme: Option<ThemePreference>,
    /// Opens the documents at this zoom (relative to fit width).
    pub zoom: Option<f32>,
    /// The window's size in points, fixed (`--window`, for screenshots of
    /// narrow layouts).
    pub window: Option<[f32; 2]>,
    /// Shows the second document beside the first, on this side.
    pub split: Option<Zone>,
    /// Turns (quarter turns clockwise) and spreads the documents' pages,
    /// without remembering it.
    pub arrangement: Option<Arrangement>,
    /// Scripted runs: search for this text, select this page's text (from
    /// 1), show this sidebar tab, lay EPUBs out at this font size.
    pub find: Option<String>,
    /// Opens the documents at this page (from 1).
    pub page: Option<usize>,
    /// Selects the text of these pages, first to last (from 1).
    pub select_pages: Option<(usize, usize)>,
    pub sidebar: Option<SidebarTab>,
    pub font_size: Option<f32>,
    /// Translate through a local fake provider instead of the settings'.
    pub translate_fake: bool,
    /// Scripted runs: translate the first document, or the selection.
    pub translate_document: bool,
    pub translate_selection: bool,
    /// Ask through a local fake model; scripted runs ask this at startup.
    pub ai_fake: bool,
    pub ask: Option<String>,
    /// Scripted runs: the assistant in a canned state, for screenshots.
    pub assistant_demo: Option<String>,
    /// The external agent `ask` goes to (`claude-code`, `codex`).
    pub agent: Option<String>,
    /// Scripted runs: open the palette with this query, settings at this
    /// section, or the keyboard shortcuts tab.
    pub palette: Option<String>,
    pub settings: Option<Section>,
    /// A group of the section to scroll to (`--settings appearance:colours`).
    pub settings_group: Option<String>,
    pub shortcuts: bool,
    /// Shows frame 01 of the design (every component) instead of home.
    pub gallery: bool,
    /// Scripted runs: the library tab over a canned library (`all`,
    /// `item` with a paper chosen, `review`).
    pub library_demo: Option<String>,
    /// Scripted runs: commands run once the documents are open.
    pub commands: Vec<Command>,
    /// Scripted runs: a region drawn on a page (from 1) at startup.
    pub select_region: Option<(usize, [f32; 4])>,
    /// Scripted runs: a pad playing these steps (`--pad-demo`) instead of
    /// the real pads, as this family (`--pad-family`).
    pub pad_demo: Option<String>,
    pub pad_family: crate::pad::Family,
    /// Scripted runs: start in gamepad mode, with the clock at 21:30.
    pub gamepad_mode: bool,
    pub fixed_clock: bool,
}

/// One pane's frame of the document view.
/// How a tab becomes active.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Switch {
    /// Where it comes from, in points; else the origin set for the next
    /// switch, or this frame's click.
    pub origin: Option<egui::Pos2>,
    /// +1 or -1; else by the tabs' order.
    pub direction: Option<f32>,
    /// The book switcher showing a book before it is picked.
    pub peek: bool,
    /// How many times faster than its own length the transition runs.
    pub speed: f32,
}

impl Default for Switch {
    fn default() -> Self {
        Self {
            origin: None,
            direction: None,
            peek: false,
            speed: 1.0,
        }
    }
}

struct PaneView<'a> {
    /// Where the pages are laid out.
    rect: egui::Rect,
    /// Where they are drawn: `rect`, or reaching left under a floating
    /// drawer.
    draw: egui::Rect,
    /// The tab shown, an index into `tabs`.
    index: usize,
    focused: bool,
    /// Keys, commands and the gamepad reach only the focused pane.
    actions: Vec<ViewAction>,
    pad: Option<&'a GamepadInput>,
    seconds: f64,
    now: Instant,
}

/// What a primary-button drag on the document does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Pan,
    /// Extending the selection, onto other pages too.
    Select,
    /// Drawing a region with the region tool.
    Region,
}

/// The popover editing a highlight or note clicked on the page.
struct NoteEditor {
    id: AnnotationId,
    at: egui::Pos2,
    body: String,
    color: HighlightColor,
    /// Opened this frame: the click that opened it is not a click outside.
    fresh: bool,
}

/// What a toast's `[ undo ]` takes back.
enum ToastUndo {
    /// The highlight just saved.
    Highlight(AnnotationId),
    /// The merge just made.
    Merge(Box<glyphst_library::LibraryEdit>),
}

pub struct ReaderApp {
    pub(crate) prefs: Prefs,
    /// The theme and window opacity applied to egui.
    theme_applied: Option<theme::Appearance>,
    /// The applied theme's ink and paper, for themed page tints.
    pub(crate) theme_colours: crate::book_effects::ThemeColours,
    /// The theme's base colour, which a solid background takes until the
    /// user sets one (and Home shows).
    pub(crate) theme_bg: [f32; 3],
    fonts_applied: Option<glyphst_ui::fonts::FontChoice>,
    /// Watches the desktop's light or dark choice for the System theme.
    desktop_theme: DesktopTheme,
    /// The desktop's latest answer: dark or light, `None` for no
    /// preference.
    desktop_dark: Option<bool>,
    /// The last theme the desktop (or winit) asked for, which System keeps
    /// while the desktop says nothing.
    pub(crate) system_theme: Option<ThemeKind>,
    /// The compositor's blur behind the transparent window.
    window_blur: WindowBlur,
    pub(crate) library: LibraryStore,
    pub(crate) tabs: Vec<DocumentTab>,
    /// Index into `tabs` of the focused pane's tab; `None` shows the home
    /// tab.
    pub(crate) active: Option<usize>,
    /// The documents shown side by side on the stage.
    pub(crate) panes: Panes,
    next_tab_id: u64,
    home: Home,
    pickers: Pickers,
    pub(crate) render_state: egui_wgpu::RenderState,
    pub(crate) transition: Option<ViewTransition>,
    /// Picks random transitions, their varied parameters and seeds.
    transition_rng: Rng,
    /// The last transition Random picked, so the next one differs.
    last_random: Option<String>,
    /// Where the next switch comes from, in points (a card picked); else
    /// this frame's click.
    pub(crate) next_origin: Option<egui::Pos2>,
    /// Where the pointer clicked this frame.
    click: Option<egui::Pos2>,
    /// The book switcher (Ctrl+Tab) and the order books were read in.
    pub(crate) switcher: crate::switcher::Switcher,
    /// Each tab's card for the switcher: its last view, smaller, and the
    /// image's size in pixels.
    switcher_cards: std::collections::HashMap<u64, (egui::TextureId, [u32; 2])>,
    /// Where the switcher's cards were drawn last, for the transitions
    /// they start.
    switcher_places: Vec<(u64, egui::Pos2)>,
    /// The card under the pointer and since when.
    switcher_hover: Option<(usize, Instant)>,
    switcher_click: Option<usize>,
    /// The animations over pages that are slow to load.
    pub(crate) generations: Generations,
    started: Instant,
    /// Links other processes forward to this Reader (`instance.rs`), and
    /// links waiting for the library's search to locate their passages.
    instance: Option<crate::instance::Instance>,
    pending_links: Vec<crate::links::Link>,
    /// Which app opens `glyphst://` links, asked once for Settings.
    link_handler: Option<Option<String>>,
    /// Concept pages being written (id and title), and topics for new ones.
    page_writer: crate::page_writer::PageWriter,
    page_writing: Vec<(u64, String)>,
    page_topics: Vec<crate::knowledge::TopicSuggestion>,
    /// A library demo set up a vault in a temporary folder.
    demo_vault: bool,
    /// The items close in meaning related suggestions were last made from.
    neighbours_seen: Option<std::sync::Arc<crate::library_search::Neighbours>>,
    pub(crate) pads: Pads,
    pad_router: Router,
    pub(crate) pad_keymap: crate::pad_keys::PadKeymap,
    /// This frame's pad input for the document, routed before the frame,
    /// and the commands its buttons ran.
    pad_input: GamepadInput,
    pad_commands: Vec<Command>,
    pad_ring_steps: Vec<crate::pad::route::RingStep>,
    pad_ring: Option<ring::PadRing>,
    /// Pads connecting and going, for the prompts.
    pad_events: Vec<crate::pad::PadEvent>,
    /// Whether the window had focus last frame, to let go of the pad when
    /// it goes.
    pad_focused: bool,
    prompts: crate::prompts::Prompts,
    pub(crate) couch: crate::couch::Couch,
    fixed_clock: bool,
    /// Where the pointer was when the pad was last used: moving it away
    /// makes the pointer the last input again.
    pad_pointer: Option<egui::Pos2>,
    last_input: glyphst_ui::pad_glyph::InputDevice,
    pub(crate) frames: FrameTimes,
    continuous: bool,
    pub(crate) repaints: u64,
    /// Whether the window was fullscreen at the start of the frame.
    fullscreen: bool,
    notices: glyphst_ui::overlay::Toasts,
    show_stats: bool,
    /// Zen mode: the pages alone, without the tabs, rail, drawers, dock or
    /// title chips.
    pub(crate) zen: bool,
    /// The next drag on a page draws a region (S).
    region_tool: bool,
    /// The region drawn, with its popover.
    pub(crate) region: Option<Region>,
    /// Region images being rendered.
    region_jobs: Vec<region::Job>,
    /// A region waiting for the save dialog's path.
    region_save: Option<Region>,
    /// The region being drawn goes to the assistant's next message.
    pub(crate) region_for_assistant: bool,
    pub(crate) quick_ask: QuickAsk,
    sidebar_state: SidebarState,
    /// The tab whose view a primary-button drag is in, and what it does.
    drag: Option<(u64, Drag)>,
    note_editor: Option<NoteEditor>,
    /// The page a context menu was opened over.
    context_page: Option<usize>,
    pub(crate) launch_find: Option<String>,
    pub(crate) launch_select: Option<(usize, usize)>,
    pub(crate) launch_translate_document: bool,
    pub(crate) launch_translate_selection: bool,
    font_size: Option<f32>,
    pub(crate) translator: Translator,
    pub(crate) user_settings: UserSettings,
    /// The ChatGPT Subscription sign-in, kept with the other keys.
    chatgpt: ChatGptSubscription,
    pub(crate) assistant: Assistant,
    /// The agents' way to the app: tool calls wait here for a frame.
    agent_bridge: crate::ai_bridge::UiBridge,
    agent_calls: std::sync::mpsc::Receiver<crate::ai_bridge::UiCall>,
    /// The open documents, for the agents' reading tools.
    agent_documents: crate::ai_tools::DocumentDirectory,
    /// The search index, for the assistant's library search.
    search_index: crate::search_tools::IndexHandle,
    /// What agents changed, for review and undo.
    pub(crate) agent_changes: crate::ai_changes::ChangeLog,
    /// The MCP server for external agents, started when first needed.
    mcp: Option<glyphst_agent_host::mcp::McpServer>,
    /// The MCP token of each conversation's latest external turn.
    mcp_tokens: HashMap<u64, String>,
    /// The coding agents found on this machine.
    pub(crate) agent_programs: Vec<glyphst_agent_host::external::AgentSpec>,
    agent_search: Option<std::sync::mpsc::Receiver<Vec<glyphst_agent_host::external::AgentSpec>>>,
    pub(crate) launch_ask: Option<String>,
    pub(crate) launch_demo: Option<String>,
    pub(crate) launch_commands: Vec<Command>,
    pub(crate) launch_region: Option<(usize, [f32; 4])>,
    page_input: String,
    saved_positions: HashMap<u64, DocumentPosition>,
    last_position_save: Instant,
    /// Scripted runs neither save preferences nor touch the library.
    persist: bool,
    pub(crate) harness: Option<Harness>,
    pub(crate) first_page_ms: Option<f64>,
    /// When a preview part was first drawable, for `--bench`.
    pub(crate) first_part_ms: Option<f64>,
    pub(crate) upload_ms: Vec<f64>,
    /// Per uploaded page, milliseconds until it first showed (coarse) and
    /// until it was complete.
    pub(crate) upload_visible_ms: Vec<f64>,
    pub(crate) upload_complete_ms: Vec<f64>,
    /// Actions queued for the next frame of the document view.
    pub(crate) pending_actions: Vec<ViewAction>,
    chrome: chrome::StageChrome,
    /// What the toast's `[ undo ]` takes back.
    toast_undo: Option<ToastUndo>,
    /// The presentation's tools and clock while presenting.
    pub(crate) presenting: Option<crate::present::Presenting>,
    /// The user's effects folder, what it held when last loaded, and when
    /// it was last looked at.
    pub(crate) effects_dir: Option<PathBuf>,
    effects_stamp: Vec<(PathBuf, Option<std::time::SystemTime>, u64)>,
    effects_checked: Instant,
    /// The wallpaper being read, and the one the GPU has.
    wallpaper_loading: Option<crate::wallpaper::Loading>,
    wallpaper_shown: Option<String>,
    /// The window had the keyboard focus last frame (files changed while
    /// it was away are reloaded when it comes back).
    was_focused: bool,
    /// Home's document count and their size on disk.
    library_size: Option<(usize, u64)>,
    /// The span of reading running, for the activity log.
    reading: crate::reading_clock::ReadingClock,
    pub(crate) keymap: Keymap,
    /// The start of a key sequence and a count typed so far.
    pub(crate) pending_keys: keys::Pending,
    pub(crate) palette: Palette,
    pub(crate) settings_window: SettingsWindow,
    /// Tabs that are not documents: shortcuts, new tabs, reading activity.
    pages: Vec<Page>,
    /// The page tab shown; `active` is then `None`, so everything for
    /// documents treats it like home.
    page_focused: Option<usize>,
    /// Frame 01 of the design in place of home (`--gallery`).
    gallery: Option<glyphst_ui::gallery::Gallery>,
    /// The document tab to go back to when a page tab shown closes.
    page_return: Option<u64>,
    /// First-page covers for home and the launcher.
    covers: crate::covers::Covers,
    /// Files coming into the library (dropped, or added as they open).
    importer: crate::import::Importer,
    /// Metadata lookups and the library model's pass, in the background.
    library_jobs: crate::library_jobs::LibraryJobs,
    /// The settings the library jobs last ran with; a change lets a paused
    /// AI pass try again.
    library_jobs_settings: Option<std::sync::Arc<glyphst_settings::Settings>>,
    /// Item notes in the Markdown vault.
    vault_sync: crate::vault_sync::VaultSync,
    /// The sidebar's item card (its note, read once per item).
    sidebar_card: crate::library_tab::LibraryTab,
    /// The library's dialog showing (frames 04h, 04k, 06d).
    library_dialog: Option<crate::library_dialogs::Dialog>,
    /// The library's files moving to the folder chosen.
    folder_move: Option<crate::library_dialogs::FolderMove>,
    /// Works added by identifier, their PDFs downloading.
    work_adds: Vec<crate::library_dialogs::WorkAdd>,
    /// Importing from Zotero (frame 04j).
    zotero: Option<Box<crate::zotero_import::ZoteroImport>>,
    /// The library's text search (frame 04n); started when the app keeps
    /// its data, or by a demo.
    library_search: Option<crate::library_search::SearchService>,
    /// Search by meaning's engine and model, downloaded when asked
    /// (frame 04o).
    meaning: glyphst_embed::setup::Setup,
    /// The `llama-server` path `meaning` was made with.
    meaning_engine: String,
    /// Its engine, once downloaded.
    meaning_runtime: crate::meaning::MeaningRuntime,
    /// The reranker's model (on the same engine) and its engine.
    rerank: glyphst_embed::setup::Setup,
    rerank_runtime: crate::meaning::RerankRuntime,
}

impl ReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the reader needs eframe's wgpu renderer"))?;
        let user_effects = dirs::config_dir().map(|dir| dir.join("glyphst").join("effects"));
        ReaderGpu::install(&render_state, user_effects.as_deref());
        let effects_stamp = user_effects
            .as_deref()
            .map(glyphst_effects::user_stamp)
            .unwrap_or_default();
        egui_extras::install_image_loaders(&cc.egui_ctx);
        // Ctrl +/- and Ctrl+0 are commands that change the ui scale
        // preference, not egui's own zoom, so Settings shows what they did.
        cc.egui_ctx
            .options_mut(|options| options.zoom_with_keyboard = false);
        let persist = launch.harness.is_none();
        // Screens never read the real pads, so a pad on the desk can't
        // change them.
        let pads = match &launch.pad_demo {
            Some(script) => Pads::scripted(
                crate::pad::Script::parse(script, launch.pad_family)
                    .map_err(|error| anyhow::anyhow!("--pad-demo: {error}"))?,
            ),
            None if !persist => Pads::scripted(crate::pad::Script::default()),
            None => Pads::start(&cc.egui_ctx),
        };
        let mut pads = pads;
        // Screens play a script once the pages are drawn; by hand it plays
        // at once.
        if persist {
            pads.play();
        }
        let mut prefs = Prefs::load(cc.storage);
        // Before the first frame: every text style names these families.
        crate::fonts::install(&cc.egui_ctx, &prefs.fonts);
        let fonts_applied = Some(prefs.fonts.clone());
        if let Some(background) = launch.background {
            prefs.background = background;
        }
        if let Some(theme) = launch.theme {
            prefs.theme = theme;
        }
        if let Some(colours) = launch.colours {
            prefs.colours = colours;
        }
        if let Some(transition) = launch.transition {
            // Book switches and page turns both.
            prefs.transition = (transition != "none").then_some(transition);
            prefs.switch_transition = prefs.transition.clone();
        }
        if launch.reduce_motion {
            prefs.reduce_motion = true;
        }
        if let Some(layout) = launch.switcher_layout {
            prefs.switcher_layout = layout;
        }
        if let Some(overlay) = launch.overlay {
            let tint = (overlay != "none").then_some(overlay);
            prefs.tint_light = tint.clone();
            prefs.tint_dark = tint;
        }
        if let Some(wallpaper) = launch.wallpaper {
            prefs.wallpaper = Some(wallpaper);
        }
        if let Some(params) = launch.background_params {
            prefs.effect_params.insert(prefs.background.clone(), params);
        }
        if launch.gamepad_mode {
            prefs.gamepad_mode = true;
        }
        if let Some(tab) = launch.sidebar {
            prefs.sidebar = tab;
            prefs.show_sidebar = true;
        }
        // Scripted runs and fakes keep settings and keys in memory, so fake
        // providers never reach the user's config or keychain.
        let user_settings =
            if launch.translate_fake || launch.ai_fake || launch.opacity.is_some() || !persist {
                let mut settings = glyphst_settings::Settings::default();
                if let Some(opacity) = launch.opacity {
                    settings.window.set_transparent(true);
                    settings.window.opacity = opacity;
                    settings.window.sanitize();
                }
                if launch.translate_fake {
                    let port = crate::fake_providers::start_translation_server()?;
                    crate::fake_providers::use_fake_translation(&mut settings, port);
                }
                if launch.ai_fake {
                    let port = crate::fake_providers::start_ai_server()?;
                    crate::fake_providers::use_fake_ai(&mut settings, port);
                }
                UserSettings::in_memory(&cc.egui_ctx, settings, Default::default())
            } else {
                UserSettings::load(&cc.egui_ctx)
            };
        let translator = Translator::new(persist, false);
        let chatgpt = {
            let ctx = cc.egui_ctx.clone();
            ChatGptSubscription::start(user_settings.store(), move || ctx.request_repaint())
        };
        let (agent_bridge, agent_calls) = crate::ai_bridge::UiBridge::new(&cc.egui_ctx);
        let prefs_engine = prefs.library.llama_server.clone();
        let meaning = if persist {
            crate::meaning::setup(Some(prefs.library.llama_server.as_str()))
        } else {
            // Screens show a state, never the user's downloads.
            glyphst_embed::setup::Setup::showing(glyphst_embed::setup::SetupState::Offer {
                kept: 0,
                total: glyphst_embed::manifest::MODEL.size + 31_000_000,
            })
        };
        let rerank = if persist {
            crate::meaning::rerank_setup(Some(prefs.library.llama_server.as_str()))
        } else {
            glyphst_embed::setup::Setup::showing(glyphst_embed::setup::SetupState::Offer {
                kept: 0,
                total: glyphst_embed::manifest::RERANKER.size,
            })
        };
        let mut app = Self {
            prefs,
            theme_applied: None,
            theme_colours: crate::book_effects::theme_colours(&Tokens::for_kind(ThemeKind::Light)),
            theme_bg: Tokens::for_kind(ThemeKind::Light).bg_rgb(),
            fonts_applied,
            desktop_theme: {
                let ctx = cc.egui_ctx.clone();
                DesktopTheme::start(move || ctx.request_repaint())
            },
            desktop_dark: None,
            system_theme: None,
            window_blur: WindowBlur::default(),
            library: LibraryStore::load(),
            tabs: Vec::new(),
            active: None,
            panes: Panes::default(),
            next_tab_id: 1,
            home: Home::default(),
            pickers: Pickers::default(),
            render_state,
            transition: None,
            // Screenshots repeat their random picks.
            transition_rng: if launch.harness.is_some() {
                Rng::new(1)
            } else {
                Rng::from_time()
            },
            last_random: None,
            next_origin: None,
            click: None,
            switcher: crate::switcher::Switcher::default(),
            switcher_cards: std::collections::HashMap::new(),
            switcher_places: Vec::new(),
            switcher_hover: None,
            switcher_click: None,
            generations: Generations::default(),
            started: Instant::now(),
            instance: if persist {
                crate::instance::Instance::listen(&crate::instance::socket_path(), &cc.egui_ctx)
                    .map_err(|error| log::info!("links from other apps go elsewhere: {error}"))
                    .ok()
            } else {
                None
            },
            pending_links: launch
                .urls
                .iter()
                .filter_map(|url| crate::links::parse_link(url))
                .collect(),
            link_handler: None,
            neighbours_seen: None,
            page_writer: Default::default(),
            page_writing: Vec::new(),
            page_topics: Vec::new(),
            demo_vault: false,
            pads,
            pad_router: Router::default(),
            pad_keymap: Default::default(),
            pad_input: GamepadInput::default(),
            pad_commands: Vec::new(),
            pad_ring_steps: Vec::new(),
            pad_ring: None,
            pad_events: Vec::new(),
            pad_focused: false,
            prompts: Default::default(),
            couch: Default::default(),
            fixed_clock: launch.fixed_clock,
            pad_pointer: None,
            last_input: Default::default(),
            frames: FrameTimes::default(),
            continuous: false,
            repaints: 0,
            fullscreen: false,
            notices: Default::default(),
            show_stats: launch.harness.is_some(),
            zen: false,
            region_tool: false,
            region: None,
            region_jobs: Vec::new(),
            region_save: None,
            region_for_assistant: false,
            quick_ask: QuickAsk::default(),
            sidebar_state: SidebarState::default(),
            drag: None,
            note_editor: None,
            context_page: None,
            launch_find: launch.find,
            launch_select: launch.select_pages,
            launch_translate_document: launch.translate_document,
            launch_translate_selection: launch.translate_selection,
            font_size: launch.font_size,
            translator,
            user_settings,
            chatgpt,
            assistant: Assistant::default(),
            agent_bridge,
            agent_calls,
            agent_documents: Default::default(),
            search_index: Default::default(),
            agent_changes: Default::default(),
            mcp: None,
            mcp_tokens: HashMap::new(),
            agent_programs: Vec::new(),
            agent_search: Some(crate::app::agent::find_agents(&cc.egui_ctx)),
            launch_ask: launch.ask,
            launch_demo: launch.assistant_demo,
            launch_commands: launch.commands,
            launch_region: launch.select_region,
            page_input: String::new(),
            saved_positions: HashMap::new(),
            last_position_save: Instant::now(),
            persist,
            harness: launch.harness,
            first_page_ms: None,
            first_part_ms: None,
            upload_ms: Vec::new(),
            upload_visible_ms: Vec::new(),
            upload_complete_ms: Vec::new(),
            pending_actions: Vec::new(),
            chrome: Default::default(),
            toast_undo: None,
            presenting: None,
            effects_dir: user_effects,
            effects_stamp,
            effects_checked: Instant::now(),
            wallpaper_loading: None,
            wallpaper_shown: None,
            was_focused: true,
            library_size: None,
            reading: Default::default(),
            keymap: Keymap::default(),
            pending_keys: keys::Pending::default(),
            palette: Palette::default(),
            settings_window: SettingsWindow::default(),
            pages: Vec::new(),
            page_focused: None,
            gallery: launch.gallery.then(Default::default),
            page_return: None,
            covers: crate::covers::Covers::new(&cc.egui_ctx),
            importer: crate::import::Importer::default(),
            library_jobs: crate::library_jobs::LibraryJobs::new(&cc.egui_ctx),
            library_jobs_settings: None,
            vault_sync: crate::vault_sync::VaultSync::new(&cc.egui_ctx),
            sidebar_card: Default::default(),
            library_dialog: None,
            folder_move: None,
            work_adds: Vec::new(),
            zotero: None,
            library_search: None,
            meaning_engine: prefs_engine,
            meaning_runtime: Default::default(),
            rerank,
            rerank_runtime: Default::default(),
            meaning,
        };
        app.rebuild_keymap();
        if let Some(state) = &launch.library_demo {
            app.library = crate::library_demo::library(launch.files.first().map(PathBuf::as_path));
            // With a document the run shows it (its sidebar's item tab).
            if launch.files.is_empty() {
                app.open_library();
            }
            let mut demo_merge = None;
            // Pages, related items and backlinks (04p, 04q, 04f).
            if matches!(state.as_str(), "item" | "pages" | "page" | "page-cost")
                && let Some((root, neighbours)) = crate::library_demo::knowledge(&mut app.library)
            {
                app.prefs.library.vault = Some(root.display().to_string());
                app.demo_vault = true;
                app.neighbours_seen = Some(std::sync::Arc::new(neighbours));
            }
            if let Some(Page::Library(tab)) = app.pages.last_mut() {
                match state.as_str() {
                    "pages" | "page-cost" => tab.view = crate::library_tab::View::Pages,
                    "page" => tab.view = crate::library_tab::View::Page(1),
                    "item" => {
                        let id = crate::library_demo::first_item(&app.library);
                        tab.view = id
                            .and_then(|id| {
                                app.library.collections_for_item(id).first().map(|c| c.id)
                            })
                            .map(crate::library_tab::View::Collection)
                            .unwrap_or_default();
                        tab.selected = id;
                    }
                    "review" => tab.view = crate::library_tab::View::NeedsReview,
                    search if search.starts_with("search") => {
                        tab.filter = "multi-head attention".to_string();
                        tab.selected = crate::library_demo::first_item(&app.library);
                    }
                    "duplicate" | "merged" => {
                        let first = crate::library_demo::first_item(&app.library);
                        let copy = crate::library_demo::duplicate(&mut app.library);
                        tab.view = first
                            .and_then(|id| {
                                app.library.collections_for_item(id).first().map(|c| c.id)
                            })
                            .map(crate::library_tab::View::Collection)
                            .unwrap_or_default();
                        tab.selected = copy;
                        if state == "merged" {
                            demo_merge = first.zip(copy);
                        }
                    }
                    _ => {}
                }
            }
            // The search reads the demo's index, partly read and paused.
            // Search by meaning's card: hidden in plain search (04n), and
            // as offered, downloading or stopped (04o).
            use glyphst_embed::setup::{Phase, SetupError, SetupState};
            app.prefs.library.meaning_offer_hidden = state == "search";
            let (done, total) = (142_000_000, 341_000_000);
            match state.as_str() {
                "search-downloading" => {
                    app.meaning = glyphst_embed::setup::Setup::showing(SetupState::Running {
                        phase: Phase::Model,
                        done,
                        total,
                        rate: 4_100_000.0,
                        retry_in: None,
                    });
                }
                "search-failed" => {
                    app.meaning = glyphst_embed::setup::Setup::showing(SetupState::Failed {
                        error: SetupError::Network("connection reset".to_string()),
                        done,
                        total,
                    });
                }
                _ => {}
            }
            if state.starts_with("search")
                && let Some(path) = crate::library_demo::search_index(&app.library)
            {
                match crate::library_search::SearchService::start(&path, &cc.egui_ctx) {
                    Ok(mut service) => {
                        service.set_paused(true);
                        if state == "search" {
                            service.set_reranker(Some(std::sync::Arc::new(
                                crate::library_demo::DemoReranker,
                            )));
                        }
                        app.search_index
                            .set_service(Some((path, Some(service.tool_searcher()))));
                        app.library_search = Some(service);
                    }
                    Err(error) => eprintln!("failed to start the demo search: {error:#}"),
                }
            }
            // Merged as the item pane's `[ merge ]` does, toast and all.
            if let Some((keep, absorb)) = demo_merge {
                app.library_tab_action(
                    crate::library_tab::LibraryTabAction::Merge { keep, absorb },
                    &cc.egui_ctx,
                );
            }
            app.library_dialog = match state.as_str() {
                "backfill" => app.backfill_dialog(None),
                "folder" => Some(crate::library_dialogs::Dialog::ChooseFolder {
                    path: "~/Documents/Glyphst".to_string(),
                    files: 128,
                    bytes: 2_400_000_000,
                    linked: 9,
                    error: None,
                }),
                "identifier" => Some(crate::library_dialogs::Dialog::add_by_identifier(None)),
                "page-cost" => {
                    crate::knowledge::suggest_topics(&app.library, app.neighbours_seen.as_deref())
                        .into_iter()
                        .next()
                        .and_then(|topic| {
                            app.page_dialog(&crate::knowledge::PageAsk::Topic(topic))
                                .map_err(|error| eprintln!("no page dialog: {error}"))
                                .ok()
                        })
                }
                _ => None,
            };
            if state.starts_with("zotero") {
                let mut zotero = crate::zotero_import::ZoteroImport::with_data_dir(
                    crate::library_demo::zotero_folder(),
                    &cc.egui_ctx,
                );
                if state == "zotero-done" {
                    // Into an empty library on disk (highlights need one),
                    // so all of it is new.
                    let folder = std::env::temp_dir()
                        .join(format!("glyphst-zotero-target-{}", std::process::id()));
                    let _ = std::fs::remove_dir_all(&folder);
                    app.library = glyphst_library::LibraryStore::open_apart(
                        folder.join("library.redb"),
                        folder.join("files"),
                    )
                    .unwrap_or_else(|error| {
                        eprintln!("failed to open the demo library: {error:#}");
                        glyphst_library::LibraryStore::memory()
                    });
                    zotero.import_now(&app.library, &cc.egui_ctx);
                }
                app.zotero = Some(Box::new(zotero));
            }
        }
        if let Some(query) = launch.palette {
            app.palette.toggle();
            app.palette.query = query;
        }
        if launch.shortcuts {
            app.open_keymap_tab();
            // `--settings shortcuts:gamepad` shows the pad's side.
            if launch.settings_group.as_deref() == Some("gamepad")
                && let Some(Page::Keymap(page)) = app.pages.last_mut()
            {
                page.gamepad = true;
            }
        }
        if let Some(section) = launch.settings {
            app.settings_window.show_section(section);
            app.settings_window.scroll_to = launch.settings_group;
            // Scripted runs show the fake model's list.
            if section == Section::Ai
                && launch.ai_fake
                && let Some(settings) = app.user_settings.get().cloned()
            {
                let provider = settings.ai.provider_order[0];
                app.settings_window
                    .fetch_models(&settings, provider, &cc.egui_ctx);
            }
        }
        let ctx = cc.egui_ctx.clone();
        if launch.files.is_empty() && app.prefs.reopen_last_session && persist {
            let session = app.prefs.last_session.clone();
            for file in &session.files {
                app.open_quietly(Path::new(&file.path), &ctx, Some(file.page));
            }
            app.active = session.active.filter(|&active| active < app.tabs.len());
            // Panes only when every document opened again, so the indices
            // still match.
            let ids: Vec<u64> = app.tabs.iter().map(|tab| tab.id).collect();
            if ids.len() == session.files.len()
                && let Some(layout) = session
                    .panes
                    .as_ref()
                    .and_then(|layout| panes::map_layout(layout, |&index| ids.get(index).copied()))
            {
                let focused = app.active.map(|active| ids[active]);
                app.panes = Panes::restore(layout, focused);
            }
            if let Some(active) = app.active {
                app.panes.show(ids[active]);
            }
        } else {
            let page = launch
                .select_pages
                .map(|(first, _)| first)
                .or(launch.page)
                .map(|page| page - 1);
            for file in &launch.files {
                app.open_quietly(file, &ctx, page);
                if let Some(tab) = app.tabs.last_mut() {
                    if let Some(arrangement) = launch.arrangement {
                        tab.arrange(arrangement);
                    }
                    if let Some(zoom) = launch.zoom {
                        tab.set_initial_zoom(zoom);
                    }
                }
            }
            if let (Some(zone), [first, second, ..]) = (launch.split, app.tabs.as_slice()) {
                let (first, second) = (first.id, second.id);
                app.panes.show(first);
                app.panes.split(second, first, zone);
                app.active = Some(1);
            }
        }
        app.transition = None;
        for (number, id) in &launch.book_backgrounds {
            if let Some(index) = number
                .checked_sub(1)
                .filter(|index| *index < app.tabs.len())
            {
                let key = app.book_key(index);
                app.prefs.books.entry(key).or_default().background =
                    crate::book_effects::Choice::Effect(id.clone());
            }
        }
        app.assistant.agent = launch.agent;
        Ok(app)
    }

    pub(crate) fn with_gpu<R>(&self, f: impl FnOnce(&mut ReaderGpu) -> R) -> Option<R> {
        let mut renderer = self.render_state.renderer.write();
        renderer.callback_resources.get_mut::<ReaderGpu>().map(f)
    }

    fn is_resident(&self, key: PageKey) -> bool {
        self.with_gpu(|gpu| gpu.is_resident(key)).unwrap_or(false)
    }

    /// Opens `path` in a new tab (or shows the tab that has it) and makes it
    /// active. `page` overrides the remembered position.
    pub fn open(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.activate(Some(index));
            return;
        }
        let remember_zoom = self.prefs.remember_zoom;
        let mut restore = self
            .library
            .history_position(path)
            .map(|position| opening_zoom(position, remember_zoom));
        if let Some(page) = page {
            let mut position =
                restore.unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, 1.0, page + 1));
            position.page = page + 1;
            restore = Some(position);
        }
        let layout = self.layout_for(path, ctx);
        let arrangement = self.arrangement_of(path);
        match DocumentTab::open(self.next_tab_id, path, ctx, restore, layout, arrangement) {
            Ok(mut tab) => {
                tab.annotations = Annotations::load(&self.library, path);
                self.next_tab_id += 1;
                self.tabs.push(tab);
                if self.persist {
                    let _ = self.library.mark_library_item_opened(path);
                    // Settings › Library › when a file is opened.
                    if self.prefs.library.add_on_open == crate::prefs::AddOnOpen::InPlace
                        && crate::import::is_library_document(path)
                        && self.library.library_item_for_path(path).is_none()
                    {
                        self.importer.start(
                            &self.library,
                            vec![path.to_path_buf()],
                            crate::prefs::ImportMode::Link,
                            None,
                            true,
                            ctx,
                        );
                    }
                }
                self.activate(Some(self.tabs.len() - 1));
                let tab = &self.tabs[self.tabs.len() - 1];
                let (page, pages) = (tab.current_page(), tab.layout.len());
                self.prefs
                    .page_counts
                    .insert(path.display().to_string(), pages);
                self.log_activity(ActivityKind::Open, path, format!("p.{}", page + 1), 0);
                // Translate as I read: a PDF starts translating as it opens.
                let is_pdf = path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
                if self.prefs.translate_as_read && is_pdf && self.user_settings.get().is_some() {
                    self.start_document_translation(ctx);
                }
            }
            Err(error) => self
                .notices
                .push(format!("Could not open {}: {error:#}", path.display())),
        }
    }

    /// Opens without a transition (startup, session restore).
    fn open_quietly(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        self.open(path, ctx, page);
        self.transition = None;
    }

    /// Makes another tab active: it shows in the focused pane (or its own
    /// pane gets the focus), with the book-switch transition from the
    /// document it replaces (or from nothing, coming from Home).
    pub(crate) fn activate(&mut self, next: Option<usize>) {
        self.activate_with(next, Switch::default());
    }

    /// [`Self::activate`] with where the switch comes from.
    pub(crate) fn activate_with(&mut self, next: Option<usize>, switch: Switch) {
        self.page_focused = None;
        if next == self.active {
            return;
        }
        let previous = self.active;
        // The book left keeps a card of where it was, for the switcher.
        if let Some(previous) = previous {
            self.capture_card(self.tabs[previous].id);
        }
        self.active = next;
        let Some(next) = next else {
            return;
        };
        if !switch.peek {
            self.couch.book_opened = true;
        }
        let tab = self.tabs[next].id;
        let replaced = self.panes.show(tab);
        let capture = match (previous, replaced) {
            (None, _) => Capture::Clear,
            (Some(_), Some(from)) => Capture::View(from),
            // Already on the stage in another pane: only the focus moves.
            (Some(_), None) => return,
        };
        let direction = switch.direction.unwrap_or_else(|| {
            let from = previous.filter(|_| replaced.is_some());
            if from.is_none_or(|from| next >= from) {
                1.0
            } else {
                -1.0
            }
        });
        let origin = switch.origin.or(self.next_origin.take()).or(self.click);
        let from_background = self.background_for(previous.filter(|_| replaced.is_some()));
        self.begin_switch(
            next,
            capture,
            from_background,
            direction,
            origin,
            switch.speed,
        );
    }

    /// Starts the book-switch transition into tab `index` from `capture`.
    /// Going back to the book a running switch left plays it backwards;
    /// another switch while one runs starts from what is on screen, with
    /// the same effect.
    /// `from_background` is the background of what it leaves; the
    /// transition sweeps it too when it differs from the new book's.
    fn begin_switch(
        &mut self,
        index: usize,
        capture: Capture,
        from_background: EffectUse,
        direction: f32,
        origin: Option<egui::Pos2>,
        speed: f32,
    ) {
        let tab = self.tabs[index].id;
        let now = Instant::now();
        let background = self.background_for(Some(index));
        let sweep = (from_background != background).then_some(from_background);
        if let Some(running) = self.transition.as_mut().filter(|t| !t.finished(now)) {
            let from = match capture {
                Capture::View(from) => Some(from),
                _ => None,
            };
            if running.from_tab == Some(tab) && from == Some(running.tab) {
                running.reverse(now);
                running.from_background = sweep;
                return;
            }
            if from == Some(running.tab) {
                let composite = running.composite(now);
                let planned = running.planned();
                let mut transition =
                    ViewTransition::new(planned, composite, tab, direction, origin, now)
                        .faster(speed);
                transition.from_background = sweep;
                self.transition = Some(transition);
                return;
            }
        }
        self.transition = None;
        let Some(choice) = self.effects_for(Some(index)).switch else {
            return;
        };
        let reduce_motion = self.prefs.reduce_motion;
        let saved = &self.prefs.effect_params;
        let (rng, last) = (&mut self.transition_rng, self.last_random.as_deref());
        let planned = {
            let renderer = self.render_state.renderer.read();
            renderer
                .callback_resources
                .get::<ReaderGpu>()
                .and_then(|gpu| {
                    crate::transition::plan(
                        &choice,
                        gpu.effects().defs(),
                        saved,
                        reduce_motion,
                        rng,
                        last,
                    )
                })
        };
        let Some(planned) = planned else {
            return;
        };
        if choice.id == glyphst_effects::RANDOM {
            self.last_random = Some(planned.effect.id.clone());
        }
        let mut transition =
            ViewTransition::new(planned, capture, tab, direction, origin, now).faster(speed);
        transition.from_background = sweep;
        self.transition = Some(transition);
    }

    /// Opens the Keyboard shortcuts tab, or goes to it.
    pub(crate) fn open_keymap_tab(&mut self) {
        self.open_page(Page::Keymap(KeymapPage::default()));
    }

    /// Opens a page tab and shows it; one of a kind there is only one of
    /// is shown again instead.
    pub(crate) fn open_page(&mut self, page: Page) -> usize {
        let index = match self
            .pages
            .iter()
            .position(|open| page.single() && open.same_kind(&page))
        {
            Some(index) => index,
            None => {
                self.pages.push(page);
                self.pages.len() - 1
            }
        };
        self.focus_page(index);
        index
    }

    /// Shows page tab `index`.
    pub(crate) fn focus_page(&mut self, index: usize) {
        if self.page_focused.is_none() {
            self.page_return = self.active.map(|active| self.tabs[active].id);
        }
        self.activate(None);
        self.gallery = None;
        self.page_focused = Some(index);
    }

    /// The reading activity tab, at `day`.
    pub(crate) fn open_activity(&mut self, day: Option<chrono::NaiveDate>) {
        let index = self.open_page(Page::Activity(ActivityView::at(day)));
        if let Some(Page::Activity(view)) = self.pages.get_mut(index)
            && day.is_some()
        {
            view.select(day);
        }
    }

    /// The library tab. The first time, it asks where the library should
    /// live (frame 04k).
    pub(crate) fn open_library(&mut self) {
        self.open_page(Page::Library(Box::default()));
        if self.persist
            && !self.prefs.library.folder_asked
            && !self.library.files_root_chosen()
            && self.library.files_root().is_some()
            && self.library_dialog.is_none()
        {
            self.library_dialog = Some(self.choose_folder_dialog());
        }
    }

    /// Frame 04k with what a move would take.
    fn choose_folder_dialog(&self) -> crate::library_dialogs::Dialog {
        let attachments = self.library.library_attachments();
        let managed = attachments.iter().filter(|a| !a.is_linked());
        crate::library_dialogs::Dialog::ChooseFolder {
            path: "~/Documents/Glyphst".to_string(),
            files: managed.clone().count(),
            bytes: managed.map(|attachment| attachment.file_size).sum(),
            linked: attachments.iter().filter(|a| a.is_linked()).count(),
            error: None,
        }
    }

    /// Frame 04h: the items without an AI pass (of `only`, when given) and
    /// what it would take.
    fn backfill_dialog(
        &self,
        only: Option<&[glyphst_library::DocumentId]>,
    ) -> Option<crate::library_dialogs::Dialog> {
        let settings = self.user_settings.get()?;
        let model = settings.ai.resolved_library()?;
        let items = self
            .library
            .library_items()
            .iter()
            .filter(|item| {
                item.trashed_at.is_none()
                    && self.library.item_enrichment(item.id).is_none()
                    && only.is_none_or(|only| only.contains(&item.id))
            })
            .collect::<Vec<_>>();
        let (tokens_in, tokens_out) = crate::library_jobs::enrich_estimate(&self.library, &items);
        Some(crate::library_dialogs::Dialog::Backfill {
            items: items.iter().map(|item| item.id).collect(),
            model: model.model,
            tokens_in,
            tokens_out,
            create_tags: self.prefs.library.ai_create_tags,
            suggest_collections: self.prefs.library.suggest_collections,
        })
    }

    /// Shows the library's dialog, follows a move and adds works found by
    /// identifier.
    fn library_dialogs(&mut self, ctx: &egui::Context) {
        use crate::library_dialogs::{Dialog, DialogAction, MoveEvent};
        if let Some(moving) = &self.folder_move {
            let plan = moving.plan.clone();
            for event in moving.events() {
                match event {
                    MoveEvent::Copied(done, total) => {
                        self.library_dialog = Some(Dialog::Moving { done, total });
                    }
                    MoveEvent::Done(result) => {
                        let outcome = result
                            .map_err(anyhow::Error::msg)
                            .and_then(|()| self.library.commit_files_move(&plan));
                        match outcome {
                            Ok(moved) => self.notices.push(format!(
                                "The library is in {} ({} files).",
                                plan.new_root().display(),
                                moved.files
                            )),
                            Err(error) => self
                                .notices
                                .push(format!("The library stayed where it was: {error:#}")),
                        }
                        self.prefs.library.folder_asked = true;
                        self.library_dialog = None;
                    }
                }
            }
            if self.library_dialog.is_none() {
                self.folder_move = None;
            }
        }
        let mut finished = Vec::new();
        for (index, add) in self.work_adds.iter().enumerate() {
            if let Some(ready) = add.take() {
                finished.push((index, ready));
            }
        }
        for (index, (work, prepared)) in finished.into_iter().rev() {
            self.work_adds.remove(index);
            if let Err(error) = self.add_work(*work, prepared) {
                self.notices.push(format!("Could not add it: {error:#}"));
            }
        }
        if self.zotero.is_some() {
            self.zotero_dialog(ctx);
            return;
        }
        let Some(dialog) = &mut self.library_dialog else {
            return;
        };
        let (action, close) = dialog.show(ctx, &self.library);
        // What the backfill dialog's options were set to stays.
        if let Dialog::Backfill {
            create_tags,
            suggest_collections,
            ..
        } = dialog
        {
            self.prefs.library.ai_create_tags = *create_tags;
            self.prefs.library.suggest_collections = *suggest_collections;
        }
        if close {
            self.library_dialog = None;
        }
        let result = match action {
            None => Ok(()),
            Some(DialogAction::Enrich(items)) => self
                .library
                .enqueue_library_jobs(
                    &items,
                    glyphst_library::LibraryJobKind::Enrich,
                    glyphst_library::LibraryJobOrigin::Backfill,
                    false,
                )
                .map(|_| ()),
            Some(DialogAction::LaterFolder) => {
                self.prefs.library.folder_asked = true;
                Ok(())
            }
            Some(DialogAction::UseFolder(path)) => match self.library.plan_files_move(&path) {
                Ok(plan) => {
                    self.library_dialog = Some(Dialog::Moving {
                        done: 0,
                        total: plan.files(),
                    });
                    self.folder_move = Some(crate::library_dialogs::FolderMove::start(plan, ctx));
                    Ok(())
                }
                Err(error) => {
                    if let Some(Dialog::ChooseFolder { error: shown, .. }) =
                        &mut self.library_dialog
                    {
                        *shown = Some(format!("{error:#}"));
                    }
                    Ok(())
                }
            },
            Some(DialogAction::StopMove) => {
                if let Some(moving) = &self.folder_move {
                    moving.stop();
                }
                Ok(())
            }
            Some(DialogAction::WritePage(job)) => {
                self.start_page(*job, ctx);
                Ok(())
            }
            Some(DialogAction::AddWork(work)) => {
                self.work_adds.push(crate::library_dialogs::WorkAdd::start(
                    *work,
                    self.library.files_root().map(std::path::Path::to_path_buf),
                    ctx,
                ));
                Ok(())
            }
        };
        if let Err(error) = result {
            self.notices.push(format!("{error:#}"));
        }
    }

    /// Frame 04j, and what it asks when it closes.
    fn zotero_dialog(&mut self, ctx: &egui::Context) {
        use crate::zotero_import::ZoteroAction;
        let Some(zotero) = &mut self.zotero else {
            return;
        };
        let vault = self
            .prefs
            .library
            .vault_root(self.library.files_root(), self.library.files_root_chosen());
        let (action, close) = zotero.show(ctx, &mut self.library, vault.as_deref());
        if close {
            self.zotero = None;
        }
        match action {
            None => {}
            Some(ZoteroAction::OpenLibrary) => self.open_library(),
            Some(ZoteroAction::Enrich(items)) => match self.backfill_dialog(Some(&items)) {
                Some(dialog) => self.library_dialog = Some(dialog),
                None => self
                    .notices
                    .push("Choose a library model in Settings › AI first.".to_string()),
            },
            Some(ZoteroAction::SaveReport(text)) => {
                let folder = vault
                    .clone()
                    .or_else(|| self.library.files_root().map(std::path::Path::to_path_buf));
                let saved = folder.map(|folder| {
                    let path = folder.join("zotero import.md");
                    std::fs::create_dir_all(&folder)
                        .and_then(|()| std::fs::write(&path, &text))
                        .map(|()| path)
                });
                self.notices.push(match saved {
                    Some(Ok(path)) => format!("The report is in {}.", path.display()),
                    Some(Err(error)) => format!("Could not save the report: {error}"),
                    None => "There is no library folder to save the report in.".to_string(),
                });
            }
        }
    }

    /// Adds a work found by its identifier: with its PDF when one was
    /// downloaded, else without a file; its metadata from the catalogue.
    fn add_work(
        &mut self,
        work: glyphst_metadata::WorkMetadata,
        prepared: Result<Option<glyphst_library::PreparedManagedImport>, String>,
    ) -> anyhow::Result<()> {
        let prepared = prepared.unwrap_or_else(|error| {
            self.notices.push(format!(
                "The PDF could not be downloaded ({error}); added without it."
            ));
            None
        });
        let metadata = glyphst_library::LibraryItemMetadata {
            title: work.title.clone(),
            ..Default::default()
        };
        let id = match prepared {
            Some(prepared) => {
                match self
                    .library
                    .finalize_managed_import(prepared, metadata, None)?
                {
                    glyphst_library::LibraryImportOutcome::Added(id)
                    | glyphst_library::LibraryImportOutcome::Duplicate(id)
                    | glyphst_library::LibraryImportOutcome::DuplicateInTrash(id) => id,
                }
            }
            None => self.library.add_metadata_item(metadata, None)?,
        };
        let recognized = glyphst_metadata::Recognized {
            work: Some(work),
            ..Default::default()
        };
        crate::library_jobs::apply_metadata(&mut self.library, id, &recognized);
        if self.prefs.library.enrich_new {
            self.library.enqueue_library_jobs(
                &[id],
                glyphst_library::LibraryJobKind::Enrich,
                glyphst_library::LibraryJobOrigin::Manual,
                false,
            )?;
        }
        self.notices.push("Added to the library.".to_string());
        Ok(())
    }

    /// Closes page tab `index`; when it was shown, the document shown
    /// before it comes back (or home).
    pub(crate) fn close_page(&mut self, index: usize) {
        if index >= self.pages.len() {
            return;
        }
        self.pages.remove(index);
        match self.page_focused {
            Some(focused) if focused == index => {
                self.page_focused = None;
                if let Some(id) = self.page_return.take() {
                    self.focus_tab(id);
                }
            }
            Some(focused) if focused > index => self.page_focused = Some(focused - 1),
            _ => {}
        }
    }

    /// Draws page tab `index` and does what it asks.
    fn show_page(&mut self, ui: &mut egui::Ui, index: usize, ctx: &egui::Context) {
        let meaning = self.meaning_card();
        let docs = || crate::home::documents(&self.library, &self.prefs.page_counts);
        match &mut self.pages[index] {
            Page::Keymap(page) => {
                if page.show(ui, &mut self.prefs, &self.keymap, &self.pad_keymap) {
                    self.rebuild_keymap();
                }
            }
            Page::Activity(view) => {
                let docs = docs();
                if let Some(ActivityAction::Open(path)) = view.ui(ui, &self.library, &docs) {
                    self.open(&path, ctx, None);
                }
            }
            Page::Library(tab) => {
                let docs = docs();
                let vault = self
                    .prefs
                    .library
                    .vault_root(self.library.files_root(), self.library.files_root_chosen());
                let vault_index = self.vault_sync.index();
                if tab.view == crate::library_tab::View::Pages {
                    self.page_topics = crate::knowledge::suggest_topics(
                        &self.library,
                        self.neighbours_seen.as_deref(),
                    );
                }
                let action = tab.ui(
                    ui,
                    crate::library_tab::TabContext {
                        library: &self.library,
                        covers: &mut self.covers,
                        docs: &docs,
                        vault: vault.as_deref(),
                        import: self.importer.progress(),
                        search: self.library_search.as_mut(),
                        meaning,
                        vault_index: Some(&vault_index),
                        topics: &self.page_topics,
                        writing: &self.page_writing,
                    },
                );
                if let Some(action) = action {
                    self.library_tab_action(action, ctx);
                }
            }
            Page::NewTab(launcher) => {
                let action = launcher.ui(
                    ui,
                    LauncherContext {
                        library: &self.library,
                        page_counts: &self.prefs.page_counts,
                        covers: &mut self.covers,
                        picking: self.pickers.is_showing(),
                        open_key: self.keymap.label(Command::OpenWithDialog),
                    },
                );
                match action {
                    // The new tab turns into the document.
                    Some(LauncherAction::Open(path)) => {
                        self.open(&path, ctx, None);
                        if self.tabs.iter().any(|tab| tab.path == path) {
                            self.pages.remove(index);
                            self.page_return = None;
                        }
                    }
                    Some(LauncherAction::OpenDialog) => self.pickers.open_documents(ctx),
                    Some(LauncherAction::Home) => self.activate(None),
                    Some(LauncherAction::Activity) => self.open_activity(None),
                    Some(LauncherAction::Library) => self.open_library(),
                    None => {}
                }
            }
        }
    }

    /// Does what the library tab asked.
    fn library_tab_action(
        &mut self,
        action: crate::library_tab::LibraryTabAction,
        ctx: &egui::Context,
    ) {
        use crate::library_tab::LibraryTabAction as Action;
        let result = match action {
            Action::Open(path) => {
                self.open(&path, ctx, None);
                Ok(())
            }
            Action::Ask { item, path } => {
                if let Some(path) = &path {
                    self.open(path, ctx, None);
                }
                if !self.assistant.open {
                    self.toggle_assistant();
                }
                self.assistant.ask_about(item, &self.library);
                Ok(())
            }
            Action::Enrich(id) => self
                .library
                .enqueue_library_jobs(
                    &[id],
                    glyphst_library::LibraryJobKind::Enrich,
                    glyphst_library::LibraryJobOrigin::Manual,
                    true,
                )
                .map(|_| ()),
            Action::Accept(id, target) => self.library.accept_assignment(id, &target),
            Action::Reject(id, target) => self.library.reject_assignment(id, &target),
            Action::OpenNote(path) => {
                ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}", path.display())));
                Ok(())
            }
            Action::AddOpened(paths) => {
                if self.persist {
                    self.importer.start(
                        &self.library,
                        paths,
                        crate::prefs::ImportMode::Link,
                        None,
                        false,
                        ctx,
                    );
                }
                Ok(())
            }
            Action::OpenDialog => {
                self.pickers.open_documents(ctx);
                Ok(())
            }
            Action::ShowInLibrary(id) => {
                self.open_library();
                if let Some(Page::Library(tab)) = self
                    .page_focused
                    .and_then(|index| self.pages.get_mut(index))
                {
                    tab.view = crate::library_tab::View::All;
                    tab.selected = Some(id);
                }
                Ok(())
            }
            Action::WritePage(ask) => {
                match self.page_dialog(&ask) {
                    Ok(dialog) => self.library_dialog = Some(dialog),
                    Err(error) => self.notices.push(error),
                }
                Ok(())
            }
            Action::DismissTopic(key) => self.library.dismiss_topic(&key),
            Action::KeepPage(id) => crate::page_writer::keep(&mut self.library, id),
            Action::UndoPage(id) => match self.vault_root() {
                Some(root) => crate::page_writer::undo(&mut self.library, &root, id),
                None => Err(anyhow::anyhow!("there is no vault")),
            },
            Action::ShowPage(id) => {
                self.show_concept_page(id);
                Ok(())
            }
            Action::AcceptRelated(a, b) => self.library.accept_related(a, b),
            Action::RejectRelated(a, b) => self.library.reject_related(a, b),
            Action::AddRelated(a, b) => self.library.add_related_pairs(&[(a, b)]).map(|_| ()),
            Action::RemoveRelated(a, b) => self.library.remove_related(a, b),
            Action::CancelImport => {
                if self.importer.is_running() {
                    self.importer.cancel();
                } else {
                    self.importer.dismiss();
                }
                Ok(())
            }
            Action::AcceptAll(ids) => self.review_all(&ids, true),
            Action::RejectAll(ids) => self.review_all(&ids, false),
            Action::KeepBoth(a, b) => self.library.mark_distinct(a, b),
            Action::Merge { keep, absorb } => {
                self.library.merge_library_items(keep, absorb).map(|edit| {
                    let title = self
                        .library
                        .library_item(keep)
                        .map(|item| item.title.clone())
                        .unwrap_or_default();
                    // `merged into “…” [ undo ]` (frame 04m).
                    self.notices.push_with(
                        format!("merged into “{title}”"),
                        glyphst_ui::overlay::Tone::Ok,
                        Some("undo"),
                    );
                    self.toast_undo = Some(ToastUndo::Merge(Box::new(edit)));
                })
            }
            Action::TagColor(tag, color) => self.library.set_tag_color(tag, color),
            Action::OpenAt {
                path,
                page,
                anchor,
                query,
            } => {
                if let Some(query) = query
                    && self.prefs.library.learn_from_opened
                    && self.persist
                    && let Some(search) = &self.library_search
                    && let Err(error) = search.record_opened(&query, &path, page)
                {
                    log::warn!("could not keep the search opened: {error:#}");
                }
                self.open(&path, ctx, Some(page));
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.path == path) {
                    tab.go_to_passage(page, anchor);
                }
                Ok(())
            }
            Action::Meaning(action) => {
                self.meaning_action(action, ctx);
                Ok(())
            }
            Action::AskLibrary(words) => {
                // A new conversation about the library, the question ready
                // to edit (frame 05 · ask the library).
                if !self.assistant.open {
                    self.toggle_assistant();
                }
                self.assistant.new_session();
                self.assistant.compose(
                    ctx,
                    crate::ai_editor::Draft::plain(format!(
                        "What does my library say about {words}?"
                    )),
                );
                Ok(())
            }
            Action::AddByIdentifier => {
                let mailto = Some(self.prefs.library.contact_email.trim().to_string())
                    .filter(|address| !address.is_empty());
                self.library_dialog =
                    Some(crate::library_dialogs::Dialog::add_by_identifier(mailto));
                Ok(())
            }
            Action::Backfill => {
                match self.backfill_dialog(None) {
                    Some(dialog) => self.library_dialog = Some(dialog),
                    None => self
                        .notices
                        .push("Choose a library model in Settings › AI first.".to_string()),
                }
                Ok(())
            }
            Action::ImportZotero => {
                if self.zotero.is_none() {
                    self.zotero = Some(Box::new(crate::zotero_import::ZoteroImport::new(ctx)));
                }
                Ok(())
            }
        };
        if let Err(error) = result {
            self.notices.push(format!("{error:#}"));
        }
    }

    /// Search by meaning's card for the search results: hidden once
    /// declined (unless a download is under way) and once it is on.
    fn meaning_card(&self) -> Option<crate::meaning::MeaningCard> {
        use crate::meaning::MeaningCard;
        let card = crate::meaning::with_engine(
            crate::meaning::card(&self.meaning.state()),
            &self.meaning_runtime.status(),
        );
        match card {
            MeaningCard::Ready | MeaningCard::OnCpu => None,
            MeaningCard::Offer { kept: 0, .. } if self.prefs.library.meaning_offer_hidden => None,
            card => Some(card),
        }
    }

    /// Does what the reranker's row asked: downloading it turns it on.
    fn rerank_action(&mut self, action: crate::meaning::MeaningAction, ctx: &egui::Context) {
        use crate::meaning::MeaningAction;
        match action {
            MeaningAction::Download if self.rerank.is_ready() => self.rerank_runtime.retry(),
            MeaningAction::Download => {
                self.prefs.library.rerank = true;
                let ctx = ctx.clone();
                self.rerank.start(move || ctx.request_repaint());
            }
            MeaningAction::Pause => self.rerank.pause(),
            MeaningAction::Discard => self.rerank.discard(),
            MeaningAction::NotNow | MeaningAction::Settings => {}
        }
    }

    /// Does what search by meaning's card or row asked.
    pub(crate) fn meaning_action(
        &mut self,
        action: crate::meaning::MeaningAction,
        ctx: &egui::Context,
    ) {
        use crate::meaning::MeaningAction;
        match action {
            // Download, resume, retry: or, downloaded, start the engine again.
            MeaningAction::Download if self.meaning.is_ready() => self.meaning_runtime.retry(),
            MeaningAction::Download => {
                let ctx = ctx.clone();
                self.meaning.start(move || ctx.request_repaint());
            }
            MeaningAction::Pause => self.meaning.pause(),
            MeaningAction::Discard => self.meaning.discard(),
            MeaningAction::NotNow => self.prefs.library.meaning_offer_hidden = true,
            MeaningAction::Settings => {
                self.settings_window
                    .show_section(crate::settings_window::Section::Library);
                self.settings_window.scroll_to = Some("search".to_string());
            }
        }
    }

    /// Opens what a `glyphst://` link names: a passage the index knows, or
    /// an item at a page.
    /// Shows concept page `id` in the library tab.
    fn show_concept_page(&mut self, id: u64) {
        self.open_library();
        if let Some(Page::Library(tab)) = self
            .page_focused
            .and_then(|index| self.pages.get_mut(index))
        {
            tab.view = crate::library_tab::View::Page(id);
            tab.selected = None;
        }
    }

    /// Opens a `glyphst://` link: a passage, an item, or a concept page.
    fn follow_link(&mut self, link: crate::links::Link, ctx: &egui::Context) {
        match link {
            crate::links::Link::Open(link) => {
                if let Err(error) = self.open_link(link, ctx) {
                    self.notices.push(error);
                }
            }
            crate::links::Link::Page(id) => self.show_concept_page(id),
        }
    }

    pub(crate) fn open_link(
        &mut self,
        link: crate::links::OpenLink,
        ctx: &egui::Context,
    ) -> Result<String, String> {
        let passage = link
            .chunk
            .and_then(|chunk| self.library_search.as_mut()?.locate(chunk));
        let (path, page, anchor) = match passage {
            Some(chunk) => (
                chunk.path.clone(),
                chunk.page,
                Some(glyphst_core::SemanticTextAnchor::new(
                    chunk.doc_offset,
                    chunk.quote,
                    chunk.prefix,
                    chunk.suffix,
                )),
            ),
            None => {
                let item = link
                    .item
                    .ok_or("that passage is not in the index any more")?;
                let id = self
                    .library
                    .resolve_item_id(glyphst_library::DocumentId(item));
                let attachment = self
                    .library
                    .primary_attachment(id)
                    .ok_or_else(|| format!("lib_{item} has no file to open"))?;
                (
                    attachment.path.clone(),
                    link.page.unwrap_or(1).saturating_sub(1),
                    None,
                )
            }
        };
        let title = self
            .library
            .library_item_for_path(&path)
            .map(|item| item.title.clone())
            .unwrap_or_else(|| path.display().to_string());
        self.library_tab_action(
            crate::library_tab::LibraryTabAction::OpenAt {
                path,
                page,
                anchor,
                query: None,
            },
            ctx,
        );
        Ok(format!("Showing \"{title}\" at page {}.", page + 1))
    }

    /// Keeps (or rejects) every unreviewed AI change of these items.
    fn review_all(
        &mut self,
        ids: &[glyphst_library::DocumentId],
        keep: bool,
    ) -> anyhow::Result<()> {
        for id in ids {
            let targets = self
                .library
                .ai_assignments(*id)
                .iter()
                .filter(|assignment| assignment.state.needs_review())
                .map(|assignment| assignment.target.clone())
                .collect::<Vec<_>>();
            for target in targets {
                if keep {
                    self.library.accept_assignment(*id, &target)?;
                } else {
                    self.library.reject_assignment(*id, &target)?;
                }
            }
        }
        Ok(())
    }

    /// Focuses the pane showing tab `id`, without a transition.
    fn focus_tab(&mut self, id: u64) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == id) {
            self.active = Some(index);
            self.panes.show(id);
        }
    }

    /// Opens the next document not on the stage in a new pane beside the
    /// focused one.
    fn split_next(&mut self, zone: Zone) -> Result<(), &'static str> {
        let active = self.active.ok_or(Needs::Document.reason())?;
        let beside = self.tabs[active].id;
        let count = self.tabs.len();
        let next = (1..count)
            .map(|step| self.tabs[(active + step) % count].id)
            .find(|&id| !self.panes.contains(id))
            .ok_or("Open another document to split")?;
        self.panes.show(beside);
        if self.panes.split(next, beside, zone) {
            self.focus_tab(next);
        }
        Ok(())
    }

    /// Shows a dragged tab on the stage: beside the pane under the pointer,
    /// or in it.
    pub(crate) fn drop_tab(&mut self, tab: u64, pane: u64, zone: Zone) {
        if self.panes.split(tab, pane, zone) {
            self.focus_tab(tab);
        }
    }

    /// How a document's pages were last arranged (kept in the library).
    fn arrangement_of(&self, path: &Path) -> Arrangement {
        Arrangement {
            rotation: match self.library.document_rotation(path) {
                DocumentRotation::Degrees0 => 0,
                DocumentRotation::Degrees90 => 1,
                DocumentRotation::Degrees180 => 2,
                DocumentRotation::Degrees270 => 3,
            },
            spread: self.library.document_page_layout(path) == DocumentPageLayout::TwoColumn,
        }
    }

    /// Turns or spreads the active document's pages and remembers it.
    fn arrange(&mut self, change: impl FnOnce(&mut Arrangement)) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &mut self.tabs[active];
        let mut arrangement = tab.layout.arrangement;
        change(&mut arrangement);
        tab.arrange(arrangement);
        if self.persist {
            let path = tab.path.clone();
            let rotation = [
                DocumentRotation::Degrees0,
                DocumentRotation::Degrees90,
                DocumentRotation::Degrees180,
                DocumentRotation::Degrees270,
            ][usize::from(arrangement.rotation % 4)];
            let page_layout = if arrangement.spread {
                DocumentPageLayout::TwoColumn
            } else {
                DocumentPageLayout::SingleColumn
            };
            let _ = self.library.set_document_rotation(&path, rotation);
            let _ = self.library.set_document_page_layout(&path, page_layout);
            let _ = self.library.save();
        }
    }

    /// Opens tab `index`'s document again from its file, in place: the
    /// position stays, and what was made from the old file (GPU pages,
    /// text, a translation) goes.
    pub(crate) fn reload(&mut self, index: usize, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (id, path, position) = (tab.id, tab.path.clone(), tab.position());
        let layout = self.layout_for(&path, ctx);
        let arrangement = tab.layout.arrangement;
        match DocumentTab::open(id, &path, ctx, Some(position), layout, arrangement) {
            Ok(mut reloaded) => {
                reloaded.annotations = Annotations::load(&self.library, &path);
                self.with_gpu(|gpu| gpu.drop_tab(id));
                let mut old = std::mem::replace(&mut self.tabs[index], reloaded);
                self.free_textures(&old.take_freed_textures(true));
            }
            Err(error) => self
                .notices
                .push(format!("Could not reload {}: {error:#}", path.display())),
        }
    }

    /// Reloads documents whose files changed while the window was away.
    fn reload_changed(&mut self, ctx: &egui::Context) {
        let changed: Vec<usize> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| {
                let now = std::fs::metadata(&tab.path)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                now.is_some() && now != tab.modified
            })
            .map(|(index, _)| index)
            .collect();
        for index in changed {
            self.reload(index, ctx);
        }
    }

    /// Reflowable documents (EPUB) open laid out for the view; others keep
    /// their pages.
    fn layout_for(&self, path: &Path, ctx: &egui::Context) -> DocumentLayout {
        let epub = path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            == Some(DocumentFormat::Epub);
        if !epub {
            return DocumentLayout::Fixed;
        }
        let mut width = ctx.content_rect().width();
        if self.prefs.show_sidebar {
            width -= SIDEBAR_WIDTH;
        }
        DocumentLayout::Reflow(self.epub_layout(path, width))
    }

    /// The reflowed layout for a view `width` points wide, from the
    /// document's reading preferences in the library.
    fn epub_layout(&self, path: &Path, width: f32) -> EpubLayout {
        let mut preferences = self
            .library
            .epub_reader_preferences(path)
            .unwrap_or_default();
        if let Some(size) = self.font_size {
            preferences.font_size = size;
        }
        preferences.layout_for_viewport(width - 2.0 * REFLOW_MARGIN, REFLOW_PAGE_HEIGHT)
    }

    /// Lays a reflowed tab out again and drops its pages from the GPU.
    fn relayout(&mut self, index: usize, layout: EpubLayout, ctx: &egui::Context) {
        let tab = &mut self.tabs[index];
        let id = tab.id;
        if let Err(error) = tab.relayout(DocumentLayout::Reflow(layout), ctx) {
            self.notices
                .push(format!("Could not lay out the book: {error:#}"));
        }
        self.with_gpu(|gpu| gpu.drop_tab(id));
    }

    /// Reflows a tab once the view has kept a width that no longer fits
    /// its column.
    fn reflow_to_view(&mut self, index: usize, view_width: f32, now: Instant, ctx: &egui::Context) {
        let tab = &self.tabs[index];
        let DocumentLayout::Reflow(current) = tab.document_layout else {
            return;
        };
        let wanted = self.epub_layout(&tab.path, view_width);
        let changed = (wanted.width - current.width).abs() > current.width * 0.1
            || wanted.font_size != current.font_size;
        let tab = &mut self.tabs[index];
        if !changed {
            tab.reflow_mismatch = None;
            return;
        }
        let since = *tab.reflow_mismatch.get_or_insert(now);
        if now - since >= REFLOW_SETTLE && self.drag.is_none() {
            self.relayout(index, wanted, ctx);
        }
    }

    /// Makes the active EPUB's text larger or smaller.
    fn change_font_size(&mut self, step: f32) {
        let Some(active) = self.active else {
            return;
        };
        let path = self.tabs[active].path.clone();
        let mut preferences = self
            .library
            .epub_reader_preferences(&path)
            .unwrap_or_default();
        let size = (self.font_size.unwrap_or(preferences.font_size) + step).clamp(8.0, 48.0);
        if self.persist {
            preferences.font_size = size;
            if let Err(error) = self.library.set_epub_reader_preferences(&path, preferences) {
                self.notices
                    .push(format!("Could not save the text size: {error:#}"));
            }
            self.font_size = None;
        } else {
            self.font_size = Some(size);
        }
        // The next frame sees the layout no longer matches and reflows.
        self.tabs[active].reflow_mismatch = Some(Instant::now() - REFLOW_SETTLE);
    }

    fn reload_annotations(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        tab.annotations = Annotations::load(&self.library, &tab.path);
        let text = &tab.text;
        tab.annotations.resolve(text);
    }

    /// Highlights the selection, or adds a note on it and opens the note.
    fn annotate_selection(&mut self, color: HighlightColor, note: bool, at: egui::Pos2) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let Some(selection) = tab.selection.as_ref() else {
            return;
        };
        let record = match annotations::selection_record(selection, &tab.text, tab.is_reflowed()) {
            Ok(record) => record,
            Err(annotations::RecordError::TextPending) => {
                self.notices
                    .push("The selection's text is still being read; try again in a moment.");
                return;
            }
            Err(annotations::RecordError::Empty) => return,
        };
        let annotations::SelectionRecord {
            rects,
            quote,
            semantic,
        } = record;
        let path = tab.path.clone();
        let created = match (semantic, note) {
            (Some(range), false) => self
                .library
                .add_reflow_highlight(&path, color, range, rects, quote, "")
                .map(|h| AnnotationId::Highlight(h.id)),
            (None, false) => self
                .library
                .add_highlight(&path, color, rects, quote, "")
                .map(|h| AnnotationId::Highlight(h.id)),
            (Some(range), true) => self
                .library
                .add_reflow_note(&path, color, range, rects, quote, "")
                .map(|n| AnnotationId::Note(n.id)),
            (None, true) => self
                .library
                .add_note(&path, color, rects, quote, "")
                .map(|n| AnnotationId::Note(n.id)),
        };
        let page = selection.pages().next().unwrap_or(0);
        match created {
            Ok(id) => {
                let kind = if note {
                    ActivityKind::Note
                } else {
                    ActivityKind::Highlight
                };
                self.log_activity(kind, &path, format!("p.{}", page + 1), 0);
                self.tabs[active].selection = None;
                self.reload_annotations(active);
                if note {
                    self.note_editor = Some(NoteEditor {
                        id,
                        at,
                        body: String::new(),
                        color,
                        fresh: true,
                    });
                } else {
                    // `highlight saved [ undo ]` (frame 06c).
                    self.notices.push_with(
                        "highlight saved",
                        glyphst_ui::overlay::Tone::Ok,
                        Some("undo"),
                    );
                    self.toast_undo = Some(ToastUndo::Highlight(id));
                }
            }
            Err(error) => self.notices.push(format!("Could not save: {error:#}")),
        }
    }

    fn set_annotation_color(&mut self, id: AnnotationId, color: HighlightColor) {
        let result = match id {
            AnnotationId::Highlight(id) => self.library.set_highlight_color(id, color),
            AnnotationId::Note(note) => {
                let body = self.annotation_body(id);
                self.library.update_note(note, body, color)
            }
        };
        self.after_edit(result);
    }

    fn set_note_body(&mut self, id: AnnotationId, body: String) {
        let AnnotationId::Note(note) = id else {
            return;
        };
        let color = self
            .active
            .and_then(|active| self.tabs[active].annotations.get(id).map(|item| item.color))
            .unwrap_or(HighlightColor::Yellow);
        let result = self.library.update_note(note, body, color);
        self.after_edit(result);
    }

    fn remove_annotation(&mut self, id: AnnotationId) {
        let result = match id {
            AnnotationId::Highlight(id) => self.library.remove_highlight(id),
            AnnotationId::Note(id) => self.library.remove_note(id),
        };
        self.after_edit(result);
    }

    fn annotation_body(&self, id: AnnotationId) -> String {
        self.active
            .and_then(|active| {
                self.tabs[active]
                    .annotations
                    .get(id)
                    .map(|item| item.body.clone())
            })
            .unwrap_or_default()
    }

    fn after_edit(&mut self, result: anyhow::Result<bool>) {
        if let Err(error) = result {
            self.notices.push(format!("Could not save: {error:#}"));
        }
        if let Some(active) = self.active {
            self.reload_annotations(active);
        }
    }

    /// Bookmarks the page being read, or removes its bookmark. From the
    /// pad a toast says which, rather than the sidebar opening.
    fn toggle_bookmark(&mut self) {
        let Some(active) = self.active else {
            return;
        };
        let page = self.tabs[active].position().page.max(1);
        let existing = self.tabs[active]
            .annotations
            .bookmarks
            .iter()
            .find(|bookmark| bookmark.position.page.max(1) == page)
            .map(|bookmark| bookmark.id);
        let pad = self.last_input == glyphst_ui::pad_glyph::InputDevice::Pad;
        match existing {
            Some(id) => {
                let result = self.library.remove_bookmark(id);
                let removed = matches!(result, Ok(true));
                self.after_edit(result);
                if removed {
                    self.notices
                        .push(format!("Removed the bookmark on p. {page}"));
                }
            }
            None if pad => {
                self.add_bookmark_quietly();
                self.notices.push(format!("Bookmarked p. {page}"));
            }
            None => self.add_bookmark(),
        }
    }

    fn add_bookmark(&mut self) {
        if self.add_bookmark_quietly() {
            self.prefs.show_sidebar = true;
            self.prefs.sidebar = SidebarTab::Bookmarks;
        }
    }

    /// Bookmarks the page being read; whether there was one to.
    fn add_bookmark_quietly(&mut self) -> bool {
        let Some(active) = self.active else {
            return false;
        };
        let tab = &self.tabs[active];
        let (path, position, title) = (tab.path.clone(), tab.position(), tab.words_at_top(8));
        let page = position.page.max(1);
        let result = self.library.add_bookmark(&path, title, position);
        if result.is_ok() {
            self.log_activity(ActivityKind::Bookmark, &path, format!("p.{page}"), 0);
        }
        self.after_edit(result.map(|_| true));
        true
    }

    fn apply_sidebar(
        &mut self,
        action: SidebarAction,
        actions: &mut Vec<ViewAction>,
        ctx: &egui::Context,
    ) {
        match action {
            SidebarAction::View(action) => actions.push(action),
            SidebarAction::GoToBookmark(id) => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let position = tab
                        .annotations
                        .bookmarks
                        .iter()
                        .find(|bookmark| bookmark.id == id)
                        .map(|bookmark| bookmark.position.clone());
                    if let Some(position) = position {
                        tab.go_to_position(&position);
                    }
                }
            }
            SidebarAction::AddBookmark => self.add_bookmark(),
            SidebarAction::RenameBookmark(id, title) => {
                let result = self.library.update_bookmark_title(id, Some(title));
                self.after_edit(result);
            }
            SidebarAction::RemoveBookmark(id) => {
                let result = self.library.remove_bookmark(id);
                self.after_edit(result);
            }
            SidebarAction::StartTranslation => self.start_document_translation(ctx),
            SidebarAction::CancelTranslation => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_ref())
                {
                    session.cancel();
                }
            }
            SidebarAction::ExportTranslation => {
                if let Some(active) = self.active {
                    let tab = &self.tabs[active];
                    let stem = tab
                        .path
                        .file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "document".into());
                    let target = tab
                        .translation
                        .as_ref()
                        .map(|session| session.target.clone())
                        .unwrap_or_default();
                    let name = format!("{stem}.{target}.pdf");
                    self.pickers.save_pdf(ctx, &name, tab.id);
                }
            }
            SidebarAction::ShowTranslated(shown) => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_mut())
                {
                    session.show_translated = shown;
                }
            }
            SidebarAction::Unpin(index) => {
                if index < self.translator.pinned.len() {
                    self.translator.pinned.remove(index);
                }
            }
            SidebarAction::SetColor(id, color) => self.set_annotation_color(id, color),
            SidebarAction::SetBody(id, body) => self.set_note_body(id, body),
            SidebarAction::Remove(id) => self.remove_annotation(id),
            // The locate toggle: off keeps the outline still; on follows
            // the current section again.
            SidebarAction::LocateOutline => {
                if let Some(active) = self.active {
                    let view = &mut self.tabs[active].outline_view;
                    self.prefs.outline_follow = !view.following();
                    view.set_follow(self.prefs.outline_follow);
                    if self.prefs.outline_follow {
                        view.locate();
                    }
                }
            }
            SidebarAction::Close => self.prefs.show_sidebar = false,
            SidebarAction::Command(command) => self.run_or_notify(command, ctx),
            SidebarAction::OpenBookmark(id) => {
                let found = self
                    .library
                    .bookmarks()
                    .iter()
                    .find(|bookmark| bookmark.id == id)
                    .map(|bookmark| (bookmark.path.clone(), bookmark.position.clone()));
                if let Some((path, position)) = found {
                    self.open(&path, ctx, None);
                    if let Some(active) = self.active
                        && self.tabs[active].path == path
                    {
                        self.tabs[active].go_to_position(&position);
                    }
                }
            }
            SidebarAction::SetSourceLanguage(language) => {
                self.user_settings
                    .edit(|settings| settings.translation.source_language = language);
            }
            SidebarAction::SetTargetLanguage(language) => {
                self.prefs.target_language = Some(language);
            }
            SidebarAction::SetProvider(provider) => {
                self.user_settings
                    .edit(|settings| settings.translation.active_provider = provider);
            }
            SidebarAction::SetOriginalOnHover(on) => self.prefs.original_on_hover = on,
            SidebarAction::SetTranslateAsRead(on) => {
                self.prefs.translate_as_read = on;
                if on
                    && self
                        .active
                        .is_some_and(|active| self.tabs[active].translation.is_none())
                {
                    self.start_document_translation(ctx);
                }
            }
        }
    }

    /// Adds an event to the activity log; a failure is only reported.
    /// Scripted runs leave the log alone.
    /// Logs a span of reading: to the activity log, and as a reading
    /// session for the heatmaps.
    fn log_reading(&mut self, read: crate::reading_clock::Read) {
        self.log_activity(ActivityKind::Read, &read.path, String::new(), read.seconds);
        if self.persist
            && let Err(error) =
                self.library
                    .record_reading(&read.path, read.start, read.seconds, read.pages)
        {
            log::warn!("could not record reading: {error:#}");
        }
    }

    pub(crate) fn log_activity(
        &mut self,
        kind: ActivityKind,
        path: &std::path::Path,
        detail: String,
        seconds: u64,
    ) {
        if !self.persist {
            return;
        }
        if let Err(error) = self.library.record_activity(kind, path, detail, seconds) {
            log::warn!("could not log activity: {error:#}");
        }
    }

    /// When the document of tab `active` was last opened, as the
    /// document card writes it.
    pub(crate) fn opened_label(&self, active: usize) -> Option<String> {
        let path = glyphst_library::normalize_path(&self.tabs[active].path);
        self.library
            .history_entries()
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| sidebar::when(entry.opened))
    }

    /// The other documents' bookmarks, for the Bookmarks tab.
    pub(crate) fn other_bookmarks(&self, active: usize) -> Vec<OtherBookmark> {
        let path = &self.tabs[active].path;
        self.library
            .bookmarks()
            .iter()
            .filter(|bookmark| &bookmark.path != path)
            .map(|bookmark| {
                let document = bookmark
                    .path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let short: String = document.chars().take(12).collect();
                OtherBookmark {
                    id: bookmark.id,
                    title: bookmark
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("page {}", bookmark.position.page.max(1))),
                    place: format!(
                        "{} p.{}",
                        short.to_lowercase(),
                        bookmark.position.page.max(1)
                    ),
                    document,
                }
            })
            .collect()
    }

    /// Steps to the next or previous match.
    fn find_step(&mut self, forward: bool) {
        if let Some(active) = self.active {
            self.tabs[active].find.step(forward);
        }
    }

    /// Translates the selection into a card at `at`.
    pub(crate) fn translate_selection(&mut self, at: egui::Pos2, ctx: &egui::Context) {
        let Some(text) = self
            .active
            .and_then(|active| self.tabs[active].selected_text())
        else {
            return;
        };
        let target = self.prefs.target_language.clone();
        self.translator
            .translate_selection(text, target.as_deref(), at, ctx);
    }

    /// Starts translating the active document.
    pub(crate) fn start_document_translation(&mut self, ctx: &egui::Context) {
        let Some(active) = self.active else {
            return;
        };
        let target = self.prefs.target_language.clone();
        let started =
            self.translator
                .start_session(&self.tabs[active].path, target.as_deref(), ctx);
        match started {
            Ok(session) => {
                let detail = format!("→ {}", session.target.to_lowercase());
                let path = self.tabs[active].path.clone();
                self.tabs[active].translation = Some(session);
                self.log_activity(ActivityKind::Translate, &path, detail, 0);
                self.prefs.show_sidebar = true;
                self.prefs.sidebar = SidebarTab::Translation;
            }
            Err(error) => self.notices.push(format!("Could not translate: {error}")),
        }
    }

    /// The card of a selection translation: the translation, Copy and Pin.
    fn translation_card(&mut self, ctx: &egui::Context) {
        let Some(card) = self.translator.card.clone() else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let (mut close, mut pin) = (false, false);
        let area = egui::Area::new(egui::Id::unique("translation-card"))
            .order(egui::Order::Foreground)
            .fixed_pos(card.at + egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                glyphst_ui::pane::popover(ctx)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.horizontal(|ui| {
                            ui.add(icons::image("languages", 15.0, tokens.accent));
                            ui.label(
                                egui::RichText::new("Translation").font(theme::semibold(13.0)),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    close |= icons::button(ui, "x", 24.0, false, "Close").clicked();
                                },
                            );
                        });
                        let source: String = card.source.chars().take(160).collect();
                        ui.label(egui::RichText::new(source).italics().color(tokens.muted));
                        ui.add_space(4.0);
                        match &card.state {
                            CardState::Loading => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label("Translating…");
                                });
                            }
                            CardState::Done(text) => {
                                egui::ScrollArea::vertical()
                                    .max_height(320.0)
                                    .show(ui, |ui| ui.label(text));
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(glyphst_ui::button::Button::ghost("copy").icon("copy"))
                                        .clicked()
                                    {
                                        ui.ctx().copy_text(text.clone());
                                    }
                                    pin = ui
                                        .add(glyphst_ui::button::Button::ghost("pin").icon("pin"))
                                        .on_hover_text("Keep in the Translation panel")
                                        .clicked();
                                });
                            }
                            CardState::Failed(error) => {
                                ui.label(egui::RichText::new(error).color(tokens.err));
                            }
                        }
                    });
            });
        if pin {
            self.translator.pin_card();
            self.prefs.show_sidebar = true;
            self.prefs.sidebar = SidebarTab::Translation;
            close = true;
        }
        let fresh = ctx.input(|i| i.pointer.any_pressed())
            && area.response.rect.contains(card.at + egui::vec2(9.0, 9.0));
        if close
            || (area.response.clicked_elsewhere()
                && !fresh
                && !matches!(card.state, CardState::Loading))
        {
            self.translator.card = None;
        }
    }

    /// Shows or hides the assistant.
    fn toggle_assistant(&mut self) {
        self.assistant.open = !self.assistant.open;
        if self.assistant.open {
            self.assistant.refresh_sessions(&self.library);
        }
    }

    /// Asks the assistant about the active document.
    pub(crate) fn ask(&mut self, draft: crate::ai_editor::Draft, ctx: &egui::Context) {
        let Some(ask) = self.ask_context() else {
            return;
        };
        if let Some(active) = self.active {
            let question: String = draft.prompt().0.chars().take(48).collect();
            let path = self.tabs[active].path.clone();
            self.log_activity(ActivityKind::Ask, &path, question, 0);
        }
        let Some(settings) = self.user_settings.get().cloned() else {
            self.assistant.error =
                Some("Your AI settings are still loading; try again in a moment.".into());
            return;
        };
        let tools = self.agent_tools();
        let external = match self.assistant.agent.clone() {
            Some(id) => match self.external_access(&id, &ask) {
                Ok(access) => Some(access),
                Err(error) => {
                    self.assistant.error = Some(format!("{error:#}"));
                    return;
                }
            },
            None => None,
        };
        self.assistant
            .send(draft, ask, tools, external, &settings, ctx);
    }

    /// What a question knows about the active document; with none open, a
    /// question about the library.
    fn ask_context(&self) -> Option<AskContext> {
        let Some(active) = self.active else {
            let items = self
                .library
                .library_items()
                .iter()
                .filter(|item| item.trashed_at.is_none())
                .count();
            return Some(AskContext::library(items, Vec::new()));
        };
        let tab = &self.tabs[active];
        let current_page = tab.current_page();
        let others = self
            .tabs
            .iter()
            .filter(|other| other.id != tab.id)
            .map(|other| glyphst_ai::prompt::DocumentFacts {
                id: Some(crate::ai_tools::document_id(other.id)),
                title: other.title.clone(),
                format: if other.is_reflowed() { "EPUB" } else { "PDF" }.into(),
                authors: None,
                page_count: other.layout.len(),
            })
            .collect();
        Some(AskContext {
            path: tab.path.clone(),
            id: crate::ai_tools::document_id(tab.id),
            authors: tab
                .metadata
                .iter()
                .find(|(label, _)| *label == "Author")
                .map(|(_, author)| author.clone()),
            section: tab
                .current_outline_entry()
                .map(|entry| tab.outline[entry].title.clone()),
            others,
            title: tab.title.clone(),
            layout: tab.document_layout,
            page_count: tab.layout.len(),
            current_page,
            page_text: tab.text.get(current_page).map(|text| text.plain_text()),
            selection: tab.selected_text(),
            library: None,
        })
    }

    /// The quick ask card over `stage`, and what it asked for.
    pub(crate) fn quick_ask_ui(&mut self, ctx: &egui::Context, stage: egui::Rect) {
        let settings = self
            .user_settings
            .get()
            .map(|settings| settings.ai.quick_ask)
            .unwrap_or_default();
        let selection = self
            .active
            .and_then(|active| self.tabs[active].selected_text());
        match self
            .quick_ask
            .ui(ctx, stage, &settings, selection.as_deref())
        {
            Some(quick_ask::Action::Send(text)) => {
                let (ask, settings) = (self.ask_context(), self.user_settings.get().cloned());
                let tools = self.agent_tools();
                let quick = &mut self.quick_ask.assistant;
                let (Some(ask), Some(settings)) = (ask, settings) else {
                    quick.error =
                        Some("Your AI settings are still loading; try again in a moment.".into());
                    return;
                };
                // One question at a time, with what is on screen.
                quick.new_session();
                quick.include_page = true;
                quick.include_selection = true;
                quick.send(text, ask, tools, None, &settings, ctx);
            }
            Some(quick_ask::Action::Continue) => {
                if self.assistant.adopt(&mut self.quick_ask.assistant) {
                    self.quick_ask.close();
                    self.assistant.open = true;
                    self.assistant.refresh_sessions(&self.library);
                } else {
                    self.notices
                        .push("The assistant is answering; try again when it is done.");
                }
            }
            Some(quick_ask::Action::Close) => self.quick_ask.close(),
            None => {}
        }
    }

    /// Ends a region drag: the region keeps its page's part, and its text
    /// becomes a column selection; too small a region is dropped.
    pub(crate) fn finish_region(&mut self, active: usize) {
        self.region_tool = false;
        let tab = &mut self.tabs[active];
        let clamped = self.region.and_then(|region| {
            let bounds = *tab.layout.bounds.get(region.page)?;
            region.clamped(bounds).map(|rect| (region, rect))
        });
        self.region = clamped.map(|(region, [x0, y0, x1, y1])| {
            let region = Region {
                from: [x0, y0],
                to: [x1, y1],
                ..region
            };
            tab.selection = Some(Selection::Column {
                page: region.page,
                from: region.from,
                to: region.to,
            });
            region
        });
    }

    /// Outlines the region drawn on tab `active` and, once drawn, shows its
    /// popover. The region goes when its selection does (Esc, a click).
    fn region_overlay(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        maps: &[PageMap],
        active: usize,
        tokens: &Tokens,
    ) {
        let tab = &self.tabs[active];
        let Some(region) = self.region.filter(|region| region.tab == tab.id) else {
            return;
        };
        let drawing = matches!(self.drag, Some((id, Drag::Region)) if id == tab.id);
        let selected = matches!(tab.selection, Some(Selection::Column { page, from, to })
            if page == region.page && from == region.from && to == region.to);
        if !drawing && !selected {
            self.region = None;
            return;
        }
        let Some(map) = maps.iter().find(|map| map.page == region.page) else {
            return;
        };
        let screen = map.rect_to_screen(region.rect());
        // A dotted accent outline on an accent wash, and its size in
        // points under it.
        painter.rect_filled(screen, 0, Tokens::fade(tokens.accent_soft, 0.5));
        for (from, to) in [
            (screen.left_top(), screen.right_top()),
            (screen.right_top(), screen.right_bottom()),
            (screen.right_bottom(), screen.left_bottom()),
            (screen.left_bottom(), screen.left_top()),
        ] {
            painter.add(egui::Shape::dotted_line(
                &[from, to],
                tokens.accent,
                4.0,
                1.0,
            ));
        }
        let [x0, y0, x1, y1] = region.rect();
        let size = format!("{:.0} × {:.0}", (x1 - x0).abs(), (y1 - y0).abs());
        let galley = painter.layout_no_wrap(size, theme::mono(10.0), tokens.on_accent);
        let chip = egui::Rect::from_min_size(
            screen.left_bottom() + egui::vec2(0.0, 4.0),
            galley.size() + egui::vec2(10.0, 4.0),
        );
        painter.rect_filled(chip, tokens.ctl_radius(), tokens.accent);
        painter.galley(chip.min + egui::vec2(5.0, 2.0), galley, tokens.on_accent);
        if drawing || self.active != Some(active) {
            return;
        }
        let has_text = tab
            .selected_text()
            .is_some_and(|text| !text.trim().is_empty());
        // A region drawn from the assistant's menu goes straight to it.
        let action = if self.region_for_assistant {
            Some(region::Action::Ask)
        } else {
            region::popover(ctx, screen.right_top(), has_text)
        };
        let Some(action) = action else {
            return;
        };
        let path = tab.path.clone();
        match action {
            region::Action::CopyImage => self.region_jobs.push(region::Job::start(
                &path,
                region.page,
                region.rect(),
                region::Use::Copy,
                ctx,
            )),
            region::Action::SaveImage => {
                self.region_save = Some(region);
                let name = format!(
                    "{} p{}.png",
                    path.file_stem().unwrap_or_default().to_string_lossy(),
                    region.page + 1
                );
                self.pickers.save_png(ctx, &name);
            }
            region::Action::CopyText => self.copy_selection(ctx),
            region::Action::Translate => self.translate_selection(screen.right_bottom(), ctx),
            region::Action::Ask => {
                self.region_jobs.push(region::Job::start(
                    &path,
                    region.page,
                    region.rect(),
                    region::Use::Ask(region),
                    ctx,
                ));
                if self.region_for_assistant {
                    self.region = None;
                    self.tabs[active].selection = None;
                }
            }
            region::Action::Dismiss => {
                self.region = None;
                self.tabs[active].selection = None;
            }
        }
    }

    /// Takes region images that finished rendering, and the save dialog's
    /// path for one.
    fn finish_region_jobs(&mut self, ctx: &egui::Context) {
        if let (Some(region), Some(path)) = (self.region_save, self.pickers.take_saved_image())
            && let Some(tab) = self.tabs.iter().find(|tab| tab.id == region.tab)
        {
            self.region_save = None;
            self.region_jobs.push(region::Job::start(
                &tab.path,
                region.page,
                region.rect(),
                region::Use::Save(path),
                ctx,
            ));
        }
        let mut finished = Vec::new();
        self.region_jobs.retain(|job| match job.poll() {
            Some(result) => {
                finished.push((job.for_.clone(), result));
                false
            }
            None => true,
        });
        for (for_, result) in finished {
            let image = match result {
                Ok(image) => image,
                Err(error) => {
                    self.notices
                        .push(format!("Could not render the region: {error:#}"));
                    continue;
                }
            };
            match for_ {
                region::Use::Copy => {
                    ctx.copy_image(region::color_image(&image));
                    self.notices.push("Copied the region as an image.");
                }
                region::Use::Save(path) => {
                    let written = region::png(&image).and_then(|png| {
                        std::fs::write(&path, png).map_err(|error| anyhow::anyhow!("{error}"))
                    });
                    self.notices.push(match written {
                        Ok(()) => format!("Saved {}", path.display()),
                        Err(error) => format!("Could not save {}: {error:#}", path.display()),
                    });
                }
                region::Use::Ask(region) => match region::png(&image) {
                    Ok(png) => {
                        let thumbnail = ctx.load_texture(
                            format!("region-{}-{}", region.tab, self.region_jobs.len()),
                            region::color_image(&image),
                            egui::TextureOptions::LINEAR,
                        );
                        let attachment = crate::ai::Attachment {
                            image: crate::ai::AttachedImage::Region(
                                glyphst_ai::ai_chat::ImageContext::new_region(
                                    region.reference(),
                                    "image/png",
                                    png,
                                ),
                            ),
                            thumbnail,
                            label: format!("Region p. {}", region.page + 1),
                        };
                        if std::mem::take(&mut self.region_for_assistant) {
                            self.assistant.open = true;
                            self.assistant.attach(ctx, attachment);
                        } else {
                            self.quick_ask.attach(ctx, attachment);
                            self.quick_ask.show_composer();
                        }
                    }
                    Err(error) => self.notices.push(format!("{error:#}")),
                },
            }
        }
    }

    fn copy_selection(&mut self, ctx: &egui::Context) {
        let Some(active) = self.active else {
            return;
        };
        match self.tabs[active].selected_text() {
            Some(text) => ctx.copy_text(text),
            None if self.tabs[active].selection.is_some() => self
                .notices
                .push("The selection's text is still being read; try again in a moment."),
            None => {}
        }
    }

    /// A page turn's transition in tab `tab`'s view.
    fn begin_transition(&mut self, effect: EffectUse, tab: u64, direction: f32) {
        let reduce_motion = self.prefs.reduce_motion;
        let saved = &self.prefs.effect_params;
        let rng = &mut self.transition_rng;
        let planned = {
            let renderer = self.render_state.renderer.read();
            renderer
                .callback_resources
                .get::<ReaderGpu>()
                .and_then(|gpu| {
                    crate::transition::plan(
                        &effect,
                        gpu.effects().defs(),
                        saved,
                        reduce_motion,
                        rng,
                        None,
                    )
                })
        };
        self.transition = planned.map(|planned| {
            ViewTransition::new(
                planned,
                Capture::View(tab),
                tab,
                direction,
                None,
                Instant::now(),
            )
        });
    }

    pub(crate) fn close(&mut self, index: usize) {
        self.save_position(index, true);
        // Closing the book shown alone reveals the next one out of its last
        // image.
        let shown_alone = self.active == Some(index) && self.panes.count() == 1;
        let closed_background = self.background_for(Some(index));
        let mut tab = self.tabs.remove(index);
        self.free_textures(&tab.take_freed_textures(true));
        self.drop_card(tab.id);
        self.saved_positions.remove(&tab.id);
        self.with_gpu(|gpu| gpu.drop_tab(tab.id));
        self.panes.remove_tab(tab.id);
        self.transition = None;
        let focused = self
            .panes
            .focused()
            .and_then(|id| self.tabs.iter().position(|tab| tab.id == id));
        self.active = match self.active {
            Some(active) if active == index => {
                if self.tabs.is_empty() {
                    None
                } else {
                    focused.or(Some(index.min(self.tabs.len() - 1)))
                }
            }
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
        if let Some(active) = self.active {
            self.panes.show(self.tabs[active].id);
            if shown_alone {
                let direction = if active < index { -1.0 } else { 1.0 };
                let origin = self.click;
                self.begin_switch(
                    active,
                    Capture::View(tab.id),
                    closed_background,
                    direction,
                    origin,
                    1.0,
                );
            }
        }
    }

    /// Records a tab's position in the library when it moved.
    fn save_position(&mut self, index: usize, flush: bool) {
        if !self.persist {
            return;
        }
        // A tab never shown (restored with the session, or behind another)
        // was not read: its position and opening time stay as they were.
        let Some(tab) = self.tabs.get(index).filter(|tab| tab.is_shown()) else {
            return;
        };
        let position = tab.position();
        if self.saved_positions.get(&tab.id) != Some(&position) {
            self.library.record_history(&tab.path, position.clone());
            self.saved_positions.insert(tab.id, position);
            if flush {
                let _ = self.library.save();
            }
        }
    }

    fn session(&self) -> LastSession {
        LastSession {
            files: self
                .tabs
                .iter()
                .map(|tab| SessionFile {
                    path: tab.path.display().to_string(),
                    page: tab.current_page(),
                })
                .collect(),
            active: self.active,
            panes: panes::map_layout(self.panes.layout(), |id| {
                self.tabs.iter().position(|tab| tab.id == *id)
            }),
        }
    }

    /// Queues raster layers re-rasterized for the current view; they are
    /// written after the uploads before them, a chunk per frame.
    fn upload_ready_layers(&mut self) {
        let device = self.render_state.device.clone();
        for index in 0..self.tabs.len() {
            let tab_id = self.tabs[index].id;
            while let Some((page, layer, image)) = self.tabs[index].ready_layers.pop_front() {
                let key = PageKey { tab: tab_id, page };
                self.with_gpu(|gpu| gpu.replace_layer(&device, key, layer, image));
            }
        }
    }

    /// Writes the current upload a chunk further, starting the next loaded
    /// page (the active tab's first) when no page is being uploaded, so a
    /// heavy page costs a few ordinary frames rather than one long one.
    fn upload_ready_page(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        self.with_gpu(|gpu| gpu.step_overview(&device, &queue));
        for index in 0..self.tabs.len() {
            let tab = self.tabs[index].id;
            while let Some((page, prepared)) = self.tabs[index].ready_parts.pop_front() {
                let key = PageKey { tab, page };
                self.with_gpu(|gpu| gpu.begin_part(&device, key, prepared));
            }
        }
        if !self.with_gpu(|gpu| gpu.is_uploading()).unwrap_or(false) {
            let order: Vec<usize> = self.active.into_iter().chain(0..self.tabs.len()).collect();
            let next = order.into_iter().find_map(|index| {
                let ready = self.tabs[index].ready.pop_front()?;
                let key = PageKey {
                    tab: self.tabs[index].id,
                    page: ready.page,
                };
                Some((key, ready))
            });
            if let Some((key, ready)) = next {
                self.with_gpu(|gpu| gpu.begin_upload(&device, key, ready.prepared, ready.layers));
            }
        }
        let times = self.with_gpu(|gpu| gpu.step_upload(&device, &queue, UPLOAD_BYTES_PER_FRAME));
        if self.first_part_ms.is_none() && self.with_gpu(|gpu| gpu.has_parts()).unwrap_or(false) {
            self.first_part_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(Some(times)) = times {
            self.upload_ms.push(times.cpu_ms);
            self.upload_visible_ms.push(times.visible_ms);
            self.upload_complete_ms.push(times.complete_ms);
            if self.first_page_ms.is_none() {
                self.first_page_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }

    /// Draws thumbnails of the active tab's wanted pages that are wholly on
    /// the GPU but have none yet (dense pages the indexer skips get theirs
    /// this way), and frees the ones tabs dropped.
    fn draw_gpu_thumbnails(&mut self) {
        let freed: Vec<egui::TextureId> = self
            .tabs
            .iter_mut()
            .flat_map(|tab| tab.take_freed_textures(false))
            .collect();
        self.free_textures(&freed);
        let Some(active) = self.active else {
            return;
        };
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        let tab = self.tabs[active].id;
        let wanted = self.tabs[active].thumbnails_wanted().to_vec();
        let mut drawn = 0;
        for page in wanted {
            if drawn == GPU_THUMBNAILS_PER_FRAME {
                break;
            }
            let key = PageKey { tab, page };
            let Some(Some(texture)) = self.with_gpu(|gpu| {
                gpu.render_thumbnail(
                    &device,
                    &queue,
                    key,
                    THUMBNAIL_SCALE,
                    GPU_THUMBNAIL_MAX_SIDE,
                )
            }) else {
                continue;
            };
            drawn += 1;
            let view = texture.create_view(&Default::default());
            let id = self.render_state.renderer.write().register_native_texture(
                &device,
                &view,
                wgpu::FilterMode::Linear,
            );
            self.tabs[active].set_gpu_thumbnail(page, id, [texture.width(), texture.height()]);
        }
    }

    fn free_textures(&self, ids: &[egui::TextureId]) {
        if ids.is_empty() {
            return;
        }
        let mut renderer = self.render_state.renderer.write();
        for id in ids {
            renderer.free_texture(id);
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        if let Some(dark) = self.desktop_theme.take_change() {
            self.desktop_dark = dark;
        }
        let desktop = self.desktop_dark.map(|dark| {
            if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            }
        });
        let system = ctx.system_theme().or(desktop);
        if let Some(system) = system {
            self.system_theme = Some(match system {
                egui::Theme::Dark => ThemeKind::Dark,
                egui::Theme::Light => ThemeKind::Light,
            });
        }
        let kind = self.prefs.theme.resolve(system, self.system_theme);
        let applied = self.appearance(kind);
        if self.theme_applied.as_ref() != Some(&applied) {
            theme::apply(ctx, &applied);
            self.theme_colours = crate::book_effects::theme_colours(&Tokens::get(ctx));
            self.theme_bg = Tokens::get(ctx).bg_rgb();
            self.theme_applied = Some(applied);
        }
        if self.fonts_applied.as_ref() != Some(&self.prefs.fonts) {
            crate::fonts::install(ctx, &self.prefs.fonts);
            self.fonts_applied = Some(self.prefs.fonts.clone());
        }
    }

    /// Settings → Appearance for the colours of `kind`.
    pub(crate) fn appearance(&self, kind: ThemeKind) -> theme::Appearance {
        theme::Appearance {
            kind,
            palette: self.palette(),
            opacity: self.window_opacity(),
            density: self.prefs.density,
            // Gamepad mode draws at the design's sizes, zoomed by its own
            // text size.
            ui_scale: if self.prefs.gamepad_mode {
                theme::BASE_SCALE
            } else {
                self.prefs.ui_scale
            },
            zoom: if self.prefs.gamepad_mode {
                self.prefs.couch_text
            } else {
                1.0
            },
            ornaments: self.prefs.glyph_ornaments,
            fonts: self.prefs.fonts.clone(),
        }
    }

    /// Where the colours come from: the accent swatch, or a scheme from the
    /// background or the custom colour. A background with no colour of its
    /// own (the theme's, an animated one, a wallpaper not yet read) keeps
    /// the swatch.
    pub(crate) fn palette(&self) -> theme::Palette {
        let seed = match self.prefs.colours {
            Colours::Preset => None,
            Colours::Background => self.background_seed(),
            Colours::Custom => Some(self.prefs.custom_colour),
        };
        match seed {
            Some(seed) => theme::Palette::Generated(glyphst_ui::scheme::Scheme {
                seed,
                style: self.prefs.scheme_style,
                contrast: self.prefs.scheme_contrast,
            }),
            None => theme::Palette::Preset(self.prefs.accent),
        }
    }

    /// The colour of the default background a scheme can be built on: a
    /// solid colour the user set, or the wallpaper's seed.
    pub(crate) fn background_seed(&self) -> Option<[u8; 3]> {
        let prefs = &self.prefs;
        if prefs.background == glyphst_effects::SOLID {
            let [r, g, b, _] = prefs.effect_params.get(glyphst_effects::SOLID)?;
            return Some(
                [*r, *g, *b].map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8),
            );
        }
        let draws_wallpaper = prefs.background == glyphst_effects::IMAGE
            || self
                .with_gpu(|gpu| {
                    gpu.effects()
                        .find(&prefs.background)
                        .is_some_and(|def| def.image)
                })
                .unwrap_or(false);
        let wallpaper = prefs.wallpaper.as_deref()?;
        prefs
            .wallpaper_seed
            .as_ref()
            .filter(|seed| draws_wallpaper && seed.path == wallpaper)
            .map(|seed| seed.seed)
    }

    /// How opaque the window's surfaces are: below 1 while transparency is
    /// on, unless fullscreen turns it opaque.
    pub(crate) fn window_opacity(&self) -> f32 {
        self.user_settings
            .get()
            .map_or(1.0, |settings| settings.window.opacity_in(self.fullscreen))
    }

    /// Turns the window's transparency on or off, in the settings file.
    fn toggle_transparency(&mut self) {
        let on = self
            .user_settings
            .get()
            .is_some_and(|settings| settings.window.transparent());
        self.user_settings
            .edit(|settings| settings.window.set_transparent(!on));
    }

    /// Notes a key or a click as the last input, for the hints that follow
    /// it. A pointer moved only slightly since the pad was used doesn't count.
    fn note_input_device(&mut self, events: &[egui::Event]) {
        use glyphst_ui::pad_glyph::InputDevice;
        for event in events {
            match event {
                egui::Event::Key { pressed: true, .. } | egui::Event::Text(_) => {
                    self.last_input = InputDevice::Keyboard;
                }
                egui::Event::PointerButton { pressed: true, .. }
                | egui::Event::MouseWheel { .. } => {
                    self.last_input = InputDevice::Pointer;
                }
                egui::Event::PointerMoved(at)
                    if self.last_input == InputDevice::Pad
                        && self.pad_pointer.is_none_or(|from| from.distance(*at) > 4.0) =>
                {
                    self.last_input = InputDevice::Pointer;
                }
                _ => {}
            }
        }
    }

    fn global_actions(&mut self, ctx: &egui::Context) -> Vec<ViewAction> {
        let mut actions = Vec::new();
        if self.active.is_some() {
            let (back, forward) = ctx.input(|i| {
                (
                    i.pointer.button_pressed(egui::PointerButton::Extra1),
                    i.pointer.button_pressed(egui::PointerButton::Extra2),
                )
            });
            if back {
                actions.push(ViewAction::Back);
            }
            if forward {
                actions.push(ViewAction::Forward);
            }
        }
        // Key presses go to a shortcut being recorded, and the settings
        // window and prompts stand in front of the document.
        if self.settings_window.open
            || self.prompts.is_open()
            || self.pages.iter().any(Page::is_recording)
        {
            return actions;
        }
        // The switcher takes the keys it uses; only Ctrl shortcuts get past.
        let typing = self.palette.open
            || self.switcher.is_open()
            || ctx.memory(|memory| memory.focused().is_some());
        // A list drawn last frame (the palette, the @ menu, the launcher)
        // takes Ctrl+N and Ctrl+P.
        let list_open = glyphst_ui::row::take_list_open(ctx) || self.palette.open;
        let presenting = self.presenting.is_some();
        let pressed =
            self.keymap
                .take_pressed(ctx, typing, list_open, presenting, &mut self.pending_keys);
        for keys::Pressed { command, count } in pressed {
            if let Err(reason) = self.run_counted(command, count, ctx)
                && self.active.is_some()
                && command.needs() != Needs::Document
            {
                self.notices.push(reason.to_string());
            }
        }
        if let Some(active) = self.active
            && !typing
        {
            let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
            if copy {
                self.copy_selection(ctx);
            }
            // Enter opens the overview's page.
            let tab = &mut self.tabs[active];
            if let Some(overview) = &tab.overview
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                let page = overview.cursor;
                tab.overview = None;
                actions.push(tab.overview_exit(page));
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                let tab = &mut self.tabs[active];
                if self.note_editor.is_some() {
                    self.note_editor = None;
                } else if self.region_tool {
                    self.region_tool = false;
                } else if tab.overview.is_some() {
                    tab.overview = None;
                } else if tab.selection.is_some() {
                    tab.selection = None;
                } else if tab.find.open {
                    tab.find.close();
                } else if tab.present.is_some() {
                    if !self.present_cancel() {
                        actions.push(ViewAction::TogglePresent);
                    }
                } else if self.zen {
                    self.zen = false;
                } else if self.prefs.gamepad_mode {
                    // B leaves the book for gamepad mode's home, which
                    // mustn't take the same press as asking to leave.
                    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                    self.activate(None);
                }
            }
        }
        actions
    }

    /// Why `command` can't run now, if it can't.
    pub(crate) fn command_blocked(&self, command: Command) -> Option<&'static str> {
        let Some(tab) = self.active.map(|active| &self.tabs[active]) else {
            return (command.needs() != Needs::Nothing).then(|| Needs::Document.reason());
        };
        // Presenting keeps to its own commands, the page turns and a few
        // others (frame 02i).
        if tab.present.is_some() && !present_allows(command) {
            return Some("Not while presenting");
        }
        let can = match command.needs() {
            Needs::Nothing | Needs::Document => true,
            Needs::Presenting => tab.present.is_some(),
            Needs::Selection => tab.selection.is_some(),
            Needs::Reflow => matches!(tab.document_layout, DocumentLayout::Reflow(_)),
            Needs::Translation => tab.translation.is_some(),
            Needs::Panes => self.panes.count() > 1,
            Needs::FixedLayout => !tab.is_reflowed(),
        };
        (!can).then(|| command.needs().reason())
    }

    /// The keymap for the preferences' preset and shortcuts.
    pub(crate) fn rebuild_keymap(&mut self) {
        self.keymap = Keymap::new(self.prefs.keymap, &self.prefs.shortcuts);
        self.pad_keymap =
            crate::pad_keys::PadKeymap::new(&self.prefs.pad_shortcuts, self.prefs.gamepad_mode);
        self.pending_keys.clear();
    }

    /// Runs a command a key ran, with the count typed before its keys:
    /// first and last page go to that page, counted commands repeat.
    fn run_counted(
        &mut self,
        command: Command,
        count: Option<u32>,
        ctx: &egui::Context,
    ) -> Result<(), &'static str> {
        let Some(count) = count else {
            return self.run(command, ctx);
        };
        match command {
            Command::FirstPage | Command::LastPage => {
                if let Some(reason) = self.command_blocked(command) {
                    return Err(reason);
                }
                let page = count.saturating_sub(1) as usize;
                self.pending_actions.push(ViewAction::GoToPage(page));
                ctx.request_repaint();
                Ok(())
            }
            command if command.counted() => {
                for _ in 0..count.min(MAX_REPEAT) {
                    self.run(command, ctx)?;
                }
                Ok(())
            }
            command => self.run(command, ctx),
        }
    }

    /// Runs a command: keys, the palette, menus and buttons all come here.
    pub(crate) fn run(
        &mut self,
        command: Command,
        ctx: &egui::Context,
    ) -> Result<(), &'static str> {
        // Closing a tab closes the page tab shown.
        if command == Command::CloseTab
            && let Some(index) = self.page_focused
        {
            self.close_page(index);
            return Ok(());
        }
        if let Some(reason) = self.command_blocked(command) {
            return Err(reason);
        }
        let view = |action| Some(action);
        let action = match command {
            Command::Open => {
                let start = self.open_start();
                self.palette.open_path(ctx, start);
                None
            }
            Command::OpenWithDialog => {
                self.pickers.open_documents(ctx);
                None
            }
            Command::CloseTab => {
                if let Some(active) = self.active {
                    self.close(active);
                }
                None
            }
            Command::SwitchBook | Command::SwitchBookBack | Command::BookSwitcher => {
                self.switch_book(command, ctx);
                None
            }
            Command::NextTab | Command::PreviousTab => {
                let count = self.tabs.len() + 1;
                let current = self.active.map_or(0, |active| active + 1);
                let next = if command == Command::PreviousTab {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                self.activate(next.checked_sub(1));
                None
            }
            Command::Palette => {
                self.palette.toggle();
                None
            }
            Command::Settings => {
                self.settings_window.open = true;
                None
            }
            Command::KeyboardShortcuts => {
                self.settings_window.open = false;
                self.open_keymap_tab();
                None
            }
            Command::NewTab => {
                self.open_page(Page::NewTab(Launcher::default()));
                None
            }
            Command::ReadingActivity => {
                self.open_activity(None);
                None
            }
            Command::Library => {
                self.open_library();
                None
            }
            Command::LibraryImport => {
                self.library_tab_action(crate::library_tab::LibraryTabAction::OpenDialog, ctx);
                None
            }
            Command::LibraryAddByIdentifier => {
                self.library_tab_action(crate::library_tab::LibraryTabAction::AddByIdentifier, ctx);
                None
            }
            Command::LibraryEnrichAll => {
                self.library_tab_action(crate::library_tab::LibraryTabAction::Backfill, ctx);
                None
            }
            Command::LibraryImportZotero => {
                self.library_tab_action(crate::library_tab::LibraryTabAction::ImportZotero, ctx);
                None
            }
            Command::LibraryReviewAi => {
                self.open_library();
                if let Some(Page::Library(tab)) = self
                    .page_focused
                    .and_then(|index| self.pages.get_mut(index))
                {
                    tab.view = crate::library_tab::View::NeedsReview;
                }
                None
            }
            Command::LibraryMove => {
                self.library_dialog = Some(self.choose_folder_dialog());
                None
            }
            Command::LibraryPauseJobs => {
                if self.library_jobs.is_paused() {
                    self.library_jobs.resume();
                    self.notices
                        .push("The library's AI pass goes on.".to_string());
                } else {
                    self.library_jobs.pause();
                    self.notices
                        .push("The library's AI pass is paused.".to_string());
                }
                None
            }
            Command::LibraryOpenNote => {
                let active = self.active.ok_or(Needs::Document.reason())?;
                let note = self
                    .library
                    .library_item_for_path(&self.tabs[active].path)
                    .and_then(|item| self.library.vault_note(item.id))
                    .map(|record| record.relative_path.clone());
                let vault = self
                    .prefs
                    .library
                    .vault_root(self.library.files_root(), self.library.files_root_chosen());
                match note.zip(vault) {
                    Some((relative, vault)) => self.library_tab_action(
                        crate::library_tab::LibraryTabAction::OpenNote(vault.join(relative)),
                        ctx,
                    ),
                    None => return Err("This document has no note yet"),
                }
                None
            }
            Command::SidebarItem => {
                let shown = self.prefs.show_sidebar && self.prefs.sidebar == SidebarTab::Item;
                self.prefs.show_sidebar = !shown;
                self.prefs.sidebar = SidebarTab::Item;
                None
            }
            Command::Home => {
                self.activate(None);
                None
            }
            Command::ToggleTransparency => {
                self.toggle_transparency();
                None
            }
            Command::UiScaleUp | Command::UiScaleDown | Command::UiScaleReset => {
                let range = theme::UI_SCALE_RANGE;
                self.prefs.ui_scale = match command {
                    Command::UiScaleUp => (self.prefs.ui_scale.round() + 1.0).min(*range.end()),
                    Command::UiScaleDown => (self.prefs.ui_scale.round() - 1.0).max(*range.start()),
                    _ => theme::BASE_SCALE,
                };
                None
            }
            Command::ToggleSidebar => {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
                None
            }
            Command::Outline if self.prefs.gamepad_mode => {
                self.open_couch(crate::couch::Overlay::Contents { cursor: 0 });
                None
            }
            Command::Outline => {
                let shown = self.prefs.show_sidebar && self.prefs.sidebar == SidebarTab::Outline;
                self.prefs.show_sidebar = !shown;
                self.prefs.sidebar = SidebarTab::Outline;
                None
            }
            Command::GoToPage => {
                self.open_couch(crate::couch::Overlay::GoTo { page: 0 });
                None
            }
            Command::GamepadMenu => {
                if !self.prefs.gamepad_mode {
                    return Err("Only in gamepad mode");
                }
                self.open_couch(crate::couch::Overlay::Menu { cursor: 0 });
                None
            }
            Command::GamepadMode => {
                self.set_gamepad_mode(!self.prefs.gamepad_mode, ctx);
                None
            }
            Command::ToggleStats => {
                self.show_stats = !self.show_stats;
                None
            }
            Command::PageColumns => {
                self.prefs.page_columns = self.prefs.page_columns.toggled();
                None
            }
            Command::PageOverview => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    tab.overview = match tab.overview {
                        Some(_) => None,
                        None => {
                            if tab.present.is_some() {
                                tab.apply(ViewAction::TogglePresent, false);
                            }
                            // The grid takes the room the sidebar had,
                            // unless it lists search matches to step
                            // through on the grid.
                            if self.prefs.sidebar != SidebarTab::Search {
                                self.prefs.show_sidebar = false;
                            }
                            Some(Overview::new(tab.current_page()))
                        }
                    };
                }
                None
            }
            Command::McpConfig => {
                match self.mcp_config() {
                    Ok(config) => {
                        ctx.copy_text(config);
                        self.notices.push(
                            "Copied an MCP server config for Glyphst; it works while the Reader runs.",
                        );
                    }
                    Err(error) => self
                        .notices
                        .push(format!("Could not start the MCP server: {error:#}")),
                }
                None
            }
            Command::QuickAsk => {
                if self.quick_ask.open {
                    self.quick_ask.close();
                } else {
                    self.quick_ask.show_composer();
                }
                None
            }
            Command::RegionSelect => {
                self.region_tool = !self.region_tool;
                if self.region_tool {
                    self.notices
                        .push("Drag a rectangle on the page. Esc cancels.");
                }
                None
            }
            Command::Zen => {
                self.zen = !self.zen;
                if self.zen {
                    let key = self.keymap.label(Command::Zen).unwrap_or_default();
                    self.notices
                        .push(format!("Zen mode. Press Esc or {key} to leave."));
                }
                None
            }
            Command::ThemeSystem => {
                self.prefs.theme = ThemePreference::System;
                None
            }
            Command::ThemeLight => {
                self.prefs.theme = ThemePreference::Light;
                None
            }
            Command::ThemeDark => {
                self.prefs.theme = ThemePreference::Dark;
                None
            }
            Command::KeysStandard | Command::KeysVim => {
                self.prefs.keymap = if command == Command::KeysVim {
                    KeymapPreset::Vim
                } else {
                    KeymapPreset::Standard
                };
                self.rebuild_keymap();
                self.notices
                    .push(format!("{} keys", self.prefs.keymap.label()));
                None
            }
            Command::ScrollDown
            | Command::ScrollUp
            | Command::ScrollLeft
            | Command::ScrollRight => {
                // When presenting they turn the page.
                let presenting = self
                    .active
                    .is_some_and(|active| self.tabs[active].present.is_some());
                let forward = matches!(command, Command::ScrollDown | Command::ScrollRight);
                let step = if forward { 1 } else { -1 };
                view(match command {
                    _ if presenting && forward => ViewAction::NextPage,
                    _ if presenting => ViewAction::PreviousPage,
                    Command::ScrollDown | Command::ScrollUp => ViewAction::Step(step),
                    _ => ViewAction::Sideways(step),
                })
            }
            Command::NextPage => view(ViewAction::NextPage),
            Command::PreviousPage => view(ViewAction::PreviousPage),
            Command::NextHalfPage => view(ViewAction::HalfPage(1)),
            Command::PreviousHalfPage => view(ViewAction::HalfPage(-1)),
            Command::FirstPage => view(ViewAction::GoToPage(0)),
            Command::LastPage => view(ViewAction::GoToPage(usize::MAX)),
            Command::Back => view(ViewAction::Back),
            Command::Forward => view(ViewAction::Forward),
            Command::ZoomIn => view(ViewAction::ZoomIn),
            Command::ZoomOut => view(ViewAction::ZoomOut),
            // Gamepad mode fits the width in one step, without the second
            // press's edge to edge.
            Command::FitWidth if self.prefs.gamepad_mode => view(ViewAction::FitMargins),
            Command::FitWidth => view(ViewAction::FitWidth),
            Command::FitPage => view(ViewAction::FitPage),
            Command::RotateClockwise | Command::RotateCounterclockwise => {
                let turn = if command == Command::RotateClockwise {
                    1
                } else {
                    3
                };
                self.arrange(|arrangement| {
                    arrangement.rotation = (arrangement.rotation + turn) % 4
                });
                None
            }
            Command::ToggleSpread => {
                self.arrange(|arrangement| arrangement.spread = !arrangement.spread);
                None
            }
            Command::Reload => {
                if let Some(active) = self.active {
                    self.reload(active, ctx);
                }
                None
            }
            Command::Present => view(ViewAction::TogglePresent),
            Command::ToggleFullscreen => {
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                // The window is the user's now: leaving keeps it.
                if let Some(state) = self.presenting.as_mut() {
                    state.made_fullscreen = false;
                }
                None
            }
            Command::PresentDraw
            | Command::PresentHighlight
            | Command::PresentErase
            | Command::PresentClear
            | Command::PresentUndo
            | Command::PresentRedo
            | Command::PresentCursor
            | Command::PresentBlack
            | Command::PresentWhite
            | Command::PresentTimer
            | Command::PresentTimerStop => {
                self.run_present(command, Instant::now());
                None
            }
            Command::SplitRight | Command::SplitDown => {
                let zone = if command == Command::SplitRight {
                    Zone::Right
                } else {
                    Zone::Bottom
                };
                self.split_next(zone)?;
                None
            }
            Command::FocusNextPane => {
                if let Some(next) = self.panes.next() {
                    self.focus_tab(next);
                }
                None
            }
            Command::ClosePane => {
                if let Some(active) = self.active
                    && self.panes.close_pane(self.tabs[active].id)
                    && let Some(focused) = self.panes.focused()
                {
                    self.focus_tab(focused);
                }
                None
            }
            Command::LargerText | Command::SmallerText => {
                let step = if command == Command::LargerText {
                    1.0
                } else {
                    -1.0
                };
                self.change_font_size(step);
                None
            }
            Command::Find => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let page = tab.current_page();
                    tab.find.show(page);
                }
                None
            }
            Command::FindNext | Command::FindPrevious => {
                self.find_step(command == Command::FindNext);
                None
            }
            Command::SelectPage => {
                if let Some(active) = self.active {
                    let tab = &mut self.tabs[active];
                    let page = tab.current_page();
                    tab.select_page(page);
                }
                None
            }
            Command::Bookmark => {
                self.toggle_bookmark();
                None
            }
            Command::TranslateSelection => {
                let at = ctx
                    .pointer_latest_pos()
                    .unwrap_or_else(|| ctx.content_rect().center());
                self.translate_selection(at, ctx);
                None
            }
            Command::TranslateDocument => {
                self.start_document_translation(ctx);
                None
            }
            Command::ShowTranslated => {
                if let Some(session) = self
                    .active
                    .and_then(|active| self.tabs[active].translation.as_mut())
                {
                    session.show_translated = !session.show_translated;
                }
                None
            }
            Command::Assistant => {
                self.toggle_assistant();
                None
            }
        };
        if let Some(action) = action {
            self.pending_actions.push(action);
        }
        ctx.request_repaint();
        Ok(())
    }

    /// Runs a command, telling the user when it can't run.
    pub(crate) fn run_or_notify(&mut self, command: Command, ctx: &egui::Context) {
        if let Err(reason) = self.run(command, ctx) {
            self.notices.push(reason.to_string());
        }
    }

    /// A menu entry for `command`: its icon, `text` and its keys.
    fn menu_button(
        &self,
        _ui: &egui::Ui,
        text: &str,
        command: Command,
    ) -> impl egui::Widget + use<> {
        let (text, key, icon) = (
            text.to_owned(),
            self.keymap.label(command).unwrap_or_default(),
            command.icon(),
        );
        move |ui: &mut egui::Ui| {
            glyphst_ui::menu::Item::new(&text)
                .icon(icon)
                .key(key.as_str())
                .show(ui)
        }
    }

    /// The palette and the settings window, drawn over everything.
    fn overlays(&mut self, ctx: &egui::Context) {
        if self.palette.open {
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
            let hits = if self.palette.mode == palette::Mode::OpenPath {
                self.palette.path_hits(&home)
            } else {
                self.palette_hits()
            };
            self.palette.current = self
                .active
                .and_then(|active| self.tabs[active].current_outline_entry())
                .map(Item::Section);
            match self.palette.show(ctx, &hits, &home) {
                Some(Item::Command(command)) => self.run_or_notify(command, ctx),
                Some(Item::Tab(index)) => self.activate(Some(index)),
                Some(Item::Open(path)) => self.open(&path, ctx, None),
                Some(Item::GoToPage(page)) => self.pending_actions.push(ViewAction::GoToPage(page)),
                Some(Item::Settings(section)) => self.settings_window.show_section(section),
                Some(Item::Section(index)) => {
                    self.pending_actions.push(ViewAction::GoToEntry(index));
                }
                Some(Item::Browse(_)) | None => {}
            }
        }
        if self.settings_window.open {
            let effects = self.effect_defs();
            let errors = self.effect_errors();
            let background_seed = self.background_seed();
            let library_facts = crate::settings_window::LibraryFacts {
                folder: self.library.files_root().map(std::path::Path::to_path_buf),
                folder_chosen: self.library.files_root_chosen(),
                unenriched: self
                    .library
                    .library_items()
                    .iter()
                    .filter(|item| {
                        item.trashed_at.is_none() && self.library.item_enrichment(item.id).is_none()
                    })
                    .count(),
                model: self
                    .user_settings
                    .get()
                    .and_then(|settings| settings.ai.resolved_library())
                    .map(|model| model.model),
                zotero: dirs::home_dir()
                    .map(|home| home.join("Zotero").join("zotero.sqlite"))
                    .filter(|path| path.is_file()),
                meaning: Some(crate::meaning::with_engine(
                    crate::meaning::card(&self.meaning.state()),
                    &self.meaning_runtime.status(),
                )),
                link_handler: if self.persist {
                    self.link_handler
                        .get_or_insert_with(crate::instance::handler)
                        .clone()
                } else {
                    None
                },
                rerank: Some(crate::meaning::with_engine(
                    crate::meaning::card(&self.rerank.state()),
                    &self.rerank_runtime.status(),
                )),
                index: self.library_search.as_ref().map(|search| {
                    (
                        search.status(),
                        glyphst_search::Store::size_on_disk(search.path()),
                    )
                }),
            };
            self.settings_window.show(
                ctx,
                SettingsContext {
                    prefs: &mut self.prefs,
                    effects: &effects,
                    effect_errors: &errors,
                    effects_folder: self.effects_dir.as_deref(),
                    time: self.started.elapsed().as_secs_f32(),
                    user: &mut self.user_settings,
                    chatgpt: &self.chatgpt,
                    system_theme: self.system_theme,
                    blur_supported: self.window_blur.support().known(),
                    background_seed,
                    library: library_facts,
                },
            );
            if std::mem::take(&mut self.settings_window.open_shortcuts) {
                self.open_keymap_tab();
            }
            let actions = std::mem::take(&mut self.settings_window.effects_actions);
            for action in actions {
                self.effects_action(action, ctx);
            }
            for action in std::mem::take(&mut self.settings_window.library_actions) {
                use crate::settings_window::LibraryAction;
                if action == LibraryAction::Reindex {
                    if let Some(search) = &self.library_search {
                        search.reindex();
                    }
                    continue;
                }
                match action {
                    LibraryAction::Meaning(action) => {
                        self.meaning_action(action, ctx);
                        continue;
                    }
                    LibraryAction::Rerank(action) => {
                        self.rerank_action(action, ctx);
                        continue;
                    }
                    LibraryAction::ForgetOpened => {
                        if let Some(search) = &self.library_search {
                            search.forget_opened();
                        }
                        continue;
                    }
                    LibraryAction::RegisterLinks => {
                        match crate::instance::register() {
                            Ok(()) => self.notices.push(
                                "glyphst:// links from other apps open here now.".to_string(),
                            ),
                            Err(error) => self.notices.push(format!(
                                "Could not register for glyphst:// links: {error:#}"
                            )),
                        }
                        self.link_handler = None;
                        continue;
                    }
                    _ => {}
                }
                self.settings_window.open = false;
                match action {
                    LibraryAction::Reindex
                    | LibraryAction::Meaning(_)
                    | LibraryAction::Rerank(_)
                    | LibraryAction::ForgetOpened
                    | LibraryAction::RegisterLinks => {}
                    LibraryAction::ChooseFolder => {
                        self.library_dialog = Some(self.choose_folder_dialog());
                    }
                    LibraryAction::Enrich => match self.backfill_dialog(None) {
                        Some(dialog) => self.library_dialog = Some(dialog),
                        None => self
                            .notices
                            .push("Choose a library model in Settings › AI first.".to_string()),
                    },
                    LibraryAction::ImportZotero => {
                        self.library_tab_action(
                            crate::library_tab::LibraryTabAction::ImportZotero,
                            ctx,
                        );
                    }
                }
            }
        }
        if let Some(path) = self.pickers.take_image() {
            self.prefs.wallpaper = Some(path.display().to_string());
        }
    }

    /// The command palette's hits for its query.
    fn palette_hits(&self) -> Vec<palette::Hit> {
        let presenting = self.presenting.is_some();
        let sources = palette::Sources {
            commands: Command::ALL
                .into_iter()
                .filter(|&command| !presenting || present_palette(command))
                .map(|command| {
                    (
                        command,
                        self.keymap.label(command),
                        self.command_blocked(command),
                    )
                })
                .collect(),
            tabs: self.tabs.iter().map(|tab| tab.title.clone()).collect(),
            recent: self.recent_documents(),
            pages: self.active.map(|active| self.tabs[active].layout.len()),
            labels: self.present_labels(),
            presenting,
            sections: self
                .active
                .map(|active| {
                    let outline = &self.tabs[active].outline;
                    outline
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entry)| {
                            Some(palette::Heading {
                                index,
                                title: entry.title.clone(),
                                page: entry.page?,
                                depth: entry.depth,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        palette::hits(&self.palette.query, &sources, self.palette.used())
    }

    /// Where Open document starts: the active document's folder, else the
    /// latest recent document's, else home; `~` for home, ending in `/`.
    fn open_start(&self) -> String {
        let home = dirs::home_dir();
        let folder = self
            .active
            .map(|active| self.tabs[active].path.clone())
            .into_iter()
            .chain(self.recent_documents())
            .find_map(|path| {
                path.parent()
                    .filter(|folder| folder.is_dir())
                    .map(Path::to_path_buf)
            })
            .or_else(|| home.clone());
        let Some(folder) = folder else {
            return "/".into();
        };
        let text = match home
            .as_deref()
            .and_then(|home| folder.strip_prefix(home).ok())
        {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Some(rest) => format!("~/{}", rest.display()),
            None => folder.display().to_string(),
        };
        if text.ends_with('/') {
            text
        } else {
            format!("{text}/")
        }
    }

    /// Every effect, for the galleries.
    pub(crate) fn effect_defs(&self) -> Vec<EffectDef> {
        self.with_gpu(|gpu| gpu.effects().defs().cloned().collect())
            .unwrap_or_default()
    }

    /// The user's effects that failed to load, for display.
    fn effect_errors(&self) -> Vec<String> {
        self.with_gpu(|gpu| {
            gpu.effects()
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
    }

    /// Does what the Effects page asked.
    fn effects_action(&mut self, action: crate::effects_page::Action, ctx: &egui::Context) {
        use crate::effects_page::{Action, open_folder, write_template};
        let Some(dir) = self.effects_dir.clone() else {
            self.notices
                .push("There is no configuration folder for effects.");
            return;
        };
        match action {
            Action::ChooseWallpaper => self.pickers.choose_image(ctx),
            Action::OpenFolder => {
                if let Err(error) = open_folder(&dir) {
                    self.notices
                        .push(format!("Could not open {}: {error}", dir.display()));
                }
            }
            Action::NewEffect => match write_template(&dir) {
                Ok(path) => {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.notices.push(format!(
                        "Wrote {name}: edit it, and the Reader reloads it when you save."
                    ));
                    let _ = open_folder(&dir);
                    self.reload_effects_if_changed(Instant::now());
                }
                Err(error) => self
                    .notices
                    .push(format!("Could not write a new effect: {error}")),
            },
        }
    }

    /// Recently opened documents that still exist, latest first.
    fn recent_documents(&self) -> Vec<PathBuf> {
        let mut entries: Vec<_> = self.library.history_entries().iter().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
        entries
            .into_iter()
            .filter(|entry| self.tabs.iter().all(|tab| tab.path != entry.path))
            .map(|entry| entry.path.clone())
            .filter(|path| path.exists())
            .take(20)
            .collect()
    }

    /// Does what a tab's context menu chose for tab `index`.
    fn tab_menu_action(&mut self, index: usize, action: TabMenu, ctx: &egui::Context) {
        let id = self.tabs[index].id;
        match action {
            TabMenu::Close | TabMenu::CloseOthers | TabMenu::CloseToTheRight => {
                for other in tabs_to_close(self.tabs.len(), index, action) {
                    self.close(other);
                }
            }
            TabMenu::Open(zone) => match self.panes.focused() {
                Some(pane) if self.active.is_some() => self.drop_tab(id, pane, zone),
                _ => self.activate(Some(index)),
            },
            TabMenu::Reload => self.reload(index, ctx),
            TabMenu::CopyPath => ctx.copy_text(self.tabs[index].path.display().to_string()),
            TabMenu::ShowInFolder => {
                let path = &self.tabs[index].path;
                let folder = path.parent().unwrap_or(path);
                if let Err(error) = crate::effects_page::open_folder(folder) {
                    self.notices
                        .push(format!("Could not open {}: {error}", folder.display()));
                }
            }
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut run = None;
        for command in [
            Command::Open,
            Command::OpenWithDialog,
            Command::Palette,
            Command::Settings,
            Command::KeyboardShortcuts,
        ] {
            let enabled = command != Command::OpenWithDialog || !self.pickers.is_showing();
            if ui
                .add_enabled(enabled, self.menu_button(ui, command.label(), command))
                .clicked()
            {
                run = Some(command);
                ui.close();
            }
        }
        glyphst_ui::menu::separator(ui);
        let sidebar = self
            .keymap
            .label(Command::ToggleSidebar)
            .unwrap_or_default();
        ui.checkbox(&mut self.prefs.show_sidebar, format!("Sidebar  {sidebar}"));
        let stats = self.keymap.label(Command::ToggleStats).unwrap_or_default();
        ui.checkbox(&mut self.show_stats, format!("Statistics  {stats}"));
        if let Some(command) = run {
            self.run_or_notify(command, &ctx);
        }
        ui.label(format!("Gamepad: {}", self.pads.status()));
    }

    /// A pane's document view: input, layout, loading, the GPU callback
    /// and loading placeholders. Pages it wants are added to `keep`.
    /// Returns whether it is animating and whether every visible page is
    /// drawn.
    fn document_view(
        &mut self,
        ui: &mut egui::Ui,
        view: PaneView<'_>,
        keep: &mut Vec<PageKey>,
    ) -> (bool, bool) {
        // `rect` is where pages are drawn; they are laid out in `content`.
        let PaneView {
            rect: content,
            draw: rect,
            index: active,
            focused,
            actions,
            pad,
            seconds,
            now,
        } = view;
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        // Pressing in another pane focuses it, before its input runs.
        if !focused && response.is_pointer_button_down_on() {
            let id = self.tabs[active].id;
            self.focus_tab(id);
        }
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let size = pixel_size(rect, ppp);
        self.tabs[active].inset = f64::from((content.left() - rect.left()) * ppp);
        self.tabs[active].inset_right = f64::from((rect.right() - content.right()) * ppp);
        let pointer = response.hover_pos().map(|pos| {
            [
                f64::from((pos.x - rect.min.x) * ppp),
                f64::from((pos.y - rect.min.y) * ppp),
            ]
        });
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        let maps: Vec<PageMap> = self.tabs[active]
            .visible_draws(size)
            .iter()
            .map(|draw| PageMap::new(draw, rect, ppp))
            .collect();
        let mut actions = actions;
        let presenting = self.tabs[active].present.is_some() && self.presenting.is_some();
        let autoscroll = if presenting {
            self.present_input(&ctx, &response, &maps, active, &mut actions, now);
            0.0
        } else {
            self.text_input(&ctx, &response, &maps, active, rect, &mut actions)
        };
        let tab_id = self.tabs[active].id;
        let drag = if self.drag == Some((tab_id, Drag::Pan)) {
            response.drag_delta()
        } else {
            egui::Vec2::ZERO
        };
        let ppp64 = f64::from(ppp);
        let input = ViewInput {
            zoom: if pointer.is_some() {
                f64::from(zoom)
            } else {
                1.0
            },
            scroll: [
                -f64::from(drag.x) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.x) * ppp64
                    } else {
                        0.0
                    },
                -f64::from(drag.y) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.y) * ppp64
                    } else {
                        0.0
                    }
                    + autoscroll * ppp64,
            ],
            // The view zooms about points of the laid-out area.
            pointer: pointer.map(|[x, y]| [x - self.tabs[active].inset, y]),
            actions,
            gamepad: pad,
            seconds,
            now: Some(now),
            smooth: !glyphst_ui::motion::is_reduced(&ctx) && self.prefs.smooth_scrolling,
        };
        let turning = self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.tab == tab_id);
        self.tabs[active].pixels_per_point = f64::from(ppp);
        let mut frame = self.tabs[active].frame(size, input, turning);
        if let Some(direction) = frame.turn
            && let Some(effect) = self.effects_for(Some(active)).transition
        {
            self.begin_transition(effect, tab_id, direction);
        }

        // Loading: ask for wanted pages; the stage drops pages over the
        // budget once every pane has said what it keeps.
        keep.extend(
            frame
                .wanted
                .iter()
                .map(|&page| PageKey { tab: tab_id, page }),
        );
        let resident: std::collections::HashSet<usize> = frame
            .wanted
            .iter()
            .copied()
            .filter(|&page| self.is_resident(PageKey { tab: tab_id, page }))
            .collect();
        self.tabs[active].want(&frame.wanted, |page| resident.contains(&page));
        // A page being uploaded keeps its placeholder until it can be drawn.
        let drawable: std::collections::HashSet<usize> = frame
            .wanted
            .iter()
            .copied()
            .filter(|&page| {
                self.with_gpu(|gpu| gpu.is_drawable(PageKey { tab: tab_id, page }))
                    .unwrap_or(false)
            })
            .collect();
        self.tabs[active].placeholders(&mut frame, |page| drawable.contains(&page));
        // Pages showing only their preview are not drawn yet.
        let previewing = frame.wanted.iter().any(|&page| {
            self.with_gpu(|gpu| gpu.is_previewing(PageKey { tab: tab_id, page }))
                .unwrap_or(false)
        });
        for draw in &frame.draws {
            let [x0, y0, x1, y1] = draw.paper;
            let tab = &self.tabs[active];
            let state = PageState {
                loading: !drawable.contains(&draw.page),
                thumbnail: tab.thumbnail(draw.page).is_some(),
                size: egui::vec2(x1 - x0, y1 - y0) / ppp,
            };
            let key = PageKey {
                tab: tab_id,
                page: draw.page,
            };
            self.generations
                .update(key, state, tab.sketch(draw.page), now);
        }

        let tokens = Tokens::get(&ctx);
        // Origins and the composite capture's are in points; the view
        // works in its own pixels.
        let to_pixels = |point: egui::Pos2| {
            let at = (point - rect.min) * ppp;
            [at.x, at.y]
        };
        let transition = self
            .transition
            .as_mut()
            .filter(|transition| transition.tab == tab_id)
            .map(|transition| TransitionUse {
                effect: transition.effect.clone(),
                progress: transition.progress(now),
                direction: transition.direction,
                origin: transition.origin.map(to_pixels),
                seed: transition.seed,
                reversed: transition.reversed(),
                capture: transition.take_capture().map(|capture| match capture {
                    Capture::View(tab) => GpuCapture::View(tab),
                    Capture::Clear => GpuCapture::Clear,
                    Capture::Composite {
                        tab,
                        effect,
                        progress,
                        direction,
                        origin,
                        seed,
                        reversed,
                    } => GpuCapture::Composite {
                        tab,
                        effect,
                        progress,
                        direction,
                        origin: origin.map(to_pixels),
                        seed,
                        reversed,
                    },
                }),
            });
        let page_color = tokens.page.to_normalized_gamma_f32();
        // The pages draw into a window-sized image, so a pane changing
        // size (a panel sliding) doesn't make a new one; the pane shows its
        // top-left part.
        let window = pixel_size(ctx.content_rect(), ppp);
        let target = crate::gpu::target_size(size, window);
        let mut draws = frame.draws.clone();
        for draw in &mut draws {
            draw.camera.viewport = target;
        }
        // Pages sit flat on the base layer: the design has no shadows.
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewCallback {
                rect,
                target,
                tab: tab_id,
                draws,
                page_color,
                transition,
                overlay: self.effects_for(Some(active)).overlay,
                time: self.started.elapsed().as_secs_f32(),
                pointer: pointer.map(|[x, y]| [x as f32, y as f32]),
            },
        ));
        let painter = ui.painter_at(rect);
        let maps: Vec<PageMap> = frame
            .draws
            .iter()
            .map(|draw| PageMap::new(draw, rect, ppp))
            .collect();
        self.paint_text_overlays(&painter, &maps, active, &tokens);
        if presenting {
            self.paint_ink(&painter, &maps, active);
        }
        self.region_overlay(&ctx, &painter, &maps, active, &tokens);
        let turns = self.tabs[active].layout.arrangement.rotation;
        for (paper, thumbnail) in &frame.placeholders {
            let paper = egui::Rect::from_min_max(
                rect.min + egui::vec2(paper[0], paper[1]) / ppp,
                rect.min + egui::vec2(paper[2], paper[3]) / ppp,
            );
            if let Some(texture) = thumbnail {
                glyphst_ui::paint::paint_turned_image(&painter, *texture, paper, turns);
            }
        }
        let papers: Vec<egui::Rect> = maps.iter().map(|map| map.paper).collect();
        for map in &maps {
            let key = PageKey {
                tab: tab_id,
                page: map.page,
            };
            self.generations
                .paint(&painter, key, map, &papers, &tokens, now);
        }
        self.page_chrome(ui, &painter, rect, &maps, active, now);
        let focused = self.active == Some(active);
        if self.show_stats && focused {
            self.stats_overlay(&painter, rect, active);
        }
        let enter_leaves = self.keymap.preset() == KeymapPreset::Vim;
        let tab = &mut self.tabs[active];
        if focused
            && tab.find.open
            && let Some(action) = find::find_bar(&mut tab.find, rect, &ctx, now, enter_leaves)
        {
            match action {
                FindBarAction::Step(forward) => self.find_step(forward),
                FindBarAction::Close => tab.find.close(),
            }
        }
        if self.tabs[active].is_reflowed() {
            self.reflow_to_view(active, content.width(), now, &ctx);
        }
        (
            frame.animating || autoscroll != 0.0,
            frame.placeholders.is_empty() && !previewing,
        )
    }

    /// Pointer input on page text: hover cursors, selecting by dragging,
    /// clicking links and highlights, double and triple clicks, and the
    /// context menu. Returns the autoscroll, in points per frame, while a
    /// selection is dragged past the view's edge.
    fn text_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        maps: &[PageMap],
        active: usize,
        rect: egui::Rect,
        actions: &mut Vec<ViewAction>,
    ) -> f64 {
        let modifiers = ctx.input(|i| i.modifiers);
        let tab = &self.tabs[active];
        let tab_id = tab.id;
        // This view's drag; another pane may have its own.
        let own = |drag: Option<(u64, Drag)>| {
            drag.filter(|(tab, _)| *tab == tab_id).map(|(_, drag)| drag)
        };
        let reflowed = tab.is_reflowed();
        // What is under a screen position: the page map and page point.
        let under = |pos: egui::Pos2| page_at(maps, pos).map(|map| (*map, map.page_point(pos)));

        if let Some(pos) = response.hover_pos()
            && let Some((map, point)) = under(pos)
        {
            let link = tab.link_at(map.page, point).is_some();
            let text = tab.text.get(map.page).is_some_and(|text| {
                text.glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
                    .is_some()
            });
            if (self.region_tool && !reflowed) || matches!(own(self.drag), Some(Drag::Region)) {
                ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
            } else if matches!(own(self.drag), Some(Drag::Select)) || (text && !link) {
                ctx.set_cursor_icon(egui::CursorIcon::Text);
            } else if link {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }

        // Over a translated block: its outline, and the original text.
        if self.drag.is_none()
            && self.prefs.original_on_hover
            && let Some(pos) = response.hover_pos()
            && let Some((map, point)) = under(pos)
            && let Some((block, original)) = tab.original_at(map.page, point)
        {
            let painter = ctx
                .layer_painter(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::unique("translation-hover"),
                ))
                .with_clip_rect(rect.intersect(map.paper));
            let tokens = Tokens::get(ctx);
            // The block in a dotted accent outline, its original in a card
            // at the pointer (frame 02c).
            let outline = map.rect_to_screen(block).expand(3.0);
            for (from, to) in [
                (outline.left_top(), outline.right_top()),
                (outline.right_top(), outline.right_bottom()),
                (outline.right_bottom(), outline.left_bottom()),
                (outline.left_bottom(), outline.left_top()),
            ] {
                painter.add(egui::Shape::dotted_line(
                    &[from, to],
                    tokens.accent,
                    4.0,
                    0.9,
                ));
            }
            let source = tab
                .translation
                .as_ref()
                .map(|session| format!("original — p.{} — → {}", map.page + 1, session.target))
                .unwrap_or_else(|| "original".into());
            let mut tip = egui::Tooltip::for_enabled(response).at_pointer();
            tip.popup = tip
                .popup
                .frame(glyphst_ui::pane::popover(ctx).inner_margin(egui::Margin::symmetric(12, 8)));
            tip.show(|ui| {
                ui.set_max_width(320.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                glyphst_ui::motif::rule_title(ui, &source.to_lowercase(), None);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(original)
                            .font(theme::regular(14.0))
                            .color(tokens.text),
                    )
                    .wrap(),
                );
            });
        }

        if response.drag_started_by(egui::PointerButton::Primary) {
            self.drag = Some((tab_id, Drag::Pan));
            let origin = ctx
                .input(|i| i.pointer.press_origin())
                .or(response.interact_pointer_pos());
            let region_start = origin
                .and_then(under)
                .filter(|_| self.region_tool && !reflowed);
            if let Some((map, point)) = region_start {
                self.drag = Some((tab_id, Drag::Region));
                self.region = Some(Region {
                    tab: tab_id,
                    page: map.page,
                    from: point,
                    to: point,
                });
                self.tabs[active].selection = None;
            } else if let Some((map, point)) = origin.and_then(under)
                && let Some(text) = self.tabs[active].text.get(map.page).cloned()
            {
                let on_text = text
                    .glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
                    .is_some();
                let selection = if modifiers.alt {
                    Some(Selection::Column {
                        page: map.page,
                        from: point,
                        to: point,
                    })
                } else if on_text {
                    text.nearest(point[0], point[1]).map(|glyph| {
                        let at = TextPos {
                            page: map.page,
                            glyph,
                        };
                        Selection::Run {
                            anchor: at,
                            head: at,
                        }
                    })
                } else {
                    None
                };
                if let Some(selection) = selection {
                    self.drag = Some((tab_id, Drag::Select));
                    self.tabs[active].selection = Some(selection);
                }
            }
        }
        let mut autoscroll = 0.0;
        if let Some(Drag::Select) = own(self.drag)
            && let Some(pos) = response.interact_pointer_pos()
        {
            let tab = &mut self.tabs[active];
            match &mut tab.selection {
                // A run follows the pointer onto any page shown.
                Some(Selection::Run { head, .. }) => {
                    if let Some(map) = nearest_page(maps, pos)
                        && let Some(text) = tab.text.get(map.page)
                    {
                        let point = map.page_point(pos);
                        if let Some(glyph) = text.nearest(point[0], point[1]) {
                            *head = TextPos {
                                page: map.page,
                                glyph,
                            };
                        }
                    }
                }
                Some(Selection::Column { page, to, .. }) => {
                    if let Some(map) = maps.iter().find(|map| map.page == *page) {
                        *to = map.page_point(pos);
                    }
                }
                None => {}
            }
            if pos.y < rect.top() + 24.0 {
                autoscroll = -SELECT_SCROLL;
            } else if pos.y > rect.bottom() - 24.0 {
                autoscroll = SELECT_SCROLL;
            }
        }
        if let Some(Drag::Region) = own(self.drag)
            && let Some(pos) = response.interact_pointer_pos()
            && let Some(region) = &mut self.region
            && let Some(map) = maps.iter().find(|map| map.page == region.page)
        {
            region.to = map.page_point(pos);
            if pos.y < rect.top() + 24.0 {
                autoscroll = -SELECT_SCROLL;
            } else if pos.y > rect.bottom() - 24.0 {
                autoscroll = SELECT_SCROLL;
            }
        }
        if own(self.drag).is_some() && (response.drag_stopped() || !response.dragged()) {
            if let Some(Drag::Region) = own(self.drag) {
                self.finish_region(active);
            }
            self.drag = None;
        }

        let click = response.interact_pointer_pos().and_then(under);
        // Shift+click extends a run to the glyph clicked, on any page.
        let extended = response.clicked_by(egui::PointerButton::Primary)
            && modifiers.shift
            && click.is_some_and(|(map, point)| {
                let tab = &mut self.tabs[active];
                let glyph = tab
                    .text
                    .get(map.page)
                    .and_then(|text| text.nearest(point[0], point[1]));
                match (&mut tab.selection, glyph) {
                    (Some(Selection::Run { head, .. }), Some(glyph)) => {
                        *head = TextPos {
                            page: map.page,
                            glyph,
                        };
                        true
                    }
                    _ => false,
                }
            });
        if response.clicked_by(egui::PointerButton::Primary) && !extended {
            let tab = &mut self.tabs[active];
            match click {
                Some((map, point)) => {
                    if let Some(target) = tab.link_at(map.page, point).cloned() {
                        match target {
                            LinkTarget::Page { page, y } => {
                                actions.push(ViewAction::GoTo { page, y });
                            }
                            LinkTarget::Uri(uri) => ctx.open_url(egui::OpenUrl::new_tab(uri)),
                        }
                    } else if let Some(id) = tab.annotations.hit(map.page, point, reflowed) {
                        let item = tab.annotations.get(id);
                        self.note_editor = item.map(|item| NoteEditor {
                            id,
                            at: response.interact_pointer_pos().unwrap_or_default(),
                            body: item.body.clone(),
                            color: item.color,
                            fresh: true,
                        });
                    } else {
                        tab.selection = None;
                    }
                }
                None => tab.selection = None,
            }
        }
        if let Some((map, point)) = click
            && (response.double_clicked() || response.triple_clicked())
        {
            let tab = &mut self.tabs[active];
            if let Some(text) = tab.text.get(map.page)
                && let Some(glyph) = text.glyph_at(point[0], point[1], map.points(GLYPH_SLOP))
            {
                let range = if response.triple_clicked() {
                    text.line_at(glyph)
                } else {
                    text.word_at(glyph)
                };
                tab.selection = range.map(|range| Selection::run(map.page, range));
            }
        }
        if response.secondary_clicked() {
            self.context_page = click.map(|(map, _)| map.page);
        }
        let menu_at = response.interact_pointer_pos().unwrap_or(rect.center());
        response.context_menu(|ui| {
            glyphst_ui::menu::style(ui);
            self.context_menu(ui, ctx, active, menu_at, actions)
        });
        autoscroll
    }

    fn context_menu(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        active: usize,
        at: egui::Pos2,
        actions: &mut Vec<ViewAction>,
    ) {
        let selected = self.tabs[active].selected_text();
        let translated = self.tabs[active]
            .selection
            .as_ref()
            .is_some_and(|selection| {
                selection
                    .pages()
                    .any(|page| self.tabs[active].shows_translation(page))
            });
        if let Some(text) = &selected {
            if ui.add(menu_entry(ui, "copy", "Copy", "Ctrl+C")).clicked() {
                ctx.copy_text(text.clone());
                ui.close();
            }
            if ui
                .add(self.menu_button(ui, "Translate", Command::TranslateSelection))
                .clicked()
            {
                self.translate_selection(at, ctx);
                ui.close();
            }
            if ui
                .add(self.menu_button(ui, "Ask about it…", Command::QuickAsk))
                .clicked()
            {
                self.quick_ask.show_composer();
                ui.close();
            }
        }
        if let Some(text) = selected.as_ref().filter(|_| !translated) {
            glyphst_ui::menu::separator(ui);
            glyphst_ui::menu::group(ui, "highlight");
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.spacing_mut().item_spacing.x = 6.0;
                for color in HighlightColor::ALL {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                    let swatch = if response.hovered() {
                        rect.shrink(1.0)
                    } else {
                        rect.shrink(3.0)
                    };
                    ui.painter()
                        .rect_filled(swatch, 1, annotations::color(color));
                    if response.tip(annotations::color_name(color)).clicked() {
                        self.annotate_selection(color, false, at);
                        ui.close();
                    }
                }
            });
            if ui
                .add(menu_entry(ui, "sticky-note", "Add note…", ""))
                .clicked()
            {
                self.annotate_selection(HighlightColor::Yellow, true, at);
                ui.close();
            }
            if text.chars().count() <= 64
                && ui
                    .add(menu_entry(ui, "search", "Find in document", ""))
                    .clicked()
            {
                let tab = &mut self.tabs[active];
                let page = tab.current_page();
                tab.find.show(page);
                tab.find.query = text.split_whitespace().collect::<Vec<_>>().join(" ");
                ui.close();
            }
            glyphst_ui::menu::separator(ui);
        }
        if let Some(page) = self.context_page
            && ui
                .add(self.menu_button(ui, "Select all on page", Command::SelectPage))
                .clicked()
        {
            self.tabs[active].select_page(page);
            ui.close();
        }
        if ui
            .add(self.menu_button(ui, "Add bookmark", Command::Bookmark))
            .clicked()
        {
            self.add_bookmark();
            ui.close();
        }
        let (back, forward) = (
            self.menu_button(ui, "Back", Command::Back),
            self.menu_button(ui, "Forward", Command::Forward),
        );
        let tab = &self.tabs[active];
        if ui.add_enabled(tab.can_go_back(), back).clicked() {
            actions.push(ViewAction::Back);
            ui.close();
        }
        if ui.add_enabled(tab.can_go_forward(), forward).clicked() {
            actions.push(ViewAction::Forward);
            ui.close();
        }
    }

    /// Library highlights, search matches and the selection over the pages.
    fn paint_text_overlays(
        &self,
        painter: &egui::Painter,
        maps: &[PageMap],
        active: usize,
        tokens: &Tokens,
    ) {
        let tab = &self.tabs[active];
        let reflowed = tab.is_reflowed();
        for map in maps {
            // Highlights belong to the original text.
            let annotations = if tab.shows_translation(map.page) {
                Vec::new()
            } else {
                tab.annotations.on_page(map.page, reflowed)
            };
            for (item, rects) in annotations {
                fill_rects(
                    painter,
                    map,
                    rects.iter().copied(),
                    annotations::fill(item.color),
                );
                if let (AnnotationId::Note(_), Some(last)) = (item.id, rects.last()) {
                    let corner = map.screen_pos([last[2], last[1]]);
                    painter.circle_filled(corner, 4.0, annotations::color(item.color));
                }
            }
            let Some(text) = tab.text.get(map.page) else {
                continue;
            };
            for index in tab.find.on_page(map.page) {
                let found = &tab.find.matches[index];
                let current = tab.find.current == Some(index);
                let (tint, edge) = find::match_tint(tokens, current);
                let rects = text.selection_rects(found.range.clone());
                fill_rects(painter, map, rects.iter().copied(), tint);
                if let Some(edge) = edge {
                    for rect in rects {
                        painter.rect_stroke(
                            map.rect_to_screen(rect).expand(1.0),
                            1,
                            edge,
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
            if let Some(selection) = &tab.selection
                && selection.touches(map.page)
            {
                fill_rects(
                    painter,
                    map,
                    selection.rects_on(map.page, text),
                    tokens.accent.gamma_multiply(0.35),
                );
            }
        }
    }

    /// The popover of a highlight or note clicked on the page: colour,
    /// note text, delete.
    fn note_popover(&mut self, ctx: &egui::Context) {
        let Some(editor) = &mut self.note_editor else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let mut color = None;
        let mut save = false;
        let mut delete = false;
        let is_note = matches!(editor.id, AnnotationId::Note(_));
        let area = egui::Area::new(egui::Id::unique("note-editor"))
            .order(egui::Order::Foreground)
            .fixed_pos(editor.at + egui::vec2(8.0, 8.0))
            .show(ctx, |ui| {
                glyphst_ui::pane::popover(ctx)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_max_width(280.0);
                        ui.horizontal(|ui| {
                            for candidate in HighlightColor::ALL {
                                let (rect, response) = ui.allocate_exact_size(
                                    egui::vec2(22.0, 22.0),
                                    egui::Sense::click(),
                                );
                                let chosen = candidate == editor.color;
                                if chosen {
                                    ui.painter().circle_stroke(
                                        rect.center(),
                                        10.0,
                                        egui::Stroke::new(2.0, tokens.accent),
                                    );
                                }
                                ui.painter().circle_filled(
                                    rect.center(),
                                    if chosen || response.hovered() {
                                        7.5
                                    } else {
                                        7.0
                                    },
                                    annotations::color(candidate),
                                );
                                if response.clicked() {
                                    color = Some(candidate);
                                }
                            }
                        });
                        if is_note {
                            glyphst_ui::text::fitted(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut editor.body)
                                        .margin(glyphst_ui::text::FIELD_MARGIN)
                                        .hint_text("Note")
                                        .desired_rows(3)
                                        .desired_width(260.0),
                                )
                            });
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            if is_note
                                && ui
                                    .add(glyphst_ui::button::Button::primary("save").icon("check"))
                                    .clicked()
                            {
                                save = true;
                            }
                            if ui
                                .add(glyphst_ui::button::Button::ghost("delete").icon("trash-2"))
                                .clicked()
                            {
                                delete = true;
                            }
                        });
                    });
            });
        let outside = area.response.clicked_elsewhere() && !editor.fresh;
        editor.fresh = false;
        let (id, body) = (editor.id, editor.body.clone());
        if let Some(color) = color {
            editor.color = color;
            if is_note {
                let result = match id {
                    AnnotationId::Note(note) => self.library.update_note(note, body.clone(), color),
                    AnnotationId::Highlight(_) => Ok(false),
                };
                self.after_edit(result);
            } else {
                self.set_annotation_color(id, color);
            }
        }
        if delete {
            self.remove_annotation(id);
            self.note_editor = None;
        } else if save || outside {
            if is_note && body != self.annotation_body(id) {
                self.set_note_body(id, body);
            }
            self.note_editor = None;
        }
    }

    pub(crate) fn fit_scale(&self) -> Option<f64> {
        self.active.map(|active| self.tabs[active].fit_scale())
    }

    fn stats_overlay(&self, painter: &egui::Painter, rect: egui::Rect, active: usize) {
        let tab = &self.tabs[active];
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        let key = PageKey {
            tab: tab.id,
            page: tab.current_page(),
        };
        let source = self
            .with_gpu(|gpu| gpu.source(key, tab.view.scale as f32))
            .flatten()
            .unwrap_or_else(|| "loading".into());
        let text = format!(
            "frame p50 {:.1} ms  p95 {:.1} ms\nencode {:.2} ms  draws {}  segments {}\nresident {}  GPU {:.0} MiB  {source}\nrepaints {}",
            self.frames.percentile_ms(0.5),
            self.frames.percentile_ms(0.95),
            stats.encode_ms,
            stats.draw.draw_calls,
            stats.draw.segments,
            stats.resident,
            stats.gpu_bytes as f64 / 1048576.0,
            self.repaints,
        );
        let font = theme::mono(11.0);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
        let at = rect.right_top() + egui::vec2(-20.0 - galley.size().x, 20.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(10.0),
            10.0,
            egui::Color32::from_black_alpha(180),
        );
        painter.galley(at, galley, egui::Color32::WHITE);
    }
}

/// Whether `command` can run while presenting: its own commands, page
/// turns, and leaving, fullscreen, the palette and settings.
fn present_allows(command: Command) -> bool {
    use crate::commands::Category;
    matches!(command.category(), Category::Present | Category::Navigation)
        || matches!(
            command,
            Command::Present
                | Command::ToggleFullscreen
                | Command::Palette
                | Command::Settings
                | Command::PageOverview
        )
}

/// The palette's commands while presenting (frame 02i): presenting's,
/// then leaving, fullscreen and settings; page numbers go to their page.
fn present_palette(command: Command) -> bool {
    command.category() == crate::commands::Category::Present
        || matches!(
            command,
            Command::Present | Command::ToggleFullscreen | Command::Settings
        )
}

impl eframe::App for ReaderApp {
    /// Routes the pads before the frame reads its input, so the keys they
    /// press reach every widget as typed ones do.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.note_input_device(&raw_input.events);
        // Pads reach every app (they aren't Wayland input); the window
        // takes them only while it has focus, and lets go when it loses it.
        let focused = raw_input.focused;
        if self.pad_focused && !focused {
            self.pad_ring_steps.extend(self.pad_router.lose_focus());
        }
        self.pad_focused = focused;
        let frame = self.pads.frame(focused);
        let state = RouteState {
            recording: self.pages.iter().any(Page::is_recording),
            modal: self.settings_window.open
                || self.palette.open
                || self.prompts.is_open()
                || self.switcher.is_open()
                || self.couch.overlay.is_some(),
            document: self.active.is_some(),
            overview: self
                .active
                .is_some_and(|active| self.tabs[active].overview.is_some()),
            toast_action: self.notices.has_action(),
        };
        self.pad_events.extend(
            frame
                .events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        crate::pad::PadEvent::Connected { .. }
                            | crate::pad::PadEvent::Disconnected(_)
                    )
                })
                .cloned(),
        );
        let faces: Vec<glyphst_ui::pad_glyph::Face> = frame
            .events
            .iter()
            .filter_map(|event| match event {
                crate::pad::PadEvent::Pressed(button) => button.face(),
                _ => None,
            })
            .collect();
        glyphst_ui::pad_glyph::push_faces(ctx, &faces);
        crate::pad::set_frame_events(ctx, &frame.events);
        let routed = self
            .pad_router
            .route(&frame, &state, &self.pad_keymap, Instant::now());
        let mut keys = routed.keys;
        // egui's arrows move its focus only once something has it: on a
        // page tab with nothing focused, the first arrow focuses its first
        // control instead (home focuses its own).
        let arrow = |key: &egui::Key| {
            matches!(
                key,
                egui::Key::ArrowUp
                    | egui::Key::ArrowDown
                    | egui::Key::ArrowLeft
                    | egui::Key::ArrowRight
            )
        };
        if route::owner(&state) == route::Owner::Keys
            && !state.modal
            && self.active.is_none()
            && self.page_focused.is_some()
            && !self.prefs.gamepad_mode
            && keys.iter().any(|(key, _)| arrow(key))
            && ctx.memory(|memory| memory.focused().is_none())
        {
            keys.retain(|(key, _)| !arrow(key));
            keys.insert(0, (egui::Key::Tab, egui::Modifiers::NONE));
        }
        raw_input.events.extend(route::key_events(&keys));
        self.pad_input = routed.document;
        self.pad_commands.extend(routed.commands);
        self.pad_ring_steps.extend(routed.ring);
        if let Some(wait) = routed.wake {
            ctx.request_repaint_after(wait);
        }
        if frame.used() {
            self.last_input = glyphst_ui::pad_glyph::InputDevice::Pad;
            self.pad_pointer = ctx.input(|i| i.pointer.latest_pos());
        }
        let time = ctx.input(|i| i.time);
        match routed.hints {
            route::HintsWish::None => {}
            route::HintsWish::Show => {
                self.couch.hints_until = time + crate::couch::HINTS_SECONDS;
            }
            // Pins them on, even while they show for a while; again, hides
            // them.
            route::HintsWish::Toggle => {
                self.couch.hints_pinned = !self.couch.hints_pinned;
                self.couch.hints_until = 0.0;
            }
        }
        if frame.events.iter().any(|event| {
            matches!(
                event,
                crate::pad::PadEvent::Connected { .. } | crate::pad::PadEvent::Disconnected(_)
            )
        }) {
            // Glyphs are labelled for the pad connected last.
            let labels = self
                .pads
                .connected()
                .last()
                .map(|pad| pad.family.labels())
                .unwrap_or_default();
            glyphst_ui::pad_glyph::set_labels(ctx, labels);
        }
        // With the last pad gone its glyphs would name nothing.
        if frame
            .events
            .iter()
            .any(|event| matches!(event, crate::pad::PadEvent::Disconnected(_)))
            && self.pads.connected().is_empty()
        {
            self.last_input = glyphst_ui::pad_glyph::InputDevice::Pointer;
        }
        glyphst_ui::pad_glyph::set_input(ctx, self.last_input);
    }

    /// The window is transparent-capable, and the panels paint every pixel:
    /// opaque, or as translucent as the window's opacity.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    /// Runs before every frame, and also while the window is hidden: the
    /// agents' tool calls are answered here.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.serve_agent_calls(ctx);
        self.poll_agents();
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        // egui's frame time: after an idle frame, a predicted one rather
        // than the idle gap, so an animation starting then doesn't leap.
        let seconds = f64::from(ctx.input(|i| i.stable_dt)).min(0.1);
        self.repaints += 1;
        if !self.continuous {
            self.frames.idle();
        }
        self.frames.tick();
        self.fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        self.apply_theme(&ctx);
        // Screenshots are taken of settled frames.
        let reduce_motion =
            self.harness.as_ref().is_some_and(|h| !h.moves()) || self.prefs.reduce_motion;
        glyphst_ui::motion::set_reduced(&ctx, reduce_motion);
        let blur = self.user_settings.get().is_some_and(|settings| {
            settings.window.blur() && settings.window.opacity_in(self.fullscreen) < 1.0
        });
        self.window_blur
            .update(frame, blur, ctx.content_rect().size());

        for path in self.pickers.take() {
            self.open(&path, &ctx, None);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect()
        });
        // On the library, everything dropped comes in; elsewhere documents
        // open and folders come in.
        let on_library = self.active.is_none()
            && self
                .page_focused
                .is_some_and(|index| matches!(self.pages.get(index), Some(Page::Library(_))));
        let (imports, opens): (Vec<PathBuf>, Vec<PathBuf>) = dropped
            .into_iter()
            .filter(|path| path.is_dir() || is_document(path))
            .partition(|path| on_library || path.is_dir());
        if !imports.is_empty() && self.persist {
            self.importer.start(
                &self.library,
                imports,
                self.prefs.library.import_mode,
                None,
                false,
                &ctx,
            );
        }
        for path in opens {
            self.open(&path, &ctx, None);
        }
        // The vault follows the library (in a scripted run only the demo's,
        // in a temporary folder).
        if self.persist || self.demo_vault {
            let vault = self.vault_root();
            for notice in self.vault_sync.poll(&mut self.library, vault) {
                self.notices.push(notice);
            }
        }
        if self.persist {
            self.importer.poll(&mut self.library);
            // New items get their metadata looked up (and then the AI pass).
            let added = self.importer.take_added();
            if !added.is_empty()
                && let Err(error) = self.library.enqueue_library_jobs(
                    &added,
                    glyphst_library::LibraryJobKind::Metadata,
                    glyphst_library::LibraryJobOrigin::OnAdd,
                    false,
                )
            {
                self.notices.push(format!(
                    "Could not queue the new items' metadata: {error:#}"
                ));
            }
            let settings = self.user_settings.get().cloned();
            if settings.as_ref().map(std::sync::Arc::as_ptr)
                != self
                    .library_jobs_settings
                    .as_ref()
                    .map(std::sync::Arc::as_ptr)
            {
                self.library_jobs.resume();
                self.library_jobs_settings = settings.clone();
            }
            let model = settings.and_then(|settings| settings.ai.resolved_library());
            for notice in self
                .library_jobs
                .poll(&mut self.library, &self.prefs.library, model)
            {
                self.notices.push(notice);
            }
            self.poll_pages();
            if self.library_search.is_none() {
                let path = crate::library_search::SearchService::default_path();
                match crate::library_search::SearchService::start(&path, &ctx) {
                    Ok(service) => {
                        self.search_index
                            .set_service(Some((path, Some(service.tool_searcher()))));
                        self.library_search = Some(service);
                    }
                    Err(error) => {
                        log::warn!("the library's search could not start: {error:#}");
                    }
                }
            }
        }
        // Naming another llama-server makes a new setup (not mid-download).
        if self.persist
            && self.prefs.library.llama_server != self.meaning_engine
            && !self.meaning.is_running()
        {
            self.meaning_engine = self.prefs.library.llama_server.clone();
            self.meaning = crate::meaning::setup(Some(self.meaning_engine.as_str()));
            if !self.rerank.is_running() {
                self.rerank = crate::meaning::rerank_setup(Some(self.meaning_engine.as_str()));
            }
        }
        // The engines start once their models are downloaded (the
        // reranker only when on) and the search takes them.
        if let Some(search) = &mut self.library_search {
            if let Some(change) = self.meaning_runtime.poll(self.meaning.paths(), &ctx) {
                search.set_embedder(change.map(|embedder| {
                    embedder as std::sync::Arc<dyn glyphst_search::embed::Embedder>
                }));
            }
            let wanted = self.rerank.paths().filter(|_| self.prefs.library.rerank);
            if let Some(change) = self.rerank_runtime.poll(wanted, &ctx) {
                search.set_reranker(change.map(|reranker| {
                    reranker as std::sync::Arc<dyn glyphst_search::embed::Reranker>
                }));
            }
        }
        if let Some(search) = &mut self.library_search {
            if self.persist {
                search.set_paused(self.prefs.library.index_paused);
            }
            search.follow(&self.library);
            search.poll(&ctx);
            // Items close in meaning, worked out anew: related suggestions.
            if let Some(found) = search.neighbours()
                && !self
                    .neighbours_seen
                    .as_ref()
                    .is_some_and(|seen| std::sync::Arc::ptr_eq(seen, &found))
            {
                if let Err(error) = crate::knowledge::refresh_related(&mut self.library, &found) {
                    log::warn!("could not suggest related items: {error:#}");
                }
                self.neighbours_seen = Some(found);
            }
        }
        // Pictures dropped while the assistant is open go with its next
        // message.
        if self.assistant.open {
            let pictures: Vec<PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|file| file.path().to_path_buf())
                    .filter(|path| {
                        path.extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(|extension| {
                                ["png", "jpg", "jpeg", "gif", "webp"]
                                    .contains(&extension.to_ascii_lowercase().as_str())
                            })
                    })
                    .collect()
            });
            for path in pictures {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned());
                let attached = std::fs::read(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| self.assistant.attach_picture(&ctx, name, bytes));
                if let Err(error) = attached {
                    self.notices
                        .push(format!("Could not attach {}: {error}", path.display()));
                }
            }
        }
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            self.reload_changed(&ctx);
        }
        self.was_focused = focused;
        // Reading time for the activity log.
        let input = ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || !i.events.is_empty());
        let document = self.active.map(|active| self.tabs[active].path.clone());
        let page = self
            .active
            .map(|active| self.tabs[active].current_page() + 1);
        if let Some(read) = self
            .reading
            .tick(now, document.as_deref(), page, input, focused)
        {
            self.log_reading(read);
        }
        self.user_settings.poll(now);
        self.translator.set_settings(self.user_settings.get());
        self.translator.poll();
        self.assistant
            .poll(self.persist.then_some(&mut self.library));
        if let Some(draft) = self.assistant.take_queued() {
            self.ask(draft, &ctx);
        }
        if let Some(path) = self.pickers.take_picture() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let attached = std::fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| self.assistant.attach_picture(&ctx, name, bytes));
            if let Err(error) = attached {
                self.notices
                    .push(format!("Could not attach {}: {error}", path.display()));
            }
        }
        self.quick_ask.assistant.poll(None);
        if let Some((tab_id, path)) = self.pickers.take_saved()
            && let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == tab_id)
            && let Some(session) = tab.translation.as_mut()
            && let Err(error) = self.translator.export(session, path, &ctx)
        {
            self.notices.push(format!("Could not export: {error}"));
        }
        for tab in &mut self.tabs {
            tab.receive(&ctx);
            tab.sync_translation();
            tab.update_text(now);
        }
        self.upload_ready_page();
        self.draw_gpu_thumbnails();
        self.finish_region_jobs(&ctx);
        self.upload_ready_layers();
        self.watch_effects(&ctx, now);
        self.update_wallpaper(&ctx);
        let pad = std::mem::take(&mut self.pad_input);
        let pad_events = std::mem::take(&mut self.pad_events);
        if !pad_events.is_empty() {
            let connected = self.pads.connected();
            self.prompts.note(
                &pad_events,
                &connected,
                self.prefs.gamepad_mode,
                self.prefs.ask_gamepad_mode,
            );
        }
        for step in std::mem::take(&mut self.pad_ring_steps) {
            self.pad_ring_step(step, &ctx, now);
        }
        for command in std::mem::take(&mut self.pad_commands) {
            if let Err(reason) = self.run(command, &ctx)
                && command.needs() != Needs::Document
            {
                self.notices.push(reason.to_string());
            }
        }
        self.couch_input(&ctx);
        self.click = ctx.input(|i| {
            i.pointer
                .any_click()
                .then(|| i.pointer.interact_pos())
                .flatten()
        });
        self.switcher_input(&ctx);
        let mut actions = self.global_actions(&ctx);
        actions.append(&mut self.pending_actions);
        if let Some(active) = self.active {
            let tab = &mut self.tabs[active];
            let found = tab.find.take_reveal().map(|m| (m.page, m.range.clone()));
            if let Some((page, range)) = found
                && let Some(rect) = tab
                    .text
                    .get(page)
                    .and_then(|text| text.selection_rects(range).first().copied())
            {
                actions.push(ViewAction::Reveal { page, rect });
            }
        }

        let tokens = Tokens::get(&ctx);
        let presenting = self
            .active
            .is_some_and(|active| self.tabs[active].present.is_some());
        self.presenting_edges(&ctx, presenting, now);
        // Zen mode is for reading a document; the home page leaves it.
        self.zen &= self.active.is_some();
        // The bars slide away for present, zen and gamepad modes, and back.
        let bars = glyphst_ui::motion::smooth_bool(
            &ctx,
            egui::Id::unique("bars-shown"),
            !presenting && !self.bare(),
            glyphst_ui::motion::PANEL,
        );
        if bars > 0.0 {
            let height = glyphst_ui::tab::BAR_HEIGHT;
            egui::Panel::top("tabs")
                .exact_size(height * bars)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let shown = ui.max_rect();
                    let whole = egui::Rect::from_min_max(
                        egui::pos2(shown.left(), shown.bottom() - height),
                        shown.max,
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(whole), |ui| {
                        ui.set_clip_rect(shown.intersect(ui.clip_rect()));
                        self.tab_strip(ui);
                    });
                });
            let height = glyphst_ui::status::HEIGHT;
            egui::Panel::bottom("status")
                .exact_size(height * bars)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    let shown = ui.max_rect();
                    let whole =
                        egui::Rect::from_min_size(shown.min, egui::vec2(shown.width(), height));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(whole), |ui| {
                        ui.set_clip_rect(shown.intersect(ui.clip_rect()));
                        self.status_bar(ui);
                    });
                });
        }

        let mut animating = false;
        let mut pages_drawn = true;
        // A document shown (opened, or chosen from the palette) leaves the
        // page tab.
        self.page_focused = self
            .page_focused
            .filter(|&index| self.active.is_none() && index < self.pages.len());
        egui::CentralPanel::no_frame()
            // A document's stage is filled by its background effect; a
            // fill under it would double a transparent window's opacity.
            .frame(egui::Frame::new().fill(if self.active.is_some() {
                egui::Color32::TRANSPARENT
            } else {
                tokens.bg
            }))
            .show(ui, |ui| {
                if self.active.is_some() {
                    (animating, pages_drawn) = self.stage(ui, actions, &pad, seconds, now);
                } else if self.prefs.gamepad_mode {
                    self.couch_home(ui);
                } else if let Some(gallery) = self.gallery.as_mut() {
                    gallery.show(ui);
                } else if let Some(index) = self.page_focused {
                    // The assistant beside the library's pages asks about
                    // the library (frame 05 · ask the library).
                    if self.assistant.open {
                        let area = ui.max_rect();
                        let width = self.prefs.assistant_width.min(area.width() * 0.5);
                        let right = egui::Rect::from_min_max(
                            egui::pos2(area.right() - width, area.top()),
                            area.max,
                        );
                        let left = egui::Rect::from_min_max(
                            area.min,
                            egui::pos2(right.left(), area.bottom()),
                        );
                        let mut page_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(left)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        self.show_page(&mut page_ui, index, &ctx);
                        let tokens = Tokens::get(&ctx);
                        ui.painter().rect_filled(right, 0, tokens.surface);
                        glyphst_ui::pane::vline(ui, right.left() + 0.5, right.y_range());
                        self.assistant_pane(ui, right, None);
                    } else {
                        self.show_page(ui, index, &ctx);
                    }
                } else if let Some(action) = self.home.ui(
                    ui,
                    crate::home::HomeContext {
                        library: &self.library,
                        page_counts: &self.prefs.page_counts,
                        picking: self.pickers.is_showing(),
                        open_key: self.keymap.label(Command::OpenWithDialog),
                        covers: &mut self.covers,
                    },
                ) {
                    match action {
                        HomeAction::Open(path) => self.open(&path, &ctx, None),
                        HomeAction::OpenDialog => self.pickers.open_documents(&ctx),
                        HomeAction::AskAbout(path) => {
                            self.open(&path, &ctx, None);
                            if !self.assistant.open {
                                self.toggle_assistant();
                            }
                        }
                        HomeAction::Activity(day) => self.open_activity(day),
                        HomeAction::Library => self.open_library(),
                    }
                }
            });

        if ctx.input(|i| !i.pointer.any_down()) {
            self.chrome.tab_drag = None;
        }
        self.overlays(&ctx);
        self.library_dialogs(&ctx);
        self.switcher_overlay(&ctx);
        if presenting && let Some(active) = self.active {
            self.present_overlay(&ctx, active, now);
        }
        self.couch_overlay(&ctx);
        self.pad_ring_overlay(&ctx);
        if let Some(answer) = self.prompts.show(&ctx, self.prefs.gamepad_mode) {
            use crate::prompts::Answer;
            match answer {
                Answer::GamepadMode => self.set_gamepad_mode(true, &ctx),
                Answer::LeaveGamepadMode => self.set_gamepad_mode(false, &ctx),
                Answer::NeverAsk => self.prefs.ask_gamepad_mode = false,
                Answer::NotNow | Answer::Stay => {}
            }
        }
        // Above the status bar while it shows.
        let lift = if presenting || self.bare() {
            28.0
        } else {
            glyphst_ui::status::HEIGHT + 16.0
        };
        if self.notices.show(&ctx, lift).as_deref() == Some("undo") {
            match self.toast_undo.take() {
                Some(ToastUndo::Highlight(id)) => self.remove_annotation(id),
                Some(ToastUndo::Merge(edit)) => {
                    if let Err(error) = self.library.revert_edit(&edit) {
                        self.notices
                            .push(format!("Could not undo the merge: {error:#}"));
                    }
                }
                None => {}
            }
        }
        self.pending_keys_chip(&ctx, lift);
        let wallpaper_ready = self.wallpaper_loading.is_none();
        self.covers.poll(&ctx);
        let covers_ready = !self.covers.pending()
            && !self.vault_sync.busy()
            && !self.zotero.as_ref().is_some_and(|zotero| zotero.busy());
        self.run_harness(&ctx, pages_drawn && wallpaper_ready && covers_ready, now);
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.finished(now))
        {
            self.transition = None;
        }
        if let Some(active) = self.active
            && now - self.last_position_save >= POSITION_SAVE_INTERVAL
        {
            self.save_position(active, true);
            self.last_position_save = now;
        }

        // Links to the library (the assistant's citations) open here, and
        // those other apps forwarded, once the search can find passages.
        let forwarded = self
            .instance
            .as_ref()
            .map(|instance| instance.take())
            .unwrap_or_default();
        if !forwarded.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        self.pending_links.extend(
            forwarded
                .iter()
                .filter_map(|url| crate::links::parse_link(url)),
        );
        if !self.pending_links.is_empty()
            && (self.library_search.is_some() || self.started.elapsed() > Duration::from_secs(3))
        {
            for link in std::mem::take(&mut self.pending_links) {
                self.follow_link(link, &ctx);
            }
        }
        for link in crate::links::take(&ctx) {
            self.follow_link(link, &ctx);
        }
        self.settle_switcher();
        // A pending overview advances one tile per frame, so keep drawing.
        let overviews = self.with_gpu(|gpu| gpu.has_pending_work()).unwrap_or(false);
        let continuous = animating
            || glyphst_ui::motion::take_moving(&ctx)
            || self.transition.is_some()
            || pad.is_active()
            || self.harness.is_some()
            || overviews;
        self.continuous = continuous;
        if continuous {
            ctx.request_repaint();
        } else if self.active.is_some() && self.background_is_animated() && ctx.input(|i| i.focused)
        {
            let fps = self.prefs.background_fps.max(1);
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / f64::from(fps)));
        }
    }

    fn on_exit(&mut self) {
        if let Some(read) = self.reading.finish() {
            self.log_reading(read);
        }
        self.window_blur.detach();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.persist {
            return;
        }
        for index in 0..self.tabs.len() {
            self.save_position(index, false);
        }
        let _ = self.library.save();
        self.prefs.last_session = if self.prefs.reopen_last_session {
            self.session()
        } else {
            LastSession::default()
        };
        self.prefs.save(storage);
    }
}

impl ReaderApp {
    /// Reloads the user's effects when their files changed, looking at
    /// most once a second while the window has focus.
    pub(crate) fn watch_effects(&mut self, ctx: &egui::Context, now: Instant) {
        let due = now - self.effects_checked >= EFFECTS_CHECK_INTERVAL
            && (ctx.input(|i| i.focused) || self.harness.is_some());
        if !due {
            return;
        }
        self.reload_effects_if_changed(now);
    }

    pub(crate) fn reload_effects_if_changed(&mut self, now: Instant) {
        self.effects_checked = now;
        let Some(dir) = self.effects_dir.clone() else {
            return;
        };
        let stamp = glyphst_effects::user_stamp(&dir);
        if stamp == self.effects_stamp {
            return;
        }
        self.effects_stamp = stamp;
        let device = self.render_state.device.clone();
        let report = self.with_gpu(|gpu| {
            gpu.reload_effects(&device, &dir);
            let loaded = gpu.effects().defs().filter(|def| def.user).count();
            let errors: Vec<String> = gpu
                .effects()
                .errors()
                .iter()
                .map(ToString::to_string)
                .collect();
            (loaded, errors)
        });
        if let Some((loaded, errors)) = report {
            match errors.first() {
                Some(first) => self.notices.push(format!(
                    "Effects reloaded; {} could not load. {first}",
                    errors.len()
                )),
                None => self.notices.push(format!(
                    "Effects reloaded: {loaded} of your own effect{}.",
                    if loaded == 1 { "" } else { "s" }
                )),
            }
        }
    }

    /// Reads a newly chosen wallpaper, and hands a read one to the GPU.
    fn update_wallpaper(&mut self, ctx: &egui::Context) {
        if let Some(loading) = &self.wallpaper_loading
            && let Some(result) = loading.take()
        {
            let path = loading.path.clone();
            self.wallpaper_loading = None;
            let (device, queue) = (
                self.render_state.device.clone(),
                self.render_state.queue.clone(),
            );
            match result {
                Ok(image) => {
                    self.with_gpu(|gpu| gpu.set_wallpaper(&device, &queue, Some(&image)));
                    self.prefs.wallpaper_seed =
                        image.seed.map(|seed| crate::prefs::WallpaperSeed {
                            path: path.clone(),
                            seed,
                        });
                }
                Err(error) => {
                    self.with_gpu(|gpu| gpu.set_wallpaper(&device, &queue, None));
                    self.notices
                        .push(format!("Could not use the wallpaper: {error}"));
                }
            }
            self.wallpaper_shown = Some(path);
            ctx.request_repaint();
        }
        let wanted = self.prefs.wallpaper.clone();
        let loading = self.wallpaper_loading.as_ref().map(|l| &l.path);
        if wanted == self.wallpaper_shown || wanted.as_ref() == loading {
            return;
        }
        match wanted {
            Some(path) => {
                self.wallpaper_loading = Some(crate::wallpaper::Loading::start(path, ctx));
            }
            None => {
                let (device, queue) = (
                    self.render_state.device.clone(),
                    self.render_state.queue.clone(),
                );
                self.with_gpu(|gpu| gpu.set_wallpaper(&device, &queue, None));
                self.wallpaper_loading = None;
                self.wallpaper_shown = None;
            }
        }
    }

    fn background_is_animated(&self) -> bool {
        let id = self.effects_for(self.active).background.id;
        self.with_gpu(|gpu| gpu.effects().find(&id).is_some_and(|def| def.animated))
            .unwrap_or(false)
    }
}

/// A menu entry with an icon, a label and a key at the right.
fn menu_entry<'a>(
    _ui: &egui::Ui,
    icon: &'a str,
    text: &'a str,
    key: &'a str,
) -> glyphst_ui::menu::Item<'a> {
    glyphst_ui::menu::Item::new(text).icon(icon).key(key)
}

/// What a tab's context menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabMenu {
    Close,
    CloseOthers,
    CloseToTheRight,
    /// Shows the tab beside the focused pane.
    Open(Zone),
    Reload,
    CopyPath,
    ShowInFolder,
}

/// The context menu of tab `index` of `count`; `shown` when it is on the
/// stage already. Returns the entry chosen.
fn tab_menu_entries(ui: &mut egui::Ui, index: usize, count: usize, shown: bool) -> Option<TabMenu> {
    let mut chosen = None;
    let mut entry = |ui: &mut egui::Ui, enabled: bool, icon: &str, text: &str, action| {
        if ui
            .add_enabled(enabled, menu_entry(ui, icon, text, ""))
            .clicked()
        {
            chosen = Some(action);
        }
    };
    entry(ui, true, "x", "Close", TabMenu::Close);
    entry(ui, count > 1, "x", "Close other tabs", TabMenu::CloseOthers);
    entry(
        ui,
        index + 1 < count,
        "x",
        "Close tabs to the right",
        TabMenu::CloseToTheRight,
    );
    glyphst_ui::menu::separator(ui);
    if !shown {
        entry(
            ui,
            true,
            "columns-2",
            "Open to the right",
            TabMenu::Open(Zone::Right),
        );
        entry(
            ui,
            true,
            "rows-2",
            "Open below",
            TabMenu::Open(Zone::Bottom),
        );
    }
    entry(ui, true, "refresh-cw", "Reload", TabMenu::Reload);
    glyphst_ui::menu::separator(ui);
    entry(ui, true, "copy", "Copy path", TabMenu::CopyPath);
    entry(
        ui,
        true,
        "folder-open",
        "Show in folder",
        TabMenu::ShowInFolder,
    );
    chosen
}

/// The tabs a close entry of tab `index`'s menu closes, last first so each
/// index stays valid as the ones after it go.
fn tabs_to_close(count: usize, index: usize, action: TabMenu) -> Vec<usize> {
    let all = (0..count).rev();
    match action {
        TabMenu::Close => vec![index],
        TabMenu::CloseOthers => all.filter(|&other| other != index).collect(),
        TabMenu::CloseToTheRight => all.filter(|&other| other > index).collect(),
        _ => Vec::new(),
    }
}

/// Remembered zoom, relative to fit width, that a document opens at; one
/// outside it (a view saved before it had a size) opens at fit width.
const SANE_ZOOM: std::ops::RangeInclusive<f32> = 0.25..=8.0;

/// The zoom `position` opens at: the remembered one when it is sane and
/// `remember` is on (Settings › Reading › remember zoom), else fit width.
fn opening_zoom(mut position: DocumentPosition, remember: bool) -> DocumentPosition {
    if !remember || !SANE_ZOOM.contains(&position.zoom) {
        position.zoom = 1.0;
    }
    position
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut short: String = text.chars().take(limit - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_zoom_out_of_range_opens_at_fit_width() {
        let at = |zoom| opening_zoom(DocumentPosition::fixed(0.0, 120.0, zoom, 3), true);
        assert_eq!(at(636.0).zoom, 1.0);
        assert_eq!(at(0.01).zoom, 1.0);
        assert_eq!(at(f32::NAN).zoom, 1.0);
        assert_eq!(at(2.5).zoom, 2.5);
        assert_eq!(at(636.0).page, 3, "the page stays");
        let fit = opening_zoom(DocumentPosition::fixed(0.0, 120.0, 2.5, 3), false);
        assert_eq!(
            (fit.zoom, fit.page),
            (1.0, 3),
            "not remembering opens at fit width"
        );
    }

    #[test]
    fn tab_menus_close_the_tabs_they_name_last_first() {
        assert_eq!(tabs_to_close(4, 1, TabMenu::Close), [1]);
        assert_eq!(tabs_to_close(4, 1, TabMenu::CloseOthers), [3, 2, 0]);
        assert_eq!(tabs_to_close(4, 1, TabMenu::CloseToTheRight), [3, 2]);
        assert!(tabs_to_close(4, 3, TabMenu::CloseToTheRight).is_empty());
    }

    #[test]
    fn long_tab_titles_are_shortened() {
        assert_eq!(truncate("short", 8), "short");
        assert_eq!(truncate("a rather long title", 8), "a rathe…");
        assert_eq!(truncate("中文标题很长很长很长", 4), "中文标…");
    }
}
