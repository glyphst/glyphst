use super::helpers::*;
use super::*;

impl AiPanel {
    pub(super) fn index_for_mouse_position(
        &self,
        position: GpuiPoint<Pixels>,
        window: &mut Window,
    ) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        source_index_for_mouse_position(
            self.content.len(),
            bounds,
            &self.last_lines,
            self.last_display.as_ref(),
            position,
            window.line_height(),
        )
    }

    pub(super) fn position_for_offset(
        &self,
        offset: usize,
        window: &mut Window,
    ) -> Option<GpuiPoint<Pixels>> {
        let offset = self
            .last_display
            .as_ref()
            .map(|display| display.display_offset(offset))
            .unwrap_or(offset);
        let line_height = window.line_height();
        let mut line_top = px(0.0);
        for laid_out in &self.last_lines {
            let line_end = laid_out.start + laid_out.line.len();
            if offset <= line_end {
                let local_offset = offset.saturating_sub(laid_out.start);
                return laid_out
                    .line
                    .position_for_index(local_offset, line_height)
                    .map(|position| point(position.x, line_top + position.y));
            }
            line_top += laid_out.line.size(line_height).height;
        }

        self.last_lines.last().and_then(|line| {
            line.line
                .position_for_index(line.line.len(), line_height)
                .map(|position| point(position.x, line_top + position.y))
        })
    }

    pub(super) fn offset_to_utf16(&self, offset: usize) -> usize {
        offset_to_utf16(&self.content, offset)
    }

    pub(super) fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        range_to_utf16(&self.content, range)
    }

    pub(super) fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        range_from_utf16(&self.content, range_utf16)
    }
}

impl EntityInputHandler for AiPanel {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
        self.composer_history.break_group();
        self.model_search_history.break_group();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_menu_open {
            if !new_text.is_empty() {
                let end = self.model_search_query.len();
                self.edit_model_search_query(end..end, new_text, EditGroup::Typing, cx);
            }
            return;
        }
        if self.active_page_number_edit.is_some()
            && !new_text.is_empty()
            && !new_text.bytes().any(|byte| byte.is_ascii_digit())
        {
            return;
        }

        let had_marked_range = self.marked_range.is_some();
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = self.constrain_deletion_range(range);

        let before = self.composer_snapshot();
        if !self.apply_page_number_edit(range.clone(), new_text) {
            self.apply_regular_edit(range, new_text);
        }
        self.selection_reversed = false;
        self.marked_range.take();
        self.finish_composer_edit(
            before,
            if had_marked_range {
                EditGroup::Composition
            } else {
                EditGroup::Typing
            },
            cx,
        );
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.model_menu_open {
            if !new_text.is_empty() {
                let end = self.model_search_query.len();
                self.edit_model_search_query(end..end, new_text, EditGroup::Composition, cx);
            }
            return;
        }
        if self.active_page_number_edit.is_some()
            && !new_text.is_empty()
            && !new_text.bytes().any(|byte| byte.is_ascii_digit())
        {
            return;
        }

        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let range = self.constrain_deletion_range(range);
        let selected_range_start = range.start;
        let before = self.composer_snapshot();

