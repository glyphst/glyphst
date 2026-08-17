use std::ops::Range;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, Render, ScrollStrategy, SharedString, Subscription,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, ThemeColor, ThemeStyled as _,
    input::{Input, InputEvent, InputState},
    list::{List, ListDelegate, ListItem, ListState},
};

use crate::{
    menu,
    style::{INPUT_ROW_HEIGHT, TextSize},
};

pub enum PickerEvent {
    Dismissed,
}

/// Application-specific data for a picker.
///
/// Glyphst retains this small protocol because picker rows perform domain
/// actions, while all input, list navigation, selection presentation,
/// virtualization, and scrolling are supplied by gpui-component.
pub trait PickerDelegate: Sized + 'static {
    fn key_context(&self) -> &'static str;
    fn placeholder(&self) -> SharedString;
    fn empty_message(&self) -> SharedString;
    fn width(&self) -> f32;
    fn row_height(&self) -> f32;
    fn max_visible_matches(&self) -> usize;
    fn match_count(&self) -> usize;
    fn update_matches(&mut self, query: &str);
    fn match_id(&self, match_ix: usize) -> ElementId {
        ("picker-match", match_ix).into()
    }
    fn render_match(
        &self,
        match_ix: usize,
        theme: ThemeColor,
        previous_focus_handle: &FocusHandle,
        window: &Window,
    ) -> AnyElement;
    fn confirm(
        &mut self,
        match_ix: usize,
        previous_focus_handle: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    );
    fn confirm_input(
        &mut self,
        _: SharedString,
        _: &FocusHandle,
        _: &mut Window,
        _: &mut Context<Picker<Self>>,
    ) {
    }
    fn remove_match(&mut self, _: usize, _: &mut Context<Picker<Self>>) {}
    fn selection_changed(&mut self, _match_ix: usize, _cx: &mut Context<Picker<Self>>) {}
    fn dismissed(&mut self, _cx: &mut Context<Picker<Self>>) {}
}

pub struct Picker<D: PickerDelegate> {
    previous_focus_handle: FocusHandle,
    input: Entity<InputState>,
    list: Entity<ListState<PickerListDelegate<D>>>,
    selected_ix: usize,
    delegate: D,
    _input_subscription: Subscription,
}

impl<D: PickerDelegate> Picker<D> {
    pub fn new(
        previous_focus_handle: FocusHandle,
        delegate: D,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_content_and_selected_ix(
            previous_focus_handle,
            delegate,
            "".into(),
            0,
            window,
            cx,
        )
    }

    pub fn new_with_selected_ix(
        previous_focus_handle: FocusHandle,
        delegate: D,
        selected_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_content_and_selected_ix(
            previous_focus_handle,
            delegate,
            "".into(),
            selected_ix,
            window,
            cx,
        )
    }

    pub fn with_content(
        previous_focus_handle: FocusHandle,
        delegate: D,
        content: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::with_content_and_selected_ix(previous_focus_handle, delegate, content, 0, window, cx)
    }

