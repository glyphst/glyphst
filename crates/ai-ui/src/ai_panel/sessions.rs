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

        let session = AiSession {
            id: self.session_id,
            path: self.document_path.clone(),
            title: self.session_title.clone(),
            page: self.current_page,
            messages: persisted_session_messages(&self.messages, failed_assistant),
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
        self.hide_mention_preview(cx);
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
        self.messages.clear();
        self.markdown_views.clear();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.composer_mentions.clear();
        self.invalidate_mention_preview();
        self.active_page_number_edit = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.generation_failure = None;
        self.active_generation = None;
        self.message_context_menu = None;
        self.composer_history.clear();
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
        self.messages = session
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    AiSessionRole::User => Role::User,
                    AiSessionRole::Assistant => Role::Assistant,
                };
                let references = message
                    .references
                    .iter()
                    .filter_map(library_reference_to_ai)
                    .collect();
                let region_references = message
                    .references
                    .iter()
                    .filter_map(library_reference_to_region)
                    .collect();
                let occurrences = message
                    .occurrences
                    .iter()
                    .filter_map(library_occurrence_to_ai)
                    .collect();
                ChatMessage::from_persisted(role, message.content, message.request_content)
                    .with_references(references)
                    .with_region_references(region_references)
                    .with_mention_occurrences(occurrences)
            })
            .collect();
        self.markdown_views = self
            .messages
            .iter()
            .map(|message| {
                let source = message.display_text();
                cx.new(|cx| Markdown::new(source.into(), cx))
            })
            .collect();
        self.content = SharedString::default();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.mention_completion = None;
        self.composer_mentions.clear();
        self.invalidate_mention_preview();
        self.active_page_number_edit = None;
        self.last_cursor_pos = None;
        self.pending_title_task = None;
        self.generation_failure = None;
        self.active_generation = None;
        self.message_context_menu = None;
        self.composer_history.clear();
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
        let mut list = div().flex().flex_col().gap_1().p_2();
        list = list.child(
            div()
                .id("ai-new-session")
                .flex()
                .items_center()
                .justify_between()
                .border_1()
                .border_color(rgb(theme.accent))
                .bg(rgba(with_alpha(theme.accent, 0x18)))
                .px_2()
                .py_2()
                .text_size(ui_rems(13.0))
                .text_color(if loading {
                    rgb(theme.text_muted)
                } else {
                    rgb(theme.text)
                })
                .cursor_pointer()
                .hover(move |style| style.bg(rgba(with_alpha(theme.accent, 0x24))))
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
                    .text_color(rgb(theme.text_muted))
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
                    .bg(if active {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
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
                            .text_color(rgb(if active { theme.accent } else { theme.text }))
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(meta),
                    ),
            );
        }

        div()
            .id("ai-panel-sessions")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(scroll_handle)
            .child(list)
            .vertical_scrollbar_for(scroll_handle, window, cx)
    }
}
