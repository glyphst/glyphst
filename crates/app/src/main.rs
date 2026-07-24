use std::path::PathBuf;

use anyhow::{Result, anyhow};
use futures::{StreamExt, channel::mpsc};
use gpui::{
    App, AppContext, Bounds, Pixels, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};
use gpui_component::Root;
use gpui_platform::application;
use tikv_jemalloc_sys as _;
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod instance;

use glyphst_keymap::keybindings;
use glyphst_library::{DocumentPosition, LibraryStore};
use glyphst_pdf_translation_app::PdfTranslationService;
use glyphst_pdf_ui::pdf_view::RenderCache;
use glyphst_settings::{SettingsStore, localize, sync_component_theme};
use glyphst_translation::TranslationService;
use glyphst_ui::{assets, fonts, notification_center, prompt};
use glyphst_workspace::{
    NewTab, Workspace, WorkspaceCallbacks, WorkspaceWindowHandle, WorkspaceWindowRequest,
};

fn main() {
    let request = instance::parse_args();
    match instance::try_forward(&request) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => eprintln!("failed to forward open request: {error:#}"),
    }

    let (open_tx, mut open_rx) = mpsc::unbounded();
    if let Err(error) = instance::listen(open_tx) {
        eprintln!("failed to start IPC listener: {error:#}");
        return;
    }

    ztracing::init();

    application()
        .with_assets(assets::Assets::default())
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            keybindings::capture_framework_bindings(cx);

            if let Err(error) = fonts::load_embedded_fonts(cx) {
                eprintln!("failed to load embedded fonts: {error:#}");
            }
            let settings_store = SettingsStore::load();
            if let Err(error) = settings_store.install_theme_catalog(cx) {
                eprintln!("failed to install theme catalog: {error:#}");
            }
            localize::select_language(settings_store.settings.language);
            cx.set_global(settings_store);
            sync_component_theme(cx);
            prompt::init(cx);
            notification_center::init(cx);
            cx.set_global(LibraryStore::load());
            cx.set_global(RenderCache::new());
            match TranslationService::new() {
                Ok(service) => {
                    cx.set_global(service.clone());
                    cx.set_global(PdfTranslationService::new(service.clone()));
                    cx.spawn(async move |cx| {
                        let result = cx
                            .background_spawn(async move { service.initialize_persistent_cache() })
                            .await;
                        if let Err(error) = result {
                            eprintln!(
                                "failed to initialize persistent translation cache: {error:#}"
                            );
                        }
                    })
                    .detach();
                }
                Err(error) => eprintln!("failed to initialize translation service: {error:#}"),
            }
            cx.on_app_quit(|cx| {
                if let Err(error) = cx.global_mut::<SettingsStore>().save() {
                    eprintln!("failed to save settings: {error:#}");
                }
                if let Err(error) = cx.global::<LibraryStore>().save() {
                    eprintln!("failed to save PDF library: {error:#}");
                }
                if let Err(error) = instance::remove_socket_file() {
                    eprintln!("failed to remove IPC socket: {error:#}");
                }
                async {}
            })
            .detach();

            let (keymap_preset, keybinding_overrides) = {
                let store = cx.global::<SettingsStore>();
                (
                    store.settings.keymap_preset,
                    store.settings.keybinding_overrides.clone(),
                )
            };
            keybindings::apply(cx, keymap_preset, &keybinding_overrides);

            if let Err(error) = open_workspace_window(request.path.clone(), cx) {
                eprintln!("failed to open workspace window: {error:#}");
                return;
            }

            cx.activate(true);

            cx.spawn(async move |cx| {
                while let Some(request) = open_rx.next().await {
                    cx.update(|cx| handle_open_request(request, cx));
                }
            })
            .detach();
        });
}

