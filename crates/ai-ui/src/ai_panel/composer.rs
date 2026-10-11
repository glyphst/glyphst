use super::helpers::*;
use super::*;

#[derive(Debug, Default, PartialEq, Eq)]
struct ExternalAiDrop {
    images: Vec<PathBuf>,
    remaining: Vec<PathBuf>,
    feedback: Option<ExternalAiDropFeedback>,
}

fn is_external_image_path(path: &std::path::Path) -> bool {
    path.is_file()
        && matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp")
        )
}

fn is_external_document_path(path: &std::path::Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension)
            .is_some()
}

fn classify_external_ai_paths(paths: &[PathBuf]) -> ExternalAiDrop {
    let mut drop = ExternalAiDrop::default();
    let mut documents = 0;
    let mut rejected = 0;

    for path in paths {
        if is_external_image_path(path) {
            drop.images.push(path.clone());
        } else {
            if is_external_document_path(path) {
                documents += 1;
            } else {
                rejected += 1;
            }
            drop.remaining.push(path.clone());
        }
    }

    let images = drop.images.len();
    drop.feedback =
        (images > 0 || documents > 0 || rejected > 0).then_some(ExternalAiDropFeedback {
            images,
            documents,
            rejected,
        });
    drop
}

impl AiPanel {
    pub(super) fn track_external_paths_drag(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_inside = event.bounds.contains(&event.event.position);
        let feedback = is_inside
            .then(|| classify_external_ai_paths(event.drag(cx).paths()))
            .and_then(|drop| drop.feedback);
        if self.external_drop_feedback != feedback {
            self.external_drop_feedback = feedback;
            cx.notify();
        }
        if is_inside {
            cx.stop_propagation();
        }
    }

