use std::path::PathBuf;

use anyhow::Result;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;
use tikv_jemalloc_sys as _;
use tikv_jemallocator::Jemalloc;

#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod ai_panel;
mod bookmarks;
mod command_palette;
mod history;
mod keybindings;
mod library;
mod note_editor;
mod notes;
mod pdf_view;
mod picker;
mod search;
mod settings;
mod settings_panel;
mod smooth_scroll;
mod start_board;
mod theme;
mod toc;
mod workspace;

use library::LibraryStore;
use pdf_view::RenderCache;
use settings::SettingsStore;
use start_board::StartBoard;
use workspace::Workspace;

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    application().run(move |cx: &mut App| {
        cx.set_global(SettingsStore::load());
        cx.set_global(LibraryStore::load());
        cx.set_global(RenderCache::new());
        cx.on_app_quit(|cx| {
            if let Err(error) = cx.global::<LibraryStore>().save() {
                eprintln!("failed to save PDF library: {error:#}");
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

        open_workspace_window(path.clone(), cx).expect("failed to open workspace window");

        cx.activate(true);
    });
}

pub(crate) fn open_workspace_window(path: Option<PathBuf>, cx: &mut App) -> Result<()> {
    let window_size = if path.is_some() {
        size(px(800.0), px(1000.0))
    } else {
        size(px(StartBoard::WINDOW_WIDTH), px(StartBoard::WINDOW_HEIGHT))
    };
    let bounds = Bounds::centered(None, window_size, cx);
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| Workspace::new(path.clone(), window, cx)),
    )?;

    Ok(())
}
