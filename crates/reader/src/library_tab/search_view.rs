//! The search's results in the library tab (frame 04n): each item with its
//! best pages, `p 4` and the passage with the words found marked, the
//! section at the right, and the weaker matches folded.

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};
use glyphst_search::{ItemHit, PageHit};
use glyphst_ui::{button::Button, text};

use super::{LibraryTab, LibraryTabAction, TabContext, authors_short, empty_line};
use crate::{
    meaning::{MeaningAction, MeaningCard},
    theme::{self, Tokens},
};

const ITEM_ROW: f32 = 26.0;
const PAGE_ROW: f32 = 22.0;
const MORE_ROW: f32 = 16.0;

impl LibraryTab {
    pub(super) fn search_part(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        width: f32,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let mut action = None;
        if let Some(card) = &context.meaning {
            if let Some(chosen) = meaning_card(ui, tokens, card, width) {
                action = Some(LibraryTabAction::Meaning(chosen));
            }
            ui.add_space(12.0);
        }
        let service = context.search.as_deref()?;
        let Some(results) = service.results() else {
            ui.add_space(16.0);
            empty_line(ui, "searching…");
            return action;
        };
        if results.items.is_empty() {
            ui.add_space(16.0);
            empty_line(
                ui,
                if service.is_stale() {
                    "searching…"
                } else {
                    "no good matches"
                },
            );
            return action;
        }
        let strong = results.items.len() - results.weaker;
        egui::ScrollArea::vertical()
            .id_salt("library-search")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                for hit in &results.items[..strong] {
                    if let Some(chosen) = self.hit(ui, tokens, library, hit, width) {
                        action = Some(chosen);
                    }
                }
                if strong == 0 {
                    ui.add_space(16.0);
                    empty_line(ui, "no good matches");
                    ui.add_space(8.0);
                }
                if results.weaker > 0 {
                    let label = format!(
                        "{} {} weaker {}",
                        if self.weaker_open { "▾" } else { "▸" },
                        results.weaker,
                        if results.weaker == 1 {
                            "match"
                        } else {
                            "matches"
                        }
                    );
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(width, 36.0), Sense::click());
                    let label_rect = text::line(
                        ui,
                        pos2(rect.left() + 8.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        &label,
                        theme::mono(11.0),
                        if response.hovered() {
                            tokens.text
                        } else {
                            tokens.muted
                        },
                        width / 2.0,
                    );
                    ui.painter().hline(
                        label_rect.right() + 8.0..=rect.right() - 8.0,
                        rect.center().y,
                        tokens.hairline(),
                    );
                    if response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.weaker_open = !self.weaker_open;
                    }
                    if self.weaker_open {
                        for hit in &results.items[strong..] {
                            if let Some(chosen) = self.hit(ui, tokens, library, hit, width) {
                                action = Some(chosen);
                            }
                        }
                    }
                }
            });
        action
    }

    /// One item and its pages.
    fn hit(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &glyphst_library::LibraryStore,
        hit: &ItemHit,
        width: f32,
    ) -> Option<LibraryTabAction> {
        let id = glyphst_library::DocumentId(hit.item_id);
        let item = library.library_item(id)?;
        let mut action = None;
        let height = 6.0
            + ITEM_ROW
            + hit.pages.len() as f32 * PAGE_ROW
            + if hit.more_pages > 0 { MORE_ROW } else { 0.0 }
            + 8.0;
        let (group, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
        let selected = self.selected == Some(id);
        if selected {
            ui.painter().rect_filled(group, 0, tokens.accent_soft);
        }
        ui.painter()
            .hline(group.x_range(), group.bottom() - 0.5, tokens.hairline());

        // The item: title, authors, year, tags.
        let row = Rect::from_min_size(group.min + vec2(8.0, 6.0), vec2(width - 16.0, ITEM_ROW));
        let response = ui.interact(
            row,
            egui::Id::unique(("library-search-item", hit.item_id)),
            Sense::click(),
        );
        let y = row.center().y;
        let mut right = row.right();
        let tags = library
            .tags_for_item(id)
            .iter()
            .map(|tag| format!("#{}", tag.name))
            .collect::<Vec<_>>()
            .join(" ");
        for (value, column) in [
            (tags, 150.0),
            (
                item.year.map(|year| year.to_string()).unwrap_or_default(),
                44.0,
            ),
            (authors_short(item), 130.0),
        ] {
            right -= column;
            text::row(
                ui,
                pos2(right, y),
                value,
                theme::mono(11.0),
                tokens.muted,
                column - 8.0,
            );
            right -= 12.0;
        }
        text::row(
            ui,
            pos2(row.left(), y),
            &item.title,
            theme::regular(13.0),
            tokens.text,
            right - row.left() - 8.0,
        );
        if response.clicked() {
            self.selected = Some(id);
        }
        if response.double_clicked()
            && let Some(attachment) = library.primary_attachment(id)
        {
            action = Some(LibraryTabAction::Open(attachment.path.clone()));
        }

        // Its pages.
        let mut top = row.bottom();
        for page in &hit.pages {
            let line = Rect::from_min_size(
                pos2(row.left() + 12.0, top),
                vec2(row.width() - 12.0, PAGE_ROW),
            );
            if page_line(ui, tokens, line, page).clicked() {
                self.selected = Some(id);
                action = Some(LibraryTabAction::OpenAt {
                    path: page.path.clone(),
                    page: page.page,
                    query: Some(search_words(&self.filter)),
                    anchor: Some(glyphst_core::SemanticTextAnchor::new(
                        page.doc_offset,
                        page.quote.clone(),
                        page.prefix.clone(),
                        page.suffix.clone(),
                    )),
                });
            }
            top += PAGE_ROW;
        }
        if hit.more_pages > 0 {
            let more = if hit.more_pages == 1 {
                "+ 1 more page".to_string()
            } else {
                format!("+ {} more pages", hit.more_pages)
            };
            text::row(
                ui,
                pos2(row.left() + 58.0, top + MORE_ROW / 2.0),
                more,
                theme::mono(10.0),
                tokens.faint,
                row.width(),
            );
        }
        action
    }
}

