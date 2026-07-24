use gpui::{AnyView, App, SharedString, Window};
use gpui_component::tooltip::Tooltip;

pub fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}
