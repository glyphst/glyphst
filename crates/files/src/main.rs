use gpui::{
    App, AppContext, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, size,
};
use gpui_component::{Root, label::Label};
use gpui_platform::application;

const APP_ID: &str = "glyphst-files";
const WINDOW_TITLE: &str = "Glyphst Files";

struct FilesView;

impl Render for FilesView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new("hello-world"))
    }
}

fn main() {
    application()
        .with_assets(glyphst_ui::assets::Assets::default())
        .run(|cx: &mut App| {
            glyphst_ui::init(cx);

            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(640.0), px(480.0)), cx)),
                app_id: Some(APP_ID.to_owned()),
                ..Default::default()
            };

            cx.open_window(window_options, |window, cx| {
                window.set_window_title(WINDOW_TITLE);
                let view = cx.new(|_| FilesView);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open Glyphst Files window");

            cx.activate(true);
        });
}
