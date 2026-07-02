use crate::{
    menu,
    picker::fuzzy_match,
    scrollbar::WithScrollbar as _,
    settings::SettingsStore,
    settings_window::SettingsTextInput,
    theme::Theme,
};
use gpui::{
    Anchor, AnyElement, App, AppContext, Context, Entity, EventEmitter, Focusable, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Point, Render, ScrollHandle, Window,
    anchored, deferred, div, prelude::*, point, px, rgb, rgba,
};
use std::ops::Range;

const ITEM_HEIGHT: f32 = 30.0;
const DROPDOWN_WIDTH: f32 = 280.0;
const DROPDOWN_MAX_HEIGHT: f32 = 320.0;

pub enum SearchableDropdownEvent {
    Selected(String),
}

pub struct SearchableItem {
    pub value: String,
    pub label: String,
}

pub struct SearchableDropdown {
    items: Vec<SearchableItem>,
    selected_value: String,
    open: bool,
    anchor: Option<Point<Pixels>>,
    search_input: Entity<SettingsTextInput>,
    scroll_handle: ScrollHandle,
    selected_index: usize,
    just_closed_by_out_click: bool,
}

impl SearchableDropdown {
    pub fn new(
        items: Vec<SearchableItem>,
        selected_value: String,
        placeholder: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_input =
            cx.new(|cx| SettingsTextInput::new("".into(), placeholder.into(), false, cx));
        cx.subscribe(&search_input, |_, _, _, cx| cx.notify())
            .detach();

        Self {
            items,
            selected_value,
            open: false,
            anchor: None,
            search_input,
            scroll_handle: ScrollHandle::new(),
            selected_index: 0,
            just_closed_by_out_click: false,
        }
    }

    pub fn selected_label(&self) -> &str {
        self.items
            .iter()
            .find(|item| item.value == self.selected_value)
            .map(|item| item.label.as_str())
            .unwrap_or(&self.selected_value)
    }


