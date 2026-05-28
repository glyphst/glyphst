use std::path::PathBuf;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_platform::application;

mod pdf_view;

use pdf_view::PdfView;

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            eprintln!("Usage: cargo run -- <path-to-pdf>");
            std::process::exit(2);
        });

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(800.0), px(1000.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| PdfView::open(&path).expect("failed to open PDF")),
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
