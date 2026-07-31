use super::*;

impl DocumentView {
    pub(super) fn can_translate_document(&self) -> bool {
        self.format == DocumentFormat::Pdf && self.path.is_absolute()
    }

    pub(super) fn document_translation_output_path(
        source: &Path,
        target: &LanguageCode,
    ) -> PathBuf {
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("document");
        source
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(format!("{stem}.{}.pdf", target.as_str()))
    }

    pub(super) fn translate_document(&mut self, cx: &mut Context<Self>) {
        if !self.can_translate_document() {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .read(cx)
            .has_job_for_source(&source_path)
        {
            return;
        }

        let settings = cx.global::<SettingsStore>().settings.clone();
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&settings.translation.target_language) {
            Ok(language) => language,
            Err(_) => {
                let source_path = self.path.clone();
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Document,
                        "Choose a valid target language in Translation Settings.",
                        cx,
                    );
                });
                return;
            }
        };
        let source_path = self.path.clone();
        let output_path = Self::document_translation_output_path(&source_path, &target_language);
        let directory = output_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let suggested_name = output_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("translated.pdf")
            .to_string();
        let dialog = cx.prompt_for_new_path(&directory, Some(&suggested_name));

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            _ = this.update(cx, |this, cx| match result {
                Ok(Ok(Some(output_path))) => {
                    let request = PdfTranslationRequest {
                        source_path,
                        output_path,
                        source_language,
                        target_language,
                    };
                    this.start_document_translation(
                        settings,
                        request,
                        DocumentTranslationOpenMode::Document,
                        cx,
                    );
                }
                Ok(Err(error)) => {
                    eprintln!("failed to choose translated PDF output path: {error:#}");
                }
                Ok(Ok(None)) | Err(_) => {}
            });
        })
        .detach();
    }

    fn start_document_translation(
        &mut self,
        settings: crate::settings::Settings,
        request: PdfTranslationRequest,
        open_mode: DocumentTranslationOpenMode,
        cx: &mut Context<Self>,
    ) {
        if !self.can_translate_document() || request.source_path != self.path {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .read(cx)
            .has_job_for_source(&source_path)
        {
            return;
        }

        if normalize_path(&request.output_path) == self.path {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    open_mode,
                    "Choose an output path different from the source PDF.",
                    cx,
                );
            });
            return;
        }

        if !cx.has_global::<PdfTranslationService>() {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    open_mode,
                    "Document translation is unavailable.",
                    cx,
                );
            });
            return;
        }

        self.document_translation_jobs.update(cx, |jobs, cx| {
            jobs.submit(settings, request, open_mode, cx);
        });
    }

    pub(super) fn cancel_document_translation(&mut self, cx: &mut Context<Self>) {
        let source_path = self.path.clone();
        self.document_translation_jobs.update(cx, |jobs, cx| {
            jobs.cancel_for_source(&source_path, cx);
        });
    }

    pub(super) fn open_translation_split(&mut self, cx: &mut Context<Self>) {
        if !self.can_translate_document() {
            return;
        }
        let source_path = self.path.clone();
        if self
            .document_translation_jobs
            .read(cx)
            .has_job_for_source(&source_path)
        {
            return;
        }
        if !cx.has_global::<PdfTranslationService>() {
            let source_path = self.path.clone();
            self.document_translation_jobs.update(cx, |jobs, cx| {
                jobs.report_preflight_error(
                    source_path,
                    DocumentTranslationOpenMode::Split,
                    "Document translation is unavailable.",
                    cx,
                );
            });
            return;
        }

        let settings = cx.global::<SettingsStore>().settings.clone();
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&settings.translation.target_language) {
            Ok(language) => language,
            Err(_) => {
                let source_path = self.path.clone();
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Split,
                        "Choose a valid target language in Translation Settings.",
                        cx,
                    );
                });
                return;
            }
        };
        let service = cx.global::<PdfTranslationService>().clone();
        let request = match service.artifact_request(
            &settings,
            self.path.clone(),
            source_language,
            target_language,
        ) {
            Ok(request) => request,
            Err(error) => {
                let source_path = self.path.clone();
                let message = error.message;
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.report_preflight_error(
                        source_path,
                        DocumentTranslationOpenMode::Split,
                        message,
                        cx,
                    );
                });
                return;
            }
        };
        match service.cached_artifact(&settings, &request) {
            Some(result) => {
                self.document_translation_jobs.update(cx, |jobs, cx| {
                    jobs.open_cached_output(
                        settings,
                        request,
                        DocumentTranslationOpenMode::Split,
                        result,
                        cx,
                    );
                });
            }
            None => self.start_document_translation(
                settings,
                request,
                DocumentTranslationOpenMode::Split,
                cx,
            ),
        }
    }

    pub(super) fn translate_selection(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = self.selected_source_text() else {
            return;
        };
        self.translate_text(text, popup_position, update_pinned, cx);
    }

    pub(super) fn reset_translation_scroll(&self, pinned: bool) {
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        scroll_handle.set_offset(point(px(0.0), px(0.0)));
    }

    fn translate_text(
        &mut self,
        text: String,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let target_language = match LanguageCode::new(&self.target_language) {
            Ok(language) => language,
            Err(error) => {
                self.set_translation_error(
                    popup_position,
                    update_pinned,
                    text,
                    error.to_string(),
                    cx,
                );
                return;
            }
        };
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let generation = self.translation_request_generation;
        let state = TranslationCardState::new(
            text.clone(),
            TranslationStatus::Loading,
            popup_position,
            generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                text.clone(),
                TranslationStatus::Loading,
                None,
                generation,
            ));
        }
        cx.notify();

        if !cx.has_global::<TranslationService>() {
            self.finish_translation_request(
                generation,
                Err("translation service is unavailable".to_string()),
                cx,
            );
            return;
        }

        let service = cx.global::<TranslationService>().clone();
        let provider = settings.translation.active_provider;
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let request = TranslationRequest {
            provider,
            source_language,
            target_language,
            texts: vec![text],
            context_before: None,
            context_after: None,
        };

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    service
                        .translate(&settings, request)
                        .map(|response| {
                            response.translations.into_iter().next().unwrap_or_default()
                        })
                        .map_err(|error| error.to_string())
                })
                .await;
            _ = this.update(cx, |this, cx| {
                this.finish_translation_request(generation, result, cx);
            });
        })
        .detach();
    }

    pub(super) fn request_document_translation(&mut self, cx: &mut Context<Self>) {
        if self.view_language != DocumentViewLanguage::Translated
            || self.text_blocks.is_empty()
            || !cx.has_global::<TranslationService>()
        {
            return;
        }
        self.translation_document_generation = self.translation_document_generation.wrapping_add(1);
        let generation = self.translation_document_generation;
        let blocks = self
            .text_blocks
            .iter()
            .filter(|block| block.horizontal && !block.text.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>();
        let settings = cx.global::<SettingsStore>().settings.clone();
        let provider = settings.translation.active_provider;
        let source_language = settings
            .translation
            .source_language
            .as_deref()
            .and_then(|language| LanguageCode::new(language).ok());
        let target_language = match LanguageCode::new(&self.target_language) {
            Ok(language) => language,
            Err(error) => {
                eprintln!("invalid translation target language: {error:#}");
                return;
            }
        };
        let service = cx.global::<TranslationService>().clone();

        self.translation_document_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let texts = blocks
                        .iter()
                        .map(|block| block.text.clone())
                        .collect::<Vec<_>>();
                    service
                        .translate_batches(
                            &settings,
                            provider,
                            source_language,
                            target_language,
                            texts,
                        )
                        .map(|translations| (blocks, translations))
                        .map_err(|error| error.to_string())
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.translation_document_generation != generation {
                    return;
                }
                this.translation_document_task = None;
                match result {
                    Ok((blocks, translations)) if blocks.len() == translations.len() => {
                        this.translated_blocks = blocks
                            .into_iter()
                            .zip(translations)
                            .map(|(source, text)| (source.id, TranslatedBlock { source, text }))
                            .collect();
                        this.translated_text_layouts.clear();
                    }
                    Ok(_) => {
                        eprintln!("translation provider returned an incomplete document result");
                    }
                    Err(error) => eprintln!("failed to translate document: {error}"),
                }
                cx.notify();
            });
        }));
    }

    fn set_translation_error(
        &mut self,
        popup_position: Option<GpuiPoint<Pixels>>,
        update_pinned: bool,
        source: String,
        error: String,
        cx: &mut Context<Self>,
    ) {
        self.translation_request_generation = self.translation_request_generation.wrapping_add(1);
        let error: SharedString = error.into();
        let state = TranslationCardState::new(
            source.clone(),
            TranslationStatus::Error(error.clone()),
            popup_position,
            self.translation_request_generation,
        );
        if popup_position.is_some() {
            self.reset_translation_scroll(false);
            self.translation_popup = Some(state);
        }
        if update_pinned {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                source,
                TranslationStatus::Error(error),
                None,
                self.translation_request_generation,
            ));
        }
        cx.notify();
    }

    fn finish_translation_request(
        &mut self,
        generation: u64,
        result: std::result::Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let status = match result {
            Ok(text) if !text.trim().is_empty() => TranslationStatus::Ready {
                markdown: cx.new(|cx| Markdown::new(text.clone().into(), cx)),
                text: text.into(),
            },
            Ok(_) => TranslationStatus::Error(crate::fl!("translation-no-result").into()),
            Err(error) => TranslationStatus::Error(error.into()),
        };
        if let Some(popup) = self.translation_popup.as_mut()
            && popup.request_generation == generation
        {
            popup.status = status.clone();
        }
        if let Some(pinned) = self.pinned_translation.as_mut()
            && pinned.request_generation == generation
        {
            pinned.status = status;
        }
        cx.notify();
    }

    pub(super) fn toggle_pinned_translation(&mut self, cx: &mut Context<Self>) {
        if self.pinned_translation.is_none() {
            self.reset_translation_scroll(true);
            self.pinned_translation = Some(TranslationCardState::new(
                SharedString::default(),
                TranslationStatus::Idle,
                None,
                self.translation_request_generation,
            ));
        } else {
            self.pinned_translation = None;
        }
        cx.notify();
    }

    pub(super) fn change_target_language(
        &mut self,
        target_language: String,
        source: &Entity<TargetLanguageSelectState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(target_language) = LanguageCode::new(target_language) else {
            return;
        };
        if self.target_language == target_language.as_str() {
            return;
        }
        self.target_language = target_language.as_str().to_string();
        for select in [
            self.translation_popup_language_select.as_ref(),
            self.pinned_translation_language_select.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if select.state.entity_id() == source.entity_id() {
                continue;
            }
            let value = target_language.as_str().to_string();
            select.state.update(cx, |select, cx| {
                select.set_selected_value(&value, window, cx);
            });
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_document_target_language(&self.path, target_language.as_str())
        {
            eprintln!("failed to save document translation language: {error:#}");
        }

        let popup = self
            .translation_popup
            .as_ref()
            .map(|state| (state.source.to_string(), state.position));
        let pinned_source = self
            .pinned_translation
            .as_ref()
            .map(|state| state.source.to_string());

        match (popup, pinned_source) {
            (Some((source, position)), Some(pinned_source))
                if !source.is_empty() && source == pinned_source =>
            {
                self.translate_text(source, position, true, cx);
            }
            (popup, pinned_source) => {
                if let Some((source, position)) = popup {
                    self.translate_text(source, position, false, cx);
                }
                if let Some(source) = pinned_source
                    && !source.is_empty()
                {
                    self.translate_text(source, None, true, cx);
                }
            }
        }
        if self.view_language == DocumentViewLanguage::Translated {
            self.request_document_translation(cx);
        }
        cx.notify();
    }

    fn close_translation_popup(&mut self, cx: &mut Context<Self>) {
        if self.translation_popup.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn render_translation_card(
        &self,
        state: TranslationCardState,
        pinned: bool,
        theme: ThemeColor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language_select = if pinned {
            self.pinned_translation_language_select.clone()
        } else {
            self.translation_popup_language_select.clone()
        };
        let language_select_open = language_select
            .as_ref()
            .is_some_and(|select| select.is_open(cx));
        let font = &cx.global::<SettingsStore>().settings.font;
        let base_text_style = TextStyle {
            font_family: font.ui_font_family.clone().into(),
            font_size: ui_rems(13.0).into(),
            color: theme.foreground,
            font_weight: FontWeight(font.ui_font_weight),
            ..Default::default()
        };
        let content = match &state.status {
            TranslationStatus::Idle => div().into_any_element(),
            TranslationStatus::Loading => div()
                .py_2()
                .text_color(theme.muted_foreground)
                .child(crate::fl!("translation-loading"))
                .into_any_element(),
            TranslationStatus::Ready { markdown, .. } => div()
                .w_full()
                .min_w_0()
                .child(MarkdownElement::new(
                    markdown.clone(),
                    MarkdownStyle::from_theme(theme, base_text_style),
                ))
                .into_any_element(),
            TranslationStatus::Error(error) => Alert::error(
                if pinned {
                    "pinned-translation-error"
                } else {
                    "translation-popup-error"
                },
                error.clone(),
            )
            .xsmall()
            .into_any_element(),
        };
        let copy_text = match &state.status {
            TranslationStatus::Ready { text, .. } => Some(text.clone()),
            _ => None,
        };
        let scrollable = matches!(
            state.status,
            TranslationStatus::Ready { .. } | TranslationStatus::Error(_)
        );
        let scroll_handle = if pinned {
            &self.pinned_translation_scroll
        } else {
            &self.translation_popup_scroll
        };
        let scrollbar_id = if pinned {
            "pinned-translation-scrollbar"
        } else {
            "translation-popup-scrollbar"
        };

        div()
            .occlude()
            .w(px(cx
                .global::<SettingsStore>()
                .settings
                .translation
                .pinned
                .width))
            .overflow_hidden()
            .flex()
            .flex_col()
            .popover_style(cx)
            .text_size(ui_rems(13.0))
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(!pinned && !language_select_open, |this| {
                this.on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_translation_popup(cx);
                }))
            })
            .on_scroll_wheel(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_2()
                    .border_b(px(1.0))
                    .border_color(theme.input)
                    .text_size(ui_rems(11.0))
                    .text_color(theme.muted_foreground)
                    .when_some(language_select, |this, select| {
                        this.child(
                            Select::new(&select.state)
                                .small()
                                .w(px(180.0))
                                .menu_width(px(180.0))
                                .menu_max_h(px(240.0))
                                .search_placeholder(crate::fl!(
                                    "settings-search-languages-placeholder"
                                )),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .when_some(copy_text, |this, text| {
                                this.child(
                                    toolbar_icon_button("translation-popup-copy", IconName::Copy)
                                        .tooltip(crate::fl!("copy"))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.to_string(),
                                            ));
                                            cx.stop_propagation();
                                        })),
                                )
                            })
                            .child(
                                toolbar_icon_button("translation-popup-dismiss", IconName::X)
                                    .tooltip(crate::fl!("translation-close"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if pinned {
                                            this.toggle_pinned_translation(cx);
                                        } else {
                                            this.close_translation_popup(cx);
                                        }
                                        cx.stop_propagation();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .id(if pinned {
                                "pinned-translation-content"
                            } else {
                                "translation-popup-content"
                            })
                            .track_scroll(scroll_handle)
                            .w_full()
                            .min_w_0()
                            .max_h(px(300.0))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .p_3()
                            .cursor_text()
                            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                            .child(content),
                    )
                    .when(scrollable, |this| {
                        this.vertical_scrollbar_for_with_id(scroll_handle, scrollbar_id, window, cx)
                    }),
            )
            .into_any_element()
    }
}
