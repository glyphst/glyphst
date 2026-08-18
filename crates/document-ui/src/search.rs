use std::time::Duration;

use crate::{
    document_view::{NextSearchResult, PrevSearchResult},
    icons::{Icon, IconName, toolbar_icon_button},
    settings::ui_rems,
    style::{CONTROL_ICON_SIZE, SEARCH_BAR_HEIGHT, TextSize},
};
pub use glyphst_actions::search::{SearchInputCancel, SearchInputConfirm, SearchInputPrevious};
use glyphst_ui::tooltip::text_tooltip;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    SharedString, Task, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, ThemeStyled as _,
    input::{
        Escape as ComponentInputEscape, Input, InputEvent, InputState,
        SelectAll as ComponentSelectAll,
    },
};

pub enum SearchEvent {
    Activate {
        previous: bool,
        query: SharedString,
    },
    QueryChanged {
        query: SharedString,
    },
    IgnoreCaseChanged {
        query: SharedString,
        ignore_case: bool,
    },
    RegexChanged {
        query: SharedString,
        regex_enabled: bool,
    },
    Dismissed,
}

pub(crate) struct SearchBarSnapshot {
    pub query: SharedString,
    pub active_match: Option<usize>,
    pub match_count: usize,
    pub ignore_case: bool,
    pub regex_enabled: bool,
    pub regex_error: Option<SharedString>,
    pub indexing: bool,
}

pub struct SearchBar {
    input: Entity<InputState>,
    previous_focus_handle: FocusHandle,
    content: SharedString,
    applied_query: SharedString,
    active_match: Option<usize>,
    match_count: usize,
    ignore_case: bool,
    regex_enabled: bool,
    regex_error: Option<SharedString>,
    indexing: bool,
    search_debounce: Option<Task<()>>,
}

impl SearchBar {
    pub(crate) fn new(
        previous_focus_handle: FocusHandle,
        snapshot: SearchBarSnapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let SearchBarSnapshot {
            query: content,
            active_match,
            match_count,
            ignore_case,
            regex_enabled,
            regex_error,
            indexing,
        } = snapshot;
        let input_content = content.clone();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(input_content)
                .placeholder(crate::fl!("search-document-placeholder"))
        });

        cx.subscribe(&input, |this, input, event, cx| match event {
            InputEvent::Change => {
                let content = input.read(cx).value();
                if this.content != content {
                    this.content = content;
                    this.schedule_query(cx);
                    cx.notify();
                }
            }
            InputEvent::PressEnter { shift, .. } => this.activate(*shift, cx),
            InputEvent::CompletionAccepted { .. }
            | InputEvent::SpanHoverChanged { .. }
            | InputEvent::Focus
            | InputEvent::Blur => {}
        })
        .detach();

        let input_focus = input.read(cx).focus_handle(cx);
        window.on_next_frame(move |window, cx| {
            input_focus.focus(window, cx);
            input_focus.dispatch_action(&ComponentSelectAll, window, cx);
        });

        Self {
            input,
            previous_focus_handle,
            applied_query: content.clone(),
            content,
            active_match,
            match_count,
            ignore_case,
            regex_enabled,
            regex_error,
            indexing,
            search_debounce: None,
        }
    }

    pub(crate) fn set_match_status(&mut self, snapshot: SearchBarSnapshot, cx: &mut Context<Self>) {
        let SearchBarSnapshot {
            query: applied_query,
            active_match,
            match_count,
            ignore_case,
            regex_enabled,
            regex_error,
            indexing,
        } = snapshot;
        if self.applied_query == applied_query
            && self.active_match == active_match
            && self.match_count == match_count
            && self.ignore_case == ignore_case
            && self.regex_enabled == regex_enabled
            && self.regex_error == regex_error
            && self.indexing == indexing
        {
            return;
        }

        self.applied_query = applied_query;
        self.active_match = active_match;
        self.match_count = match_count;
        self.ignore_case = ignore_case;
        self.regex_enabled = regex_enabled;
        self.regex_error = regex_error;
        self.indexing = indexing;
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(SearchEvent::Dismissed);
    }

    fn activate(&mut self, previous: bool, cx: &mut Context<Self>) {
        cx.emit(SearchEvent::Activate {
            previous,
            query: self.content.clone(),
        });
    }

    fn toggle_ignore_case(&mut self, cx: &mut Context<Self>) {
        self.ignore_case = !self.ignore_case;
        cx.emit(SearchEvent::IgnoreCaseChanged {
            query: self.content.clone(),
            ignore_case: self.ignore_case,
        });
        cx.notify();
    }

    fn toggle_regex(&mut self, cx: &mut Context<Self>) {
        self.regex_enabled = !self.regex_enabled;
        if !self.regex_enabled {
            self.regex_error = None;
        }
        cx.emit(SearchEvent::RegexChanged {
            query: self.content.clone(),
            regex_enabled: self.regex_enabled,
        });
        cx.notify();
    }

    fn schedule_query(&mut self, cx: &mut Context<Self>) {
        self.search_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(180))
                .await;
            _ = this.update(cx, |this, cx| {
                cx.emit(SearchEvent::QueryChanged {
                    query: this.content.clone(),
                });
            });
        }));
    }

    fn confirm(&mut self, _: &SearchInputConfirm, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(false, cx);
    }

    fn previous(&mut self, _: &SearchInputPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(true, cx);
    }

    fn next_result(&mut self, _: &NextSearchResult, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(false, cx);
    }

    fn prev_result(&mut self, _: &PrevSearchResult, _: &mut Window, cx: &mut Context<Self>) {
        self.activate(true, cx);
    }

    fn cancel(&mut self, _: &SearchInputCancel, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn component_cancel(
        &mut self,
        _: &ComponentInputEscape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss(window, cx);
    }

    fn counter_text(&self) -> String {
        search_counter_text(
            &self.content,
            &self.applied_query,
            self.regex_error.is_some(),
            self.indexing,
            self.active_match,
            self.match_count,
        )
    }
}

