use super::*;

impl AiPanel {
    pub(super) fn render_quick_ask(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().colors;
        let (quick_ask, base_text_style) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: px(store.settings.ai.quick_ask.text_size).into(),
                color: theme.foreground,
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (store.settings.ai.quick_ask, base_text_style)
        };
        let loading = self.loading;
        let completion_matches = self.mention_completion_matches(cx);
        let completion_selected = self
            .mention_completion
            .as_ref()
            .map(|completion| completion.selected_ix)
            .unwrap_or(0);
        let completion_placement = if completion_matches.is_empty() {
            None
        } else {
            self.completion_menu_placement(&completion_matches, window)
        };
        let completion_scroll = self.mention_completion_scroll.clone();
        let answer_index = self
            .messages
            .iter()
            .rposition(|message| message.role == Role::Assistant);
        let answer = answer_index
            .and_then(|index| self.messages.get(index))
            .map(ChatMessage::display_text)
            .unwrap_or_default();
        let answer_md = answer_index.and_then(|index| self.markdown_views.get(index).cloned());
        let generation_failure = self.generation_failure.clone();
        let copied_answer = answer_index
            .is_some_and(|index| self.copied_target == Some(CopyFeedbackTarget::Message(index)));
        let copied_error = self.copied_target == Some(CopyFeedbackTarget::Error);
        let show_answer = loading || !answer.is_empty() || generation_failure.is_some();

