use std::path::PathBuf;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

mod command_palette;
mod keybindings;
mod pdf_view;
mod picker;
mod search;
mod settings;
mod settings_panel;
mod smooth_scroll;
mod theme;
mod toc;

use pdf_view::PdfView;
use settings::SettingsStore;

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("Usage: cargo run -- <path-to-pdf>");
            std::process::exit(2);
        });

    application().run(move |cx: &mut App| {
        cx.set_global(SettingsStore::load());
        let (keymap_preset, keybinding_overrides) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.keymap_preset,
                store.settings.keybinding_overrides.clone(),
            )
        };
        keybindings::apply(cx, keymap_preset, &keybinding_overrides);

        let bounds = Bounds::centered(None, size(px(800.0), px(1000.0)), cx);

        let window_settings = cx.global::<SettingsStore>().settings.window;
        let transparency_enabled = window_settings.transparency_enabled_by_default();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_background: window_settings.background_appearance(transparency_enabled),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window, cx);
                    let store = cx.global::<SettingsStore>();
                    PdfView::open(
                        &path,
                        focus_handle,
                        store.settings.behavior,
                        store.settings.window,
                        store.resolved_theme,
                    )
                    .expect("failed to open PDF")
                })
            },
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