        let inserted_len = if self.active_page_number_edit.is_some() {
            new_text.bytes().filter(u8::is_ascii_digit).count()
        } else {
            new_text.len()
        };
        if !self.apply_page_number_edit(range.clone(), new_text) {
            self.apply_regular_edit(range.clone(), new_text);
        }
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + inserted_len);
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| {
                let start = offset_from_utf16(new_text, range_utf16.start);
                let end = offset_from_utf16(new_text, range_utf16.end);
                selected_range_start + start..selected_range_start + end
            })
            .unwrap_or_else(|| range.start + inserted_len..range.start + inserted_len);
        self.selection_reversed = false;
        self.finish_composer_edit(before, EditGroup::Composition, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let start = self.position_for_offset(range.start, window)?;
        let end = self.position_for_offset(range.end, window)?;
        Some(Bounds::from_corners(
            point(bounds.left() + start.x, bounds.top() + start.y),
            point(
                bounds.left() + end.x,
                bounds.top() + end.y + window.line_height(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: GpuiPoint<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(point, window)))
    }
}

impl IntoElement for AiInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AiInputElement {
    type RequestLayoutState = AiInputLayoutState;
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.panel.read(cx);
        let (content, mentions, marked_range) = match &self.source {
            AiInputSource::Composer | AiInputSource::EditedMessage => (
                input.content.clone(),
                input.composer_mentions.clone(),
                input.marked_range.clone(),
            ),
            AiInputSource::UserMessage {
                content, mentions, ..
            } => (content.clone(), mentions.clone(), None),
        };
        let (empty_chat, min_lines, max_lines) =
            input_line_constraints(input.mode, &self.source, input.messages.is_empty());
        let fallback_width = px(input.width.max(1.0));
        let display = composer_display(content.as_ref(), &mentions);

        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        let measurement = Rc::new(RefCell::new(None));
        if empty_chat {
            style.size.height = relative(1.0).into();
            return (
                window.request_layout(style, [], cx),
                AiInputLayoutState {
                    display,
                    measurement,
                },
            );
        }

        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let display_text: SharedString = display.text.clone().into();
        let base_run = TextRun {
            len: display_text.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = input_text_runs(
            display_text.as_ref(),
            base_run,
            theme,
            &display.mentions,
            marked_range
                .as_ref()
                .map(|range| display.display_range(range))
                .as_ref(),
        );
        let layout_id = window.request_measured_layout(style, {
            let measurement = measurement.clone();
            move |known, available, window, _| {
                let width = known.width.unwrap_or(match available.width {
                    AvailableSpace::Definite(width) => width,
                    AvailableSpace::MinContent | AvailableSpace::MaxContent => fallback_width,
                });
                let lines = if display_text.is_empty() {
                    Vec::new()
                } else {
                    window
                        .text_system()
                        .shape_text(display_text.clone(), font_size, &runs, Some(width), None)
                        .map(|lines| lines.into_vec())
                        .unwrap_or_else(|error| {
                            eprintln!("failed to measure AI input: {error:#}");
                            Vec::new()
                        })
                };
                let measured_height = lines.iter().map(|line| line.size(line_height).height).sum();
                let height =
                    constrained_input_height(measured_height, line_height, min_lines, max_lines);
                *measurement.borrow_mut() = Some(AiInputMeasurement { width, lines });
                size(width, height)
            }
        });
        (
            layout_id,
            AiInputLayoutState {
                display,
                measurement,
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.panel.read(cx);
        let (
            content,
            selected_range,
            cursor,
            composer_mentions,
            active_page_number_edit,
            marked_range,
            editing,
        ) = match &self.source {
            AiInputSource::Composer | AiInputSource::EditedMessage => (
                input.content.clone(),
                input.selected_range.clone(),
                input.cursor_offset(),
                input.composer_mentions.clone(),
                input.active_page_number_edit.clone(),
                input.marked_range.clone(),
                true,
            ),
            AiInputSource::UserMessage {
                content, mentions, ..
            } => (
                content.clone(),
                0..0,
                0,
                mentions.clone(),
                None,
                None,
                false,
            ),
        };
        let style = window.text_style();
        let theme = cx.global::<SettingsStore>().resolved_theme;
        let (display, display_text, text_color) = if content.is_empty() {
            let placeholder = if editing && input.editing_turn.is_some() {
                tr("ai-edit-message-placeholder")
            } else {
                tr("ai-ask-placeholder")
            };
            (
                ComposerDisplay {
                    text: placeholder.clone(),
                    source_to_display: vec![0],
                    display_to_source: vec![0; placeholder.len() + 1],
                    mentions: Vec::new(),
                },
                placeholder.into(),
                rgba(theme.placeholder_text).into(),
            )
        } else {
            let display = std::mem::take(&mut layout_state.display);
            let display_text: SharedString = display.text.clone().into();
            (display, display_text, style.color)
        };

        let measured_lines = (!content.is_empty())
            .then(|| layout_state.measurement.borrow_mut().take())
            .flatten()
            .and_then(|measurement| {
                (measurement.width == bounds.size.width).then_some(measurement.lines)
            });
        let lines = if let Some(lines) = measured_lines {
            lines
        } else {
            let base_run = TextRun {
                len: display_text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if !content.is_empty() {
                input_text_runs(
                    display_text.as_ref(),
                    base_run,
                    theme,
                    &display.mentions,
                    marked_range
                        .as_ref()
                        .map(|range| display.display_range(range))
                        .as_ref(),
                )
            } else {
                vec![base_run]
            };
            let font_size = style.font_size.to_pixels(window.rem_size());
            match window.text_system().shape_text(
                display_text.clone(),
                font_size,
                &runs,
                Some(bounds.size.width),
                None,
            ) {
                Ok(lines) => lines.into_vec(),
                Err(error) => {
                    eprintln!("failed to shape AI input: {error:#}");
                    return PrepaintState {
                        lines: Some(Vec::new()),
                        display,
                        cursor: None,
                        cursor_position: None,
                        selections: Vec::new(),
                        mention_quads: Vec::new(),
                        active_number_quads: Vec::new(),
                        mention_icons: Vec::new(),
                        mention_hitboxes: Vec::new(),
                        hitbox: matches!(&self.source, AiInputSource::UserMessage { .. })
                            .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal)),
                    };
                }
            }
        };
        let line_height = window.line_height();

        let mut laid_out_lines = Vec::with_capacity(lines.len());
        let mut starts = line_starts(display_text.as_ref());
        for line in lines {
            let start = starts.next().unwrap_or(0);
            laid_out_lines.push(LaidOutLine { start, line });
        }

        let display_cursor = display.display_offset(cursor);
        let display_selection = display.display_range(&selected_range);
        let cursor_position = if editing && selected_range.is_empty() {
            cursor_position_for_lines(&laid_out_lines, display_cursor, line_height)
        } else {
            None
        };

        let selections = if !editing || content.is_empty() || selected_range.is_empty() {
            Vec::new()
        } else {
            selection_quads(
                &laid_out_lines,
                &display_selection,
                bounds,
                line_height,
                rgba(theme.selection_highlight).into(),
            )
        };

        let editor_focused = if input.editing_turn.is_some() {
            input.edit_focus_handle.is_focused(window)
        } else {
            input.focus_handle.is_focused(window)
        };
        let cursor = if editing && editor_focused && selected_range.is_empty() {
            cursor_quad(
                &laid_out_lines,
                display_cursor,
                bounds,
                line_height,
                rgb(theme.accent).into(),
            )
        } else {
            None
        };
        let mut mention_quads = Vec::new();
        let mut mention_hitboxes = Vec::new();
        for mention in &display.mentions {
            let quads = decorated_range_quads(
                &laid_out_lines,
                &mention.range,
                bounds,
                line_height,
                DecorationStyle {
                    background: rgba(with_alpha(theme.accent, 0x18)).into(),
                    border: rgba(with_alpha(theme.accent, 0x66)).into(),
                    border_width: px(1.0),
                    radius: px(4.0),
                },
            );
            mention_hitboxes.extend(quads.iter().map(|quad| MentionHitbox {
                source_range: mention.source_range.clone(),
                bounds: quad.bounds,
            }));
            mention_quads.extend(quads);
        }
        let active_number_quads = display
            .mentions
            .iter()
            .filter_map(|mention| {
                let number_range = mention.number_range.as_ref()?;
                let active = active_page_number_edit.as_ref().is_some_and(|active| {
                    composer_mentions.iter().any(|source_mention| {
                        source_mention.range.start == active.mention_start
                            && source_mention.reference.kind == mention.kind
                            && display.display_range(&source_mention.range) == mention.range
                    })
                });
                Some(decorated_range_quads(
                    &laid_out_lines,
                    number_range,
                    bounds,
                    line_height,
                    DecorationStyle {
                        background: rgba(with_alpha(
                            theme.accent,
                            if active { 0x38 } else { 0x24 },
                        ))
                        .into(),
                        border: rgba(with_alpha(theme.accent, if active { 0xcc } else { 0x77 }))
                            .into(),
                        border_width: px(1.0),
                        radius: px(3.0),
                    },
                ))
            })
            .flatten()
            .collect();
        let mention_icons = display
            .mentions
            .iter()
            .flat_map(|mention| {
                selection_quads(
                    &laid_out_lines,
                    &mention.icon_range,
                    bounds,
                    line_height,
                    rgba(0x00000000).into(),
                )
                .into_iter()
                .map(|quad| (quad.bounds, mention.kind))
            })
            .collect();

        PrepaintState {
            lines: Some(laid_out_lines),
            display,
            cursor,
            cursor_position,
            selections,
            mention_quads,
            active_number_quads,
            mention_icons,
            mention_hitboxes,
            hitbox: matches!(&self.source, AiInputSource::UserMessage { .. })
                .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal)),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if matches!(
            &self.source,
            AiInputSource::Composer | AiInputSource::EditedMessage
        ) {
            let focus_handle = {
                let input = self.panel.read(cx);
                if input.editing_turn.is_some() {
                    input.edit_focus_handle.clone()
                } else {
                    input.focus_handle.clone()
                }
            };
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.panel.clone()),
                cx,
            );
            let panel = self.panel.clone();
            window.on_mouse_event(
                move |_: &MouseMoveEvent, phase: DispatchPhase, window, cx| {
                    if phase.capture() {
                        panel.update(cx, |panel, cx| {
                            panel.schedule_mention_preview_hide(window, cx);
                        });
                    }
                },
            );
        }

        for mention in prepaint.mention_quads.drain(..) {
            window.paint_quad(mention);
        }
        for active_number in prepaint.active_number_quads.drain(..) {
            window.paint_quad(active_number);
        }
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }

        let Some(lines) = prepaint.lines.take() else {
            return;
        };

        let line_height = window.line_height();
        let mut y = bounds.top();
        for laid_out in &lines {
            let line_bounds = Bounds::new(
                point(bounds.left(), y),
                size(bounds.size.width, line_height),
            );
            if let Err(error) = laid_out.line.paint(
                point(bounds.left(), y),
                line_height,
                gpui::TextAlign::Left,
                Some(line_bounds),
                window,
                cx,
            ) {
                eprintln!("failed to paint AI input: {error:#}");
            }
            y += laid_out.line.size(line_height).height;
        }
        for (slot, kind) in prepaint.mention_icons.drain(..) {
            let icon_size = px(12.0);
            let icon_bounds = Bounds::new(
                point(
                    slot.center().x - icon_size / 2.0,
                    slot.center().y - icon_size / 2.0,
                ),
                size(icon_size, icon_size),
            );
            if let Err(error) = window.paint_svg(
                icon_bounds,
                AiPanel::mention_icon(kind).path(),
                None,
                TransformationMatrix::default(),
                rgb(cx.global::<SettingsStore>().resolved_theme.text_muted).into(),
                cx,
            ) {
                eprintln!("failed to paint AI mention icon: {error:#}");
            }
        }

        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }

        match &self.source {
            AiInputSource::Composer | AiInputSource::EditedMessage => {
                self.panel.update(cx, |input, _| {
                    input.last_lines = lines;
                    input.last_bounds = Some(bounds);
                    input.last_display = Some(std::mem::take(&mut prepaint.display));
                    input.last_cursor_pos = prepaint.cursor_position;
                    input.mention_hitboxes = std::mem::take(&mut prepaint.mention_hitboxes);
                });
            }
            AiInputSource::UserMessage {
                message_index,
                content,
                mentions,
            } => {
                let Some(hitbox) = prepaint.hitbox.take() else {
                    return;
                };
                let message_index = *message_index;
                let preview_hitboxes = std::mem::take(&mut prepaint.mention_hitboxes);
                self.panel.update(cx, |panel, _| {
                    panel
                        .message_mention_hitboxes
                        .extend(preview_hitboxes.into_iter().filter_map(|hitbox| {
                            mentions
                                .iter()
                                .find(|mention| mention.range == hitbox.source_range)
                                .cloned()
                                .map(|mention| MessageMentionHitbox {
                                    message_index,
                                    hitbox,
                                    mention,
                                })
                        }));
                });

                let panel = self.panel.clone();
                let content_len = content.len();
                let display = std::mem::take(&mut prepaint.display);
                window.on_mouse_event(
                    move |event: &MouseDownEvent, phase: DispatchPhase, window, cx| {
                        if phase.bubble()
                            && event.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            let source_offset = source_index_for_mouse_position(
                                content_len,
                                bounds,
                                &lines,
                                Some(&display),
                                event.position,
                                window.line_height(),
                            );
                            panel.update(cx, |panel, cx| {
                                panel.start_editing_message_at(
                                    message_index,
                                    source_offset,
                                    event.click_count,
                                    window,
                                    cx,
                                );
                            });
                            cx.stop_propagation();
                        }
                    },
                );
            }
        }
    }
}
