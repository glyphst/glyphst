use std::path::PathBuf;

use gpui::{App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

mod command_palette;
mod pdf_view;
mod settings;
mod settings_panel;
mod theme;

use command_palette::{
    PaletteBackspace, PaletteCancel, PaletteConfirm, PaletteDelete, PaletteEnd, PaletteHome,
    PaletteLeft, PalettePaste, PaletteRight, PaletteSelectAll, PaletteSelectNext,
    PaletteSelectPrev,
};
use pdf_view::{
    CopySelection, OpenSettings, PdfView, ResetZoom, SelectAll, ToggleCommandPalette, ZoomIn,
    ZoomOut,
};
use settings::SettingsStore;
use settings_panel::PanelClose;

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

        cx.bind_keys([
            KeyBinding::new("ctrl-c", CopySelection, Some("PdfView && !CommandPalette")),
            KeyBinding::new("cmd-c", CopySelection, Some("PdfView && !CommandPalette")),
            KeyBinding::new("ctrl-a", SelectAll, Some("PdfView && !CommandPalette")),
            KeyBinding::new("cmd-a", SelectAll, Some("PdfView && !CommandPalette")),
            KeyBinding::new("+", ZoomIn, Some("PdfView && !CommandPalette")),
            KeyBinding::new("shift-=", ZoomIn, Some("PdfView && !CommandPalette")),
            KeyBinding::new("-", ZoomOut, Some("PdfView && !CommandPalette")),
            KeyBinding::new("=", ResetZoom, Some("PdfView && !CommandPalette")),
            KeyBinding::new("ctrl-shift-p", ToggleCommandPalette, Some("PdfView")),
            KeyBinding::new("cmd-shift-p", ToggleCommandPalette, Some("PdfView")),
            KeyBinding::new("ctrl-,", OpenSettings, Some("PdfView")),
            KeyBinding::new("cmd-,", OpenSettings, Some("PdfView")),
            KeyBinding::new("enter", PaletteConfirm, Some("CommandPalette")),
            KeyBinding::new("escape", PaletteCancel, Some("CommandPalette")),
            KeyBinding::new("down", PaletteSelectNext, Some("CommandPalette")),
            KeyBinding::new("up", PaletteSelectPrev, Some("CommandPalette")),
            KeyBinding::new("backspace", PaletteBackspace, Some("CommandPalette")),
            KeyBinding::new("delete", PaletteDelete, Some("CommandPalette")),
            KeyBinding::new("left", PaletteLeft, Some("CommandPalette")),
            KeyBinding::new("right", PaletteRight, Some("CommandPalette")),
            KeyBinding::new("home", PaletteHome, Some("CommandPalette")),
            KeyBinding::new("end", PaletteEnd, Some("CommandPalette")),
            KeyBinding::new("ctrl-a", PaletteSelectAll, Some("CommandPalette")),
            KeyBinding::new("cmd-a", PaletteSelectAll, Some("CommandPalette")),
            KeyBinding::new("ctrl-v", PalettePaste, Some("CommandPalette")),
            KeyBinding::new("cmd-v", PalettePaste, Some("CommandPalette")),
            KeyBinding::new("escape", PanelClose, Some("SettingsPanel")),
        ]);

        let bounds = Bounds::centered(None, size(px(800.0), px(1000.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
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
