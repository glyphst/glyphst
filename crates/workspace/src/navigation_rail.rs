use gpui::{Div, Stateful, div, prelude::*, px};
use gpui_component::{
    Selectable as _, Sizable as _, ThemeColor,
    button::{Button, ButtonVariants as _},
};

use crate::icons::IconName;

pub(crate) const SLIM_NAVIGATION_RAIL_WIDTH: f32 = 44.0;

pub(crate) fn slim_navigation_rail(id: &'static str, theme: ThemeColor) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_string())
        .h_full()
        .w(px(SLIM_NAVIGATION_RAIL_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
        .border_r(px(1.0))
        .border_color(theme.sidebar_border)
        .bg(theme.sidebar)
}

pub(crate) fn slim_navigation_button(
    id: impl Into<gpui::ElementId>,
    icon: IconName,
    selected: bool,
    tooltip: impl Into<gpui::SharedString>,
) -> Button {
    Button::new(id)
        .xsmall()
        .compact()
        .ghost()
        .selected(selected)
        .icon(icon)
        .w(px(32.0))
        .h(px(32.0))
        .tooltip(tooltip)
}