fn handle_open_request(request: instance::OpenRequest, cx: &mut App) {
    if request.new {
        if let Err(error) = open_workspace_window(request.path, cx) {
            eprintln!("failed to open workspace window: {error:#}");
        }
        cx.activate(true);
        return;
    }

    let path = request.path;
    let Some(workspace) = active_workspace_window(cx) else {
        if let Err(error) = open_workspace_window(path, cx) {
            eprintln!("failed to open workspace window: {error:#}");
        }
        cx.activate(true);
        return;
    };

    let update_path = path.clone();
    match workspace.update(cx, |workspace, window, cx| {
        if let Some(path) = update_path
            && let Err(error) = workspace.open_document_external(path, window, cx)
        {
            eprintln!("failed to open document: {error:#}");
        }
        window.activate_window();
    }) {
        Ok(()) => cx.activate(true),
        Err(error) => {
            eprintln!("failed to update workspace window: {error:#}");
            if let Err(error) = open_workspace_window(path, cx) {
                eprintln!("failed to open workspace window: {error:#}");
            }
            cx.activate(true);
        }
    }
}

fn active_workspace_window(cx: &App) -> Option<WorkspaceWindowHandle> {
    cx.active_window()
        .and_then(|window| WorkspaceWindowHandle::from_any(window, cx))
        .or_else(|| {
            cx.window_stack().and_then(|windows| {
                windows
                    .into_iter()
                    .find_map(|window| WorkspaceWindowHandle::from_any(window, cx))
            })
        })
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| WorkspaceWindowHandle::from_any(window, cx))
        })
}

fn open_keymap_in_active_workspace(cx: &mut App) {
    let Some(workspace_window) = active_workspace_window(cx) else {
        return;
    };

    if let Err(error) = workspace_window.update(cx, |workspace, window, cx| {
        workspace.open_keymap_tab(window, cx);
        window.activate_window();
    }) {
        eprintln!("failed to open keymap editor: {error:#}");
    }
}

fn open_settings_window(cx: &mut App) {
    glyphst_settings_ui::open_settings_window(open_keymap_in_active_workspace, cx);
}

fn workspace_callbacks() -> WorkspaceCallbacks {
    WorkspaceCallbacks {
        open_workspace_window: open_detached_workspace_window,
        open_settings_window,
    }
}

fn open_workspace_window(path: Option<PathBuf>, cx: &mut App) -> Result<()> {
    open_workspace_window_at(path, None, cx)
}

fn open_workspace_window_at(
    path: Option<PathBuf>,
    bounds: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Result<()> {
    open_workspace_window_at_position(path, None, bounds, cx).map(|_| ())
}

fn open_detached_workspace_window(
    request: WorkspaceWindowRequest,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    open_workspace_window_inner(None, None, Some(request.bounds), true, cx)
}

fn open_workspace_window_at_position(
    path: Option<PathBuf>,
    initial_position: Option<DocumentPosition>,
    bounds: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    open_workspace_window_inner(path, initial_position, bounds, false, cx)
}

fn open_workspace_window_inner(
    path: Option<PathBuf>,
    initial_position: Option<DocumentPosition>,
    bounds: Option<Bounds<Pixels>>,
    empty: bool,
    cx: &mut App,
) -> Result<WorkspaceWindowHandle> {
    let window_size = if path.is_some() {
        size(px(800.0), px(1000.0))
    } else {
        size(px(NewTab::WINDOW_WIDTH), px(NewTab::WINDOW_HEIGHT))
    };
    let bounds = bounds.unwrap_or_else(|| Bounds::centered(None, window_size, cx));
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();

    let root = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: cfg!(target_os = "macos"),
                traffic_light_position: cfg!(target_os = "macos")
                    .then_some(point(px(9.0), px(9.0))),
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("glyphst");
            let workspace = cx.new(|cx| {
                if empty {
                    Workspace::new_empty(workspace_callbacks(), window, cx)
                } else {
                    Workspace::new(
                        path.clone(),
                        initial_position,
                        workspace_callbacks(),
                        window,
                        cx,
                    )
                }
            });
            cx.new(|cx| Root::new(workspace, window, cx))
        },
    )?;

    WorkspaceWindowHandle::from_root(root, cx)
        .ok_or_else(|| anyhow!("new Root window does not host a Workspace"))
}