        div()
            .key_context("QuickAsk TextInput")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .border_t(px(1.0))
            .border_color(theme.border)
            .bg(theme
                .popover
                .opacity(f32::from(quick_ask.background_alpha()) / 255.0))
            .text_size(px(quick_ask.text_size))
            .text_color(theme.foreground)
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::submit))
            .child(
                div().flex().flex_col().child(
                    div()
                        .id("quick-ask-composer-input")
                        .relative()
                        .min_h(px(32.0))
                        .child(
                            Input::new(&self.composer_input)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .w_full()
                                .px_3()
                                .py_2(),
                        ),
                ),
            )
            .when(loading, |this| {
                this.child(
                    Self::message_action_button(
                        "quick-ask-stop",
                        IconName::Stop,
                        tr("ai-stop-generation"),
                        theme.danger,
                        theme,
                        |this, _, cx| {
                            this.stop_generation(cx);
                        },
                        cx,
                    )
                    .outline()
                    .absolute()
                    .top_1()
                    .right_2(),
                )
            })
            .when(show_answer, |this| {
                this.child(
                    div()
                        .relative()
                        .group("quick-ask-answer")
                        .border_t(px(1.0))
                        .border_color(theme.input)
                        .child(
                            div()
                                .id("quick-ask-answer")
                                .track_scroll(&self.scroll_handle)
                                .max_h(px(320.0))
                                .p_3()
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .text_color(theme.foreground)
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .when(loading && answer.is_empty(), |this| {
                                    this.child(Self::render_loading_indicator(
                                        "quick-ask-loading",
                                        theme,
                                    ))
                                })
                                .when(!answer.is_empty(), |this| {
                                    this.when_some(answer_md, |this, md| {
                                        this.child(MarkdownElement::new(
                                            md,
                                            MarkdownStyle::from_theme(
                                                theme,
                                                base_text_style.clone(),
                                            ),
                                        ))
                                    })
                                })
                                .when_some(generation_failure, |this, failure| {
                                    this.child(Self::render_generation_failure_card(
                                        failure,
                                        copied_error,
                                        "quick-ask-error",
                                        theme,
                                        cx,
                                    ))
                                }),
                        )
                        .when_some(
                            answer_index.filter(|_| !answer.is_empty()),
                            |this, index| {
                                this.child(
                                    Self::message_action_button(
                                        "quick-ask-copy-response",
                                        if copied_answer {
                                            IconName::Check
                                        } else {
                                            IconName::Copy
                                        },
                                        if copied_answer {
                                            tr("ai-copied")
                                        } else {
                                            tr("ai-copy-response")
                                        },
                                        if copied_answer {
                                            theme.success
                                        } else {
                                            theme.muted_foreground
                                        },
                                        theme,
                                        move |this, _, cx| this.copy_message(index, cx),
                                        cx,
                                    )
                                    .outline()
                                    .invisible()
                                    .group_hover("quick-ask-answer", |style| style.visible())
                                    .absolute()
                                    .top_2()
                                    .right_3(),
                                )
                            },
                        )
                        .vertical_scrollbar_for(&self.scroll_handle, window, cx),
                )
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
    }
    pub(super) fn message_action_button(
        id: impl Into<ElementId>,
        icon: IconName,
        tooltip: String,
        icon_color: Hsla,
        theme: Theme,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        toolbar_icon_button(id, icon)
            .text_color(icon_color)
            .hover(move |style| style.bg(theme.list_hover))
            .tooltip(tooltip)
            .on_click(cx.listener(move |this, _, window, cx| {
                handler(this, window, cx);
                cx.stop_propagation();
            }))
    }

    pub(super) fn render_message(
        &self,
        input: MessageRenderInput,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let MessageRenderInput {
            index: message_index,
            message,
            markdown,
            theme,
            panel_width,
            base_text_style,
            panel: entity,
            editing_focused,
        } = input;
        let is_user = message.role == Role::User;
        let content = message.display_text();
        let is_active_generation = self
            .active_generation
            .is_some_and(|target| target.assistant_index == message_index);
        let has_failure = self
            .generation_failure
            .as_ref()
            .is_some_and(|failure| failure.assistant_index == message_index);
        let is_loading = is_active_generation && content.is_empty();
        let editing_index = self
            .editing_turn
            .as_ref()
            .map(|editing| editing.message_index);
        let is_editing = editing_index == Some(message_index);
        let blocked = editing_index.is_some_and(|editing| message_index > editing);
        let can_mutate = !self.loading && editing_index.is_none();
        let has_later_messages = message_index + 1 < self.messages.len();
        let copied = self.copied_target == Some(CopyFeedbackTarget::Message(message_index));
        let row_width = (panel_width - 24.0).max(160.0);

        let message_entity = entity.clone();
        let message_content = MarkdownElement::new(
            markdown,
            MarkdownStyle::from_theme(theme, base_text_style).with_code_block_actions(!is_user),
        )
        .on_context_menu(move |request, window, cx| {
            message_entity.update(cx, |this, cx| {
                this.open_message_context_menu(message_index, request, window, cx);
            });
        })
        .into_any_element();

        let copy_icon = if copied {
            IconName::Check
        } else {
            IconName::Copy
        };
        let copy_tooltip = if copied {
            tr("ai-copied")
        } else if is_user {
            tr("ai-copy-message")
        } else {
            tr("ai-copy-response")
        };
        let copy_button = Self::message_action_button(
            ("ai-message-copy", message_index),
            copy_icon,
            copy_tooltip,
            if copied {
                theme.success
            } else {
                theme.muted_foreground
            },
            theme,
            move |this, _, cx| this.copy_message(message_index, cx),
            cx,
        );

        let actions = if is_editing {
            ButtonGroup::new(("ai-message-edit-actions", message_index))
                .outline()
                .compact()
                .small()
                .child(Self::message_action_button(
                    ("ai-message-cancel-edit", message_index),
                    IconName::X,
                    tr("cancel"),
                    theme.danger,
                    theme,
                    |this, window, cx| {
                        this.cancel_editing(window, cx);
                    },
                    cx,
                ))
                .child(Self::message_action_button(
                    ("ai-message-submit-edit", message_index),
                    IconName::Return,
                    tr("ai-submit-edit"),
                    theme.muted_foreground,
                    theme,
                    |this, window, cx| this.submit_edited_message(window, cx),
                    cx,
                ))
        } else {
            let mut actions = ButtonGroup::new(("ai-message-actions", message_index))
                .outline()
                .compact()
                .small()
                .child(copy_button);
            if is_user && can_mutate {
                actions = actions
                    .child(Self::message_action_button(
                        ("ai-message-edit", message_index),
                        IconName::Pencil,
                        tr("edit"),
                        theme.muted_foreground,
                        theme,
                        move |this, window, cx| {
                            this.start_editing_message(message_index, window, cx)
                        },
                        cx,
                    ))
                    .child(Self::message_action_button(
                        ("ai-message-regenerate", message_index),
                        IconName::RotateCcw,
                        if has_later_messages {
                            tr("ai-regenerate-from-message-warning")
                        } else {
                            tr("ai-regenerate-from-message")
                        },
                        theme.muted_foreground,
                        theme,
                        move |this, window, cx| {
                            this.regenerate_from_user(message_index, window, cx)
                        },
                        cx,
                    ));
            } else if !is_user && can_mutate {
                actions = actions.child(Self::message_action_button(
                    ("ai-message-retry", message_index),
                    IconName::RotateCcw,
                    if has_later_messages {
                        tr("ai-retry-response-warning")
                    } else {
                        tr("ai-retry-response")
                    },
                    theme.muted_foreground,
                    theme,
                    move |this, window, cx| this.retry_assistant(message_index, window, cx),
                    cx,
                ));
            }
            actions
        };

        div()
            .id(("ai-message-row", message_index))
            .group("ai-message-row")
            .relative()
            .w(px(row_width))
            .flex()
            .flex_col()
            .gap_1()
            .px_1()
            .when(is_user, |this| this.pt_2().pb_3())
            .when(!is_user, |this| this.py_1())
            .when(blocked, |this| this.opacity(0.45))
            .when(!is_editing && !blocked, |this| {
                this.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_message_context_menu(
                            message_index,
                            MarkdownContextMenuRequest {
                                position: event.position,
                                selected_text: None,
                                link_url: None,
                            },
                            window,
                            cx,
                        );
                    }),
                )
            })
            .child(
                div()
                    .w_full()
                    .text_size(ui_rems(13.0))
                    .text_color(theme.foreground)
                    .when(is_user, |this| {
                        this.relative()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(if is_editing { theme.ring } else { theme.border })
                            .when(is_editing && !editing_focused, |this| this.border_dashed())
                            .bg(theme.tab_active)
                            .px_2()
                            .py_3()
                            .pb_3()
                            .hover(move |style| style.border_color(theme.ring))
                    })
                    .when(!is_user, |this| this.px_1().pt_4().pb_2())
                    .when(is_editing, |this| {
                        this.child(
                            Input::new(&self.composer_input)
                                .appearance(false)
                                .bordered(false)
                                .focus_bordered(false)
                                .w_full(),
                        )
                    })
                    .when(!is_editing && is_loading, |this| {
                        this.child(Self::render_loading_indicator("ai-message-loading", theme))
                    })
                    .when(!is_editing && !is_loading && !content.is_empty(), |this| {
                        this.child(message_content)
                    }),
            )
            .child(
                div()
                    .absolute()
                    .top(if is_user { px(-6.0) } else { px(0.0) })
                    .right_3()
                    .when(!is_editing, |this| {
                        this.opacity(0.0)
                            .group_hover("ai-message-row", |style| style.opacity(1.0))
                    })
                    .when(is_editing && !editing_focused, |this| this.invisible())
                    .child(actions),
            )
            .when(has_failure, |this| {
                let failure = self.generation_failure.as_ref().unwrap().clone();
                let copied_error = self.copied_target == Some(CopyFeedbackTarget::Error);
                this.child(Self::render_generation_failure_card(
                    failure,
                    copied_error,
                    "ai-error",
                    theme,
                    cx,
                ))
            })
            .when(blocked, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .block_mouse_except_scroll(),
                )
            })
            .into_any_element()
    }

    fn render_generation_failure_card(
        failure: GenerationFailure,
        copied_error: bool,
        id_prefix: &'static str,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let assistant_index = failure.assistant_index;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                Alert::error(
                    format!("{id_prefix}-alert-{assistant_index}"),
                    failure.message,
                )
                .xsmall(),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .px_1()
                    .child(Self::message_action_button(
                        format!("{id_prefix}-copy-{assistant_index}"),
                        if copied_error {
                            IconName::Check
                        } else {
                            IconName::Copy
                        },
                        if copied_error {
                            tr("ai-copied")
                        } else {
                            tr("ai-copy-error")
                        },
                        if copied_error {
                            theme.success
                        } else {
                            theme.muted_foreground
                        },
                        theme,
                        |this, _, cx| this.copy_generation_error(cx),
                        cx,
                    ))
                    .child(Self::message_action_button(
                        format!("{id_prefix}-retry-{assistant_index}"),
                        IconName::RotateCcw,
                        tr("ai-retry-response"),
                        theme.muted_foreground,
                        theme,
                        move |this, window, cx| this.retry_assistant(assistant_index, window, cx),
                        cx,
                    ))
                    .child(Self::message_action_button(
                        format!("{id_prefix}-dismiss-{assistant_index}"),
                        IconName::X,
                        tr("ai-dismiss-error"),
                        theme.muted_foreground,
                        theme,
                        |this, _, cx| this.dismiss_generation_error(cx),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn render_loading_indicator(id_prefix: &'static str, theme: Theme) -> impl IntoElement {
        const PHASES: [f32; 3] = [0.0, 1.0 / 3.0, 2.0 / 3.0];

        let dot = move |index| {
            div()
                .w(px(4.0))
                .h(px(4.0))
                .rounded_full()
                .bg(theme.muted_foreground)
                .with_animation(
                    (id_prefix, index),
                    Animation::new(Duration::from_millis(900)).repeat(),
                    move |element, delta| {
                        element.opacity(loading_dot_opacity(delta, PHASES[index]))
                    },
                )
        };

        div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(18.0))
            .children((0..3).map(dot))
    }

    pub(super) fn render_turn_footer(
        &self,
        assistant_index: usize,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let copied = self.copied_target == Some(CopyFeedbackTarget::Message(assistant_index));
        div()
            .w_full()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .px_2()
            .py_1()
            .opacity(0.4)
            .hover(|style| style.opacity(1.0))
            .child(Self::message_action_button(
                "ai-footer-copy-response",
                if copied {
                    IconName::Check
                } else {
                    IconName::Copy
                },
                if copied {
                    tr("ai-copied")
                } else {
                    tr("ai-copy-response")
                },
                if copied {
                    theme.success
                } else {
                    theme.muted_foreground
                },
                theme,
                move |this, _, cx| this.copy_message(assistant_index, cx),
                cx,
            ))
            .child(Self::message_action_button(
                "ai-footer-latest-user",
                IconName::Return,
                tr("ai-scroll-latest-user"),
                theme.muted_foreground,
                theme,
                |this, _, cx| this.scroll_to_latest_user(cx),
                cx,
            ))
            .child(Self::message_action_button(
                "ai-footer-scroll-top",
                IconName::ArrowUp,
                tr("ai-scroll-top"),
                theme.muted_foreground,
                theme,
                |this, _, cx| this.scroll_to_top(cx),
                cx,
            ))
            .into_any_element()
    }
}