/// Search by meaning's card (frame 04o): what it is and its size, then
/// its download as it goes, and what went wrong.
fn meaning_card(
    ui: &mut Ui,
    tokens: &Tokens,
    card: &MeaningCard,
    width: f32,
) -> Option<MeaningAction> {
    use crate::meaning::{megabytes, speed};
    let mut action = None;
    let failed = matches!(
        card,
        MeaningCard::Failed { .. } | MeaningCard::EngineFailed { .. }
    );
    let (title, color) = match card {
        MeaningCard::Offer { .. } => ("◇ search by meaning".to_string(), tokens.text),
        MeaningCard::Downloading { what, .. } => (format!("◇ downloading · {what}"), tokens.text),
        MeaningCard::Retrying { seconds, .. } => (
            format!("◇ network lost · again in {seconds} s"),
            tokens.text_2,
        ),
        MeaningCard::Paused { .. } => ("◇ paused".to_string(), tokens.text_2),
        MeaningCard::Failed { title, .. } => (format!("◇ {title}"), tokens.err),
        MeaningCard::Starting => ("◇ starting search by meaning".to_string(), tokens.text_2),
        MeaningCard::EngineFailed { .. } => ("◇ engine did not start".to_string(), tokens.err),
        MeaningCard::OnCpu => (
            "◇ search by meaning is on · on the CPU".to_string(),
            tokens.text_2,
        ),
        MeaningCard::Ready => ("search by meaning is on".to_string(), tokens.accent_text),
    };
    egui::Frame::new()
        .fill(tokens.surface)
        .stroke(egui::Stroke::new(
            1.0,
            if failed { tokens.err } else { tokens.rule },
        ))
        .corner_radius(tokens.ctl_radius())
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(width - 24.0);
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&title)
                        .font(theme::mono(12.0))
                        .color(color),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let mut button = |label: &str, accent: bool, chosen: MeaningAction| {
                        let button = if accent {
                            Button::bracket(label)
                        } else {
                            Button::bracket_muted(label)
                        };
                        if ui.add(button).clicked() {
                            action = Some(chosen);
                        }
                    };
                    match card {
                        MeaningCard::Offer { kept, .. } => {
                            let label = if *kept > 0 { "resume" } else { "download" };
                            button(label, true, MeaningAction::Download);
                            button("not now", false, MeaningAction::NotNow);
                        }
                        MeaningCard::Downloading { .. } | MeaningCard::Retrying { .. } => {
                            button("pause", false, MeaningAction::Pause);
                        }
                        MeaningCard::Paused { .. } => {
                            button("resume", true, MeaningAction::Download);
                            button("discard", false, MeaningAction::Discard);
                        }
                        MeaningCard::Failed { again, .. } => {
                            let label = if *again { "download again" } else { "retry" };
                            button(label, true, MeaningAction::Download);
                        }
                        MeaningCard::EngineFailed { .. } => {
                            button("retry", true, MeaningAction::Download);
                            button("settings", false, MeaningAction::Settings);
                        }
                        MeaningCard::Starting => {}
                        MeaningCard::OnCpu | MeaningCard::Ready => {
                            button("settings", false, MeaningAction::Settings);
                        }
                    }
                });
            });
            let body = match card {
                MeaningCard::Offer { total, .. } => {
                    let model = glyphst_embed::manifest::MODEL.size;
                    let mb = |bytes: u64| (bytes as f64 / 1_000_000.0).round() as u64;
                    let size = if *total > model {
                        format!(
                            "a {} MB model and a {} MB engine (llama.cpp)",
                            mb(model),
                            mb(*total - model)
                        )
                    } else {
                        format!("a {} MB model", mb(model))
                    };
                    Some(format!(
                        "Also find passages that say it in other words. EmbeddingGemma 2 runs on this computer: {size}, downloaded once."
                    ))
                }
                MeaningCard::Failed { detail, .. } if !detail.is_empty() => Some(detail.clone()),
                MeaningCard::OnCpu => Some(
                    "No GPU it can use was found, so passages are read more slowly.".to_string(),
                ),
                _ => None,
            };
            if let MeaningCard::EngineFailed { log } = card {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(log)
                            .font(theme::mono(10.0))
                            .color(tokens.faint),
                    )
                    .wrap(),
                );
            }
            if let Some(body) = body {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(body)
                            .font(theme::regular(12.0))
                            .color(tokens.text_2),
                    )
                    .wrap(),
                );
            }
            let progress = match card {
                MeaningCard::Downloading { done, total, .. }
                | MeaningCard::Retrying { done, total, .. }
                | MeaningCard::Paused { done, total }
                | MeaningCard::Failed { done, total, .. }
                    if *total > 0 && *done > 0 =>
                {
                    Some((*done, *total))
                }
                _ => None,
            };
            if let Some((done, total)) = progress {
                let kept = !matches!(card, MeaningCard::Downloading { .. });
                let label = format!("{}{}", megabytes(done, total), if kept { " kept" } else { "" });
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
                glyphst_ui::meter::Meter::new(done as f32 / total as f32)
                    .bar()
                    .width(rect.width() - 150.0)
                    .label(&label)
                    .paint(ui, rect);
            }
            if let MeaningCard::Downloading {
                rate, seconds_left, ..
            } = card
            {
                text::row(
                    ui,
                    ui.cursor().min + vec2(0.0, 6.0),
                    format!("{} · keeps going if you quit", speed(*rate, *seconds_left)),
                    theme::mono(10.0),
                    tokens.faint,
                    ui.available_width(),
                );
                ui.add_space(12.0);
            }
        });
    action
}

