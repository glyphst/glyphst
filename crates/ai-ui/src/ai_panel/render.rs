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
        let can_send = !self.loading
            && !editing
            && self.generation_failure.is_none()
            && !self.content.trim().is_empty();
        let resolved_main = ai_settings.resolved_main();
        let selected_provider = resolved_main.provider;
        let selected_model = resolved_main.model;
        let selected_thinking_level = ai_settings
            .thinking_level
            .clamp_for(selected_provider, &selected_model);
        let provider_models = AiProvider::ALL
            .into_iter()
            .filter_map(|provider| {
                let config = ai_settings.provider_config(provider);
                (config.enabled && !config.models.is_empty()).then_some((provider, config.models))
            })
            .collect::<Vec<_>>();
        let model_label = ai_settings
            .main_model
            .as_ref()
            .map(|model_ref| ai_settings.model_label(model_ref))
            .unwrap_or_else(|| selected_model.clone());
        let model_search_query = self.model_search_query.clone();
        let model_menu_scroll = self.model_menu_scroll.clone();
        let (has_current_selection, has_current_region) = self.current_mention_availability(cx);
        let sessions = self.sessions.clone();
        let current_session_id = self.session_id;
        let session_title = self.session_title.clone();
        let showing_sessions = self.view == AiPanelView::Sessions;
        let loading = self.loading;
        let empty_chat = self.messages.is_empty();
        let panel_width = self.width;
        let validation_notice = self.validation_notice.clone();
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
        let attachment_trigger = toolbar_icon_button("ai-attachment-menu-button", IconName::Plus)
            .text_color(theme.muted_foreground)
            .tooltip(tr("attach"));
        let attachment_control = if loading {
            attachment_trigger.disabled(true).into_any_element()
        } else {
            let panel = entity.clone();
            let page_count = self.page_count;
            attachment_trigger
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    Self::build_attachment_menu(
                        menu,
                        page_count,
                        has_current_selection,
                        has_current_region,
                        panel.clone(),
                    )
                })
                .into_any_element()
        };
        let thinking_trigger = Button::new("ai-thinking-menu-button")
            .xsmall()
            .compact()
            .ghost()
            .label(format!("Think {}", selected_thinking_level.label()))
            .dropdown_caret(true)
            .selected(selected_thinking_level != AiThinkingLevel::Off);
        let thinking_control = if loading {
            thinking_trigger.disabled(true).into_any_element()
        } else {
            let panel = entity.clone();
            let selected_model = selected_model.clone();
            thinking_trigger
                .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                    Self::build_thinking_menu(
                        menu,
                        selected_provider,
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
            .label(model_label.clone())
            .truncate_label()
            .tooltip(model_label)
            .dropdown_caret(true)
            .max_w(px(120.0));
        let model_control = if loading {
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
            .when(loading, |button| button.danger().icon(IconName::Stop))
            .when(!loading, |button| button.primary().icon(IconName::ArrowUp))
            .disabled(!loading && !can_send)
            .tooltip(if loading {
                tr("ai-stop-generation")
            } else if can_send {
                tr("send")
            } else {
                tr("ai-ask-placeholder")
            })
            .w(ui_rems(28.0))
            .h(ui_rems(28.0))
            .on_click(move |_, window, cx| {
                send_panel.update(cx, |this, cx| {
                    if this.loading {
                        this.stop_generation(cx);
                    } else if can_send {
                        this.send_message(window, cx);
                    }
                    cx.stop_propagation();
                });
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
            .bg(theme.popover)
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
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(42.0))
                    .px_2()
                    .border_b(px(1.0))
                    .border_color(theme.border)
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
                                theme,
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
                                theme,
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
                                theme,
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
                this.when(!empty_chat, |this| {
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
                                        theme,
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
                            .py_2()
                            .bg(theme.tab_active)
                            .when(empty_chat, |this| this.flex_1().min_h_0())
                            .when(!empty_chat, |this| {
                                this.border_t(px(1.0)).border_color(theme.border)
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .px_2()
                                    .when(empty_chat, |this| this.flex_1().min_h_0())
                                    .child(
                                        div()
                                            .id("ai-panel-composer-input")
                                            .relative()
                                            .w_full()
                                            .min_h_0()
                                            .when(empty_chat, |this| this.flex_1())
                                            .child(
                                                Input::new(&self.composer_input)
                                                    .appearance(false)
                                                    .bordered(false)
                                                    .focus_bordered(false)
                                                    .w_full()
                                                    .when(empty_chat, |input| input.h_full()),
                                            ),
                                    )
                                    .when_some(validation_notice.clone(), |this, notice| {
                                        this.child(
                                            Alert::error("ai-composer-validation-alert", notice)
                                                .xsmall(),
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
