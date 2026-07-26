use super::helpers::*;
use super::*;

impl AiPanel {
    pub(super) fn edit_model_search_query(
        &mut self,
        range: Range<usize>,
        new_text: &str,
        group: EditGroup,
        cx: &mut Context<Self>,
    ) {
        let before = self.model_search_query.clone();
        let query = self.model_search_query.to_string();
        self.model_search_query =
            (query[..range.start].to_owned() + new_text + &query[range.end..]).into();
        self.model_search_history
            .record(before, &self.model_search_query, group);
        cx.notify();
    }
    pub(super) fn open_message_context_menu(
        &mut self,
        message_index: usize,
        request: MarkdownContextMenuRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_composer_menus();
        self.message_context_menu = Some(MessageContextMenu {
            position: request.position,
            message_index,
            selected_text: request.selected_text,
            link_url: request.link_url,
        });
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn close_message_context_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let was_open = self.message_context_menu.take().is_some();
        if was_open {
            cx.notify();
        }
        was_open
    }
    fn context_menu_entry(
        id: impl Into<ElementId>,
        label: String,
        icon: IconName,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .px_3()
            .text_size(ui_rems(13.0))
            .text_color(rgb(theme.text))
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    handler(this, window, cx);
                    this.close_message_context_menu(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                Icon::new(icon)
                    .size(ui_rems(14.0))
                    .text_color(rgb(theme.text_muted)),
            )
            .child(label)
            .into_any_element()
    }

