use super::helpers::*;
use super::*;

impl AiPanel {
    pub(super) fn open_message_context_menu(
        &mut self,
        message_index: usize,
        request: MarkdownContextMenuRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_composer_menus();
        let Some(message) = self.messages.get(message_index) else {
            return;
        };
        let state = MessageContextMenuBuildState {
            message_index,
            is_user: message.role == Role::User,
            can_mutate: !self.loading && self.editing_turn.is_none(),
            has_later_messages: message_index + 1 < self.messages.len(),
            selected_text: request.selected_text,
            link_url: request.link_url,
        };
        let panel = cx.entity();
        let action_context = self.focus_handle.clone();
        let popup = PopupMenu::build(window, cx, move |menu, _, _| {
            Self::build_message_context_popup(
                menu.action_context(action_context.clone()),
                state,
                panel.clone(),
            )
        });
        cx.subscribe_in(&popup, window, |this, popup, _: &DismissEvent, _, cx| {
            if this
                .message_context_menu
                .as_ref()
                .is_some_and(|menu| &menu.popup == popup)
            {
                this.close_message_context_menu(cx);
            }
        })
        .detach();
        self.message_context_menu = Some(MessageContextMenu {
            position: request.position,
            popup: popup.clone(),
        });
        popup.focus_handle(cx).focus(window, cx);
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

    fn message_context_item(
        label: String,
        icon: IconName,
        panel: Entity<Self>,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> PopupMenuItem {
        PopupMenuItem::new(label)
            .icon(icon)
            .on_click(move |_, window, cx| {
                panel.update(cx, |this, cx| {
                    handler(this, window, cx);
                    this.close_message_context_menu(cx);
                });
            })
    }

    fn build_message_context_popup(
        mut menu: PopupMenu,
        state: MessageContextMenuBuildState,
        panel: Entity<Self>,
    ) -> PopupMenu {
        let MessageContextMenuBuildState {
            message_index,
            is_user,
            can_mutate,
            has_later_messages,
            selected_text,
            link_url,
        } = state;
        menu = menu.min_w(px(220.0));
        if let Some(selected_text) = selected_text {
            menu = menu.item(Self::message_context_item(
                tr("ai-copy-selection"),
                IconName::Copy,
                panel.clone(),
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(selected_text.to_string()));
                },
            ));
        }
        if let Some(link_url) = link_url {
            menu = menu.item(Self::message_context_item(
                tr("ai-copy-link"),
                IconName::Copy,
                panel.clone(),
                move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(link_url.to_string()));
                },
            ));
        }
        menu = menu.item(Self::message_context_item(
            if is_user {
                tr("ai-copy-message")
            } else {
                tr("ai-copy-response")
            },
            IconName::Copy,
            panel.clone(),
            move |this, _, cx| this.copy_message(message_index, cx),
        ));

        if is_user && can_mutate {
            menu = menu
                .item(Self::message_context_item(
                    tr("edit"),
                    IconName::Pencil,
                    panel.clone(),
                    move |this, window, cx| this.start_editing_message(message_index, window, cx),
                ))
                .item(Self::message_context_item(
                    if has_later_messages {
                        tr("ai-regenerate-from-message-warning")
                    } else {
                        tr("ai-regenerate-from-message")
                    },
                    IconName::RotateCcw,
                    panel.clone(),
                    move |this, window, cx| this.regenerate_from_user(message_index, window, cx),
                ));
        } else if !is_user && can_mutate {
            menu = menu.item(Self::message_context_item(
                if has_later_messages {
                    tr("ai-retry-response-warning")
                } else {
                    tr("ai-retry-response")
                },
                IconName::RotateCcw,
                panel.clone(),
                move |this, window, cx| this.retry_assistant(message_index, window, cx),
            ));
        }

        menu.separator()
            .item(Self::message_context_item(
                tr("ai-scroll-top"),
                IconName::ArrowUp,
                panel.clone(),
                |this, _, cx| this.scroll_to_top(cx),
            ))
            .item(Self::message_context_item(
                tr("ai-scroll-bottom"),
                IconName::ChevronDown,
                panel,
                |this, _, cx| this.scroll_to_bottom(cx),
            ))
    }
    pub(super) fn close_composer_menus(&mut self) {
        self.model_search_query = SharedString::default();
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
    pub(super) fn build_attachment_menu(
        mut menu: PopupMenu,
        page_count: usize,
        has_selection: bool,
        has_region: bool,
        has_selected_pages: bool,
        panel: Entity<Self>,
    ) -> PopupMenu {
        menu = menu.min_w(px(260.0)).max_w(px(320.0));

        menu = menu
            .item(
                PopupMenuItem::new(tr("ai-attach-image-file"))
                    .icon(IconName::Image)
                    .on_click({
                        let panel = panel.clone();
                        move |_, window, cx| {
                            panel.update(cx, |this, cx| this.pick_user_images(window, cx));
                        }
                    }),
            )
            .item(
                PopupMenuItem::new(tr("ai-paste-image"))
                    .icon(IconName::Copy)
                    .on_click({
                        let panel = panel.clone();
                        move |_, _, cx| {
                            panel.update(cx, |this, cx| this.paste_user_image(cx));
                        }
                    }),
            )
            .separator();

        for mention in MentionKind::ALL.into_iter().filter(|mention| {
            mention_is_available(
                *mention,
                page_count,
                has_selection,
                has_region,
                has_selected_pages,
            )
        }) {
            menu = menu.item(
                PopupMenuItem::element(move |_, cx| {
                    let theme = cx.theme().colors;
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_color(theme.foreground).child(mention.label()))
                        .child(
                            div()
                                .truncate()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.muted_foreground)
                                .child(mention.description()),
                        )
                })
                .icon(Self::mention_icon(mention))
                .on_click({
                    let panel = panel.clone();
                    move |_, window, cx| {
                        panel.update(cx, |this, cx| {
                            this.add_attachment(mention, window, cx);
                        });
                    }
                }),
            );
        }

        menu
    }

    pub(super) fn build_thinking_menu(
        mut menu: PopupMenu,
        selected_api_style: AiApiStyle,
        selected_model: String,
        selected_level: AiThinkingLevel,
        panel: Entity<Self>,
    ) -> PopupMenu {
        menu = menu.min_w(px(280.0));
        for level in AiThinkingLevel::supported_for_style(selected_api_style, &selected_model) {
            let level = *level;
            menu = menu.item(
                PopupMenuItem::element(move |_, cx| {
                    let theme = cx.theme().colors;
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(level.label())
                        .child(
                            div()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.muted_foreground)
                                .child(thinking_level_description(level)),
                        )
                })
                .checked(level == selected_level)
                .on_click({
                    let panel = panel.clone();
                    move |_, _, cx| {
                        panel.update(cx, |this, cx| {
                            this.select_thinking_level(level, cx);
                        });
                    }
                }),
            );
        }

        menu
    }

    pub(super) fn render_model_menu(
        state: ModelMenuRenderState,
        theme: Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let query = state.search_query.to_string();
        let query_lower = query.to_ascii_lowercase();
        let mut entries = Vec::new();
        for (provider, provider_name, models) in state.provider_models {
            let mut provider_entries = models
                .into_iter()
                .filter(|model| {
                    query_lower.is_empty()
                        || model.id.to_ascii_lowercase().contains(&query_lower)
                        || model.name.to_ascii_lowercase().contains(&query_lower)
                        || model.group.to_ascii_lowercase().contains(&query_lower)
                        || provider_name.to_ascii_lowercase().contains(&query_lower)
                })
                .map(move |model| ModelMenuEntry::Model(provider, model))
                .collect::<Vec<_>>();
            if !provider_entries.is_empty() {
                entries.push(ModelMenuEntry::Header(provider_name));
                entries.append(&mut provider_entries);
            }
        }
        let entry_count = entries.len();
        let scroll_handle = state.scroll_handle.clone();
        let search_input = state.search_input;

        let mut menu = div()
            .w(px(340.0))
            .flex()
            .flex_col()
            .overflow_hidden()
            .text_size(ui_rems(12.0));

        menu = menu.child(
            div()
                .w_full()
                .min_w_0()
                .px_2()
                .pt_2()
                .pb_2()
                .child(Input::new(&search_input).w_full().min_w_0()),
        );

        if entries.is_empty() {
            menu = menu.child(
                div()
                    .px_2()
                    .py_2()
                    .text_color(theme.muted_foreground)
                    .child(tr("no-matching-models")),
            );
        }

        if entry_count > 0 {
            let selected_provider = state.selected_provider;
            let selected_model = state.selected_model;
            let rows = entries
                .into_iter()
                .map(|entry| match entry {
                    ModelMenuEntry::Header(provider_name) => div()
                        .h(px(26.0))
                        .flex()
                        .items_center()
                        .px_2()
                        .text_size(ui_rems(11.0))
                        .text_color(theme.muted_foreground)
                        .child(provider_name)
                        .into_any_element(),
                    ModelMenuEntry::Model(provider, model) => {
                        let selected = provider == selected_provider && model.id == selected_model;
                        let image_support = model.image_input_support(provider);
                        let tool_support = model.tool_use_support(provider);
                        let capability_marker = |support| match support {
                            AiCapabilitySupport::Supported => "✓",
                            AiCapabilitySupport::Unsupported => "—",
                            AiCapabilitySupport::Unknown => "?",
                        };
                        let model_for_click = model.id.clone();
                        let panel = state.panel.clone();
                        let popover = state.popover.clone();
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
                                theme.list_active
                            } else {
                                transparent_black()
                            })
                            .text_color(if selected {
                                theme.primary
                            } else {
                                theme.foreground
                            })
                            .cursor_pointer()
                            .hover(move |style| style.bg(theme.list_hover))
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                panel.update(cx, |this, cx| {
                                    this.select_model(provider, model_for_click.clone(), cx);
                                });
                                popover.update(cx, |popover, cx| {
                                    popover.dismiss(window, cx);
                                });
                                cx.stop_propagation();
                            })
                            .child(div().truncate().min_w_0().child(model.name))
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_1()
                                    .text_size(ui_rems(9.0))
                                    .text_color(theme.muted_foreground)
                                    .child(format!(
                                        "{}{} {}{}",
                                        tr("ai-capability-image-short"),
                                        capability_marker(image_support),
                                        tr("ai-capability-tool-short"),
                                        capability_marker(tool_support)
                                    ))
                                    .child(
                                        div()
                                            .max_w(px(72.0))
                                            .truncate()
                                            .text_size(ui_rems(11.0))
                                            .child(model.group),
                                    ),
                            )
                            .into_any_element()
                    }
                })
                .collect::<Vec<_>>();
            menu = menu.child(
                div()
                    .child(
                        div()
                            .id("ai-model-menu-list")
                            .track_scroll(&scroll_handle)
                            .max_h(px(300.0))
                            .pb_1()
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .children(rows),
                    )
                    .vertical_scrollbar_for_with_id(
                        &scroll_handle,
                        "ai-model-menu-scrollbar",
                        window,
                        cx,
                    ),
            );
        }

        menu.into_any_element()
    }
}