    fn with_content_and_selected_ix(
        previous_focus_handle: FocusHandle,
        mut delegate: D,
        content: SharedString,
        selected_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        delegate.update_matches(content.as_ref());
        let selected_ix = selected_ix.min(delegate.match_count().saturating_sub(1));
        let placeholder = delegate.placeholder();
        let content_len = content.len();

        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(content)
        });
        input.update(cx, |input, cx| {
            input.set_selected_range(0..content_len, cx);
        });

        let picker = cx.entity().downgrade();
        let list = cx.new(|cx| {
            ListState::new(
                PickerListDelegate {
                    picker: picker.clone(),
                    selected_ix: None,
                },
                window,
                cx,
            )
        });
        if delegate.match_count() > 0 {
            list.update(cx, |list, cx| {
                list.set_selected_index(Some(IndexPath::new(selected_ix)), window, cx);
            });
        }

        let _input_subscription = cx.subscribe_in(&input, window, Self::on_input_event);
        let focus_handle = input.focus_handle(cx);
        window.defer(cx, move |window, cx| {
            if picker.upgrade().is_some() {
                focus_handle.focus(window, cx);
            }
        });

        Self {
            previous_focus_handle,
            input,
            list,
            selected_ix,
            delegate,
            _input_subscription,
        }
    }

    pub fn update_delegate(&mut self, update: impl FnOnce(&mut D), cx: &mut Context<Self>) {
        update(&mut self.delegate);
        let query = self.input.read(cx).value();
        self.delegate.update_matches(query.as_ref());
        self.clamp_selection();
        cx.notify();
    }

    fn on_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let query = input.read(cx).value();
                self.delegate.update_matches(query.as_ref());
                self.clamp_selection();
                self.sync_list_selection(true, window, cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => self.confirm_selected(window, cx),
            InputEvent::CompletionAccepted { .. }
            | InputEvent::SpanHoverChanged { .. }
            | InputEvent::Focus
            | InputEvent::Blur => {}
        }
    }

    fn clamp_selection(&mut self) {
        self.selected_ix = self
            .selected_ix
            .min(self.delegate.match_count().saturating_sub(1));
    }

    fn sync_list_selection(&self, reveal: bool, window: &mut Window, cx: &mut Context<Self>) {
        let selected = (self.delegate.match_count() > 0).then(|| IndexPath::new(self.selected_ix));
        self.list.update(cx, |list, cx| {
            if list.selected_index() != selected {
                list.set_selected_index(selected, window, cx);
            }
            if reveal && let Some(selected) = selected {
                list.scroll_to_item(selected, ScrollStrategy::Center, window, cx);
            }
        });
    }

    fn select_hovered(&mut self, match_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if match_ix >= self.delegate.match_count() || self.selected_ix == match_ix {
            return;
        }

        self.selected_ix = match_ix;
        self.delegate.selection_changed(match_ix, cx);
        self.sync_list_selection(false, window, cx);
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.delegate.dismissed(cx);
        self.previous_focus_handle.focus(window, cx);
        cx.emit(PickerEvent::Dismissed);
    }

    fn confirm_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix >= self.delegate.match_count() {
            let content = self.input.read(cx).value();
            self.previous_focus_handle.focus(window, cx);
            self.delegate
                .confirm_input(content, &self.previous_focus_handle, window, cx);
            self.dismiss(window, cx);
            return;
        }

        self.confirm_match(self.selected_ix, window, cx);
    }

    fn confirm_match(&mut self, match_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if match_ix >= self.delegate.match_count() {
            return;
        }

        self.selected_ix = match_ix;
        self.previous_focus_handle.focus(window, cx);
        self.delegate
            .confirm(match_ix, &self.previous_focus_handle, window, cx);
        self.delegate.dismissed(cx);
        cx.emit(PickerEvent::Dismissed);
    }

    fn cancel(&mut self, _: &menu::Cancel, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix + 1 < self.delegate.match_count() {
            self.selected_ix += 1;
            self.delegate.selection_changed(self.selected_ix, cx);
            self.sync_list_selection(true, window, cx);
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &menu::SelectPrev, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix > 0 {
            self.selected_ix -= 1;
            self.delegate.selection_changed(self.selected_ix, cx);
            self.sync_list_selection(true, window, cx);
            cx.notify();
        }
    }

    fn remove_selected(&mut self, _: &menu::Remove, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_ix < self.delegate.match_count() {
            self.delegate.remove_match(self.selected_ix, cx);
            let query = self.input.read(cx).value();
            self.delegate.update_matches(query.as_ref());
            self.clamp_selection();
            self.sync_list_selection(true, window, cx);
            cx.notify();
        }
    }
}

impl<D: PickerDelegate> EventEmitter<PickerEvent> for Picker<D> {}

impl<D: PickerDelegate> Focusable for Picker<D> {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_list_selection(false, window, cx);

        let match_count = self.delegate.match_count();
        let list_height = if match_count == 0 {
            44.0
        } else {
            self.delegate.row_height() * match_count.min(self.delegate.max_visible_matches()) as f32
        };

        div()
            .key_context(self.delegate.key_context())
            .flex()
            .flex_col()
            .w(px(self.delegate.width()))
            .max_h(px(INPUT_ROW_HEIGHT + list_height))
            .popover_style(cx)
            .rounded(cx.theme().radius_lg)
            .overflow_hidden()
            .text_size(TextSize::Default.rems())
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::remove_selected))
            .child(
                div()
                    .h(px(INPUT_ROW_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .debug_selector(|| "picker-input-row".to_string())
                    .child(Input::new(&self.input).appearance(false).px_2()),
            )
            .child(
                div()
                    .h(px(list_height))
                    .max_h(px(list_height))
                    .debug_selector(|| "picker-list".to_string())
                    .child(
                        List::new(&self.list)
                            .h(px(list_height))
                            .max_h(px(list_height))
                            .scrollbar_visible(true),
                    ),
            )
    }
}

