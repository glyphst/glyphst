//! Previews of what a chip attaches, after the GPUI assistant's mention
//! preview: a card with the text (as Markdown, cut at 4,000 characters) or
//! the page images, shown while the pointer is on a chip and beside the
//! `@` menu's highlighted entry (as Zed's completion aside).
//!
//! Pages are read and rendered on worker threads; results are kept for the
//! last few chips, and an answer nobody waits for any more is dropped.

use std::{
    sync::mpsc::{self, Receiver},
    thread,
};

use egui::{Align2, Rect, Stroke, vec2};
use glyphst_ai::agent::AgentCancellationToken;

use crate::{
    ai::AttachedImage,
    ai_editor::ChipKind,
    ai_mentions::Mention,
    theme::{self, Tokens},
};

/// Text previews stop here; the whole text still goes with the message.
pub const PREVIEW_CHARS: usize = 4_000;
/// The card's width and its text's height, beside the menu as on hover.
pub(crate) const CARD_WIDTH: f32 = 320.0;
pub(crate) const TEXT_HEIGHT: f32 = 274.0;
/// The gap between a chip and its card the pointer may cross.
const BRIDGE: f32 = 6.0;
/// How long a card stays after the pointer leaves the chip and the card.
const HIDE_DELAY: f64 = 0.15;
/// Previews kept.
const CACHE: usize = 16;

/// What a card shows.
pub enum Content {
    Loading,
    Text { text: String, cut: bool },
    Images(Vec<egui::TextureHandle>),
    Failed(String),
}

enum Loaded {
    Text(String),
    Images(Vec<egui::ColorImage>),
}

struct Job {
    key: String,
    result: Receiver<Result<Loaded, String>>,
}

struct Hover {
    key: String,
    kind: ChipKind,
    anchor: Rect,
    hide_at: Option<f64>,
}

#[derive(Default)]
pub struct Previews {
    /// Newest last.
    cache: Vec<(String, Content)>,
    jobs: Vec<Job>,
    hover: Option<Hover>,
    /// A chip under the pointer this frame.
    hovered: Option<(ChipKind, Rect)>,
    /// Where the hover card was last frame.
    card: Option<Rect>,
    markdown: egui_commonmark::CommonMarkCache,
}

/// The key a preview is kept under, and whether it needs loading.
fn key(kind: &ChipKind) -> String {
    match kind {
        ChipKind::Mention(mention) => match mention {
            Mention::Pages {
                document,
                first,
                last,
            }
            | Mention::Section {
                document,
                first,
                last,
                ..
            } => format!("pages:{}:{first}-{last}", document.path.display()),
            Mention::PageImages { document, pages } => {
                format!("images:{}:{pages:?}", document.path.display())
            }
            Mention::Document(document) => format!("document:{}", document.path.display()),
            Mention::Text { kind, label, .. } => format!("text:{kind}:{label}"),
            Mention::Saved { label, .. } => format!("saved:{label}"),
        },
        ChipKind::Image(attachment) => format!("image:{:?}", attachment.thumbnail.id()),
    }
}

impl Previews {
    /// Notes the chip under the pointer this frame (from the composer or a
    /// sent message).
    pub fn hovered(&mut self, kind: &ChipKind, rect: Rect) {
        self.hovered = Some((kind.clone(), rect));
    }

