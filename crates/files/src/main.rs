use std::ffi::OsString;

use gpui::{App, AppContext, WindowBounds, WindowOptions, px, size};
use gpui_component::Root;
use gpui_platform::application;

mod file_manager;

use file_manager::FileManagerView;

const APP_ID: &str = "glyphst-files";
const WINDOW_TITLE: &str = "Glyphst Files";

fn main() {
    let start_path = std::env::args_os().nth(1);

    application()
        .with_assets(glyphst_ui::assets::Assets::default())
        .run(move |cx: &mut App| {
            glyphst_ui::init(cx);
            file_manager::init(cx);

            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1100.0), px(720.0)), cx)),
                app_id: Some(APP_ID.to_owned()),
                ..Default::default()
            };

            open_window(start_path, window_options, cx);
            cx.activate(true);
        });
}

fn open_window(start_path: Option<OsString>, options: WindowOptions, cx: &mut App) {
    cx.open_window(options, move |window, cx| {
        window.set_window_title(WINDOW_TITLE);
        let view = cx.new(|cx| FileManagerView::new(start_path, cx));
        view.update(cx, |view, cx| view.focus(window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    })
    .expect("failed to open Glyphst Files window");
}
