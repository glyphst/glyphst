use super::helpers::{agent_image_preview, tool_cards_from_trace};
use super::*;

fn truncate_tool_summary(value: &str, limit: usize) -> String {
    let value = value.trim();
    if value.chars().count() <= limit {
        value.to_string()
    } else {
        format!("{}…", value.chars().take(limit).collect::<String>())
    }
}

fn humanize_tool_name(name: &str) -> String {
    let words = name.replace(['_', '-'], " ");
    let mut chars = words.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn tool_card_label(card: &AgentToolCard) -> String {
    let base = match card.name.as_str() {
        "search_document" => tr("ai-tool-search-document"),
        "read_document" => tr("ai-tool-read-document"),
        "render_document_pages" => tr("ai-tool-render-document-pages"),
        name => humanize_tool_name(name),
    };
    let arguments = serde_json::from_str::<serde_json::Value>(&card.arguments).ok();
    let summary = match card.name.as_str() {
        "search_document" => arguments
            .as_ref()
            .and_then(|arguments| arguments.get("query"))
            .and_then(serde_json::Value::as_str)
            .map(|query| format!("“{}”", truncate_tool_summary(query, 64))),
        "read_document" | "render_document_pages" => arguments
            .as_ref()
            .and_then(|arguments| arguments.get("pages"))
            .and_then(serde_json::Value::as_array)
            .map(|pages| {
                pages
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .map(|page| page.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|pages| !pages.is_empty())
            .or_else(|| {
                let arguments = arguments.as_ref()?;
                let start = arguments.get("start_page")?.as_u64()?;
                let end = arguments
                    .get("end_page")
                    .and_then(serde_json::Value::as_u64);
                Some(end.map_or_else(|| start.to_string(), |end| format!("{start}–{end}")))
            }),
        _ => None,
    };

    summary.map_or(base.clone(), |summary| format!("{base} · {summary}"))
}

fn tool_card_icon(name: &str) -> IconName {
    match name {
        "search_document" => IconName::Search,
        "read_document" => IconName::FileText,
        "render_document_pages" => IconName::Image,
        _ => IconName::Bot,
    }
}

fn pretty_tool_arguments(arguments: &str) -> String {
    let arguments = arguments.trim();
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|arguments| serde_json::to_string_pretty(&arguments).ok())
        .unwrap_or_else(|| arguments.to_string())
}

impl AiPanel {
    fn render_tool_cards(
        &self,
        cards: Vec<AgentToolCard>,
        message_index: usize,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cards = cards
            .into_iter()
            .enumerate()
            .map(|(card_index, card)| {
                let selector = format!("ai-tool-card-{message_index}-{card_index}");
                let detail_selector = format!("ai-tool-detail-{message_index}-{card_index}");
                let key = (message_index, card_index);
                let is_expanded = self.expanded_tool_cards.contains(&key);
                let (status, color) = match card.status {
                    AgentToolCardStatus::Requested => {
                        (tr("ai-tool-requested"), theme.muted_foreground)
                    }
                    AgentToolCardStatus::Running => (tr("ai-tool-running"), theme.primary),
                    AgentToolCardStatus::Completed => (tr("ai-tool-completed"), theme.success),
                    AgentToolCardStatus::Error => (tr("ai-tool-error"), theme.danger),
                    AgentToolCardStatus::Canceled => {
                        (tr("ai-tool-canceled"), theme.muted_foreground)
                    }
                };
                let icon = match card.status {
                    AgentToolCardStatus::Error => IconName::TriangleAlert,
                    AgentToolCardStatus::Canceled => IconName::X,
                    _ => tool_card_icon(&card.name),
                };
                let label = tool_card_label(&card);
                let result_text = card
                    .result
                    .iter()
                    .filter_map(|part| match part {
                        AgentToolResultPart::Text(text) => Some(text.as_str()),
                        AgentToolResultPart::Image(_) => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let images = card
                    .result
                    .iter()
                    .filter_map(|part| match part {
                        AgentToolResultPart::Image(image) => {
                            agent_image_preview(image).map(|image| {
                                div()
                                    .flex_none()
                                    .w(px(92.0))
                                    .h(px(72.0))
                                    .overflow_hidden()
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(img(image).size_full().object_fit(ObjectFit::Cover))
                            })
                        }
                        AgentToolResultPart::Text(_) => None,
                    })
                    .collect::<Vec<_>>();
                let arguments = pretty_tool_arguments(&card.arguments);
                let has_arguments = !arguments.is_empty() && arguments != "{}";
                let has_result = !result_text.is_empty() || !images.is_empty();
                let is_collapsible = has_arguments || has_result;
                div()
                    .id(selector.clone())
                    .debug_selector(move || selector)
                    .w_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1()
                            .min_h(ui_rems(22.0))
                            .rounded(px(4.0))
                            .px_1()
                            .py_0p5()
                            .hover(|style| style.bg(theme.muted.opacity(0.22)))
                            .when(is_collapsible, |this| {
                                this.cursor_pointer().on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        if !this.expanded_tool_cards.remove(&key) {
                                            this.expanded_tool_cards.insert(key);
                                        }
                                        cx.stop_propagation();
                                        cx.notify();
                                    }),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .min_w_0()
                                    .flex_1()
                                    .child(Icon::new(icon).xsmall().text_color(color))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ui_rems(11.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.muted_foreground)
                                            .child(label),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_size(ui_rems(9.0))
                                            .text_color(color)
                                            .child(status),
                                    )
                                    .when(is_collapsible, |this| {
                                        this.child(
                                            Icon::new(if is_expanded {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .xsmall()
                                            .text_color(theme.muted_foreground),
                                        )
                                    }),
                            ),
                    )
                    .when(is_expanded && is_collapsible, |this| {
                        this.child(
                            div()
                                .id(detail_selector.clone())
                                .debug_selector(move || detail_selector)
                                .ml(ui_rems(6.0))
                                .pl_3()
                                .pr_1()
                                .py_1()
                                .max_h(px(240.0))
                                .overflow_y_scroll()
                                .border_l(px(1.0))
                                .border_color(if card.status == AgentToolCardStatus::Error {
                                    theme.danger.opacity(0.5)
                                } else {
                                    theme.border
                                })
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .flex()
                                .flex_col()
                                .gap_2()
                                .when(has_arguments, |this| {
                                    this.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .text_size(ui_rems(9.0))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.muted_foreground)
                                                    .child(tr("ai-tool-input")),
                                            )
                                            .child(
                                                div()
                                                    .text_size(ui_rems(9.0))
                                                    .text_color(theme.muted_foreground)
                                                    .child(arguments),
                                            ),
                                    )
                                })
                                .when(has_result, |this| {
                                    this.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_size(ui_rems(9.0))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme.muted_foreground)
                                                    .child(tr("ai-tool-output")),
                                            )
                                            .when(!result_text.is_empty(), |this| {
                                                this.child(
                                                    div()
                                                        .text_size(ui_rems(10.0))
                                                        .text_color(theme.muted_foreground)
                                                        .child(result_text),
                                                )
                                            })
                                            .when(!images.is_empty(), |this| {
                                                this.child(
                                                    div()
                                                        .flex()
                                                        .flex_wrap()
                                                        .gap_1()
                                                        .children(images),
                                                )
                                            }),
                                    )
                                }),
                        )
                    })
            })
            .collect::<Vec<_>>();
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_0p5()
            .mb_1()
            .children(cards)
            .into_any_element()
    }

    pub(super) fn render_mention_preview(
        preview: MentionPreview,
        options: MentionPreviewRenderOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let MentionPreviewRenderOptions {
            width,
            image_height,
            scroll_handle,
            bounds_cell,
            theme,
            base_text_style,
        } = options;
        let generation = preview.generation;
        let preview_anchor = preview.anchor;
        let body = match preview.status {
            MentionPreviewStatus::Loading => div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(72.0))
                .p_3()
                .child(Self::render_loading_indicator(
                    "ai-mention-preview-loading",
                    theme,
                ))
                .into_any_element(),
            MentionPreviewStatus::Ready(MentionPreviewContent::Text {
                markdown,
                truncated,
            }) => div()
                .w_full()
                .min_w_0()
                .child(
                    div()
                        .id(("ai-mention-preview-text", generation))
                        .track_scroll(&scroll_handle)
                        .w_full()
                        .min_w_0()
                        .max_h(px(MENTION_PREVIEW_TEXT_MAX_HEIGHT))
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .p_3()
                        .cursor_text()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .child(MarkdownElement::new(
                            markdown,
                            MarkdownStyle::from_theme(theme, base_text_style),
                        ))
                        .when(truncated, |this| {
                            this.child(
                                div()
                                    .flex_none()
                                    .mt_3()
                                    .pt_2()
                                    .border_t(px(1.0))
                                    .border_color(theme.border)
                                    .text_size(ui_rems(10.0))
                                    .text_color(theme.muted_foreground)
                                    .child(tr_with_number(
                                        "ai-mention-preview-truncated",
                                        "limit",
                                        MENTION_PREVIEW_MAX_CHARS,
                                    )),
                            )
                        }),
                )
                .vertical_scrollbar_for_with_id(
                    &scroll_handle,
                    ("ai-mention-preview-scrollbar", generation),
                    window,
                    cx,
                )
                .into_any_element(),
            MentionPreviewStatus::Ready(MentionPreviewContent::Image(image)) => div()
                .w_full()
                .min_w_0()
                .h(px(image_height))
                .overflow_hidden()
                .bg(theme.tab_active)
                .child(img(image.image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            MentionPreviewStatus::Error(error) => div()
                .p_3()
                .text_size(ui_rems(12.0))
                .text_color(theme.danger)
                .child(error)
                .into_any_element(),
        };

        let card = div()
            .id(("ai-mention-preview", generation))
            .w_full()
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .overflow_hidden()
            .shadow_sm()
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(body);
        let measure = canvas(
            move |measured_bounds, _window, _cx| {
                bounds_cell.set(Some(measured_bounds));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
        .top_0()
        .left_0();

        div()
            .id(("ai-mention-preview-hover-region", generation))
            .relative()
            .occlude()
            .w(px(width))
            .when(preview_anchor == Anchor::TopLeft, |this| {
                this.pt(px(MENTION_PREVIEW_BRIDGE_HEIGHT))
            })
            .when(preview_anchor == Anchor::BottomLeft, |this| {
                this.pb(px(MENTION_PREVIEW_BRIDGE_HEIGHT))
            })
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if *hovered {
                    this.cancel_mention_preview_hide();
                } else {
                    this.schedule_mention_preview_hide(window, cx);
                }
            }))
            .child(measure)
            .child(card)
            .into_any_element()
    }

    pub(super) fn render_quick_ask(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let theme = cx.theme().colors;
        let (quick_ask, base_text_style, has_main_model) = {
            let store = cx.global::<SettingsStore>();
            let font = &store.settings.font;
            let base_text_style = TextStyle {
                font_family: font.ui_font_family.clone().into(),
                font_size: px(store.settings.ai.quick_ask.text_size).into(),
                color: theme.foreground,
                font_weight: FontWeight(font.ui_font_weight),
                ..Default::default()
            };
            (
                store.settings.ai.quick_ask,
                base_text_style,
                store.settings.ai.main_model.is_some(),
            )
        };
        let loading = self.loading;
        let can_send = self.generation_failure.is_none()
            && has_main_model
            && (!self.content.trim().is_empty() || !self.composer_images.is_empty());
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
        let answer_cards = answer_index
            .and_then(|index| self.messages.get(index).map(|message| (index, message)))
            .map(|(index, message)| {
                (
                    index,
                    self.tool_cards
                        .get(&index)
                        .cloned()
                        .unwrap_or_else(|| tool_cards_from_trace(message.agent_trace())),
                )
            });
        let has_answer_cards = answer_cards
            .as_ref()
            .is_some_and(|(_, cards)| !cards.is_empty());
        let show_answer =
            loading || !answer.is_empty() || has_answer_cards || generation_failure.is_some();
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
                        .id(("quick-ask-composer-image", index))
                        .relative()
                        .flex_none()
                        .w(px(52.0))
                        .h(px(52.0))
                        .overflow_hidden()
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.tab_active)
                        .child(img(preview).size_full().object_fit(ObjectFit::Cover))
                        .child(
                            toolbar_icon_button(
                                ("quick-ask-composer-image-remove", index),
                                IconName::X,
                            )
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
        let (has_current_selection, has_current_region, has_selected_pages) =
            self.current_mention_availability(cx);
        let page_count = self.page_count;
        let panel = entity.clone();
        let attachment_control = Button::new("quick-ask-attachment-menu-button")
            .small()
            .compact()
            .ghost()
            .icon(IconName::Plus)
            .w(ui_rems(28.0))
            .h(ui_rems(28.0))
            .debug_selector(|| "quick-ask-attachment-menu-button".to_string())
            .text_color(theme.muted_foreground)
            .tooltip(tr("attach"))
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu, _, _| {
                Self::build_attachment_menu(
                    menu,
                    page_count,
                    has_current_selection,
                    has_current_region,
                    has_selected_pages,
                    panel.clone(),
                )
            });
        let send_panel = entity.clone();
        let send_control = Button::new("quick-ask-send")
            .xsmall()
            .compact()
            .primary()
            .icon(IconName::ArrowUp)
            .disabled(!can_send)
            .tooltip(if loading {
                tr("ai-queue-message")
            } else {
                tr("send")
            })
            .on_click(move |_, window, cx| {
                send_panel.update(cx, |this, cx| {
                    if can_send {
                        this.send_message(window, cx);
                    }
                    cx.stop_propagation();
                });
            });
        let mention_preview = self.mention_preview.clone();
        let mention_preview_scroll = self.mention_preview_scroll.clone();
        let mention_preview_bounds = self.mention_preview_bounds.clone();
        let (mention_preview_width, mention_preview_image_height) = Self::mention_preview_size(
            mention_preview.as_ref(),
            self.width,
            window.viewport_size().width.as_f32(),
            window.viewport_size().height.as_f32(),
        );

        div()
            .key_context("QuickAsk")
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
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(Self::on_composer_paste))
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .when(!composer_image_previews.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_1()
                                .px_3()
                                .pt_2()
                                .children(composer_image_previews),
                        )
                    })
                    .child(
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
                    )
                    .when_some(validation_notice, |this, notice| {
                        this.child(
                            div()
                                .px_2()
                                .pb_1()
                                .child(Alert::error("quick-ask-validation-alert", notice).xsmall()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px_2()
                            .pb_1()
                            .child(attachment_control)
                            .child(send_control),
                    ),
            )
            .when(loading, |this| {
                this.child(
                    Self::message_action_button(
                        "quick-ask-stop",
                        IconName::Stop,
                        tr("ai-stop-generation"),
                        theme.danger,
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
                                .when_some(
                                    answer_cards.filter(|(_, cards)| !cards.is_empty()),
                                    |this, (index, cards)| {
                                        this.child(self.render_tool_cards(cards, index, theme, cx))
                                    },
                                )
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
    }
    pub(super) fn message_action_button(
        id: impl Into<ElementId>,
        icon: IconName,
        tooltip: String,
        icon_color: Hsla,
        handler: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Button {
        toolbar_icon_button(id, icon)
            .text_color(icon_color)
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
        let message_images = message
            .user_images()
            .enumerate()
            .filter_map(|(image_index, image)| {
                let preview = agent_image_preview(image)?;
                Some(
                    div()
                        .id(format!("ai-message-image-{message_index}-{image_index}"))
                        .flex_none()
                        .w(px(112.0))
                        .h(px(88.0))
                        .overflow_hidden()
                        .rounded(px(5.0))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.muted.opacity(0.2))
                        .child(img(preview).size_full().object_fit(ObjectFit::Cover)),
                )
            })
            .collect::<Vec<_>>();
        let cards = self
            .tool_cards
            .get(&message_index)
            .cloned()
            .unwrap_or_else(|| tool_cards_from_trace(message.agent_trace()));
        let tool_cards =
            (!cards.is_empty()).then(|| self.render_tool_cards(cards, message_index, theme, cx));
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
        let edit_panel = entity.clone();

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
                    move |this, window, cx| this.retry_assistant(message_index, window, cx),
                    cx,
                ));
            }
            actions
        };
        let message_content_selector = format!("ai-message-content-{message_index}");

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
            .when(is_editing, |this| {
                this.on_mouse_down_out(move |_, window, cx| {
                    edit_panel.update(cx, |this, cx| {
                        this.cancel_editing(window, cx);
                    });
                })
            })
            .when(is_user && can_mutate && !blocked, |this| {
                this.on_double_click(cx.listener(move |this, _, window, cx| {
                    this.start_editing_message(message_index, window, cx);
                    cx.stop_propagation();
                }))
            })
            .when_some(tool_cards, |this, cards| this.child(cards))
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(
                        div()
                            .debug_selector(move || message_content_selector)
                            .w_full()
                            .text_size(ui_rems(13.0))
                            .text_color(theme.foreground)
                            .when(is_user, |this| {
                                this.relative()
                                    .rounded(px(6.0))
                                    .border_1()
                                    .border_color(if is_editing {
                                        theme.ring
                                    } else {
                                        theme.border
                                    })
                                    .when(is_editing && !editing_focused, |this| {
                                        this.border_dashed()
                                    })
                                    .bg(theme.tab_active)
                                    .px_2()
                                    .py_3()
                                    .pb_3()
                                    .hover(move |style| style.border_color(theme.ring))
                            })
                            .when(!is_user, |this| this.px_1().pt_4().pb_2())
                            .when(!is_editing && !message_images.is_empty(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .gap_1()
                                        .mb_2()
                                        .children(message_images),
                                )
                            })
                            .when(is_editing, |this| {
                                this.child(
                                    Input::new(&self.edit_input)
                                        .appearance(false)
                                        .bordered(false)
                                        .focus_bordered(false)
                                        .w_full(),
                                )
                            })
                            .when(!is_editing && is_loading, |this| {
                                this.child(Self::render_loading_indicator(
                                    "ai-message-loading",
                                    theme,
                                ))
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
                    ),
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
                        |this, _, cx| this.copy_generation_error(cx),
                        cx,
                    ))
                    .child(Self::message_action_button(
                        format!("{id_prefix}-retry-{assistant_index}"),
                        IconName::RotateCcw,
                        tr("ai-retry-response"),
                        theme.muted_foreground,
                        move |this, window, cx| this.retry_assistant(assistant_index, window, cx),
                        cx,
                    ))
                    .child(Self::message_action_button(
                        format!("{id_prefix}-dismiss-{assistant_index}"),
                        IconName::X,
                        tr("ai-dismiss-error"),
                        theme.muted_foreground,
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
                move |this, _, cx| this.copy_message(assistant_index, cx),
                cx,
            ))
            .child(Self::message_action_button(
                "ai-footer-latest-user",
                IconName::Return,
                tr("ai-scroll-latest-user"),
                theme.muted_foreground,
                |this, _, cx| this.scroll_to_latest_user(cx),
                cx,
            ))
            .child(Self::message_action_button(
                "ai-footer-scroll-top",
                IconName::ArrowUp,
                tr("ai-scroll-top"),
                theme.muted_foreground,
                |this, _, cx| this.scroll_to_top(cx),
                cx,
            ))
            .into_any_element()
    }
}
