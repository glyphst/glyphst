use super::helpers::agent_image_preview;
use super::*;

impl Render for AiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_composer_input(window, cx);
        self.sync_model_search_input(window, cx);
        self.selected_range = self.active_composer_input().read(cx).selected_range();
        if self.mode == AiPanelMode::QuickAsk {
            return self.render_quick_ask(window, cx).into_any_element();
        }
        if self.message_scroll_interacted.replace(false) {
            self.follow_output = self.is_near_message_bottom();
        }

        let entity = cx.entity();
        let theme = cx.theme().colors;
        let (ai_settings, base_text_style) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: ui_rems(13.0).into(),
                color: theme.foreground,
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (store.settings.ai.clone(), base_text_style)
        };
        let editing = self.editing_turn.is_some();
        let can_send = !editing
            && self.generation_failure.is_none()
            && ai_settings.main_model.is_some()
            && (!self.content.trim().is_empty() || !self.composer_images.is_empty());
        let resolved_main = ai_settings.resolved_main();
        let selected_provider = resolved_main.provider;
        let selected_model = resolved_main.model;
        let selected_thinking_level = ai_settings
            .thinking_level
            .clamp_for_style(resolved_main.api_style, &selected_model);
        let selected_api_style = resolved_main.api_style;
        let provider_models = ai_settings
            .ordered_providers()
            .iter()
            .copied()
            .filter_map(|provider| {
                let config = ai_settings.provider_config(provider);
                (config.enabled && !config.models.is_empty()).then_some((
                    provider,
                    config.name,
                    config.models,
                ))
            })
            .collect::<Vec<_>>();
        let model_label = ai_settings
            .main_model
            .as_ref()
            .map(|model_ref| ai_settings.model_label(model_ref))
            .unwrap_or_else(|| tr("settings-no-enabled-provider-models"));
        let model_search_query = self.model_search_query.clone();
        let model_menu_scroll = self.model_menu_scroll.clone();
        let (has_current_selection, has_current_region, has_selected_pages) =
            self.current_mention_availability(cx);
        let sessions = self.sessions.clone();
        let current_session_id = self.session_id;
        let session_title = self.session_title.clone();
        let showing_sessions = self.view == AiPanelView::Sessions;
        let loading = self.loading;
        let empty_chat = self.messages.is_empty();
        let panel_width = self.width;
        let validation_notice = self.validation_notice.clone();
        let composer_image_previews = self
            .composer_images
            .iter()
            .enumerate()
            .filter_map(|(index, image)| {
                let preview = agent_image_preview(image)?;
                let panel = entity.clone();
                Some(
                    div()
                        .id(("ai-composer-image", index))
                        .relative()
                        .flex_none()
                        .w(px(58.0))
                        .h(px(58.0))
                        .overflow_hidden()
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.tab_active)
                        .child(img(preview).size_full().object_fit(ObjectFit::Cover))
                        .child(
                            toolbar_icon_button(("ai-composer-image-remove", index), IconName::X)
                                .xsmall()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bg(theme.popover.opacity(0.9))
                                .on_click(move |_, _, cx| {
                                    panel.update(cx, |this, cx| {
                                        this.remove_composer_image(index, cx);
                                        cx.stop_propagation();
                                    });
                                }),
                        )
                        .into_any_element(),
                )
            })
            .collect::<Vec<_>>();
        let queued_rows = self
            .queued_messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let panel = entity.clone();
                let text = message.display_text();
                let image_count = message.user_images().count();
                let label = if text.trim().is_empty() {
                    tr_with_number("ai-queued-images", "count", image_count)
                } else if text.chars().count() > 80 {
                    format!("{}…", text.chars().take(80).collect::<String>())
                } else {
                    text
                };
                div()
                    .id(("ai-queued-message", index))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted.opacity(0.18))
                    .pl_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(ui_rems(10.0))
                            .text_color(theme.muted_foreground)
                            .child(format!("{} · {label}", tr("ai-queued"))),
                    )
                    .child(
                        toolbar_icon_button(("ai-queued-remove", index), IconName::X)
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                panel.update(cx, |this, cx| {
                                    this.remove_queued_message(index, cx);
                                    cx.stop_propagation();
                                });
                            }),
                    )
            })
            .collect::<Vec<_>>();
        let message_context_menu = self.message_context_menu.clone();
        let mention_preview = self.mention_preview.clone();
        let mention_preview_scroll = self.mention_preview_scroll.clone();
        let mention_preview_bounds = self.mention_preview_bounds.clone();
        let (mention_preview_width, mention_preview_image_height) = Self::mention_preview_size(
            mention_preview.as_ref(),
            self.width,
            window.viewport_size().width.as_f32(),
            window.viewport_size().height.as_f32(),
        );
        let show_scroll_to_bottom =
            !self.follow_output && self.scroll_handle.max_offset().y > px(0.0) && !empty_chat;
        let mut message_rows = Vec::with_capacity(self.messages.len() + 1);
        for (message_index, (message, markdown)) in self
            .messages
            .iter()
            .zip(self.markdown_views.iter())
            .map(|(m, md)| (m.clone(), md.clone()))
            .enumerate()
        {
            message_rows.push(self.render_message(
                MessageRenderInput {
                    index: message_index,
                    message,
                    markdown,
                    theme,
                    panel_width,
                    base_text_style: base_text_style.clone(),
                    panel: entity.clone(),
                    editing_focused: self.edit_input.focus_handle(cx).is_focused(window),
                },
                cx,
            ));
        }
        if !loading
            && self.generation_failure.is_none()
            && let Some((assistant_index, _)) =
                self.messages
                    .iter()
                    .enumerate()
                    .next_back()
                    .filter(|(_, message)| {
                        message.role == Role::Assistant && !message.is_display_empty()
                    })
        {
            message_rows.push(self.render_turn_footer(assistant_index, theme, cx));
        }
        let message_scroll_handle = TrackedScrollHandle {
            handle: self.scroll_handle.clone(),
            interacted: self.message_scroll_interacted.clone(),
        };
        let attachment_trigger = Button::new("ai-attachment-menu-button")
            .small()
            .compact()
            .ghost()
            .icon(IconName::Plus)
            .w(ui_rems(28.0))
            .h(ui_rems(28.0))
            .debug_selector(|| "ai-attachment-menu-button".to_string())
            .text_color(theme.muted_foreground)
            .tooltip(tr("attach"));
        let panel = entity.clone();
        let page_count = self.page_count;
        let attachment_control = attachment_trigger
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                Self::build_attachment_menu(
                    menu,
                    page_count,
                    has_current_selection,
                    has_current_region,
                    has_selected_pages,
                    panel.clone(),
                )
            })
            .into_any_element();
        let thinking_trigger = Button::new("ai-thinking-menu-button")
            .xsmall()
            .compact()
            .ghost()
            .label(format!("Think {}", selected_thinking_level.label()))
            .dropdown_caret(true)
            .selected(selected_thinking_level != AiThinkingLevel::Off);
        let thinking_control = if loading || selected_model.is_empty() {
            thinking_trigger.disabled(true).into_any_element()
        } else {
            let panel = entity.clone();
            let selected_model = selected_model.clone();
            thinking_trigger
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    Self::build_thinking_menu(
                        menu,
                        selected_api_style,
                        selected_model.clone(),
                        selected_thinking_level,
                        panel.clone(),
                    )
                })
                .into_any_element()
        };
        let model_trigger = Button::new("ai-model-menu-button")
            .xsmall()
            .compact()
            .ghost()
            .flex_shrink_1()
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(model_label.clone()),
            )
            .tooltip(model_label)
            .dropdown_caret(true)
            .max_w(px(120.0));
        let model_control = if loading || provider_models.is_empty() {
            model_trigger.disabled(true).into_any_element()
        } else {
            let on_open_panel = entity.clone();
            let menu_panel = entity.clone();
            let selected_model = selected_model.clone();
            let provider_models = provider_models.clone();
            let search_input = self.model_search_input.clone();
            let search_focus = search_input.focus_handle(cx);
            Popover::new("ai-model-menu-popover")
                .anchor(Anchor::TopRight)
                .p_0()
                .track_focus(&search_focus)
                .trigger(model_trigger)
                .on_open_change(move |open, window, cx| {
                    on_open_panel.update(cx, |this, cx| {
                        this.message_context_menu = None;
                        this.model_search_query = SharedString::default();
                        this.sync_model_search_input(window, cx);
                        cx.notify();
                    });
                    if !open {
                        window.refresh();
                    }
                })
                .content(move |_, window, cx| {
                    Self::render_model_menu(
                        ModelMenuRenderState {
                            selected_provider,
                            selected_model: selected_model.clone(),
                            provider_models: provider_models.clone(),
                            search_query: model_search_query.clone(),
                            search_input: search_input.clone(),
                            scroll_handle: model_menu_scroll.clone(),
                            panel: menu_panel.clone(),
                            popover: cx.entity(),
                        },
                        theme,
                        window,
                        cx,
                    )
                })
                .into_any_element()
        };
        let send_panel = entity.clone();
        let send_control = Button::new("ai-send-button")
            .small()
            .compact()
            .primary()
            .icon(IconName::ArrowUp)
            .disabled(!can_send)
            .tooltip(if loading {
                tr("ai-queue-message")
            } else if can_send {
                tr("send")
            } else {
                tr("ai-ask-placeholder")
            })
            .w(ui_rems(28.0))
            .h(ui_rems(28.0))
            .on_click(move |_, window, cx| {
                send_panel.update(cx, |this, cx| {
                    if can_send {
                        this.send_message(window, cx);
                    }
                    cx.stop_propagation();
                });
            });
        let stop_control = loading.then(|| {
            let panel = entity.clone();
            Button::new("ai-stop-button")
                .small()
                .compact()
                .danger()
                .icon(IconName::Stop)
                .tooltip(tr("ai-stop-generation"))
                .w(ui_rems(28.0))
                .h(ui_rems(28.0))
                .on_click(move |_, _, cx| {
                    panel.update(cx, |this, cx| {
                        this.stop_generation(cx);
                        cx.stop_propagation();
                    });
                })
        });

        div()
            .key_context("AiPanel")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .w_full()
            .min_w_0()
            .h_full()
            .border_l(px(1.0))
            .border_color(theme.border)
            .bg(theme.background)
            .text_color(theme.foreground)
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.close_message_context_menu(cx);
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_mouse_down(MouseButton::Right, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_drag_move::<ExternalPaths>(cx.listener(
                |_, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        cx.stop_propagation();
                    }
                },
            ))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.attach_external_image_paths(paths.paths().to_vec(), cx);
                cx.stop_propagation();
            }))
            .on_action(cx.listener(Self::on_composer_paste))
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(46.0))
                    .px_3()
                    .border_b(px(1.0))
                    .border_color(theme.border.opacity(0.8))
                    .bg(theme.muted.opacity(0.28))
                    .text_size(ui_rems(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_rems(10.0))
                                    .text_color(theme.muted_foreground)
                                    .child(tr("ai-title")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_rems(13.0))
                                    .text_color(theme.foreground)
                                    .child(session_title),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Self::message_action_button(
                                "ai-panel-new-session",
                                IconName::Plus,
                                tr("ai-new-chat"),
                                if loading {
                                    theme.muted_foreground
                                } else {
                                    theme.foreground
                                },
                                |this, window, cx| this.new_session(window, cx),
                                cx,
                            ))
                            .child(Self::message_action_button(
                                "ai-panel-toggle-sessions",
                                IconName::NotebookTabs,
                                if showing_sessions {
                                    tr("ai-show-chat")
                                } else {
                                    tr("ai-show-history")
                                },
                                if showing_sessions {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                },
                                |this, window, cx| {
                                    if this.view == AiPanelView::Sessions {
                                        this.show_chat(cx);
                                    } else {
                                        this.show_sessions(window, cx);
                                    }
                                },
                                cx,
                            ))
                            .child(Self::message_action_button(
                                "ai-panel-close",
                                IconName::X,
                                tr("ai-close-panel"),
                                theme.muted_foreground,
                                |this, window, cx| this.dismiss(window, cx),
                                cx,
                            )),
                    ),
            )
            .when(showing_sessions, |this| {
                this.child(Self::render_sessions_view(
                    sessions,
                    current_session_id,
                    loading,
                    theme,
                    &self.sessions_scroll,
                    window,
                    cx,
                ))
            })
            .when(!showing_sessions, |this| {
                this.when(empty_chat, |this| {
                    this.child(
                        div()
                            .id("ai-panel-empty")
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .px_6()
                            .text_center()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(42.0))
                                    .rounded_full()
                                    .bg(theme.primary.opacity(0.12))
                                    .child(
                                        Icon::new(IconName::Bot)
                                            .size(ui_rems(20.0))
                                            .text_color(theme.primary),
                                    ),
                            )
                            .child(div().text_size(ui_rems(14.0)).child(tr("ai-empty-title")))
                            .child(
                                div()
                                    .max_w(px(260.0))
                                    .text_size(ui_rems(11.0))
                                    .text_color(theme.muted_foreground)
                                    .child(tr("ai-empty-description")),
                            ),
                    )
                })
                .when(!empty_chat, |this| {
                    this.child(
                        div()
                            .relative()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .child(
                                div()
                                    .id("ai-panel-messages")
                                    .flex()
                                    .flex_col()
                                    .gap_0()
                                    .p_3()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.scroll_handle)
                                    .on_scroll_wheel(cx.listener(
                                        |this, event: &ScrollWheelEvent, _, cx| {
                                            this.handle_message_scroll(event, cx);
                                        },
                                    ))
                                    .children(message_rows),
                            )
                            .vertical_scrollbar_for(&message_scroll_handle, window, cx)
                            .when(show_scroll_to_bottom, |this| {
                                this.child(
                                    Self::message_action_button(
                                        "ai-scroll-to-bottom",
                                        IconName::ChevronDown,
                                        tr("ai-scroll-bottom"),
                                        theme.muted_foreground,
                                        |this, _, cx| this.scroll_to_bottom(cx),
                                        cx,
                                    )
                                    .outline()
                                    .absolute()
                                    .right_3()
                                    .bottom_2(),
                                )
                            }),
                    )
                })
                .when(!editing, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .p_2()
                            .border_t(px(1.0))
                            .border_color(theme.border.opacity(0.8))
                            .bg(theme.background)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .overflow_hidden()
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.popover)
                                    .shadow_sm()
                                    .when(!queued_rows.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .px_2()
                                                .pt_2()
                                                .children(queued_rows),
                                        )
                                    })
                                    .when(!composer_image_previews.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .flex()
                                                .flex_wrap()
                                                .gap_1()
                                                .px_2()
                                                .pt_2()
                                                .children(composer_image_previews),
                                        )
                                    })
                                    .child(
                                        div()
                                            .id("ai-panel-composer-input")
                                            .relative()
                                            .w_full()
                                            .min_h_0()
                                            .px_2()
                                            .pt_1()
                                            .child(
                                                Input::new(&self.composer_input)
                                                    .appearance(false)
                                                    .bordered(false)
                                                    .focus_bordered(false)
                                                    .w_full(),
                                            ),
                                    )
                                    .when_some(validation_notice.clone(), |this, notice| {
                                        this.child(
                                            div().px_2().child(
                                                Alert::error(
                                                    "ai-composer-validation-alert",
                                                    notice,
                                                )
                                                .xsmall(),
                                            ),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .w_full()
                                            .flex_none()
                                            .flex_wrap()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .px_1()
                                            .pb_1()
                                            .text_size(ui_rems(11.0))
                                            .text_color(theme.muted_foreground)
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_0p5()
                                                    .child(attachment_control)
                                                    .child(thinking_control),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .justify_end()
                                                    .gap_1()
                                                    .child(model_control)
                                                    .children(stop_control)
                                                    .child(send_control),
                                            ),
                                    ),
                            ),
                    )
                })
            })
            .when_some(mention_preview, |this, preview| {
                this.child(
                    deferred(
                        anchored()
                            .position(preview.position)
                            .anchor(preview.anchor)
                            .snap_to_window_with_margin(px(MENTION_PREVIEW_WINDOW_MARGIN))
                            .child(Self::render_mention_preview(
                                preview,
                                MentionPreviewRenderOptions {
                                    width: mention_preview_width,
                                    image_height: mention_preview_image_height,
                                    scroll_handle: mention_preview_scroll,
                                    bounds_cell: mention_preview_bounds,
                                    theme,
                                    base_text_style,
                                },
                                window,
                                cx,
                            )),
                    )
                    .with_priority(2),
                )
            })
            .when_some(message_context_menu, |this, menu| {
                this.child(
                    deferred(
                        anchored()
                            .position(menu.position)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(menu.popup),
                    )
                    .with_priority(4),
                )
            })
            .into_any_element()
    }
}