    fn open(&mut self, anchor: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.anchor = Some(anchor);
        self.selected_index = 0;
        self.search_input.update(cx, |input, cx| {
            input.set_content("".into(), cx);
        });
        self.search_input
            .read(cx)
            .focus_handle
            .clone()
            .focus(window, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.anchor = None;
        cx.notify();
    }

    pub fn set_selected_value(&mut self, value: String) {
        self.selected_value = value;
    }

    fn select_item(&mut self, value: String, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_value = value.clone();
        self.close(cx);
        cx.emit(SearchableDropdownEvent::Selected(value));
    }

    fn filtered_items(&self, query: &str) -> Vec<(usize, Vec<Range<usize>>)> {
        if query.is_empty() {
            return self
                .items
                .iter()
                .enumerate()
                .map(|(ix, _)| (ix, Vec::new()))
                .collect();
        }
        let mut matches = Vec::new();
        for (ix, item) in self.items.iter().enumerate() {
            if let Some((highlights, _)) = fuzzy_match(&item.label, query) {
                matches.push((ix, highlights));
            }
        }
        matches
    }

    fn render_trigger(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open;
        let label = self.selected_label().to_string();

        div()
            .w(px(DROPDOWN_WIDTH))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(rgb(if open {
                theme.accent
            } else {
                theme.border_variant
            }))
            .bg(rgb(theme.surface_background))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if this.just_closed_by_out_click {
                        this.just_closed_by_out_click = false;
                        cx.stop_propagation();
                        return;
                    }
                    if this.open {
                        this.close(cx);
                    } else {
                        this.open(
                            point(event.position.x - px(8.0), event.position.y + px(20.0)),
                            window,
                            cx,
                        );
                    }
                    cx.stop_propagation();
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if open { "⌃" } else { "⌄" }),
            )
    }

    fn render_dropdown(
        &self,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let query = self.search_input.read(cx).content.to_string();
        let filtered = self.filtered_items(&query);
        let item_count = filtered.len();
        let search_input = self.search_input.clone();
        let scroll_handle = self.scroll_handle.clone();

        let rows = filtered
            .iter()
            .map(|(item_ix, highlights)| {
                self.render_item(*item_ix, highlights, theme, cx)
            })
            .collect::<Vec<_>>();

        let anchor = self.anchor;

        deferred(
            anchored()
                .position(anchor.unwrap_or(point(px(0.0), px(0.0))))
                .anchor(Anchor::TopLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(
                    div()
                        .id("searchable-dropdown-panel")
                        .w(px(DROPDOWN_WIDTH))
                        .flex()
                        .flex_col()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(rgb(theme.border_variant))
                        .bg(rgb(theme.surface_background))
                        .shadow_sm()
                        .overflow_hidden()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.just_closed_by_out_click = true;
                            this.close(cx);
                        }))
                                .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                                    let query = this.search_input.read(cx).content.to_string();
                                    if let Some((item_ix, _)) =
                                        this.filtered_items(&query).get(this.selected_index)
                                    {
                                        let value = this.items[*item_ix].value.clone();
                                        this.select_item(value, window, cx);
                                    }
                                }))
                                .on_action(cx.listener(|this, _: &menu::Cancel, _, cx| {
                                    this.close(cx);
                                }))
                                .on_action(cx.listener(move |this, _: &menu::SelectNext, _, cx| {
                                    if this.selected_index + 1 < item_count {
                                        this.selected_index += 1;
                                        cx.notify();
                                    }
                                }))
                                .on_action(cx.listener(move |this, _: &menu::SelectPrev, _, cx| {
                                    if this.selected_index > 0 {
                                        this.selected_index -= 1;
                                        cx.notify();
                                    }
                                }))
                                .child(
                                    div()
                                        .px_2()
                                        .py_2()
                                        .border_b(px(1.0))
                                        .border_color(rgb(theme.border_variant))
                                        .flex()
                                        .child(search_input),
                                )
                                .when(item_count == 0, |this| {
                                    this.child(
                                        div()
                                            .px_3()
                                            .py_2()
                                            .text_color(rgb(theme.text_muted))
                                            .child("No matching items"),
                                    )
                                })
                                .when(item_count > 0, |this| {
                                    this.child(
                                        div()
                                            .id("searchable-dropdown-list")
                                            .track_scroll(&scroll_handle)
                                            .max_h(px(DROPDOWN_MAX_HEIGHT))
                                            .py_1()
                                            .flex()
                                            .flex_col()
                                            .overflow_y_scroll()
                                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                            .children(rows)
                                            .vertical_scrollbar_for(&scroll_handle, window, cx),
                                    )
                                }),
                ),
        )
        .with_priority(2)
        .into_any_element()
    }

    fn render_item(
        &self,
        item_ix: usize,
        highlights: &[Range<usize>],
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = &self.items[item_ix];
        let selected = item.value == self.selected_value;
        let is_hovered = item_ix == self.selected_index;
        let value = item.value.clone();
        let label = item.label.clone();

        let highlight_ranges = highlights
            .iter()
            .cloned()
            .map(|range| {
                (
                    range,
                    gpui::HighlightStyle {
                        color: Some(rgb(theme.match_highlight).into()),
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();

        div()
            .h(px(ITEM_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(4.0))
            .bg(if is_hovered {
                rgb(theme.element_hover)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected { theme.accent } else { theme.text }))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.select_item(value.clone(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_color(rgb(theme.accent))
                    .child(if selected { "✓" } else { "" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(gpui::StyledText::new(label).with_highlights(highlight_ranges)),
            )
            .into_any_element()
    }
}

impl EventEmitter<SearchableDropdownEvent> for SearchableDropdown {}

impl Focusable for SearchableDropdown {
    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        self.search_input.focus_handle(cx)
    }
}

impl Render for SearchableDropdown {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.just_closed_by_out_click = false;
        let theme = cx.global::<SettingsStore>().resolved_theme;

        if self.open {
            div()
                .child(self.render_trigger(theme, cx))
                .child(self.render_dropdown(theme, window, cx))
        } else {
            div().child(self.render_trigger(theme, cx))
        }
    }
}
