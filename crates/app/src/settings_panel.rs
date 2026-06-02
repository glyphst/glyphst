use crate::{
    settings::{Settings, SettingsStore},
    theme::{PartialTheme, Theme, ThemeMetadata, format_rgb_hex, format_rgba_hex},
};
use gpui::{
    AnyElement, Context, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    SharedString, Window, div, prelude::*, px, rgb, rgba,
};
use std::path::Path;

gpui::actions!(settings_panel, [PanelClose]);

const NUMBER_STEP_SMALL: f32 = 0.05;
const PAGE_GAP_STEP: f32 = 4.0;
const COLOR_SWATCHES: [u32; 10] = [
    0x101010, 0x181818, 0x202020, 0x282c34, 0x1e1e2e, 0x313244, 0x61afef, 0x89b4fa, 0xf5f5f5,
    0xffffff,
];
const HIGHLIGHT_SWATCHES: [u32; 6] = [
    0x61afef66, 0x89b4fa66, 0xc678dd66, 0xe5c07b66, 0x98c37966, 0xe06c7566,
];
const ALPHA_SWATCHES: [u32; 6] = [
    0x00000033, 0x00000066, 0xffffff66, 0x61afef55, 0x3a3f4bcc, 0x45475a99,
];

struct ColorRowConfig {
    label: &'static str,
    current: u32,
    override_value: Option<u32>,
    swatches: &'static [u32],
    with_alpha: bool,
}

pub enum PanelEvent {
    Dismissed,
    Changed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Appearance,
}

pub struct SettingsPanel {
    focus_handle: FocusHandle,
    previous_focus_handle: FocusHandle,
    active_tab: SettingsTab,
    status: Option<SharedString>,
    save_error: Option<SharedString>,
}

impl SettingsPanel {
    pub fn new(previous_focus_handle: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            previous_focus_handle,
            active_tab: SettingsTab::General,
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
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings);

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

