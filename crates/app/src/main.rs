use gpui::{
    App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use gpui_platform::application;

struct TemplateApp;

impl Render for TemplateApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(rgb(0x1f2937))
            .text_color(rgb(0xf9fafb))
            .text_xl()
            .child("Welcome to GPUI")
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(480.0), px(320.0)), cx);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| TemplateApp),
        )
        .expect("failed to open window");

        cx.activate(true);
    });
}
