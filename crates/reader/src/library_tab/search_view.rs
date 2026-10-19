//! The search's results in the library tab (frame 04n): each item with its
//! best pages, `p 4` and the passage with the words found marked, the
//! section at the right, and the weaker matches folded.

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};
use glyphst_search::{ItemHit, PageHit};
use glyphst_ui::text;

use super::{LibraryTab, LibraryTabAction, TabContext, authors_short, empty_line};
use crate::theme::{self, Tokens};

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
        let service = context.search.as_deref()?;
        let Some(results) = service.results() else {
            ui.add_space(16.0);
            empty_line(ui, "searching…");
            return None;
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
            return None;
        }
        let strong = results.items.len() - results.weaker;
        let mut action = None;
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
