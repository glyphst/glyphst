use super::helpers::*;
use super::*;

impl AiPanel {
    fn prepare_composer_message(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<PreparedComposerMessage> {
        let (display_prompt, submitted_mentions) =
            trimmed_composer_content(&self.content, &self.composer_mentions);
        if display_prompt.is_empty() && self.composer_images.is_empty() {
            return None;
        }

        let request_prompt = request_prompt_without_mentions(&display_prompt, &submitted_mentions);
        let references = references_for_composer_mentions(&submitted_mentions);
        let region_references = region_references_for_composer_mentions(&submitted_mentions);
        self.update_current_page(cx);
        let include_document_text = references
            .iter()
            .any(|reference| reference.kind == MentionKind::DocumentText);
        let document_scope = self.context_provider.document_scope(cx);
        let context = self.context_provider.context_for_scope(
            document_scope.as_ref(),
            include_document_text,
            cx,
        );
        let contexts = context_blocks_for_composer_mentions(&submitted_mentions, context.as_ref());
        let document = self.document_context_from(context);
        let mention_occurrences = submitted_mentions
            .iter()
            .map(|mention| MentionOccurrence {
                range: mention.range.clone(),
                reference: mention.reference,
                region_reference: match mention.captured.as_ref() {
                    Some(CapturedMention::Region(reference)) => Some(reference.clone()),
                    _ => None,
                },
                selected_pages_reference: match mention.captured.as_ref() {
                    Some(CapturedMention::Pages(reference)) => Some(reference.clone()),
                    _ => None,
                },
            })
            .collect();
        let mut user_message =
            ChatMessage::user(display_prompt.clone(), request_prompt, contexts, references)
                .with_region_references(region_references)
                .with_mention_occurrences(mention_occurrences)
                .with_document_scope(document_scope);
        for image in &self.composer_images {
            user_message.attach_user_image(image.clone());
        }

        Some(PreparedComposerMessage {
            display_prompt,
            submitted_mentions,
            user_message,
            document,
        })
    }

    pub(super) fn submit_edited_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let Some(editing) = self.editing_turn.clone() else {
            return;
        };
        let Some(PreparedComposerMessage {
            display_prompt,
            user_message,
            document,
            ..
        }) = self.prepare_composer_message(cx)
        else {
            return;
        };
        self.invalidate_mention_preview();
        let ai_settings = cx.global::<SettingsStore>().settings.ai.clone();
        if let Err(error) = validate_composer_page_image_preflight(
            self.mode,
            &self.messages[..editing.message_index],
            &user_message,
            self.page_count,
            ai_settings.page_image_limit,
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }
        if let Err(error) = validate_model_image_support(
            self.messages[..editing.message_index]
                .iter()
                .chain(std::iter::once(&user_message)),
            main_model_image_support(cx),
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }

        self.editing_turn = None;
        self.restore_composer_snapshot(editing.saved_composer, cx);
        self.pending_title_task = None;
        self.truncate_messages(editing.message_index);
        if editing.message_index == 0 {
            self.session_title = session_title_for_prompt(&display_prompt);
        }
        self.push_message(user_message, cx);
        self.push_message(ChatMessage::assistant(String::new()), cx);
        self.close_composer_menus();
        self.clear_validation_notice();
        self.focus_handle.focus(window, cx);
        self.start_generation(document, cx);
    }

    pub(super) fn regenerate_from_user(
        &mut self,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.editing_turn.is_some() {
            return;
        }
        if self
            .messages
            .get(message_index)
            .is_none_or(|message| message.role != Role::User)
        {
            return;
        }

        self.update_current_page(cx);
        let prefix_len = message_index + 1;
        let page_image_limit = cx.global::<SettingsStore>().settings.ai.page_image_limit;
        if let Err(error) = validate_page_image_preflight(
            self.messages[..prefix_len].iter(),
            self.page_count,
            page_image_limit,
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }
        if let Err(error) = validate_model_image_support(
            self.messages[..prefix_len].iter(),
            main_model_image_support(cx),
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }

        self.pending_title_task = None;
        self.truncate_messages(prefix_len);
        if message_index == 0 {
            self.session_title =
                session_title_for_prompt(&self.messages[message_index].display_text());
        }
        let document_scope = self.messages[message_index].document_scope().cloned();
        self.push_message(ChatMessage::assistant(String::new()), cx);
        let document = self.document_context_for_scope(document_scope.as_ref(), cx);
        self.start_generation(document, cx);
    }

