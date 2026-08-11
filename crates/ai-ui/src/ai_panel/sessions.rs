use super::helpers::*;
use super::*;

impl AiPanel {
    pub(super) fn maybe_generate_title(&mut self, cx: &mut Context<Self>) {
        if self.mode == AiPanelMode::QuickAsk {
            return;
        }

        if self.pending_title_task.is_some() || !self.is_first_turn() {
            return;
        }

        let Some(settings) = cx.global::<SettingsStore>().settings.ai.resolved_title() else {
            return;
        };

        let session_id = self.session_id;
        let mut title_messages = self
            .messages
            .iter()
            .map(ChatMessage::title_safe)
            .collect::<Vec<_>>();
        title_messages.push(ChatMessage::user_text(TITLE_GENERATION_PROMPT.to_string()));
        let document = self.document_context(cx);

        let (tx, mut rx) = mpsc::unbounded::<AiStreamEvent>();

        thread::spawn(move || {
            let mut send_event = |event| tx.unbounded_send(event).map_err(|_| anyhow!("closed"));
            let result = stream_completion(settings, title_messages, document, &mut send_event);
            match result {
                Ok(()) => {
                    let _ = send_event(AiStreamEvent::Done);
                }
                Err(error) if error.to_string() != "closed" => {
                    let _ = send_event(AiStreamEvent::Error(format!("{error:#}")));
                }
                Err(_) => {}
            }
        });

        let mut title_response = String::new();
        self.pending_title_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                match event {
                    AiStreamEvent::Delta(delta) => title_response.push_str(&delta),
                    AiStreamEvent::Done => {
                        if let Some(title) = title_from_response(&title_response) {
                            let _ = this.update(cx, |this, cx| {
                                if this.session_id == session_id {
                                    this.session_title = title;
                                    this.save_current_session(cx);
                                    cx.notify();
                                }
                            });
                        }
                        break;
                    }
                    AiStreamEvent::Error(_) => break,
                }
            }