fn search_counter_text(
    content: &str,
    applied_query: &str,
    invalid_regex: bool,
    indexing: bool,
    active_match: Option<usize>,
    match_count: usize,
) -> String {
    if applied_query.is_empty() || content != applied_query {
        String::new()
    } else if invalid_regex {
        crate::fl!("search-invalid-regexp").to_string()
    } else if indexing {
        "indexing…".to_string()
    } else {
        let active_match = active_match.map_or(0, |active| active + 1);
        format!("{active_match}/{match_count}")
    }
}

impl EventEmitter<SearchEvent> for SearchBar {}

impl Focusable for SearchBar {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for SearchBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let counter = self.counter_text();
        let invalid_regex = self.regex_error.is_some();
        let counter_color = if invalid_regex {
            theme.danger
        } else if self.indexing {
            theme.muted_foreground
        } else if !counter.is_empty() && self.match_count == 0 {
            theme.danger
        } else {
            theme.muted_foreground
        };
        let ignore_case = self.ignore_case;
        let regex_enabled = self.regex_enabled;
        let regex_error = self.regex_error.clone();
        let focus_handle = self.focus_handle(cx);
        let input = Input::new(&self.input)
            .small()
            .appearance(false)
            .flex_1()
            .min_w_0()
            .when(!counter.is_empty(), |this| {
                let status = div()
                    .id("pdf-search-status")
                    .debug_selector(|| "pdf-search-status".to_string())
                    .flex_none()
                    .text_size(TextSize::Small.rems())
                    .text_color(counter_color)
                    .child(counter)
                    .when_some(regex_error, |this, error| this.tooltip(text_tooltip(error)));
                this.suffix(status)
            });

        div()
            .key_context("Search")
            .track_focus(&focus_handle)
            .flex()
            .flex_row()
            .items_center()
            .w(px(420.0))
            .h(px(SEARCH_BAR_HEIGHT))
            .pl_2()
            .pr_1()
            .popover_style(cx)
            .overflow_hidden()
            .text_size(TextSize::Default.rems())
            .text_color(theme.foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::previous))
            .on_action(cx.listener(Self::next_result))
            .on_action(cx.listener(Self::prev_result))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::component_cancel))
            .child(
                Icon::new(IconName::Search)
                    .size(ui_rems(CONTROL_ICON_SIZE))
                    .text_color(theme.muted_foreground),
            )
            .child(input)
            .child(
                div()
                    .flex_none()
                    .ml_2()
                    .mr_1()
                    .w(px(1.0))
                    .h(px(18.0))
                    .bg(theme.input),
            )
            .child(
                toolbar_icon_button("pdf-search-case-sensitive", IconName::CaseSensitive)
                    .selected(ignore_case)
                    .tooltip(crate::fl!("search-toggle-case-sensitivity"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_ignore_case(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("pdf-search-regexp", IconName::Regex)
                    .selected(regex_enabled)
                    .toggled(regex_enabled)
                    .tooltip(crate::fl!("search-toggle-regexp"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_regex(cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("pdf-search-previous", IconName::ChevronUp)
                    .tooltip(crate::fl!("search-previous-result"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.activate(true, cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("pdf-search-next", IconName::ChevronDown)
                    .tooltip(crate::fl!("search-next-result"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.activate(false, cx);
                        cx.stop_propagation();
                    })),
            )
            .child(
                toolbar_icon_button("pdf-search-close", IconName::X)
                    .tooltip(crate::fl!("search-close"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.dismiss(window, cx);
                        cx.stop_propagation();
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn counter_only_describes_the_applied_query() {
        assert_eq!(
            search_counter_text("rust", "rust", false, false, Some(1), 4),
            "2/4"
        );
        assert_eq!(
            search_counter_text("rust", "ru", false, false, Some(0), 1),
            ""
        );
        assert_eq!(
            search_counter_text("rust", "rust", false, true, None, 0),
            "indexing…"
        );
        assert_eq!(
            search_counter_text("[", "[", true, false, None, 0),
            crate::fl!("search-invalid-regexp")
        );
    }

    #[gpui::test]
    fn counter_is_a_compact_suffix_outside_the_editable_viewport(cx: &mut TestAppContext) {
        cx.update(glyphst_ui::init);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let previous_focus_handle = cx.focus_handle();
            let search_bar = cx.new(|cx| {
                SearchBar::new(
                    previous_focus_handle,
                    SearchBarSnapshot {
                        query: "sdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdfsdf".into(),
                        active_match: None,
                        match_count: 0,
                        ignore_case: true,
                        regex_enabled: false,
                        regex_error: None,
                        indexing: false,
                    },
                    window,
                    cx,
                )
            });
            gpui_component::Root::new(search_bar, window, cx)
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let search_bar = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<SearchBar>().unwrap()
        });
        let input = search_bar.read_with(cx, |search_bar, _| search_bar.input.clone());
        let editable_bounds = input.read_with(cx, |input, _| input.input_bounds());
        let status_bounds = cx
            .debug_bounds("pdf-search-status")
            .expect("search status bounds");

        assert!(editable_bounds.right() <= status_bounds.left());
        assert!(status_bounds.size.width < px(88.0));
    }
}