    pub(super) fn retry_assistant(
        &mut self,
        assistant_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.editing_turn.is_some() {
            return;
        }
        let Some(user_index) = assistant_index.checked_sub(1) else {
            return;
        };
        if self
            .messages
            .get(assistant_index)
            .is_none_or(|message| message.role != Role::Assistant)
            || self
                .messages
                .get(user_index)
                .is_none_or(|message| message.role != Role::User)
        {
            return;
        }

        self.generation_failure = None;
        self.regenerate_from_user(user_index, window, cx);
    }

    pub(super) fn push_message(&mut self, message: ChatMessage, cx: &mut Context<Self>) {
        let source = message.display_text();
        self.messages.push(message);
        self.markdown_views.push(cx.new(|cx| {
            Markdown::new_with_features(source.into(), MarkdownFeatures::embedded_content(), cx)
        }));
    }

    fn truncate_messages(&mut self, len: usize) {
        self.messages.truncate(len);
        self.markdown_views.truncate(len);
        self.tool_cards.retain(|index, _| *index < len);
        self.expanded_tool_cards
            .retain(|(message_index, _)| *message_index < len);
        debug_assert_eq!(self.messages.len(), self.markdown_views.len());
    }

    fn sync_markdown(&mut self, index: usize, cx: &mut Context<Self>) {
        if let (Some(message), Some(markdown)) =
            (self.messages.get(index), self.markdown_views.get(index))
        {
            markdown.update(cx, |markdown, cx| {
                markdown.set_source(message.display_text().into(), cx)
            });
        }
    }