/// `p 4 · the passage, its words marked · § section`.
fn page_line(ui: &mut Ui, tokens: &Tokens, rect: Rect, hit: &PageHit) -> egui::Response {
    let response = ui
        .interact(
            rect,
            egui::Id::unique(("library-search-page", hit.chunk_id)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let y = rect.center().y;
    text::row(
        ui,
        pos2(rect.left(), y),
        format!("p {}", hit.page + 1),
        theme::mono(11.0),
        tokens.accent_text,
        36.0,
    );
    let section_width = if hit.section.is_empty() { 0.0 } else { 190.0 };
    if section_width > 0.0 {
        text::row(
            ui,
            pos2(rect.right() - section_width, y),
            format!("§ {}", hit.section),
            theme::mono(10.0),
            tokens.faint,
            section_width,
        );
    }
    // The passage, with the words found on the accent's soft fill.
    let width = (rect.width() - 46.0 - section_width - 10.0).max(0.0);
    let mut job = egui::text::LayoutJob::default();
    let chars = hit.snippet.chars().collect::<Vec<_>>();
    let mut at = 0;
    let base = |color| egui::TextFormat {
        font_id: theme::regular(12.0),
        color,
        ..Default::default()
    };
    for range in &hit.highlights {
        let start = range.start.min(chars.len());
        let end = range.end.min(chars.len());
        if at < start {
            job.append(
                &chars[at..start].iter().collect::<String>(),
                0.0,
                base(tokens.text_2),
            );
        }
        job.append(
            &chars[start..end].iter().collect::<String>(),
            0.0,
            egui::TextFormat {
                background: tokens.accent_soft,
                ..base(tokens.text)
            },
        );
        at = end;
    }
    if at < chars.len() {
        job.append(
            &chars[at..].iter().collect::<String>(),
            0.0,
            base(tokens.text_2),
        );
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let color = if response.hovered() {
        tokens.text
    } else {
        tokens.text_2
    };
    ui.painter().galley(
        pos2(rect.left() + 46.0, y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
}

/// The filter's words that search the text: not `by:`, `tag:`, `#`, `year:`.
pub fn search_words(filter: &str) -> String {
    filter
        .split_whitespace()
        .filter(|word| !is_facet(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The filter's `by:`/`tag:`/`year:` words, which still filter the items.
pub fn filter_words(filter: &str) -> String {
    filter
        .split_whitespace()
        .filter(|word| is_facet(word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_facet(word: &str) -> bool {
    let lower = word.to_lowercase();
    ["by:", "tag:", "#", "year:"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facets_filter_and_the_other_words_search() {
        assert_eq!(
            search_words("multi-head attention tag:nlp year:>2017 by:vaswani"),
            "multi-head attention"
        );
        assert_eq!(
            filter_words("multi-head attention tag:nlp year:>2017 by:vaswani"),
            "tag:nlp year:>2017 by:vaswani"
        );
    }
}