struct PickerListDelegate<D: PickerDelegate> {
    picker: WeakEntity<Picker<D>>,
    selected_ix: Option<usize>,
}

impl<D: PickerDelegate> ListDelegate for PickerListDelegate<D> {
    type Item = ListItem;

    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.picker
            .upgrade()
            .map(|picker| picker.read(cx).delegate.match_count())
            .unwrap_or(0)
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let picker = self.picker.upgrade()?;
        let theme = cx.theme().colors;
        let (content, id, row_height) = {
            let picker = picker.read(cx);
            (
                picker
                    .delegate
                    .render_match(ix.row, theme, &picker.previous_focus_handle, window),
                picker.delegate.match_id(ix.row),
                picker.delegate.row_height(),
            )
        };
        let match_ix = ix.row;

        Some(
            ListItem::new(id)
                .h(px(row_height))
                .mx_1()
                .rounded(cx.theme().radius)
                .on_mouse_enter(move |_, window, cx| {
                    _ = picker.update(cx, |picker, cx| {
                        picker.select_hovered(match_ix, window, cx);
                    });
                })
                .child(content),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        let message = self
            .picker
            .upgrade()
            .map(|picker| picker.read(cx).delegate.empty_message())
            .unwrap_or_default();

        div()
            .h(px(44.0))
            .w_full()
            .flex()
            .items_center()
            .px_3()
            .text_color(cx.theme().muted_foreground)
            .child(message)
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected_ix = ix.map(|ix| ix.row);
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(match_ix) = self.selected_ix else {
            return;
        };
        _ = self.picker.update(cx, |picker, cx| {
            picker.confirm_match(match_ix, window, cx);
        });
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        _ = self.picker.update(cx, |picker, cx| {
            picker.dismiss(window, cx);
        });
    }
}

pub fn fuzzy_match(candidate: &str, query: &str) -> Option<(Vec<Range<usize>>, usize)> {
    if query.is_empty() {
        return Some((Vec::new(), 0));
    }

    let candidate_chars = candidate.char_indices().collect::<Vec<_>>();
    if candidate_chars.is_empty() {
        return None;
    }

    let mut states = vec![None::<MatchState>; candidate_chars.len()];

    for (query_ix, query_ch) in query.chars().map(|ch| ch.to_ascii_lowercase()).enumerate() {
        let mut next_states = vec![None::<MatchState>; candidate_chars.len()];

        if query_ix == 0 {
            for (char_ix, (byte_ix, candidate_ch)) in candidate_chars.iter().copied().enumerate() {
                if candidate_ch.to_ascii_lowercase() == query_ch {
                    let mut highlights = Vec::with_capacity(query.len());
                    highlights.push(byte_ix..byte_ix + candidate_ch.len_utf8());
                    let state = MatchState {
                        highlights,
                        score: fuzzy_match_score(candidate, char_ix, byte_ix, None),
                    };
                    replace_state_if_better(&mut next_states[char_ix], state);
                }
            }
        } else {
            for (previous_ix, state) in states.iter().enumerate() {
                let Some(state) = state else {
                    continue;
                };

                for (char_ix, (byte_ix, candidate_ch)) in candidate_chars
                    .iter()
                    .copied()
                    .enumerate()
                    .skip(previous_ix + 1)
                {
                    if candidate_ch.to_ascii_lowercase() != query_ch {
                        continue;
                    }

                    let mut next_state = state.clone();
                    next_state.score +=
                        fuzzy_match_score(candidate, char_ix, byte_ix, Some(previous_ix));
                    next_state
                        .highlights
                        .push(byte_ix..byte_ix + candidate_ch.len_utf8());
                    replace_state_if_better(&mut next_states[char_ix], next_state);
                }
            }
        }

        if next_states.iter().all(Option::is_none) {
            return None;
        }

        states = next_states;
    }

    states
        .into_iter()
        .flatten()
        .min_by_key(|state| state.score)
        .map(|state| (state.highlights, state.score))
}

fn replace_state_if_better(slot: &mut Option<MatchState>, state: MatchState) {
    if slot
        .as_ref()
        .is_none_or(|current| state.score < current.score)
    {
        *slot = Some(state);
    }
}