    pub(super) fn render_message_context_menu(
        &self,
        menu: MessageContextMenu,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(message) = self.messages.get(menu.message_index) else {
            return div().into_any_element();
        };
        let is_user = message.role == Role::User;
        let can_mutate = !self.loading && self.editing_turn.is_none();
        let has_later_messages = menu.message_index + 1 < self.messages.len();
        let copy_index = menu.message_index;
        let edit_index = menu.message_index;
        let regenerate_index = menu.message_index;
        let retry_index = menu.message_index;

        div()
            .occlude()
            .flex()
            .flex_col()
            .w(px(220.0))
            .py_1()
            .border_1()
            .border_color(rgb(theme.border))
            .overflow_hidden()
            .bg(rgb(theme.surface_background))
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .when_some(menu.selected_text, |this, selected_text| {
                this.child(Self::context_menu_entry(
                    ("ai-copy-selection", copy_index),
                    tr("ai-copy-selection"),
                    IconName::Copy,
                    theme,
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(selected_text.to_string()));
                    },
                    cx,
                ))
            })
            .when_some(menu.link_url, |this, link_url| {
                this.child(Self::context_menu_entry(
                    ("ai-copy-link", copy_index),
                    tr("ai-copy-link"),
                    IconName::Copy,
                    theme,
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(link_url.to_string()));
                    },
                    cx,
                ))
            })
            .child(Self::context_menu_entry(
                ("ai-copy-message", copy_index),
                if is_user {
                    tr("ai-copy-message")
                } else {
                    tr("ai-copy-response")
                },
                IconName::Copy,
                theme,
                move |this, _, cx| this.copy_message(copy_index, cx),
                cx,
            ))
            .when(is_user && can_mutate, |this| {
                this.child(Self::context_menu_entry(
                    ("ai-edit-message", edit_index),
                    tr("edit"),
                    IconName::Pencil,
                    theme,
                    move |this, window, cx| this.start_editing_message(edit_index, window, cx),
                    cx,
                ))
                .child(Self::context_menu_entry(
                    ("ai-regenerate-message", regenerate_index),
                    if has_later_messages {
                        tr("ai-regenerate-from-message-warning")
                    } else {
                        tr("ai-regenerate-from-message")
                    },
                    IconName::RotateCcw,
                    theme,
                    move |this, _, cx| this.regenerate_from_user(regenerate_index, cx),
                    cx,
                ))
            })
            .when(!is_user && can_mutate, |this| {
                this.child(Self::context_menu_entry(
                    ("ai-retry-response", retry_index),
                    if has_later_messages {
                        tr("ai-retry-response-warning")
                    } else {
                        tr("ai-retry-response")
                    },
                    IconName::RotateCcw,
                    theme,
                    move |this, _, cx| this.retry_assistant(retry_index, cx),
                    cx,
                ))
            })
            .child(div().h(px(1.0)).my_1().mx_1().bg(rgb(theme.border)))
            .child(Self::context_menu_entry(
                "ai-scroll-top",
                tr("ai-scroll-top"),
                IconName::ArrowUp,
                theme,
                |this, _, cx| this.scroll_to_top(cx),
                cx,
            ))
            .child(Self::context_menu_entry(
                "ai-scroll-bottom",
                tr("ai-scroll-bottom"),
                IconName::ChevronDown,
                theme,
                |this, _, cx| this.scroll_to_bottom(cx),
                cx,
            ))
            .into_any_element()
    }
    pub(super) fn close_composer_menus(&mut self) {
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        self.model_search_query = SharedString::default();
        self.model_search_history.clear();
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
    }

    fn menu_anchor_for_button(position: GpuiPoint<Pixels>) -> GpuiPoint<Pixels> {
        point(position.x, position.y + px(18.0))
    }

    pub(super) fn toggle_attachment_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.message_context_menu = None;
        self.attachment_menu_open = !self.attachment_menu_open;
        self.attachment_menu_anchor = self
            .attachment_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        self.model_search_query = SharedString::default();
        self.model_search_history.clear();
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
        cx.notify();
    }

    pub(super) fn toggle_model_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.message_context_menu = None;
        self.focus_handle.focus(window, cx);
        self.model_menu_open = !self.model_menu_open;
        self.model_menu_anchor = self
            .model_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.thinking_menu_open = false;
        self.thinking_menu_anchor = None;
        self.model_search_query = SharedString::default();
        self.model_search_history.clear();
        cx.notify();
    }

    pub(super) fn toggle_thinking_menu(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            return;
        }
        self.message_context_menu = None;
        self.thinking_menu_open = !self.thinking_menu_open;
        self.thinking_menu_anchor = self
            .thinking_menu_open
            .then_some(Self::menu_anchor_for_button(position));
        self.attachment_menu_open = false;
        self.attachment_menu_anchor = None;
        self.model_menu_open = false;
        self.model_menu_anchor = None;
        self.model_search_query = SharedString::default();
        self.model_search_history.clear();
        cx.notify();
    }

    fn select_model(&mut self, provider: AiProvider, model: String, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }

        let mut settings = cx.global::<SettingsStore>().settings.clone();
        settings.ai.main_model = Some(ModelRef::new(provider, model.clone()));
        settings.ai.thinking_level = settings.ai.thinking_level.clamp_for(provider, &model);
        if let Err(error) = cx.global_mut::<SettingsStore>().set_settings(settings) {
            eprintln!("failed to update AI model: {error:#}");
        }
        self.close_composer_menus();
        cx.notify();
    }

    fn select_thinking_level(&mut self, level: AiThinkingLevel, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let main = settings.ai.resolved_main();
        let provider = main.provider;
        let model = main.model;
        settings.ai.thinking_level = level.clamp_for(provider, &model);
        if let Err(error) = cx.global_mut::<SettingsStore>().set_settings(settings) {
            eprintln!("failed to update AI thinking level: {error:#}");
        }
        self.close_composer_menus();
        cx.notify();
    }
    pub(super) fn render_attachment_menu(
        page_count: usize,
        has_selection: bool,
        has_region: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .w(px(260.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (mention_ix, mention) in MentionKind::ALL
            .into_iter()
            .filter(|mention| mention_is_available(*mention, page_count, has_selection, has_region))
            .enumerate()
        {
            menu = menu.child(
                div()
                    .id(("ai-attachment-menu-entry", mention_ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.add_attachment(mention, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        Icon::new(Self::mention_icon(mention))
                            .size(ui_rems(14.0))
                            .text_color(rgb(theme.text_muted)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_color(rgb(theme.text)).child(mention.label()))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(mention.description()),
                            ),
                    ),
            );
        }

        menu
    }

    pub(super) fn render_model_menu(
        state: ModelMenuRenderState,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = state.search_query.to_string();
        let query_empty = query.is_empty();
        let query_lower = query.to_ascii_lowercase();
        let mut entries = Vec::new();
        for (provider, models) in state.provider_models {
            let mut provider_entries = models
                .into_iter()
                .filter(|model| {
                    query_lower.is_empty()
                        || model.id.to_ascii_lowercase().contains(&query_lower)
                        || model.name.to_ascii_lowercase().contains(&query_lower)
                        || model.group.to_ascii_lowercase().contains(&query_lower)
                        || provider.label().to_ascii_lowercase().contains(&query_lower)
                })
                .map(move |model| ModelMenuEntry::Model(provider, model))
                .collect::<Vec<_>>();
            if !provider_entries.is_empty() {
                entries.push(ModelMenuEntry::Header(provider));
                entries.append(&mut provider_entries);
            }
        }
        let entry_count = entries.len();
        let scroll_handle = state.scroll_handle.clone();
        let search_focused = state.focused;

        let mut menu = div()
            .w(px(340.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        menu = menu
            .child(
                div()
                    .mx_2()
                    .mt_2()
                    .mb_2()
                    .h(px(30.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .border_1()
                    .border_color(rgb(if search_focused {
                        theme.accent
                    } else {
                        theme.border_variant
                    }))
                    .bg(rgb(theme.tab_active_background))
                    .text_color(rgb(if query_empty {
                        theme.text_muted
                    } else {
                        theme.text
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.focus_handle.focus(window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .when(query_empty && search_focused, |this| {
                        this.child(div().w(px(1.0)).h(px(14.0)).bg(rgb(theme.accent)))
                    })
                    .child(if query_empty {
                        tr("ai-search-models-placeholder")
                    } else {
                        query
                    })
                    .when(!query_empty && search_focused, |this| {
                        this.child(div().w(px(1.0)).h(px(14.0)).bg(rgb(theme.accent)))
                    }),
            )
            .child(
                div()
                    .mx_2()
                    .mb_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(tr("ai-select-provider-model")),
                    ),
            );

        if entries.is_empty() {
            menu = menu.child(
                div()
                    .px_2()
                    .py_2()
                    .text_color(rgb(theme.text_muted))
                    .child(tr("no-matching-models")),
            );
        }

        if entry_count > 0 {
            let selected_provider = state.selected_provider;
            let selected_model = state.selected_model;
            let rows = entries
                .into_iter()
                .map(|entry| match entry {
                    ModelMenuEntry::Header(provider) => div()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .px_2()
                        .text_size(ui_rems(11.0))
                        .text_color(rgb(theme.text_muted))
                        .child(provider.label())
                        .into_any_element(),
                    ModelMenuEntry::Model(provider, model) => {
                        let selected = provider == selected_provider && model.id == selected_model;
                        let model_for_click = model.id.clone();
                        div()
                            .id(format!("ai-model-{provider:?}-{}", model.id))
                            .h(px(30.0))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .pl(px(20.0))
                            .pr_2()
                            .rounded(px(4.0))
                            .bg(if selected {
                                rgb(theme.element_selected)
                            } else {
                                rgba(0x00000000)
                            })
                            .text_color(rgb(if selected { theme.accent } else { theme.text }))
                            .cursor_pointer()
                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.select_model(provider, model_for_click.clone(), cx);
                                    cx.stop_propagation();
                                }),
                            )
                            .child(div().truncate().min_w_0().child(model.name))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(96.0))
                                    .truncate()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(model.group),
                            )
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>();
            menu = menu.child(
                div()
                    .id("ai-model-menu-list")
                    .track_scroll(&scroll_handle)
                    .max_h(px(300.0))
                    .pb_1()
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .children(rows)
                    .vertical_scrollbar_for(&scroll_handle, window, cx),
            );
        }

        menu
    }

    pub(super) fn render_thinking_menu(
        selected_provider: AiProvider,
        selected_model: String,
        selected_level: AiThinkingLevel,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut menu = div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .text_size(ui_rems(12.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });

        for (level_ix, level) in AiThinkingLevel::supported_for(selected_provider, &selected_model)
            .iter()
            .enumerate()
        {
            let level = *level;
            let selected = level == selected_level;
            menu = menu.child(
                div()
                    .id(("ai-thinking-level", level_ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_2()
                    .bg(if selected {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_color(rgb(if selected { theme.accent } else { theme.text }))
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_thinking_level(level, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(level.label())
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(thinking_level_description(level)),
                    ),
            );
        }

        menu
    }
}
