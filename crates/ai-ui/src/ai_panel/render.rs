use super::helpers::*;
use super::*;

impl Render for AiPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.message_mention_hitboxes.clear();
        if self.mode == AiPanelMode::QuickAsk {
            return self.render_quick_ask(window, cx).into_any_element();
        }
        if self.message_scroll_interacted.replace(false) {
            self.follow_output = self.is_near_message_bottom();
        }

        let entity = cx.entity();
        let (theme, ai_settings, base_text_style) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: ui_rems(13.0).into(),
                color: rgb(store.resolved_theme.text).into(),
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (
                store.resolved_theme,
                store.settings.ai.clone(),
                base_text_style,
            )
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
        let model_menu_selected_model = selected_model.clone();
        let thinking_menu_selected_model = selected_model.clone();
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
        let attachment_menu_open = self.attachment_menu_open;
        let attachment_menu_anchor = self.attachment_menu_anchor;
        let attachment_menu_position = attachment_menu_open
            .then_some(())
            .and(attachment_menu_anchor);
        let model_menu_open = self.model_menu_open;
        let model_menu_anchor = self.model_menu_anchor;
        let model_menu_position = model_menu_open.then_some(()).and(model_menu_anchor);
        let model_search_query = self.model_search_query.clone();
        let model_menu_scroll = self.model_menu_scroll.clone();
        let thinking_menu_open = self.thinking_menu_open;
        let thinking_menu_anchor = self.thinking_menu_anchor;
        let thinking_menu_position = thinking_menu_open.then_some(()).and(thinking_menu_anchor);
        let model_search_focused = model_menu_open && self.focus_handle.is_focused(window);
        let completion_matches = self.mention_completion_matches(cx);
        let completion_selected = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0);
        let (has_current_selection, has_current_region) = self.current_mention_availability(cx);
        let sessions = self.sessions.clone();
        let current_session_id = self.session_id;
        let session_title = self.session_title.clone();
        let showing_sessions = self.view == AiPanelView::Sessions;
        let loading = self.loading;
        let empty_chat = self.messages.is_empty();
        let panel_width = self.width;
        let validation_notice = self.validation_notice.clone();
        let validation_toast = self.validation_toast.clone();
        let message_context_menu = self.message_context_menu.clone();
        let show_scroll_to_bottom =
            !self.follow_output && self.scroll_handle.max_offset().y > px(0.0) && !empty_chat;
        let mention_preview = self.mention_preview.clone();
        let mention_preview_scroll = self.mention_preview_scroll.clone();
        let mention_preview_bounds = self.mention_preview_bounds.clone();
        let (mention_preview_width, mention_preview_image_height) = Self::mention_preview_size(
            mention_preview.as_ref(),
            panel_width,
            window.viewport_size().width.as_f32(),
            window.viewport_size().height.as_f32(),
        );
        let completion_placement = if showing_sessions || completion_matches.is_empty() {
            None
        } else {
            self.completion_menu_placement(&completion_matches, window)
        };
        let completion_scroll = self.mention_completion_scroll.clone();

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
                    editing_focused: self.edit_focus_handle.is_focused(window),
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

        div()
            .key_context("AiPanel TextInput")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.width))
            .h_full()
            .border_l(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.surface_background))
            .text_color(rgb(theme.text))
            .capture_any_mouse_down(cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.close_message_context_menu(cx);
            }))
            .on_mouse_move(cx.listener(Self::on_panel_mouse_move))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_backward))
            .on_action(cx.listener(Self::delete_word_forward))
            .on_action(cx.listener(Self::delete_to_line_start))
            .on_action(cx.listener(Self::delete_to_line_end))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::document_start))
            .on_action(cx.listener(Self::document_end))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::select_document_start))
            .on_action(cx.listener(Self::select_document_end))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(42.0))
                    .px_2()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border))
                    .text_size(ui_rems(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(ui_rems(10.0))
                                    .text_color(rgb(theme.text_muted))
                                    .child(tr("ai-title")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui_rems(13.0))
                                    .text_color(rgb(theme.text))
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
                                    theme.text_muted
                                } else {
                                    theme.text
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
                                    theme.accent
                                } else {
                                    theme.text_muted
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
                                theme.text_muted,
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
                                    div()
                                        .occlude()
                                        .absolute()
                                        .right_3()
                                        .bottom_2()
                                        .rounded(px(6.0))
                                        .border_1()
                                        .border_color(rgb(theme.border))
                                        .bg(rgb(theme.surface_background))
                                        .shadow_md()
                                        .child(Self::message_action_button(
                                            "ai-scroll-to-bottom",
                                            IconName::ChevronDown,
                                            tr("ai-scroll-bottom"),
                                            theme.text_muted,
                                            theme,
                                            |this, _, cx| this.scroll_to_bottom(cx),
                                            cx,
                                        )),
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
                            .bg(rgb(theme.tab_active_background))
                            .when(empty_chat, |this| this.flex_1().min_h_0())
                            .when(!empty_chat, |this| {
                                this.border_t(px(1.0)).border_color(rgb(theme.border))
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
                                            .pt_1()
                                            .pr_2()
                                            .key_context("TextInput")
                                            .when(empty_chat, |this| this.flex_1())
                                            .cursor(CursorStyle::IBeam)
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(Self::on_mouse_down),
                                            )
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::on_mouse_up),
                                            )
                                            .on_mouse_up_out(
                                                MouseButton::Left,
                                                cx.listener(Self::on_mouse_up_out),
                                            )
                                            .on_mouse_down_out(
                                                cx.listener(Self::on_mouse_down_out),
                                            )
                                            .on_mouse_move(cx.listener(Self::on_mouse_move))
                                            .on_hover(cx.listener(
                                                |this, hovered: &bool, window, cx| {
                                                    if !*hovered {
                                                        this.schedule_mention_preview_hide(
                                                            window, cx,
                                                        );
                                                    }
                                                },
                                            ))
                                            .child(AiInputElement {
                                                panel: entity.clone(),
                                                source: AiInputSource::Composer,
                                            }),
                                    )
                                    .when_some(validation_notice.clone(), |this, notice| {
                                        this.child(
                                            div()
                                                .text_size(ui_rems(11.0))
                                                .text_color(rgb(theme.error))
                                                .child(notice),
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
                                            .text_color(rgb(theme.text_muted))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_0p5()
                                                    .child(
                                                        toolbar_icon_button(
                                                            "ai-attachment-menu-button",
                                                            IconName::Plus,
                                                        )
                                                        .text_color(rgb(
                                                            if attachment_menu_open {
                                                                theme.accent
                                                            } else {
                                                                theme.text_muted
                                                            },
                                                        ))
                                                        .hover(move |style| {
                                                            style.bg(rgb(theme.element_hover))
                                                        })
                                                        .tooltip(tr("attach"))
                                                        .on_click(cx.listener(
                                                            |this,
                                                             event: &gpui::ClickEvent,
                                                             _,
                                                             cx| {
                                                                    this.toggle_attachment_menu(
                                                                        event.position(),
                                                                        cx,
                                                                    );
                                                                    cx.stop_propagation();
                                                                },
                                                            )),
                                                    )
                                                    .child(
                                                        div()
                                                            .id("ai-thinking-menu-button")
                                                            .flex()
                                                            .h(ui_rems(28.0))
                                                            .items_center()
                                                            .gap_1()
                                                            .px_2()
                                                            .rounded(px(4.0))
                                                            .text_color(rgb(
                                                                if thinking_menu_open
                                                                    || selected_thinking_level
                                                                        != AiThinkingLevel::Off
                                                                {
                                                                    theme.accent
                                                                } else {
                                                                    theme.text_muted
                                                                },
                                                            ))
                                                            .cursor_pointer()
                                                            .hover(move |style| {
                                                                style.bg(rgb(theme.element_hover))
                                                            })
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    |this,
                                                                     event: &MouseDownEvent,
                                                                     _,
                                                                     cx| {
                                                                        this.toggle_thinking_menu(
                                                                            event.position,
                                                                            cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ),
                                                            )
                                                            .child(format!(
                                                                "Think {}",
                                                                selected_thinking_level.label()
                                                            ))
                                                            .child(
                                                                Icon::new(
                                                                    if thinking_menu_open {
                                                                        IconName::ChevronUp
                                                                    } else {
                                                                        IconName::ChevronDown
                                                                    },
                                                                )
                                                                .size(ui_rems(11.0))
                                                                .text_color(rgb(theme.text_muted)),
                                                            ),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .min_w_0()
                                                    .flex_wrap()
                                                    .items_center()
                                                    .justify_end()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .id("ai-model-menu-button")
                                                            .flex()
                                                            .h(ui_rems(28.0))
                                                            .min_w_0()
                                                            .items_center()
                                                            .gap_1()
                                                            .px_2()
                                                            .rounded(px(4.0))
                                                            .text_color(rgb(
                                                                if model_menu_open {
                                                                    theme.accent
                                                                } else {
                                                                    theme.text_muted
                                                                },
                                                            ))
                                                            .cursor_pointer()
                                                            .hover(move |style| {
                                                                style.bg(rgb(theme.element_hover))
                                                            })
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    |this,
                                                                     event: &MouseDownEvent,
                                                                     window,
                                                                     cx| {
                                                                        this.toggle_model_menu(
                                                                            event.position,
                                                                            window,
                                                                            cx,
                                                                        );
                                                                        cx.stop_propagation();
                                                                    },
                                                                ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .max_w(px(96.0))
                                                                    .truncate()
                                                                    .child(model_label),
                                                            )
                                                            .child(
                                                                Icon::new(if model_menu_open {
                                                                    IconName::ChevronUp
                                                                } else {
                                                                    IconName::ChevronDown
                                                                })
                                                                .size(ui_rems(11.0))
                                                                .text_color(rgb(theme.text_muted)),
                                                            ),
                                                    )
                                                    .child(
                                                        div()
                                                            .id("ai-send-button")
                                                            .tooltip(text_tooltip(if loading {
                                                                tr("ai-stop-generation")
                                                            } else if can_send {
                                                                tr("send")
                                                            } else {
                                                                tr("ai-ask-placeholder")
                                                            }))
                                                            .flex()
                                                            .flex_none()
                                                            .items_center()
                                                            .justify_center()
                                                            .w(ui_rems(28.0))
                                                            .h(ui_rems(28.0))
                                                            .rounded(px(4.0))
                                                            .bg(rgb(theme.control_background))
                                                            .when(can_send, |this| {
                                                                this.cursor_pointer().hover(
                                                                    move |style| {
                                                                        style.bg(rgb(
                                                                            theme.control_hover,
                                                                        ))
                                                                    },
                                                                )
                                                            })
                                                            .when(loading, |this| {
                                                                this.cursor_pointer()
                                                                    .bg(rgba(with_alpha(
                                                                        theme.error,
                                                                        0x20,
                                                                    )))
                                                                    .hover(move |style| {
                                                                        style.bg(rgba(with_alpha(
                                                                            theme.error,
                                                                            0x30,
                                                                        )))
                                                                    })
                                                            })
                                                            .on_mouse_down(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    move |this, _, _, cx| {
                                                                        if this.loading {
                                                                            this.stop_generation(
                                                                                cx,
                                                                            );
                                                                        } else if can_send {
                                                                            this.send_message(cx);
                                                                        }
                                                                        cx.stop_propagation();
                                                                    },
                                                                ),
                                                            )
                                                            .child(if loading {
                                                                Icon::new(IconName::Stop)
                                                                    .size(ui_rems(13.0))
                                                                    .text_color(rgb(theme.error))
                                                                    .into_any_element()
                                                            } else {
                                                                Icon::new(IconName::ArrowUp)
                                                                    .size(ui_rems(13.0))
                                                                    .text_color(rgb(if can_send {
                                                                        theme.accent
                                                                    } else {
                                                                        theme.text_muted
                                                                    }))
                                                                    .into_any_element()
                                                            }),
                                                    ),
                                            ),
                                    ),
                            ),
                    )
                })
            })
            .when_some(completion_placement, |this, placement| {
                this.child(
                    deferred(
                        anchored()
                            .position(placement.position)
                            .anchor(placement.anchor)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_mention_completion_menu(
                                completion_matches,
                                completion_selected,
                                placement.width,
                                completion_scroll,
                                theme,
                                cx,
                            )),
                    )
                    .with_priority(1),
                )
            })
            .when_some(mention_preview, |this, preview| {
                this.child(
                    deferred(
                        anchored()
                            .position(preview.position)
                            .anchor(preview.anchor)
                            .snap_to_window_with_margin(px(8.0))
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
            .when_some(attachment_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_attachment_menu(
                                self.page_count,
                                has_current_selection,
                                has_current_region,
                                theme,
                                cx,
                            )),
                    )
                    .with_priority(2),
                )
            })
            .when_some(model_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_model_menu(
                                ModelMenuRenderState {
                                    selected_provider,
                                    selected_model: model_menu_selected_model,
                                    provider_models,
                                    search_query: model_search_query,
                                    scroll_handle: model_menu_scroll,
                                    focused: model_search_focused,
                                },
                                theme,
                                window,
                                cx,
                            )),
                    )
                    .with_priority(2),
                )
            })
            .when_some(thinking_menu_position, |this, anchor| {
                this.child(
                    deferred(
                        anchored()
                            .position(anchor)
                            .anchor(Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(Self::render_thinking_menu(
                                selected_provider,
                                thinking_menu_selected_model,
                                selected_thinking_level,
                                theme,
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
                            .child(self.render_message_context_menu(menu, theme, cx)),
                    )
                    .with_priority(4),
                )
            })
            .when_some(validation_toast, |this, notice| {
                this.child(deferred(Self::render_validation_toast(notice, theme)).with_priority(3))
            })
            .into_any_element()
    }
}