    /// Takes finished loads; call once a frame before drawing.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut done = Vec::new();
        self.jobs.retain(|job| match job.result.try_recv() {
            Ok(result) => {
                done.push((job.key.clone(), result));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        for (key, result) in done {
            // A preview dropped from the cache while loading is not wanted.
            let waiting = self
                .cache
                .iter()
                .any(|(cached, content)| *cached == key && matches!(content, Content::Loading));
            if !waiting {
                continue;
            }
            let content = match result {
                Ok(Loaded::Text(text)) => {
                    // One page needs no heading: the card's title names it.
                    let text = match text.strip_prefix("## Page ") {
                        Some(rest) if !rest.contains("\n## Page ") => rest
                            .split_once('\n')
                            .map_or(String::new(), |(_, body)| body.to_string()),
                        _ => text,
                    };
                    let cut = text.chars().count() > PREVIEW_CHARS;
                    Content::Text {
                        text: text.chars().take(PREVIEW_CHARS).collect(),
                        cut,
                    }
                }
                Ok(Loaded::Images(images)) => Content::Images(
                    images
                        .into_iter()
                        .enumerate()
                        .map(|(index, image)| {
                            ctx.load_texture(
                                format!("assistant-preview-{key}-{index}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            )
                        })
                        .collect(),
                ),
                Err(error) => Content::Failed(error),
            };
            self.store(key, content);
        }
    }

    fn store(&mut self, key: String, content: Content) {
        self.cache.retain(|(cached, _)| *cached != key);
        self.cache.push((key, content));
        if self.cache.len() > CACHE {
            self.cache.remove(0);
        }
    }

    /// The preview of `kind`, loading it the first time.
    fn content(&mut self, ctx: &egui::Context, kind: &ChipKind) -> &Content {
        let key = key(kind);
        if let Some(index) = self.cache.iter().position(|(cached, _)| *cached == key) {
            // The most recently used stays longest.
            let entry = self.cache.remove(index);
            self.cache.push(entry);
            return &self.cache.last().expect("just pushed").1;
        }
        let immediate = match kind {
            ChipKind::Mention(Mention::Text { text, .. } | Mention::Saved { text, .. }) => {
                Some(Content::Text {
                    text: text.chars().take(PREVIEW_CHARS).collect(),
                    cut: text.chars().count() > PREVIEW_CHARS,
                })
            }
            _ => None,
        };
        let content = match immediate {
            Some(content) => content,
            None => {
                self.start(ctx, key.clone(), kind.clone());
                Content::Loading
            }
        };
        self.store(key, content);
        &self.cache.last().expect("just stored").1
    }

    fn start(&mut self, ctx: &egui::Context, key: String, kind: ChipKind) {
        let (sender, result) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = sender.send(load(&kind));
            ctx.request_repaint();
        });
        self.jobs.push(Job { key, result });
    }

    /// The hover card for the chip under the pointer: it opens on a chip,
    /// stays while the pointer crosses to it, and closes a moment after the
    /// pointer leaves both. Call once a frame after every chip was drawn.
    pub fn show_hover(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        match self.hovered.take() {
            Some((kind, anchor)) => {
                let key = key(&kind);
                if self.hover.as_ref().is_none_or(|hover| hover.key != key) {
                    self.card = None;
                }
                self.hover = Some(Hover {
                    key,
                    kind,
                    anchor,
                    hide_at: None,
                });
            }
            None => {
                let on_card = pointer.is_some_and(|pointer| {
                    self.card
                        .is_some_and(|card| card.expand(BRIDGE).contains(pointer))
                });
                if let Some(hover) = &mut self.hover {
                    if on_card {
                        hover.hide_at = None;
                    } else {
                        let hide_at = *hover.hide_at.get_or_insert(now + HIDE_DELAY);
                        if now >= hide_at {
                            self.hover = None;
                            self.card = None;
                        } else {
                            ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                                hide_at - now,
                            ));
                        }
                    }
                }
            }
        }
        let Some(hover) = &self.hover else {
            return;
        };
        let (kind, anchor) = (hover.kind.clone(), hover.anchor);
        let screen = ctx.content_rect();
        let width = CARD_WIDTH.min(screen.width() - 16.0).max(120.0);
        let height = self.card.map_or(TEXT_HEIGHT, |card| card.height());
        let below = screen.bottom() - anchor.bottom();
        let above = anchor.top() - screen.top();
        let (pivot, y) = if below >= height + BRIDGE + 8.0 || below >= above {
            (Align2::LEFT_TOP, anchor.bottom() + BRIDGE)
        } else {
            (Align2::LEFT_BOTTOM, anchor.top() - BRIDGE)
        };
        let x = anchor.left().clamp(
            screen.left() + 8.0,
            (screen.right() - width - 8.0).max(screen.left() + 8.0),
        );
        let area = egui::Area::new(egui::Id::new("assistant-chip-preview"))
            .order(egui::Order::Tooltip)
            .pivot(pivot)
            .fixed_pos(egui::pos2(x, y))
            .constrain(true)
            .show(ctx, |ui| self.card_ui(ui, &kind, width, TEXT_HEIGHT));
        self.card = Some(area.response.rect);
    }