    fn tab_button(
        &self,
        tab: SettingsTab,
        label: &'static str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.active_tab == tab;
        div()
            .h(px(34.0))
            .px_3()
            .flex()
            .items_center()
            .rounded_md()
            .text_color(if selected {
                rgb(theme.text)
            } else {
                rgb(theme.text_muted)
            })
            .bg(if selected {
                rgba(theme.selected_row)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.control_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.active_tab = tab;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(label)
    }

    fn number_row(
        &self,
        label: &'static str,
        value: f32,
        decrement: impl FnOnce(&mut Settings) + Clone + 'static,
        increment: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(theme.text)).child(label))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(self.small_button("−", decrement, theme, cx))
                    .child(
                        div()
                            .w(px(72.0))
                            .text_center()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{value:.2}")),
                    )
                    .child(self.small_button("+", increment, theme, cx)),
            )
    }

    fn small_button(
        &self,
        label: &'static str,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.control_border))
            .bg(rgb(theme.control_background))
            .hover(move |style| style.bg(rgb(theme.control_hover)))
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
        config: ColorRowConfig,
        update: impl Fn(&mut PartialTheme, u32) + Copy + 'static,
        reset: impl Fn(&mut PartialTheme) + Copy + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let value = if config.with_alpha {
            format_rgba_hex(config.current)
        } else {
            format_rgb_hex(config.current)
        };
        let source = if config.override_value.is_some() {
            "override"
        } else {
            "theme"
        };

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
                    .child(div().text_color(rgb(theme.text)).child(config.label))
                    .child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{value} · {source}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when(config.override_value.is_some(), |this| {
                        this.child(self.small_button(
                            "Reset",
                            move |settings| reset(&mut settings.overrides),
                            theme,
                            cx,
                        ))
                    })
                    .children(config.swatches.iter().copied().map(move |color| {
                        let selected = color == config.current;
                        div()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded_md()
                            .border_2()
                            .border_color(if selected {
                                rgb(theme.text)
                            } else {
                                rgb(theme.control_border)
                            })
                            .bg(if config.with_alpha {
                                rgba(color)
                            } else {
                                rgb(color)
                            })
                            .hover(move |style| style.border_color(rgb(theme.text)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.change_settings(
                                        move |settings| update(&mut settings.overrides, color),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }),
                            )
                    })),
            )
    }

    fn theme_option(
        &self,
        metadata: ThemeMetadata,
        selected_id: &str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = metadata.id == selected_id;
        let id = metadata.id.clone();

        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(38.0))
            .px_3()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.control_border)
            })
            .bg(if selected {
                rgba(theme.selected_row)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.control_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let id = id.clone();
                    this.change_settings(move |settings| settings.theme_name = id, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().text_color(rgb(theme.text)).child(metadata.name))
            .child(
                div()
                    .text_color(rgb(theme.text_muted))
                    .child(metadata.appearance.as_str()),
            )
    }

    fn section_title(label: &'static str, theme: Theme) -> impl IntoElement {
        div()
            .pt_2()
            .pb_1()
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    fn general_tab(&self, settings: &Settings, theme: Theme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Self::section_title("Zoom", theme))
            .child(self.number_row(
                "Default zoom",
                settings.behavior.default_zoom,
                |settings| settings.behavior.default_zoom -= NUMBER_STEP_SMALL,
                |settings| settings.behavior.default_zoom += NUMBER_STEP_SMALL,
                theme,
                cx,
            ))
            .child(self.number_row(
                "Zoom step",
                settings.behavior.zoom_step,
                |settings| settings.behavior.zoom_step -= NUMBER_STEP_SMALL,
                |settings| settings.behavior.zoom_step += NUMBER_STEP_SMALL,
                theme,
                cx,
            ))
            .into_any_element()
    }

    fn appearance_tab(
        &self,
        settings: &Settings,
        resolved: Theme,
        themes: Vec<ThemeMetadata>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page_gap = resolved.page_gap;
        let theme_options = themes
            .into_iter()
            .map(|metadata| {
                self.theme_option(metadata, &settings.theme_name, resolved, cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Self::section_title("Theme", resolved))
            .child(div().flex().flex_col().gap_2().children(theme_options))
            .child(Self::section_title("Overrides", resolved))
            .child(self.number_row(
                "Page gap",
                page_gap,
                move |settings| settings.overrides.page_gap = Some(page_gap - PAGE_GAP_STEP),
                move |settings| settings.overrides.page_gap = Some(page_gap + PAGE_GAP_STEP),
                resolved,
                cx,
            ))
            .when(settings.overrides.page_gap.is_some(), |this| {
                this.child(div().flex().justify_end().child(self.small_button(
                    "Reset page gap",
                    |settings| settings.overrides.page_gap = None,
                    resolved,
                    cx,
                )))
            })
            .child(self.color_row(
                ColorRowConfig {
                    label: "Background",
                    current: resolved.background,
                    override_value: settings.overrides.background,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.background = Some(color),
                |overrides| overrides.background = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Page",
                    current: resolved.page,
                    override_value: settings.overrides.page,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.page = Some(color),
                |overrides| overrides.page = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Selection highlight",
                    current: resolved.selection_highlight,
                    override_value: settings.overrides.selection_highlight,
                    swatches: &HIGHLIGHT_SWATCHES,
                    with_alpha: true,
                },
                |overrides, color| overrides.selection_highlight = Some(color),
                |overrides| overrides.selection_highlight = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Footer background",
                    current: resolved.footer_background,
                    override_value: settings.overrides.footer_background,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.footer_background = Some(color),
                |overrides| overrides.footer_background = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Footer border",
                    current: resolved.footer_border,
                    override_value: settings.overrides.footer_border,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.footer_border = Some(color),
                |overrides| overrides.footer_border = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Footer text",
                    current: resolved.footer_text,
                    override_value: settings.overrides.footer_text,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.footer_text = Some(color),
                |overrides| overrides.footer_text = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Panel background",
                    current: resolved.panel_background,
                    override_value: settings.overrides.panel_background,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.panel_background = Some(color),
                |overrides| overrides.panel_background = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Panel border",
                    current: resolved.panel_border,
                    override_value: settings.overrides.panel_border,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.panel_border = Some(color),
                |overrides| overrides.panel_border = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Text",
                    current: resolved.text,
                    override_value: settings.overrides.text,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.text = Some(color),
                |overrides| overrides.text = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Text muted",
                    current: resolved.text_muted,
                    override_value: settings.overrides.text_muted,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.text_muted = Some(color),
                |overrides| overrides.text_muted = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Placeholder text",
                    current: resolved.placeholder_text,
                    override_value: settings.overrides.placeholder_text,
                    swatches: &ALPHA_SWATCHES,
                    with_alpha: true,
                },
                |overrides, color| overrides.placeholder_text = Some(color),
                |overrides| overrides.placeholder_text = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Accent",
                    current: resolved.accent,
                    override_value: settings.overrides.accent,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.accent = Some(color),
                |overrides| overrides.accent = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Match highlight",
                    current: resolved.match_highlight,
                    override_value: settings.overrides.match_highlight,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.match_highlight = Some(color),
                |overrides| overrides.match_highlight = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Selected row",
                    current: resolved.selected_row,
                    override_value: settings.overrides.selected_row,
                    swatches: &ALPHA_SWATCHES,
                    with_alpha: true,
                },
                |overrides, color| overrides.selected_row = Some(color),
                |overrides| overrides.selected_row = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Overlay",
                    current: resolved.overlay,
                    override_value: settings.overrides.overlay,
                    swatches: &ALPHA_SWATCHES,
                    with_alpha: true,
                },
                |overrides, color| overrides.overlay = Some(color),
                |overrides| overrides.overlay = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Control background",
                    current: resolved.control_background,
                    override_value: settings.overrides.control_background,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.control_background = Some(color),
                |overrides| overrides.control_background = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Control border",
                    current: resolved.control_border,
                    override_value: settings.overrides.control_border,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.control_border = Some(color),
                |overrides| overrides.control_border = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Control hover",
                    current: resolved.control_hover,
                    override_value: settings.overrides.control_hover,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.control_hover = Some(color),
                |overrides| overrides.control_hover = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Success",
                    current: resolved.success,
                    override_value: settings.overrides.success,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.success = Some(color),
                |overrides| overrides.success = None,
                resolved,
                cx,
            ))
            .child(self.color_row(
                ColorRowConfig {
                    label: "Error",
                    current: resolved.error,
                    override_value: settings.overrides.error,
                    swatches: &COLOR_SWATCHES,
                    with_alpha: false,
                },
                |overrides, color| overrides.error = Some(color),
                |overrides| overrides.error = None,
                resolved,
                cx,
            ))
            .into_any_element()
    }

    fn export_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(theme.control_border))
            .bg(rgb(theme.control_background))
            .hover(move |style| style.bg(rgb(theme.control_hover)))
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
        let (settings, resolved, themes) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.clone(),
                store.resolved_theme,
                store.registry.metadata(),
            )
        };
        let content = match self.active_tab {
            SettingsTab::General => self.general_tab(&settings, resolved, cx),
            SettingsTab::Appearance => self.appearance_tab(&settings, resolved, themes, cx),
        };

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .w(px(680.0))
            .max_h(px(760.0))
            .rounded_md()
            .border_1()
            .border_color(rgb(resolved.panel_border))
            .shadow_lg()
            .overflow_hidden()
            .bg(rgb(resolved.panel_background))
            .text_color(rgb(resolved.text))
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
                    .border_color(rgb(resolved.panel_border))
                    .child(div().text_size(px(16.0)).child("Settings"))
                    .child(div().text_color(rgb(resolved.text_muted)).child("Esc")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(150.0))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_3()
                            .border_r(px(1.0))
                            .border_color(rgb(resolved.panel_border))
                            .child(self.tab_button(SettingsTab::General, "General", resolved, cx))
                            .child(self.tab_button(
                                SettingsTab::Appearance,
                                "Appearance",
                                resolved,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .id("settings-content")
                            .flex_1()
                            .min_w_0()
                            .overflow_y_scroll()
                            .px_4()
                            .py_3()
                            .child(content),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t(px(1.0))
                    .border_color(rgb(resolved.panel_border))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .child(self.export_button(resolved, cx)),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_color(rgb(resolved.success)).child(status))
                    })
                    .when_some(self.save_error.clone(), |this, error| {
                        this.child(div().text_color(rgb(resolved.error)).child(error))
                    }),
            )
    }
}