#[derive(Clone)]
struct MatchState {
    highlights: Vec<Range<usize>>,
    score: usize,
}

fn fuzzy_match_score(
    candidate: &str,
    char_ix: usize,
    byte_ix: usize,
    last_match_ix: Option<usize>,
) -> usize {
    let mut score = match last_match_ix {
        Some(previous_ix) => (char_ix - previous_ix - 1) * 2,
        None => char_ix,
    };

    if is_word_start(candidate, byte_ix) {
        score = score.saturating_sub(10);
    } else {
        score += 6;
    }

    if last_match_ix.is_some_and(|previous_ix| char_ix == previous_ix + 1) {
        score = score.saturating_sub(4);
    }

    score
}

fn is_word_start(text: &str, byte_ix: usize) -> bool {
    if byte_ix == 0 {
        return true;
    }

    let previous = text[..byte_ix].chars().next_back();
    let current = text[byte_ix..].chars().next();

    matches!(
        (previous, current),
        (Some(' ' | ':' | '_' | '-'), Some(ch)) if ch.is_alphanumeric()
    )
}

pub fn normalize_query(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_char: Option<char> = None;

    for char in input.trim().chars() {
        match (last_char, char) {
            (Some(last_char), char) if last_char.is_whitespace() && char.is_whitespace() => {
                continue;
            }
            _ => {
                last_char = Some(char);
            }
        }
        result.push(char);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext};
    use gpui_component::Root;
    use std::{cell::RefCell, rc::Rc};

    struct TestPickerDelegate {
        matches: usize,
        selection_changes: Rc<RefCell<Vec<usize>>>,
    }

    impl PickerDelegate for TestPickerDelegate {
        fn key_context(&self) -> &'static str {
            "PickerFocusTest"
        }

        fn placeholder(&self) -> SharedString {
            "Search".into()
        }

        fn empty_message(&self) -> SharedString {
            "Empty".into()
        }

        fn width(&self) -> f32 {
            240.0
        }

        fn row_height(&self) -> f32 {
            32.0
        }

        fn max_visible_matches(&self) -> usize {
            4
        }

        fn match_count(&self) -> usize {
            self.matches
        }

        fn update_matches(&mut self, _: &str) {}

        fn render_match(
            &self,
            match_ix: usize,
            _: ThemeColor,
            _: &FocusHandle,
            _: &Window,
        ) -> AnyElement {
            div()
                .debug_selector(move || format!("picker-test-row-{match_ix}"))
                .w_full()
                .child("Item")
                .into_any_element()
        }

        fn confirm(
            &mut self,
            _: usize,
            _: &FocusHandle,
            _: &mut Window,
            _: &mut Context<Picker<Self>>,
        ) {
        }

        fn selection_changed(&mut self, match_ix: usize, _: &mut Context<Picker<Self>>) {
            self.selection_changes.borrow_mut().push(match_ix);
        }
    }

    struct PickerFocusHost {
        picker: Entity<Picker<TestPickerDelegate>>,
        previous_focus_handle: FocusHandle,
    }

    impl PickerFocusHost {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            Self::with_matches(0, window, cx)
        }

        fn with_matches(matches: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
            let previous_focus_handle = cx.focus_handle();
            previous_focus_handle.focus(window, cx);
            let picker = cx.new(|cx| {
                Picker::new(
                    previous_focus_handle.clone(),
                    TestPickerDelegate {
                        matches,
                        selection_changes: Rc::new(RefCell::new(Vec::new())),
                    },
                    window,
                    cx,
                )
            });
            Self {
                picker,
                previous_focus_handle,
            }
        }
    }

    impl Render for PickerFocusHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.previous_focus_handle)
                .child(self.picker.clone())
        }
    }

    #[gpui::test]
    fn picker_defers_focus_and_restores_the_previous_focus(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| PickerFocusHost::new(window, cx));
            Root::new(host, window, cx)
        });
        let host = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<PickerFocusHost>().unwrap()
        });
        let (picker, previous_focus_handle) = host.read_with(cx, |host, _| {
            (host.picker.clone(), host.previous_focus_handle.clone())
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
            assert!(picker.focus_handle(cx).is_focused(window));
        });

        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| picker.dismiss(window, cx));
            assert!(previous_focus_handle.is_focused(window));
        });
    }

    #[gpui::test]
    fn picker_input_row_meets_the_list_without_extra_space(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| PickerFocusHost::new(window, cx));
            Root::new(host, window, cx)
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let input_row = cx
            .debug_bounds("picker-input-row")
            .expect("picker input row bounds");
        let list = cx.debug_bounds("picker-list").expect("picker list bounds");
        assert_eq!(input_row.size.height, px(INPUT_ROW_HEIGHT));
        assert_eq!(input_row.bottom(), list.top());
    }

    #[gpui::test]
    fn picker_rows_are_horizontally_inset_without_changing_height(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| PickerFocusHost::with_matches(1, window, cx));
            Root::new(host, window, cx)
        });

        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let list = cx.debug_bounds("picker-list").expect("picker list bounds");
        let content = cx
            .debug_bounds("picker-test-row-0")
            .expect("picker row content bounds");
        assert_eq!(content.left(), list.left() + px(16.0));
        assert_eq!(list.size.height, px(32.0));
    }

    #[gpui::test]
    fn hovering_a_row_makes_it_the_only_selection(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| PickerFocusHost::with_matches(4, window, cx));
            Root::new(host, window, cx)
        });
        let host = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<PickerFocusHost>().unwrap()
        });
        let picker = host.read_with(cx, |host, _| host.picker.clone());
        let selection_changes =
            picker.read_with(cx, |picker, _| picker.delegate.selection_changes.clone());

        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let row = cx
            .debug_bounds("picker-test-row-2")
            .expect("third picker row bounds");

        cx.simulate_mouse_move(row.center(), None, Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));

        picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.selected_ix, 2);
            assert_eq!(
                picker.list.read(cx).selected_index(),
                Some(IndexPath::new(2))
            );
        });
        assert_eq!(&*selection_changes.borrow(), &[2]);

        cx.simulate_mouse_move(row.center(), None, Modifiers::default());
        assert_eq!(&*selection_changes.borrow(), &[2]);
    }

    #[gpui::test]
    fn keyboard_selection_reveals_but_hover_does_not_scroll(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let host = cx.new(|cx| PickerFocusHost::with_matches(12, window, cx));
            Root::new(host, window, cx)
        });
        let host = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<PickerFocusHost>().unwrap()
        });
        let picker = host.read_with(cx, |host, _| host.picker.clone());

        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.update(|window, cx| {
            for _ in 0..6 {
                picker.update(cx, |picker, cx| {
                    picker.select_next(&menu::SelectNext, window, cx)
                });
            }
            window.draw(cx).clear(cx);
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));

        let offset_before_hover = picker.read_with(cx, |picker, cx| {
            picker.list.read(cx).scroll_handle().offset()
        });
        assert!(offset_before_hover.y < px(0.0));

        let list_bounds = cx.debug_bounds("picker-list").expect("picker list bounds");
        let selectors = [
            "picker-test-row-0",
            "picker-test-row-1",
            "picker-test-row-2",
            "picker-test-row-3",
            "picker-test-row-4",
            "picker-test-row-5",
            "picker-test-row-6",
            "picker-test-row-7",
            "picker-test-row-8",
            "picker-test-row-9",
            "picker-test-row-10",
            "picker-test-row-11",
        ];
        let (hovered_ix, hovered_bounds) = selectors
            .into_iter()
            .enumerate()
            .filter(|(ix, _)| *ix != 6)
            .filter_map(|(ix, selector)| cx.debug_bounds(selector).map(|bounds| (ix, bounds)))
            .find(|(_, bounds)| {
                bounds.top() >= list_bounds.top() && bounds.bottom() <= list_bounds.bottom()
            })
            .expect("another fully visible picker row");

        cx.simulate_mouse_move(hovered_bounds.center(), None, Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));

        picker.read_with(cx, |picker, cx| {
            assert_eq!(picker.selected_ix, hovered_ix);
            assert_eq!(
                picker.list.read(cx).scroll_handle().offset(),
                offset_before_hover
            );
        });
    }

    #[test]
    fn fuzzy_match_returns_utf8_ranges() {
        let (highlights, _) = fuzzy_match("open café", "cé").unwrap();
        assert_eq!(highlights, vec![5..6, 8..10]);
    }

    #[test]
    fn fuzzy_match_prefers_word_starts() {
        let (_, word_start_score) = fuzzy_match("command palette: toggle", "pt").unwrap();
        let (_, middle_score) = fuzzy_match("capture total", "pt").unwrap();
        assert!(word_start_score < middle_score);
    }
}