    /// The preview card of `kind`, `width` wide; for the menu's aside.
    pub fn card_ui(&mut self, ui: &mut egui::Ui, kind: &ChipKind, width: f32, max_height: f32) {
        let tokens = Tokens::get(ui.ctx());
        // An area offers last frame's size; a card that was loading would
        // stay that small without room to grow into.
        ui.set_max_height(max_height + 160.0);
        glyphst_ui::pane::popover(ui.ctx())
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                header(ui, kind, &tokens);
                let ctx = ui.ctx().clone();
                let content = match self.content(&ctx, kind) {
                    Content::Loading => None,
                    Content::Text { text, cut } => Some(Ok((text.clone(), *cut))),
                    Content::Images(images) => Some(Err(images.clone())),
                    Content::Failed(error) => {
                        let error = error.clone();
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(error)
                                        .font(theme::mono(11.0))
                                        .color(tokens.err),
                                );
                            });
                        return;
                    }
                };
                match content {
                    None => {
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(width, 72.0), egui::Sense::hover());
                        glyphst_ui::paint::loading_dots(ui, rect, tokens.muted);
                    }
                    Some(Ok((text, cut))) => {
                        egui::ScrollArea::vertical()
                            .id_salt("assistant-preview-text")
                            .max_height(max_height)
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(12, 10))
                                    .show(ui, |ui| {
                                        ui.style_mut()
                                            .text_styles
                                            .insert(egui::TextStyle::Body, theme::regular(12.5));
                                        egui_commonmark::CommonMarkViewer::new().show(
                                            ui,
                                            &mut self.markdown,
                                            &text,
                                        );
                                    });
                            });
                        if cut {
                            let rect = ui.available_rect_before_wrap();
                            ui.painter().hline(
                                rect.x_range(),
                                rect.top(),
                                Stroke::new(1.0, tokens.rule),
                            );
                            egui::Frame::new()
                                .inner_margin(egui::Margin::symmetric(12, 6))
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "preview limited to {PREVIEW_CHARS} characters. the full text is still sent."
                                        ))
                                        .font(theme::mono(9.0))
                                        .color(tokens.faint),
                                    );
                                });
                        }
                    }
                    Some(Err(images)) => {
                        let columns = if images.len() > 1 { 2 } else { 1 };
                        let gap = 6.0;
                        let cell = (width - 16.0 - gap * (columns as f32 - 1.0)) / columns as f32;
                        egui::ScrollArea::vertical()
                            .id_salt("assistant-preview-images")
                            .max_height(max_height + 120.0)
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing = vec2(gap, gap);
                                        for row in images.chunks(columns) {
                                            ui.horizontal(|ui| {
                                                for image in row {
                                                    let [w, h] = image.size();
                                                    let size = vec2(cell, cell * h as f32 / w.max(1) as f32);
                                                    ui.add(
                                                        egui::Image::new(image)
                                                            .fit_to_exact_size(size)
                                                            .corner_radius(0),
                                                    );
                                                }
                                            });
                                        }
                                    });
                            });
                    }
                }
            });
    }
}

/// The card's title row: the chip's icon and what it is.
fn header(ui: &mut egui::Ui, kind: &ChipKind, tokens: &Tokens) {
    let (title, detail) = describe(kind);
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add(crate::icons::image(kind.icon(), 12.0, tokens.faint));
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("preview ── {}", title.to_lowercase()))
                            .font(theme::mono(11.0))
                            .color(tokens.text_2),
                    )
                    .truncate(),
                );
                if !detail.is_empty() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(detail.to_lowercase())
                                    .font(theme::mono(10.0))
                                    .color(tokens.faint),
                            )
                            .truncate(),
                        );
                    });
                }
            });
        });
    let rect = ui.min_rect();
    ui.painter()
        .hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, tokens.rule));
}

