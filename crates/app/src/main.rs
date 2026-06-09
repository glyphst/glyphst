use std::path::PathBuf;

use anyhow::Result;
use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

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

use library::{DocumentPosition, LibraryStore};
use pdf_view::{PdfAnnotations, PdfView};
use settings::SettingsStore;
use start_board::StartBoard;

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    application().run(move |cx: &mut App| {
        cx.set_global(SettingsStore::load());
        cx.set_global(LibraryStore::load());
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

        match path.clone() {
            Some(path) => open_pdf_window(path, cx).expect("failed to open PDF window"),
            None => open_start_board_window(cx).expect("failed to open start board window"),
        }

        cx.activate(true);
    });
}

pub(crate) fn open_pdf_window(path: PathBuf, cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(800.0), px(1000.0)), cx);
    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();

    cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            ..Default::default()
        },
        move |window, cx| {
            cx.new(|cx| {
                let focus_handle = cx.focus_handle();
                focus_handle.focus(window, cx);
                let (behavior, window_settings, theme) = {
                    let store = cx.global::<SettingsStore>();
                    (
                        store.settings.behavior,
                        store.settings.window,
                        store.resolved_theme,
                    )
                };
                let initial_position = cx
                    .global::<LibraryStore>()
                    .history_position(&path)
                    .unwrap_or(DocumentPosition {
                        scroll_x_doc: 0.0,
                        scroll_y_doc: 0.0,
                        zoom: behavior.default_zoom,
                        page: 1,
                    });
                let annotations = PdfAnnotations {
                    highlights: cx.global::<LibraryStore>().highlights_for(&path),
                    notes: cx.global::<LibraryStore>().notes_for(&path),
                };
                let pdf_view = PdfView::open(
                    &path,
                    focus_handle,
                    behavior,
                    window_settings,
                    theme,
                    Some(initial_position),
                    annotations,
                )
                .expect("failed to open PDF");
                cx.global_mut::<LibraryStore>()
                    .record_history(&path, initial_position);
                if let Err(error) = cx.global::<LibraryStore>().save() {
                    eprintln!("failed to save PDF history: {error:#}");
                }
                pdf_view
            })
        },
    )?;

    Ok(())
}

pub(crate) fn open_start_board_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(
        None,
        size(px(StartBoard::WINDOW_WIDTH), px(StartBoard::WINDOW_HEIGHT)),
        cx,
    );
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
                StartBoard::new(focus_handle, cx)
            })
        },
    )?;

    Ok(())
}