    pub(super) fn drop_external_paths(
        &mut self,
        paths: &ExternalPaths,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.external_drop_feedback = None;
        let drop = classify_external_ai_paths(paths.paths());
        if !drop.images.is_empty() {
            self.attach_external_image_paths(drop.images, cx);
        }
        if !drop.remaining.is_empty() {
            cx.emit(AiPanelEvent::ExternalPathsDropped(drop.remaining));
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_external_drop_overlay(&self, theme: Theme) -> Option<AnyElement> {
        let feedback = self.external_drop_feedback?;
        let accepts_items = feedback.images > 0 || feedback.documents > 0;
        let accent = if accepts_items {
            theme.drag_border
        } else {
            theme.danger
        };
        let background = if accepts_items {
            theme.drop_target
        } else {
            theme.danger.opacity(0.12)
        };
        let title = if feedback.images > 0 && feedback.documents > 0 {
            tr("ai-external-drop-mixed-title")
        } else if feedback.images > 0 {
            tr_with_number("ai-external-drop-attach-title", "count", feedback.images)
        } else if feedback.documents > 0 {
            tr_with_number("external-drop-open-title", "count", feedback.documents)
        } else {
            tr("external-drop-unsupported-title")
        };
        let hint = if feedback.rejected > 0 && accepts_items {
            tr_with_number("ai-external-drop-skipped-hint", "count", feedback.rejected)
        } else if !accepts_items {
            tr("ai-external-drop-unsupported-hint")
        } else if feedback.images > 0 && feedback.documents > 0 {
            tr("ai-external-drop-mixed-hint")
        } else if feedback.images > 0 {
            tr("ai-external-drop-image-hint")
        } else {
            tr("external-drop-supported-hint")
        };
        let selector = if accepts_items {
            "ai-external-drop-overlay-accepted"
        } else {
            "ai-external-drop-overlay-rejected"
        };

        Some(
            div()
                .id("ai-external-drop-overlay")
                .debug_selector(move || selector.to_string())
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .invisible()
                .drag_over::<ExternalPaths>(|style, _, _, _| style.visible())
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .bg(background)
                .border_2()
                .border_dashed()
                .border_color(accent)
                .child(
                    div()
                        .max_w(px(320.0))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .rounded(px(10.0))
                        .border_1()
                        .border_color(accent.opacity(0.55))
                        .bg(theme.background.opacity(0.94))
                        .px_5()
                        .py_4()
                        .shadow_lg()
                        .child(
                            Icon::new(if !accepts_items {
                                IconName::TriangleAlert
                            } else if feedback.images > 0 && feedback.documents == 0 {
                                IconName::Image
                            } else {
                                IconName::FileText
                            })
                            .size(ui_rems(28.0))
                            .text_color(accent),
                        )
                        .child(
                            div()
                                .text_center()
                                .text_size(ui_rems(14.0))
                                .text_color(theme.foreground)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_center()
                                .text_size(ui_rems(11.0))
                                .text_color(theme.muted_foreground)
                                .child(hint),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn active_composer_input(&self) -> Entity<TextareaState> {
        if self.editing_turn.is_some() {
            self.edit_input.clone()
        } else {
            self.composer_input.clone()
        }
    }

    pub(super) fn on_composer_input_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if input != &self.active_composer_input() {
            return;
        }
        match event {
            InputEvent::Change => {
                let (content, selected_range) = {
                    let input = input.read(cx);
                    (input.content(), input.selected_range())
                };
                if content.text() != &self.content {
                    self.invalidate_mention_preview();
                }
                self.sync_mentions_from_input(&content);
                self.content = content.text().clone();
                self.selected_range = selected_range;
                self.selection_reversed = false;
                self.marked_range = None;
                self.clear_validation_notice();
                self.refresh_mention_menu(cx);
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } if !shift && self.mention_menu.is_some() => {
                self.accept_mention_menu(None, window, cx);
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
            let selected_range = self.selected_range.clone();
            let content = self.composer_input_content();
            active_input.update(cx, |input, cx| {
                input.set_value(content, window, cx);
                input.set_selected_range(selected_range, cx);
            });
        } else if active_input.read(cx).tokens() != self.composer_input_content().tokens() {
            let selected_range = active_input.read(cx).selected_range();
            let content = self.composer_input_content();
            active_input.update(cx, |input, cx| {
                input.set_value(content, window, cx);
                input.set_selected_range(selected_range, cx);
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
        self.assign_mention_token_ids();
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

    /// Attach a pasted image instead of inserting text. Returns whether the
    /// paste was consumed.
    pub(super) fn paste_composer_clipboard(
        &mut self,
        clipboard: &ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        if !clipboard_prefers_image_content(clipboard) {
            return false;
        }
        self.attach_clipboard_images(clipboard.clone(), cx);
        true
    }

    pub(super) fn pick_user_images(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = dirs::picture_dir()
            .or_else(dirs::download_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = prompt_for_paths(
            window,
            cx,
            OpenPathOptions::files(directory, tr("ai-attach-images-prompt"))
                .multiple(true)
                .filter(PathFilter::new(
                    tr("ai-external-drop-image-hint"),
                    ["png", "jpg", "jpeg", "gif", "webp"],
                ))
                .allow_all_files(true),
        );
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

#[cfg(test)]
mod external_drop_tests {
    use std::{fs, time::SystemTime};

    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "glyphst-ai-external-drop-{}-{}-{name}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn partitions_images_from_documents_and_rejected_items() {
        let image = temporary_path("diagram.PNG");
        let pdf = temporary_path("paper.PDF");
        let unsupported = temporary_path("notes.txt");
        let folder = temporary_path("book.epub");
        fs::write(&image, b"image").unwrap();
        fs::write(&pdf, b"pdf").unwrap();
        fs::write(&unsupported, b"text").unwrap();
        fs::create_dir(&folder).unwrap();

        let classified = classify_external_ai_paths(&[
            pdf.clone(),
            image.clone(),
            unsupported.clone(),
            folder.clone(),
        ]);

        assert_eq!(classified.images, vec![image.clone()]);
        assert_eq!(
            classified.remaining,
            vec![pdf.clone(), unsupported.clone(), folder.clone()]
        );
        assert_eq!(
            classified.feedback,
            Some(ExternalAiDropFeedback {
                images: 1,
                documents: 1,
                rejected: 2,
            })
        );

        fs::remove_file(image).unwrap();
        fs::remove_file(pdf).unwrap();
        fs::remove_file(unsupported).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