/// A chip's title and detail in its card.
fn describe(kind: &ChipKind) -> (String, String) {
    match kind {
        ChipKind::Mention(mention) => match mention {
            Mention::Pages {
                document,
                first,
                last,
            } => (
                if first == last {
                    format!("Page {}", first + 1)
                } else {
                    format!("Pages {}–{}", first + 1, last + 1)
                },
                document.title.clone(),
            ),
            Mention::PageImages { document, pages } => (
                match pages.as_slice() {
                    [page] => format!("Page {} as an image", page + 1),
                    _ => format!("{} page images", pages.len()),
                },
                document.title.clone(),
            ),
            Mention::Document(document) => (
                document.title.clone(),
                format!("{} pages", document.page_count),
            ),
            Mention::Section {
                title, first, last, ..
            } => (title.clone(), format!("pp. {}–{}", first + 1, last + 1)),
            Mention::Text { kind, label, .. } => (
                label.clone(),
                match *kind {
                    "selection" => "Selection",
                    "annotation" => "Annotation",
                    "book" => "Library",
                    _ => "",
                }
                .into(),
            ),
            Mention::Saved { label, .. } => (label.clone(), "As sent".into()),
        },
        ChipKind::Image(attachment) => (attachment.label.clone(), "Picture".into()),
    }
}

/// Reads what a chip attaches, on a worker thread.
fn load(kind: &ChipKind) -> Result<Loaded, String> {
    let cancel = AgentCancellationToken::default();
    let failed = |error: anyhow::Error| format!("{error:#}");
    match kind {
        ChipKind::Mention(mention) => match mention {
            Mention::Pages {
                document,
                first,
                last,
            }
            | Mention::Section {
                document,
                first,
                last,
                ..
            } => {
                let pages = (*first..=*last).collect::<Vec<_>>();
                document
                    .page_texts(&pages, PREVIEW_CHARS + 1, &cancel)
                    .map(Loaded::Text)
                    .map_err(failed)
            }
            Mention::Document(document) => {
                let pages = (0..document.page_count).collect::<Vec<_>>();
                document
                    .page_texts(&pages, PREVIEW_CHARS + 1, &cancel)
                    .map(Loaded::Text)
                    .map_err(failed)
            }
            Mention::PageImages { document, pages } => {
                let images = document.page_images(pages, &cancel).map_err(failed)?;
                images
                    .iter()
                    .map(|image| decode(&image.bytes))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Loaded::Images)
            }
            Mention::Text { text, .. } | Mention::Saved { text, .. } => {
                Ok(Loaded::Text(text.clone()))
            }
        },
        ChipKind::Image(attachment) => {
            let bytes = match &attachment.image {
                AttachedImage::Region(image) => image.bytes.clone(),
                AttachedImage::Picture(image) => image.bytes.clone(),
            };
            decode(&bytes).map(|image| Loaded::Images(vec![image]))
        }
    }
}

/// A picture as an egui image, at most the card's width at 2x.
fn decode(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let image = image::load_from_memory(bytes).map_err(|error| error.to_string())?;
    let longest = (CARD_WIDTH * 2.0) as u32;
    let image = if image.width() > longest || image.height() > longest {
        image.thumbnail(longest, longest)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A load for `key` that answers `text`.
    fn answer(previews: &mut Previews, key: &str, text: &str) {
        let (sender, result) = mpsc::channel();
        sender.send(Ok(Loaded::Text(text.into()))).unwrap();
        previews.jobs.push(Job {
            key: key.into(),
            result,
        });
    }

    #[test]
    fn previews_are_cut_and_answers_nobody_waits_for_are_dropped() {
        let ctx = egui::Context::default();
        let mut previews = Previews::default();
        previews.store("pages:a".into(), Content::Loading);
        let long = format!("## Page 3\n{}", "word ".repeat(1_000));
        answer(&mut previews, "pages:a", &long);
        // Evicted while loading: its answer is not wanted.
        answer(&mut previews, "pages:gone", "late");
        previews.poll(&ctx);
        match &previews.cache[0].1 {
            Content::Text { text, cut } => {
                assert!(*cut, "past the limit");
                assert_eq!(text.chars().count(), PREVIEW_CHARS);
                assert!(text.starts_with("word"), "one page needs no heading");
            }
            _ => panic!("text"),
        }
        assert_eq!(previews.cache.len(), 1, "the stale answer was dropped");
        for index in 0..CACHE + 4 {
            previews.store(format!("key:{index}"), Content::Loading);
        }
        assert_eq!(previews.cache.len(), CACHE, "the oldest go");
    }
}
