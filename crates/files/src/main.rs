use std::ffi::OsString;

use gpui::{App, AppContext, WindowBounds, WindowOptions, px, size};
use gpui_component::{Root, TitleBar};
use gpui_platform::application;

mod file_manager;

use file_manager::FileManagerView;
use glyphst_ui::settings::{SettingsStore, localize, sync_component_theme};

const APP_ID: &str = "glyphst-files";
const WINDOW_TITLE: &str = "Glyphst Files";

fn main() {
    let start_path = std::env::args_os().nth(1);

    application()
        .with_assets(glyphst_ui::assets::Assets::default())
        .run(move |cx: &mut App| {
            glyphst_ui::init(cx);
            file_manager::init(cx);

            let settings_store = SettingsStore::load();
            if let Err(error) = settings_store.install_theme_catalog(cx) {
                eprintln!("failed to install theme catalog: {error:#}");
            }
            localize::select_language(settings_store.settings.language);
            cx.set_reduce_motion(settings_store.settings.behavior.reduce_motion);
            cx.set_global(settings_store);
            sync_component_theme(cx);

            let window_settings = cx.global::<SettingsStore>().settings.window;
            let transparency_enabled = window_settings.transparency_enabled_by_default();

            let window_options = WindowOptions {
                titlebar: Some(TitleBar::title_bar_options()),
                window_bounds: Some(WindowBounds::centered(size(px(1100.0), px(720.0)), cx)),
                window_background: window_settings.background_appearance(transparency_enabled),
                app_id: Some(APP_ID.to_owned()),
                window_min_size: Some(size(px(720.0), px(420.0))),
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
