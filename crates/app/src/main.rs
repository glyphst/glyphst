use std::path::PathBuf;

use anyhow::Result;
use futures::{StreamExt, channel::mpsc};
use gpui::{App, AppContext, Bounds, Pixels, WindowBounds, WindowHandle, WindowOptions, px, size};
use gpui_platform::application;
use tikv_jemalloc_sys as _;
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod ai_chat;
mod ai_client;
mod ai_panel;
mod assets;
mod bookmarks;
mod command_palette;
mod fonts;
mod go_to_page;
mod history;
mod icons;
mod instance;
mod keybindings;
mod library;
mod new_tab;
mod note_editor;
mod notes;
mod pane;
mod pdf_view;
mod picker;
mod scrollbar;
mod search;
mod settings;
mod settings_window;
mod smooth_scroll;
mod theme;
mod toc;
mod workspace;

use library::LibraryStore;
use new_tab::NewTab;
use pdf_view::RenderCache;
use settings::SettingsStore;
use workspace::Workspace;

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

    application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            if let Err(error) = fonts::load_embedded_fonts(cx) {
                eprintln!("failed to load embedded fonts: {error:#}");
            }
            cx.set_global(SettingsStore::load());
            cx.set_global(LibraryStore::load());
            cx.set_global(RenderCache::new());
            cx.on_app_quit(|cx| {
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

            open_workspace_window(request.path.clone(), cx)
                .expect("failed to open workspace window");

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
            eprintln!("failed to open PDF: {error:#}");
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

fn active_workspace_window(cx: &App) -> Option<WindowHandle<Workspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<Workspace>())
        .or_else(|| {
            cx.window_stack().and_then(|windows| {
                windows
                    .into_iter()
                    .find_map(|window| window.downcast::<Workspace>())
            })
        })
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<Workspace>())
        })
}

pub(crate) fn open_workspace_window(path: Option<PathBuf>, cx: &mut App) -> Result<()> {
    open_workspace_window_at(path, None, cx)
}

pub(crate) fn open_workspace_window_at(
    path: Option<PathBuf>,
    bounds: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Result<()> {
    let window_size = if path.is_some() {
        size(px(800.0), px(1000.0))
    } else {
        size(px(NewTab::WINDOW_WIDTH), px(NewTab::WINDOW_HEIGHT))
    };
    let bounds = bounds.unwrap_or_else(|| Bounds::centered(None, window_size, cx));
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("glyphst");
            cx.new(|cx| Workspace::new(path.clone(), window, cx))
        },
    )?;

    Ok(())
}
