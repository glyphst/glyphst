use crate::settings::{Settings, SettingsStore, format_rgb_hex, format_rgba_hex};
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Render, SharedString,
    Window, div, prelude::*, px, rgb, rgba,
};
use std::path::Path;

gpui::actions!(settings_panel, [PanelClose]);

const NUMBER_STEP_SMALL: f32 = 0.05;
const PAGE_GAP_STEP: f32 = 4.0;
const BG_SWATCHES: [u32; 6] = [0x101010, 0x181818, 0x202020, 0x282c34, 0x1e1e2e, 0xf5f5f5];
const HIGHLIGHT_SWATCHES: [u32; 6] = [
    0x61afef66, 0xc678dd66, 0xe5c07b66, 0x98c37966, 0xe06c7566, 0x56b6c266,
];

pub enum PanelEvent {
    Dismissed,
    Changed,
}

pub struct SettingsPanel {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    status: Option<SharedString>,
    save_error: Option<SharedString>,
}

impl SettingsPanel {
    pub fn new(previous_focus_handle: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle,
            status: None,
            save_error: None,
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus_handle.focus(window, cx);
        cx.emit(PanelEvent::Dismissed);
    }

    fn close(&mut self, _: &PanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn change_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings;
        update(&mut settings);
        settings.sanitize();

        let result = cx.global_mut::<SettingsStore>().set_settings(settings);
        self.status = None;
        self.save_error = result.err().map(|error| format!("{error:#}").into());
        cx.emit(PanelEvent::Changed);
        cx.notify();
    }

    fn export(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some("config.kdl"));

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match cx.global::<SettingsStore>().export_to(&path) {
                        Ok(()) => {
                            this.save_error = None;
                            this.status = Some(format!("Exported to {}", path.display()).into());
                        }
                        Err(error) => {
                            this.status = None;
                            this.save_error = Some(format!("{error:#}").into());
                        }
                    },
                    Ok(Err(error)) => {
                        this.status = None;
                        this.save_error = Some(format!("{error:#}").into());
                    }
                    Ok(Ok(None)) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn number_row(
        &self,
        label: &'static str,
        value: f32,
        decrement: impl FnOnce(&mut Settings) + Clone + 'static,
        increment: impl FnOnce(&mut Settings) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(0xd4d4d4)).child(label))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(self.step_button("−", decrement, cx))
                    .child(
                        div()
                            .w(px(72.0))
                            .text_center()
                            .text_color(rgb(0xbdbdbd))
                            .child(format!("{value:.2}")),
                    )
                    .child(self.step_button("+", increment, cx)),
            )
    }

    fn step_button(
        &self,
        label: &'static str,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .w(px(30.0))
            .h(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x3a3a3a))
            .bg(rgb(0x242424))
            .hover(|style| style.bg(rgb(0x303030)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(update.clone(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn color_row(
        &self,
        label: &'static str,
        current: u32,
        swatches: &'static [u32],
        with_alpha: bool,
        update: impl Fn(&mut Settings, u32) + Copy + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .py_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .mb_2()
                    .child(div().text_color(rgb(0xd4d4d4)).child(label))
                    .child(div().text_color(rgb(0x9a9a9a)).child(if with_alpha {
                        format_rgba_hex(current)
                    } else {
                        format_rgb_hex(current)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .children(swatches.iter().copied().map(move |color| {
                        let selected = color == current;
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded_md()
                            .border_2()
                            .border_color(if selected {
                                rgb(0xd4d4d4)
                            } else {
                                rgb(0x3a3a3a)
                            })
                            .bg(if with_alpha { rgba(color) } else { rgb(color) })
                            .hover(|style| style.border_color(rgb(0xffffff)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.change_settings(
                                        move |settings| {
                                            update(settings, color);
                                        },
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            )
                    })),
            )
    }

    fn export_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(32.0))
            .px_3()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x3a3a3a))
            .bg(rgb(0x242424))
            .hover(|style| style.bg(rgb(0x303030)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.export(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child("Export")
    }
}

impl EventEmitter<PanelEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx.global::<SettingsStore>().settings;

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w(px(560.0))
            .rounded_md()
            .border_1()
            .border_color(rgb(0x343434))
            .shadow_lg()
            .overflow_hidden()
            .bg(rgb(0x181818))
            .text_color(rgb(0xd4d4d4))
            .text_size(px(13.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(50.0))
                    .px_4()
                    .border_b(px(1.0))
                    .border_color(rgb(0x2a2a2a))
                    .child(div().text_size(px(16.0)).child("Settings"))
                    .child(div().text_color(rgb(0x888888)).child("Esc")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_4()
                    .py_3()
                    .child(self.number_row(
                        "Default zoom",
                        settings.default_zoom,
                        |settings| settings.default_zoom -= NUMBER_STEP_SMALL,
                        |settings| settings.default_zoom += NUMBER_STEP_SMALL,
                        cx,
                    ))
                    .child(self.number_row(
                        "Zoom step",
                        settings.zoom_step,
                        |settings| settings.zoom_step -= NUMBER_STEP_SMALL,
                        |settings| settings.zoom_step += NUMBER_STEP_SMALL,
                        cx,
                    ))
                    .child(self.number_row(
                        "Page gap",
                        settings.page_gap,
                        |settings| settings.page_gap -= PAGE_GAP_STEP,
                        |settings| settings.page_gap += PAGE_GAP_STEP,
                        cx,
                    ))
                    .child(self.color_row(
                        "Background",
                        settings.background,
                        &BG_SWATCHES,
                        false,
                        |settings, color| settings.background = color,
                        cx,
                    ))
                    .child(self.color_row(
                        "Selection highlight",
                        settings.selection_highlight,
                        &HIGHLIGHT_SWATCHES,
                        true,
                        |settings, color| settings.selection_highlight = color,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t(px(1.0))
                    .border_color(rgb(0x2a2a2a))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .child(self.export_button(cx)),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_color(rgb(0x98c379)).child(status))
                    })
                    .when_some(self.save_error.clone(), |this, error| {
                        this.child(div().text_color(rgb(0xe06c75)).child(error))
                    }),
            )
    }
}