    fn start_generation(&mut self, document: DocumentContext, cx: &mut Context<Self>) {
        let Some(assistant_index) = self.messages.len().checked_sub(1) else {
            return;
        };
        if self
            .messages
            .get(assistant_index)
            .is_none_or(|message| message.role != Role::Assistant)
        {
            return;
        }

        self.stream_render_task = None;
        self.generation_id = self.generation_id.wrapping_add(1);
        if let Some(cancellation) = self.agent_cancellation.take() {
            cancellation.cancel();
        }
        let target = GenerationTarget {
            id: self.generation_id,
            assistant_index,
        };
        self.active_generation = Some(target);
        self.generation_failure = None;
        self.message_context_menu = None;
        self.loading = true;
        self.follow_output = true;
        let user_index = assistant_index.saturating_sub(1);
        self.scroll_handle.scroll_to_top_of_item(user_index);
        let scroll_epoch = self.scroll_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::ZERO).await;
            let _ = this.update(cx, |this, cx| {
                if this.scroll_epoch == scroll_epoch && this.follow_output {
                    this.scroll_handle.scroll_to_top_of_item(user_index);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();

        let ai_settings = cx.global::<SettingsStore>().settings.ai.clone();
        let settings = ai_settings.resolved_main();
        let tool_support = ai_settings
            .provider_config(settings.provider)
            .model(&settings.model)
            .map(|model| model.tool_use_support(settings.provider))
            .unwrap_or_default();
        let tools = tool_support
            .is_supported()
            .then(|| self.context_provider.agent_tools(cx))
            .flatten();
        let cancellation = AgentCancellationToken::default();
        self.agent_cancellation = Some(cancellation.clone());
        self.tool_cards.remove(&assistant_index);
        let mut request_messages = self.messages[..assistant_index].to_vec();
        let resolution_tasks = request_messages
            .iter()
            .enumerate()
            .map(|(message_index, message)| {
                let scope = message.document_scope().cloned();
                let page_count = self
                    .context_provider
                    .page_state_for_scope(scope.as_ref(), cx)
                    .map_or(0, |state| state.page_count);
                let page_references =
                    unresolved_page_references(std::slice::from_ref(message), page_count);
                let region_references = unresolved_region_references(std::slice::from_ref(message));
                let page_task = self.context_provider.resolve_page_references_for_scope(
                    scope.as_ref(),
                    page_references,
                    cx,
                );
                let region_task = self.context_provider.resolve_region_references_for_scope(
                    scope.as_ref(),
                    region_references,
                    cx,
                );
                (message_index, page_task, region_task)
            })
            .collect::<Vec<_>>();

        self.pending_stream = Some(cx.spawn(async move |this, cx| {
            for (message_index, page_task, region_task) in resolution_tasks {
                let resolved_pages = match page_task.await {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            this.apply_generation_event(
                                target,
                                GenerationEvent::Error(format!("{error:#}")),
                                cx,
                            );
                            cx.notify();
                        });
                        return;
                    }
                };
                let resolved_regions = match region_task.await {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        let _ = this.update(cx, |this, cx| {
                            this.apply_generation_event(
                                target,
                                GenerationEvent::Error(format!("{error:#}")),
                                cx,
                            );
                            cx.notify();
                        });
                        return;
                    }
                };
                let message = std::slice::from_mut(&mut request_messages[message_index]);
                if let Err(error) =
                    validate_resolved_image_contexts(message, &resolved_pages, &resolved_regions)
                {
                    let _ = this.update(cx, |this, cx| {
                        this.apply_generation_event(
                            target,
                            GenerationEvent::Error(error.to_string()),
                            cx,
                        );
                        cx.notify();
                    });
                    return;
                }
                hydrate_page_contexts(message, &resolved_pages);
                hydrate_region_contexts(message, &resolved_regions);
                if this
                    .update(cx, |this, cx| {
                        if this.active_generation != Some(target)
                            || message_index >= this.messages.len()
                        {
                            return;
                        }
                        let message = std::slice::from_mut(&mut this.messages[message_index]);
                        hydrate_page_contexts(message, &resolved_pages);
                        hydrate_region_contexts(message, &resolved_regions);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            if let Err(error) = validate_resolved_image_contexts(&request_messages, &[], &[]) {
                let _ = this.update(cx, |this, cx| {
                    this.apply_generation_event(
                        target,
                        GenerationEvent::Error(error.to_string()),
                        cx,
                    );
                    cx.notify();
                });
                return;
            }

            let agent_messages = request_messages
                .iter()
                .flat_map(ChatMessage::into_agent_messages)
                .collect::<Vec<_>>();
            let (tx, mut rx) = mpsc::unbounded::<GenerationEvent>();
            thread::spawn(move || {
                let mut send_event = |event| {
                    tx.unbounded_send(GenerationEvent::Agent(event))
                        .map_err(|_| anyhow!("closed"))
                };
                let result = run_agent(
                    settings,
                    agent_messages,
                    document,
                    tools,
                    cancellation,
                    AgentLimits::default(),
                    &mut send_event,
                );
                match result {
                    Ok(result) => {
                        let _ = tx.unbounded_send(GenerationEvent::Finished {
                            trace: result.appended_messages,
                            limit_reached: result.limit_reached,
                        });
                    }
                    Err(error) if error.to_string() != "closed" => {
                        let _ = tx.unbounded_send(GenerationEvent::Error(format!("{error:#}")));
                    }
                    Err(_) => {}
                }
            });

            while let Some(event) = rx.next().await {
                if this
                    .update(cx, |this, cx| {
                        this.apply_generation_event(target, event, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    pub(super) fn send_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_turn.is_some() || self.generation_failure.is_some() {
            return;
        }

        let credentials_loading = {
            let store = cx.global::<SettingsStore>();
            let provider = store.settings.ai.resolved_main().provider;
            store.ai_provider_credentials_loading(provider)
        };
        if credentials_loading {
            self.show_validation_notice("Credentials are still loading.".to_string(), window, cx);
            return;
        }

        let Some(PreparedComposerMessage {
            display_prompt,
            submitted_mentions,
            user_message,
            document,
        }) = self.prepare_composer_message(cx)
        else {
            return;
        };
        let mut validation_history = self.messages.clone();
        validation_history.extend(self.queued_messages.iter().cloned());
        let ai_settings = cx.global::<SettingsStore>().settings.ai.clone();
        if let Err(error) = validate_composer_page_image_preflight(
            self.mode,
            &validation_history,
            &user_message,
            self.page_count,
            ai_settings.page_image_limit,
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }
        if let Err(error) = validate_model_image_support(
            validation_history
                .iter()
                .chain(std::iter::once(&user_message)),
            main_model_image_support(cx),
        ) {
            self.show_validation_notice(error.to_string(), window, cx);
            return;
        }

        if self.loading {
            self.content = SharedString::default();
            self.selected_range = 0..0;
            self.selection_reversed = false;
            self.marked_range = None;
            self.composer_mentions.clear();
            self.composer_images.clear();
            self.close_composer_menus();
            self.clear_validation_notice();
            self.queued_messages.push(user_message);
            if self.mode == AiPanelMode::Sidebar {
                self.save_current_session(cx);
            }
            cx.notify();
            return;
        }

        if self.mode == AiPanelMode::QuickAsk {
            self.messages.clear();
            self.markdown_views.clear();
            self.tool_cards.clear();
        } else if self.messages.is_empty() {
            self.session_title = session_title_for_prompt(&display_prompt);
        }
        if self.mode == AiPanelMode::Sidebar {
            self.content = SharedString::default();
            self.selected_range = 0..0;
            self.composer_mentions.clear();
            self.composer_images.clear();
        } else {
            self.content = display_prompt.clone().into();
            self.selected_range = display_prompt.len()..display_prompt.len();
            self.composer_mentions = submitted_mentions;
        }
        self.selection_reversed = false;
        self.marked_range = None;
        self.close_composer_menus();
        self.clear_validation_notice();
        self.push_message(user_message, cx);
        self.push_message(ChatMessage::assistant(String::new()), cx);
        self.start_generation(document, cx);
    }

    fn schedule_generation_render(&mut self, target: GenerationTarget, cx: &mut Context<Self>) {
        if self.stream_render_task.is_some() {
            return;
        }
        self.stream_render_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::ZERO).await;
            let _ = this.update(cx, |this, cx| {
                this.stream_render_task = None;
                if this.active_generation == Some(target) {
                    this.sync_generation_render(target, cx);
                    cx.notify();
                }
            });
        }));
    }

    fn sync_generation_render(&mut self, target: GenerationTarget, cx: &mut Context<Self>) {
        self.sync_markdown(target.assistant_index, cx);
        if self.follow_output && !self.is_near_message_bottom() {
            self.follow_output = false;
        }
        if self.follow_output {
            self.scroll_handle.scroll_to_bottom();
        }
    }

    pub(super) fn apply_generation_event(
        &mut self,
        target: GenerationTarget,
        event: GenerationEvent,
        cx: &mut Context<Self>,
    ) {
        if self.active_generation != Some(target) {
            return;
        }

        match event {
            GenerationEvent::Agent(AgentEvent::TextDelta(delta)) => {
                if let Some(message) = self.messages.get_mut(target.assistant_index) {
                    message.push_text(&delta);
                }
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(AgentEvent::ToolCallStarted { id, name }) => {
                self.tool_cards
                    .entry(target.assistant_index)
                    .or_default()
                    .push(AgentToolCard {
                        id,
                        name,
                        arguments: String::new(),
                        result: Vec::new(),
                        status: AgentToolCardStatus::Requested,
                    });
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(AgentEvent::ToolCallArgumentsDelta { id, delta }) => {
                if let Some(card) = self
                    .tool_cards
                    .entry(target.assistant_index)
                    .or_default()
                    .iter_mut()
                    .rev()
                    .find(|card| card.id == id || card.id.is_empty())
                {
                    if card.id.is_empty() {
                        card.id = id;
                    }
                    card.arguments.push_str(&delta);
                }
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(AgentEvent::ToolCallCompleted(call)) => {
                let cards = self.tool_cards.entry(target.assistant_index).or_default();
                if let Some(card) = cards
                    .iter_mut()
                    .rev()
                    .find(|card| card.id == call.id || card.name == call.name)
                {
                    card.id = call.id;
                    card.name = call.name;
                    card.arguments = call.arguments;
                } else {
                    cards.push(AgentToolCard {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                        result: Vec::new(),
                        status: AgentToolCardStatus::Requested,
                    });
                }
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(AgentEvent::ToolExecutionStarted { id, name }) => {
                let cards = self.tool_cards.entry(target.assistant_index).or_default();
                if let Some(card) = cards.iter_mut().rev().find(|card| card.id == id) {
                    card.status = AgentToolCardStatus::Running;
                } else {
                    cards.push(AgentToolCard {
                        id,
                        name,
                        arguments: String::new(),
                        result: Vec::new(),
                        status: AgentToolCardStatus::Running,
                    });
                }
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(AgentEvent::ToolExecutionCompleted(result)) => {
                let cards = self.tool_cards.entry(target.assistant_index).or_default();
                if let Some(card) = cards
                    .iter_mut()
                    .rev()
                    .find(|card| card.id == result.call_id)
                {
                    card.result = result.content;
                    card.status = if result.is_error {
                        AgentToolCardStatus::Error
                    } else {
                        AgentToolCardStatus::Completed
                    };
                }
                self.schedule_generation_render(target, cx);
            }
            GenerationEvent::Agent(
                AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnCompleted { .. }
                | AgentEvent::LimitReached
                | AgentEvent::Done,
            ) => {}
            GenerationEvent::Error(error) => {
                if let Some(cards) = self.tool_cards.get_mut(&target.assistant_index) {
                    for card in cards.iter_mut().filter(|card| {
                        matches!(
                            card.status,
                            AgentToolCardStatus::Requested | AgentToolCardStatus::Running
                        )
                    }) {
                        card.status = AgentToolCardStatus::Error;
                    }
                }
                self.stream_render_task = None;
                self.sync_generation_render(target, cx);
                self.loading = false;
                self.active_generation = None;
                self.agent_cancellation = None;
                self.generation_failure = Some(GenerationFailure {
                    assistant_index: target.assistant_index,
                    message: error,
                });
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.save_current_session(cx);
                }
                cx.notify();
            }
            GenerationEvent::Finished {
                trace,
                limit_reached,
            } => {
                if let Some(message) = self.messages.get_mut(target.assistant_index) {
                    message.set_agent_trace(trace);
                    if limit_reached {
                        message.push_text(&format!("\n\n{}", tr("ai-tool-limit-reached")));
                    }
                }
                self.stream_render_task = None;
                self.sync_generation_render(target, cx);
                self.loading = false;
                self.active_generation = None;
                self.agent_cancellation = None;
                self.generation_failure = None;
                self.pending_stream = None;
                if self.mode == AiPanelMode::Sidebar {
                    self.maybe_generate_title(cx);
                    self.save_current_session(cx);
                }
                self.start_next_queued_turn(cx);
                cx.notify();
            }
        }
    }

    fn start_next_queued_turn(&mut self, cx: &mut Context<Self>) -> bool {
        if self.loading || self.queued_messages.is_empty() {
            return false;
        }
        let user_message = self.queued_messages.remove(0);
        let document_scope = user_message.document_scope().cloned();
        self.push_message(user_message, cx);
        self.push_message(ChatMessage::assistant(String::new()), cx);
        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        let document = self.document_context_for_scope(document_scope.as_ref(), cx);
        self.start_generation(document, cx);
        true
    }

    pub(super) fn remove_queued_message(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.queued_messages.len() {
            return;
        }
        self.queued_messages.remove(index);
        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        cx.notify();
    }

    pub(super) fn stop_generation(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.active_generation.take() else {
            return false;
        };

        self.generation_id = self.generation_id.wrapping_add(1);
        if let Some(cancellation) = self.agent_cancellation.take() {
            cancellation.cancel();
        }
        self.pending_stream = None;
        self.stream_render_task = None;
        self.loading = false;
        self.generation_failure = None;
        if let Some(cards) = self.tool_cards.get_mut(&target.assistant_index) {
            for card in cards.iter_mut().filter(|card| {
                matches!(
                    card.status,
                    AgentToolCardStatus::Requested | AgentToolCardStatus::Running
                )
            }) {
                card.status = AgentToolCardStatus::Canceled;
            }
        }

        if self
            .messages
            .get(target.assistant_index)
            .is_some_and(ChatMessage::is_display_empty)
        {
            self.truncate_messages(target.assistant_index);
        } else {
            self.sync_markdown(target.assistant_index, cx);
        }

        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        cx.notify();
        true
    }

    fn show_copy_feedback(&mut self, target: CopyFeedbackTarget, cx: &mut Context<Self>) {
        self.copy_feedback_generation = self.copy_feedback_generation.wrapping_add(1);
        let generation = self.copy_feedback_generation;
        self.copied_target = Some(target);
        self.copy_feedback_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(glyphst_ui::TRANSIENT_FEEDBACK_DURATION)
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.copy_feedback_generation == generation {
                    this.copied_target = None;
                    this.copy_feedback_task = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    pub(super) fn copy_message(&mut self, message_index: usize, cx: &mut Context<Self>) {
        let Some(message) = self.messages.get(message_index) else {
            return;
        };
        let text = message.display_text();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_copy_feedback(CopyFeedbackTarget::Message(message_index), cx);
    }

    pub(super) fn copy_generation_error(&mut self, cx: &mut Context<Self>) {
        let Some(failure) = self.generation_failure.as_ref() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(failure.message.clone()));
        self.show_copy_feedback(CopyFeedbackTarget::Error, cx);
    }

    pub(super) fn dismiss_generation_error(&mut self, cx: &mut Context<Self>) {
        let Some(failure) = self.generation_failure.take() else {
            return;
        };
        self.truncate_messages(failure.assistant_index);
        if self.mode == AiPanelMode::Sidebar {
            self.save_current_session(cx);
        }
        cx.notify();
    }
    pub(super) fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.follow_output = false;
        self.scroll_handle.scroll_to_top_of_item(0);
        self.close_message_context_menu(cx);
        cx.notify();
    }

    pub(super) fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.follow_output = true;
        self.scroll_handle.scroll_to_bottom();
        self.close_message_context_menu(cx);
        cx.notify();
    }

    pub(super) fn scroll_to_latest_user(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self
            .messages
            .iter()
            .rposition(|message| message.role == Role::User)
        else {
            return;
        };
        self.follow_output = false;
        self.scroll_handle.scroll_to_top_of_item(index);
        self.close_message_context_menu(cx);
        cx.notify();
    }

    pub(super) fn reset_message_scroll(&mut self) {
        self.scroll_epoch = self.scroll_epoch.wrapping_add(1);
        self.scroll_handle = ScrollHandle::new();
        self.follow_output = true;
        self.message_scroll_task = None;
    }

    pub(super) fn is_near_message_bottom(&self) -> bool {
        message_scroll_distance_from_bottom(
            self.scroll_handle.offset().y.as_f32(),
            self.scroll_handle.max_offset().y.as_f32(),
        ) <= 24.0
    }

    pub(super) fn handle_message_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) {
        let delta_y = match event.delta {
            ScrollDelta::Pixels(delta) => delta.y.as_f32(),
            ScrollDelta::Lines(delta) => delta.y,
        };
        if delta_y > 0.0 {
            self.follow_output = false;
        }
        let scroll_epoch = self.scroll_epoch;
        self.close_message_context_menu(cx);
        self.message_scroll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::ZERO).await;
            let _ = this.update(cx, |this, cx| {
                this.message_scroll_task = None;
                if this.scroll_epoch != scroll_epoch {
                    return;
                }
                let follow_output = this.is_near_message_bottom();
                if this.follow_output != follow_output {
                    this.follow_output = follow_output;
                    cx.notify();
                }
            });
        }));
    }
    pub(super) fn document_context(&self, cx: &mut Context<Self>) -> DocumentContext {
        self.document_context_for_scope(None, cx)
    }

    fn document_context_for_scope(
        &self,
        scope: Option<&DocumentContextScope>,
        cx: &mut Context<Self>,
    ) -> DocumentContext {
        self.document_context_from(self.context_provider.context_for_scope(scope, false, cx))
    }

    fn document_context_from(&self, context: Option<AiContext>) -> DocumentContext {
        let mut document = DocumentContext {
            file_name: None,
            format: None,
            location_label: None,
            page_count: 0,
            current_page: self.current_page,
            has_selection: false,
        };

        if let Some(context) = context {
            document.file_name = context.document_file_name.or(document.file_name);
            document.format = context.document_format;
            document.location_label = context.location_label;
            document.page_count = context.page_count;
            document.current_page = context.current_page;
            document.has_selection = context.selected_text.is_some();
        }

        document
    }

    pub(super) fn update_current_page(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.context_provider.page_state(cx) {
            self.current_page = state.current_page;
            self.page_count = state.page_count;
        }
    }

    pub(super) fn current_selection_snapshot(&self, cx: &App) -> Option<String> {
        self.context_provider
            .context(false, cx)
            .and_then(|context| context.selected_text)
            .filter(|text| !text.trim().is_empty())
    }
    pub(super) fn clear_validation_notice(&mut self) {
        self.validation_notice = None;
    }

    pub(super) fn show_validation_notice(
        &mut self,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.validation_notice = Some(message.clone());
        if let Some(center) = ActivityCenter::try_global(cx) {
            center.update(cx, |center, cx| {
                center.add_notice(
                    ActivityNoticeEntry::new(
                        ActivityNoticeLevel::Error,
                        "AI request could not be sent",
                        message.clone(),
                    ),
                    cx,
                );
            });
        }
        window.push_notification(
            Notification::error(message).id::<AiValidationNotification>(),
            cx,
        );
        cx.notify();
    }
}
