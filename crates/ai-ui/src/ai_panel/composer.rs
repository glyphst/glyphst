use super::helpers::*;
use super::*;

fn changed_range(old: &str, new: &str) -> (Range<usize>, usize) {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(old, new)| old == new)
        .count();
    while prefix > 0 && (!old.is_char_boundary(prefix) || !new.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let old_tail = &old[prefix..];
    let new_tail = &new[prefix..];
    let mut suffix = old_tail
        .bytes()
        .rev()
        .zip(new_tail.bytes().rev())
        .take_while(|(old, new)| old == new)
        .count()
        .min(old_tail.len())
        .min(new_tail.len());
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }

    (prefix..old.len() - suffix, new.len() - prefix - suffix)
}

impl AiPanel {
    pub(super) fn active_composer_input(&self) -> Entity<InputState> {
        if self.editing_turn.is_some() {
            self.edit_input.clone()
        } else {
            self.composer_input.clone()
        }
    }

    pub(super) fn on_composer_input_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input != &self.active_composer_input() {
            return;
        }
        match event {
            InputEvent::Change => {
                let (new_content, selected_range) = {
                    let input = input.read(cx);
                    (input.value(), input.selected_range())
                };

                let (range, inserted_len) = changed_range(&self.content, &new_content);
                if range.start != range.end || inserted_len != 0 {
                    self.invalidate_mention_preview();
                    adjust_composer_mentions_for_input_change(
                        &mut self.composer_mentions,
                        &range,
                        inserted_len,
                        &new_content,
                    );
                }
                self.content = new_content;
                self.selected_range = selected_range;
                self.selection_reversed = false;
                self.marked_range = None;
                self.clear_validation_notice();
                self.sync_composer_spans(cx);
                cx.notify();
            }
            InputEvent::CompletionAccepted {
                item,
                inserted_range,
                ..
            } => {
                let new_content = input.read(cx).value();
                if new_content != self.content {
                    let (range, inserted_len) = changed_range(&self.content, &new_content);
                    adjust_composer_mentions_for_input_change(
                        &mut self.composer_mentions,
                        &range,
                        inserted_len,
                        &new_content,
                    );
                    self.content = new_content;
                }
                let editable_span = self.record_accepted_mention(item, inserted_range, cx);
                self.sync_composer_spans(cx);
                if let Some(span_range) = editable_span {
                    input.update(cx, |input, cx| {
                        input.activate_span_edit(span_range, window, cx);
                    });
                }
            }
            InputEvent::SpanHoverChanged { span_range, bounds } => {
                let hovered =
                    span_range
                        .as_ref()
                        .zip(bounds.as_ref())
                        .and_then(|(range, bounds)| {
                            self.composer_mentions
                                .iter()
                                .find(|mention| mention.range == *range)
                                .cloned()
                                .map(|mention| (bounds.clone(), mention))
                        });
                self.update_mention_preview(hovered, window, cx);
            }
            InputEvent::PressEnter { shift, .. } if !shift => {
                match submit_action(
                    self.editing_turn.is_some(),
                    self.generation_failure.as_ref(),
                ) {
                    SubmitAction::Edit => self.submit_edited_message(window, cx),
                    SubmitAction::Retry(assistant_index) => {
                        self.retry_assistant(assistant_index, window, cx)
                    }
                    SubmitAction::Send => self.send_message(window, cx),
                }
                self.sync_composer_input(window, cx);
            }
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {
                cx.notify();
            }
        }
    }

    pub(super) fn on_model_search_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.model_search_query = self.model_search_input.read(cx).value();
            cx.notify();
        }
    }

    pub(super) fn sync_composer_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_input = self.active_composer_input();
        let value = active_input.read(cx).value();
        if value != self.content {
            let content = self.content.clone();
            let selected_range = self.selected_range.clone();
            active_input.update(cx, |input, cx| {
                input.set_value(content, window, cx);
                input.set_selected_range(selected_range, cx);
            });
        }
        self.sync_composer_spans(cx);
    }

    pub(super) fn sync_composer_spans(&self, cx: &mut Context<Self>) {
        let spans = self.composer_input_spans();
        let active_input = self.active_composer_input();
        if active_input.read(cx).spans() != spans {
            active_input.update(cx, |input, cx| {
                input.set_spans(spans, cx);
            });
        }
    }

    pub(super) fn sync_model_search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.model_search_input.read(cx).value();
        if value != self.model_search_query {
            let query = self.model_search_query.clone();
            self.model_search_input.update(cx, |input, cx| {
                input.set_value(query, window, cx);
            });
        }
    }

    pub(super) fn composer_snapshot(&self, cx: &App) -> ComposerSnapshot {
        let input = self.composer_input.read(cx);
        ComposerSnapshot {
            content: self.content.clone(),
            selected_range: input.selected_range(),
            selection_reversed: false,
            marked_range: None,
            mentions: self.composer_mentions.clone(),
            images: self.composer_images.clone(),
        }
    }

    pub(super) fn restore_composer_snapshot(
        &mut self,
        snapshot: ComposerSnapshot,
        cx: &mut Context<Self>,
    ) {
        self.content = snapshot.content;
        self.selected_range = snapshot.selected_range;
        self.selection_reversed = snapshot.selection_reversed;
        self.marked_range = snapshot.marked_range;
        self.composer_mentions = snapshot.mentions;
        self.composer_images = snapshot.images;
        cx.notify();
    }

    pub(super) fn start_editing_message(
        &mut self,
        message_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.editing_turn.is_some() || self.mode != AiPanelMode::Sidebar {
            return;
        }
        let Some(message) = self.messages.get(message_index) else {
            return;
        };
        if message.role != Role::User {
            return;
        }

        let content = message.display_text();
        let Ok(mentions) = composer_mentions_for_message(message) else {
            self.show_validation_notice(tr("ai-edit-context-unavailable"), window, cx);
            return;
        };
        let images = message.user_images().cloned().collect();
        self.editing_turn = Some(EditingTurn {
            message_index,
            saved_composer: self.composer_snapshot(cx),
        });
        self.invalidate_mention_preview();
        self.content = content.clone().into();
        self.selected_range = content.len()..content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        self.composer_mentions = mentions;
        self.composer_images = images;
        self.close_composer_menus();
        self.clear_validation_notice();
        self.sync_composer_input(window, cx);
        self.edit_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn cancel_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(editing) = self.editing_turn.take() else {
            return false;
        };
        self.invalidate_mention_preview();
        self.restore_composer_snapshot(editing.saved_composer, cx);
        self.close_composer_menus();
        self.clear_validation_notice();
        self.sync_composer_input(window, cx);
        self.composer_input.focus_handle(cx).focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn cursor_offset(&self, cx: &App) -> usize {
        self.active_composer_input().read(cx).cursor()
    }

    pub(super) fn on_composer_paste(
        &mut self,
        _: &InputPaste,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .active_composer_input()
            .focus_handle(cx)
            .is_focused(window)
        {
            cx.propagate();
            return;
        }

        let Some(clipboard) = cx.read_from_clipboard() else {
            cx.propagate();
            return;
        };
        if !clipboard_prefers_image_content(&clipboard) {
            cx.propagate();
            return;
        }

        self.attach_clipboard_images(clipboard, cx);
        cx.stop_propagation();
    }

    pub(super) fn pick_user_images(&mut self, cx: &mut Context<Self>) {
        let dialog = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(tr("ai-attach-images-prompt").into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = dialog.await else {
                return;
            };
            let loaded = cx
                .background_spawn(async move {
                    paths
                        .into_iter()
                        .map(load_user_image_path)
                        .collect::<Result<Vec<_>>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| match loaded {
                Ok(images) => this.add_composer_images(images, cx),
                Err(error) => {
                    this.validation_notice =
                        Some(format!("{}: {error:#}", tr("ai-attach-image-error")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn paste_user_image(&mut self, cx: &mut Context<Self>) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            self.validation_notice = Some(format!(
                "{}: {}",
                tr("ai-paste-image-error"),
                "failed to read the clipboard"
            ));
            cx.notify();
            return;
        };
        self.attach_clipboard_images(clipboard, cx);
    }

    fn attach_clipboard_images(&mut self, clipboard: ClipboardItem, cx: &mut Context<Self>) {
        let loaded = cx.background_spawn(async move { load_clipboard_images(clipboard) });
        cx.spawn(async move |this, cx| {
            let result = loaded.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(images) => this.add_composer_images(images, cx),
                Err(error) => {
                    this.validation_notice =
                        Some(format!("{}: {error:#}", tr("ai-paste-image-error")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn attach_external_image_paths(
        &mut self,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let loaded = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(load_user_image_path)
                .collect::<Result<Vec<_>>>()
        });
        cx.spawn(async move |this, cx| {
            let result = loaded.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(images) => this.add_composer_images(images, cx),
                Err(error) => {
                    this.validation_notice =
                        Some(format!("{}: {error:#}", tr("ai-attach-image-error")));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add_composer_images(&mut self, images: Vec<AgentImage>, cx: &mut Context<Self>) {
        let prospective_count = self.composer_images.len().saturating_add(images.len());
        let prospective_bytes = self
            .composer_images
            .iter()
            .chain(&images)
            .map(|image| image.bytes.len())
            .sum::<usize>();
        if prospective_count > MAX_USER_IMAGES {
            self.validation_notice = Some(tr_with_number(
                "ai-too-many-images",
                "limit",
                MAX_USER_IMAGES,
            ));
        } else if prospective_bytes > MAX_USER_IMAGE_BYTES {
            self.validation_notice = Some(tr_with_number(
                "ai-image-size-limit",
                "limit",
                MAX_USER_IMAGE_BYTES / (1024 * 1024),
            ));
        } else {
            self.composer_images.extend(images);
            self.clear_validation_notice();
        }
        cx.notify();
    }

    pub(super) fn remove_composer_image(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.composer_images.len() {
            self.composer_images.remove(index);
            cx.notify();
        }
    }
}

fn load_user_image_path(path: PathBuf) -> Result<AgentImage> {
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{} is not a regular image file", path.display());
    }
    if metadata.len() as usize > MAX_USER_IMAGE_BYTES {
        bail!(
            "{} exceeds the {} MiB image limit",
            path.display(),
            MAX_USER_IMAGE_BYTES / (1024 * 1024)
        );
    }
    let bytes =
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let format = image::guess_format(&bytes).context("unsupported or invalid image")?;
    let media_type = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => bail!("only PNG, JPEG, GIF, and WebP images are supported"),
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_USER_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_USER_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_USER_IMAGE_DECODE_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().context("failed to decode image safely")?;
    validate_user_image_dimensions(decoded.width(), decoded.height())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image")
        .to_string();
    Ok(AgentImage::new(media_type, decoded.width(), decoded.height(), bytes).with_name(name))
}

fn clipboard_prefers_image_content(clipboard: &ClipboardItem) -> bool {
    matches!(
        clipboard.entries().first(),
        Some(ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_))
    )
}

fn load_clipboard_images(clipboard: ClipboardItem) -> Result<Vec<AgentImage>> {
    let mut images = Vec::new();
    for entry in clipboard.into_entries() {
        match entry {
            ClipboardEntry::String(_) => {}
            ClipboardEntry::Image(image) => images.push(load_clipboard_image(image)?),
            ClipboardEntry::ExternalPaths(paths) => {
                for path in paths.paths() {
                    images.push(load_user_image_path(path.clone())?);
                }
            }
        }
    }
    if images.is_empty() {
        bail!("the clipboard does not contain an image");
    }
    Ok(images)
}

fn load_clipboard_image(clipboard_image: Image) -> Result<AgentImage> {
    let clipboard_format = clipboard_image.format();
    let decode_format = match clipboard_format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::Pnm => image::ImageFormat::Pnm,
        ImageFormat::Svg => bail!("SVG clipboard images are not supported"),
    };
    let bytes = clipboard_image.bytes().to_vec();
    if bytes.len() > MAX_USER_IMAGE_BYTES {
        bail!(
            "clipboard image exceeds the {} MiB limit",
            MAX_USER_IMAGE_BYTES / (1024 * 1024)
        );
    }

    let mut reader = image::ImageReader::with_format(Cursor::new(bytes.as_slice()), decode_format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_USER_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_USER_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_USER_IMAGE_DECODE_BYTES);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .context("failed to decode the clipboard image safely")?;
    let width = decoded.width();
    let height = decoded.height();
    validate_user_image_dimensions(width, height)?;

    let (media_type, name, bytes) = match clipboard_format {
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Webp | ImageFormat::Gif => (
            clipboard_format.mime_type(),
            format!("clipboard.{}", clipboard_format.extension()),
            bytes,
        ),
        ImageFormat::Bmp | ImageFormat::Tiff | ImageFormat::Ico | ImageFormat::Pnm => {
            let mut output = Cursor::new(Vec::new());
            decoded
                .write_to(&mut output, image::ImageFormat::Png)
                .context("failed to encode the clipboard image")?;
            (
                "image/png",
                "clipboard.png".to_string(),
                output.into_inner(),
            )
        }
        ImageFormat::Svg => unreachable!(),
    };
    if bytes.len() > MAX_USER_IMAGE_BYTES {
        bail!(
            "clipboard image exceeds the {} MiB limit",
            MAX_USER_IMAGE_BYTES / (1024 * 1024)
        );
    }
    Ok(AgentImage::new(media_type, width, height, bytes).with_name(name))
}

fn validate_user_image_dimensions(width: u32, height: u32) -> Result<()> {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0
        || height == 0
        || width > MAX_USER_IMAGE_DIMENSION
        || height > MAX_USER_IMAGE_DIMENSION
        || pixels > MAX_USER_IMAGE_PIXELS
    {
        bail!("image dimensions {width}×{height} exceed the supported limits");
    }
    Ok(())
}
