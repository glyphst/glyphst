use glyphst_settings::{SettingsStore, ui_rems};
use gpui::{
    AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgb,
};

struct TextTooltip {
    text: SharedString,
}

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<SettingsStore>().resolved_theme;
        div()
            .px_2()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_size(ui_rems(12.0))
            .text_color(rgb(theme.text))
            .child(self.text.clone())
    }
}

pub fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| TextTooltip { text: text.clone() }).into()
}