            let _ = this.update(cx, |this, _| {
                if this.session_id == session_id {
                    this.pending_title_task = None;
                }
            });
        }));
    }

    fn is_first_turn(&self) -> bool {
        self.messages.len() == 2
            && self.messages[0].role == Role::User
            && self.messages[1].role == Role::Assistant
    }
    pub fn save_current_session(&mut self, cx: &mut Context<Self>) {
        if self.mode == AiPanelMode::QuickAsk {
            return;
        }

        if self.messages.iter().all(ChatMessage::is_display_empty) {
            return;
        }

        let now = current_timestamp();
        if self.session_created == 0 {
            self.session_created = now;
        }
        self.session_updated = now;
        let failed_assistant = self
            .generation_failure
            .as_ref()
            .map(|failure| failure.assistant_index);

        let (messages, queued_messages) = {
            let library = cx.global_mut::<LibraryStore>();
            let messages = match persisted_session_messages_with_assets(
                &self.messages,
                failed_assistant,
                library,
            ) {
                Ok(messages) => messages,
                Err(error) => {
                    eprintln!("failed to persist AI session images: {error:#}");
                    return;
                }
            };
            let queued_messages = match persisted_session_messages_with_assets(
                &self.queued_messages,
                None,
                library,
            ) {
                Ok(messages) => messages,
                Err(error) => {
                    eprintln!("failed to persist queued AI images: {error:#}");
                    return;
                }
            };
            (messages, queued_messages)
        };
        let session = AiSession {
            id: self.session_id,
            path: self.document_path.clone(),
            title: self.session_title.clone(),
            page: self.current_page,
            messages,
            queued_messages,
            created: self.session_created,
            updated: self.session_updated,
        };

        if let Err(error) = cx.global_mut::<LibraryStore>().save_ai_session(session) {
            eprintln!("failed to save AI session: {error:#}");
        }
        self.refresh_sessions(cx);
    }

    fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        self.sessions = cx
            .global::<LibraryStore>()
            .ai_sessions_for(&self.document_path);
    }

    pub(super) fn show_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_editing(window, cx);
        self.update_current_page(cx);
        self.save_current_session(cx);
        self.refresh_sessions(cx);
        self.view = AiPanelView::Sessions;
        self.message_context_menu = None;
        self.close_composer_menus();
        cx.notify();
    }

    pub(super) fn show_chat(&mut self, cx: &mut Context<Self>) {
        self.view = AiPanelView::Chat;
        cx.notify();
    }
    pub(super) fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.cancel_editing(window, cx);
        self.update_current_page(cx);
        self.save_current_session(cx);
        self.session_id = cx.global_mut::<LibraryStore>().next_ai_session_id();
        self.session_title = DEFAULT_SESSION_TITLE.to_string();
        self.session_created = 0;
        self.session_updated = 0;
        self.invalidate_mention_preview();
        self.messages.clear();
        self.queued_messages.clear();
        self.composer_images.clear();
        self.tool_cards.clear();
        self.expanded_tool_cards.clear();
        self.markdown_views.clear();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.composer_mentions.clear();
        self.pending_title_task = None;
        self.generation_failure = None;
        self.active_generation = None;
        self.message_context_menu = None;
        self.clear_validation_notice();
        self.close_composer_menus();
        self.reset_message_scroll();
        self.view = AiPanelView::Chat;
        self.focus_handle.focus(window, cx);
        self.refresh_sessions(cx);
        cx.notify();
    }

    fn load_session(&mut self, session_id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.cancel_editing(window, cx);
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
        else {
            return;
        };

        self.update_current_page(cx);
        self.save_current_session(cx);
        self.session_id = session.id;
        self.session_title = session.title;
        self.session_created = session.created;
        self.session_updated = session.updated;
        self.current_page = session.page;
        self.tool_cards.clear();
        self.expanded_tool_cards.clear();
        let library = cx.global::<LibraryStore>();
        self.messages = session
            .messages
            .into_iter()
            .map(|message| chat_message_from_session(message, library))
            .collect();
        self.queued_messages = session
            .queued_messages
            .into_iter()
            .map(|message| chat_message_from_session(message, library))
            .collect();
        self.markdown_views = self
            .messages
            .iter()
            .map(|message| {
                let source = message.display_text();
                cx.new(|cx| {
                    Markdown::new_with_features(
                        source.into(),
                        MarkdownFeatures::embedded_content(),
                        cx,
                    )
                })
            })
            .collect();
        self.invalidate_mention_preview();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.composer_mentions.clear();
        self.composer_images.clear();
        self.pending_title_task = None;
        self.generation_failure = None;
        self.active_generation = None;
        self.message_context_menu = None;
        self.clear_validation_notice();
        self.close_composer_menus();
        self.reset_message_scroll();
        self.view = AiPanelView::Chat;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
    pub(super) fn render_sessions_view(
        sessions: Vec<AiSession>,
        current_session_id: u64,
        loading: bool,
        theme: Theme,
        scroll_handle: &ScrollHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let radius = cx.theme().radius;
        let mut list = div().flex().flex_col().gap_1().p_2();
        list = list.child(
            div()
                .id("ai-new-session")
                .flex()
                .items_center()
                .justify_between()
                .border_1()
                .border_color(theme.primary)
                .rounded(radius)
                .bg(theme.primary.opacity(0x18 as f32 / 255.0))
                .px_2()
                .py_2()
                .text_size(ui_rems(13.0))
                .text_color(if loading {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .cursor_pointer()
                .hover(move |style| style.bg(theme.primary.opacity(0x24 as f32 / 255.0)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.new_session(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .child(tr("ai-new-chat"))
                .child("+"),
        );

        if sessions.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(120.0))
                    .text_size(ui_rems(13.0))
                    .text_color(theme.muted_foreground)
                    .child(tr("ai-no-previous-sessions")),
            );
        }

        for session in sessions {
            let active = session.id == current_session_id;
            let session_id = session.id;
            let title = session.title.clone();
            let meta = format!("p. {} · {} messages", session.page, session.messages.len());
            list = list.child(
                div()
                    .id(format!("ai-session-{session_id}"))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_2()
                    .rounded(radius)
                    .bg(if active {
                        theme.list_active
                    } else {
                        transparent_black()
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.list_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.load_session(session_id, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(13.0))
                            .text_color(if active {
                                theme.primary
                            } else {
                                theme.foreground
                            })
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(theme.muted_foreground)
                            .child(meta),
                    ),
            );
        }

        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(
                div()
                    .id("ai-panel-sessions")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(scroll_handle)
                    .child(list),
            )
            .vertical_scrollbar_for_with_id(
                scroll_handle,
                "ai-panel-sessions-scrollbar",
                window,
                cx,
            )
    }
}
