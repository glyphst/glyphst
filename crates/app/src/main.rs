use std::path::PathBuf;

use gpui::{App, AppContext, Bounds, KeyBinding, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

mod pdf_view;

use pdf_view::{CopySelection, PdfView, SelectAll};

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("Usage: cargo run -- <path-to-pdf>");
            std::process::exit(2);
        });

    application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("ctrl-c", CopySelection, None),
            KeyBinding::new("cmd-c", CopySelection, None),
            KeyBinding::new("ctrl-a", SelectAll, None),
            KeyBinding::new("cmd-a", SelectAll, None),
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
                    PdfView::open(&path, focus_handle).expect("failed to open PDF")
                })
            },
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
