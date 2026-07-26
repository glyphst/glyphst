use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeybindingKeystroke, MouseButton, ParentElement, Render, SharedString, Styled, Subscription,
    Window, div, prelude::*, px,
};

use crate::{
    icons::{Icon, IconName, toolbar_icon_button},
    settings::ui_rems,
};
use gpui_component::{ActiveTheme as _, ThemeColor as Theme};

pub const KEYSTROKE_COUNT_MAX: usize = 3;
const CLOSE_KEYSTROKE_WINDOW: Duration = Duration::from_millis(300);

#[derive(Clone)]
pub enum KeystrokeInputEvent {
    Changed(Vec<String>),
    Cancel,
}

pub struct KeystrokeInput {
    focus_handle: FocusHandle,
    inner_focus_handle: FocusHandle,
    keystrokes: Vec<String>,
    _subscription: Subscription,
    escape_count: usize,
    last_escape_at: Option<Instant>,
}

impl KeystrokeInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let listener = cx.listener(|this, event: &gpui::KeystrokeEvent, window, cx| {
            this.record_keystroke(event, window, cx);
        });
        let subscription = cx.intercept_keystrokes(listener);

        Self {
            focus_handle: cx.focus_handle(),
            inner_focus_handle: cx.focus_handle(),
            keystrokes: Vec::new(),
            _subscription: subscription,
            escape_count: 0,
            last_escape_at: None,
        }
    }

    pub fn keystrokes(&self) -> &[String] {
        &self.keystrokes
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.inner_focus_handle.focus(window, cx);
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.keystrokes.clear();
        self.escape_count = 0;
        self.last_escape_at = None;
        cx.emit(KeystrokeInputEvent::Changed(self.keystrokes.clone()));
        cx.notify();
    }

    fn record_keystroke(
        &mut self,
        event: &gpui::KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.inner_focus_handle.is_focused(window) {
            return;
        }

        cx.stop_propagation();

        let keystroke = KeybindingKeystroke::new_with_mapper(
            event.keystroke.clone(),
            false,
            cx.keyboard_mapper().as_ref(),
        )
        .unparse();

        if self.close_keystroke_detected(&keystroke) {
            cx.emit(KeystrokeInputEvent::Cancel);
            return;
        }

        if self.keystrokes.len() >= KEYSTROKE_COUNT_MAX {
            self.keystrokes.clear();
        }
        self.keystrokes.push(keystroke);
        cx.emit(KeystrokeInputEvent::Changed(self.keystrokes.clone()));
        cx.notify();
    }

    fn close_keystroke_detected(&mut self, keystroke: &str) -> bool {
        if keystroke != "escape" {
            self.escape_count = 0;
            self.last_escape_at = None;
            return false;
        }

        let now = Instant::now();
        self.escape_count = if self
            .last_escape_at
            .is_some_and(|then| now.duration_since(then) <= CLOSE_KEYSTROKE_WINDOW)
        {
            self.escape_count + 1
        } else {
            1
        };
        self.last_escape_at = Some(now);
        self.escape_count >= KEYSTROKE_COUNT_MAX
    }
}

impl EventEmitter<KeystrokeInputEvent> for KeystrokeInput {}

impl Focusable for KeystrokeInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for KeystrokeInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().colors;
        let recording = self.inner_focus_handle.is_focused(window);
        let has_keystrokes = !self.keystrokes.is_empty();

        let center = if has_keystrokes {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap_2()
                .children(
                    self.keystrokes
                        .iter()
                        .map(|keystroke| render_keybinding_keystroke(theme, keystroke))
                        .collect::<Vec<_>>(),
                )
                .into_any_element()
        } else {
            div()
                .text_color(theme.muted_foreground.opacity(0.7))
                .text_size(ui_rems(13.0))
                .child(if recording {
                    "Recording…"
                } else {
                    "Press shortcut…"
                })
                .into_any_element()
        };

        let left = div()
            .w(px(64.0))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .when(recording, |this| {
                this.child(
                    Icon::new(IconName::Circle)
                        .size(ui_rems(8.0))
                        .text_color(theme.danger),
                )
                .child(
                    div()
                        .text_size(ui_rems(11.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.danger)
                        .child("REC"),
                )
            });

        let right = div()
            .w(px(64.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap_1()
            .when(recording, |this| {
                this.child(
                    toolbar_icon_button("keystroke-stop", IconName::Stop)
                        .text_color(theme.danger)
                        .tooltip("Stop Recording")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_handle.focus(window, cx);
                            cx.stop_propagation();
                        })),
                )
                .child(
                    toolbar_icon_button("keystroke-clear", IconName::Delete)
                        .tooltip("Clear")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.clear(cx);
                            this.inner_focus_handle.focus(window, cx);
                            cx.stop_propagation();
                        })),
                )
            })
            .when(!recording, |this| {
                this.child(
                    toolbar_icon_button("keystroke-record", IconName::Play)
                        .tooltip("Record")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.inner_focus_handle.focus(window, cx);
                            cx.stop_propagation();
                        })),
                )
            });

        div()
            .key_context("KeystrokeInput")
            .track_focus(&self.inner_focus_handle)
            .min_h(px(40.0))
            .w_full()
            .px_3()
            .py_2()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(if recording { theme.ring } else { theme.input })
            .bg(if recording {
                theme.list_active
            } else {
                theme.popover
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.inner_focus_handle.focus(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(left)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(center),
            )
            .child(right)
    }
}

pub fn render_keybinding_keystroke(theme: Theme, keystroke: impl AsRef<str>) -> AnyElement {
    div()
        .px_2()
        .h(px(24.0))
        .flex()
        .items_center()
        .rounded(px(4.0))
        .border_1()
        .border_color(theme.input)
        .bg(theme.popover)
        .text_size(ui_rems(12.0))
        .text_color(theme.foreground)
        .child(format_keystroke(keystroke.as_ref()))
        .into_any_element()
}

pub fn format_keystroke(keystroke: &str) -> SharedString {
    keystroke
        .split_whitespace()
        .map(format_chord)
        .collect::<Vec<_>>()
        .join(" ")
        .into()
}

fn format_chord(chord: &str) -> String {
    let mut parts = chord.split('-').collect::<Vec<_>>();
    let key = parts.pop().unwrap_or_default();
    let mut labels = parts.into_iter().map(format_modifier).collect::<Vec<_>>();
    labels.push(format_key(key));
    labels.join("+")
}

fn format_modifier(modifier: &str) -> String {
    match modifier.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "Ctrl".to_string(),
        "cmd" | "command" | "super" | "platform" => "Super".to_string(),
        "alt" | "option" => "Alt".to_string(),
        "shift" => "Shift".to_string(),
        "fn" | "function" => "Fn".to_string(),
        other => capitalize_key(other),
    }
}

fn format_key(key: &str) -> String {
    match key.to_ascii_lowercase().as_str() {
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" => "Delete".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "escape" => "Escape".to_string(),
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        other => capitalize_key(other),
    }
}

fn capitalize_key(key: &str) -> String {
    if key.len() == 1 {
        key.to_ascii_uppercase()
    } else {
        let mut chars = key.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }
}
