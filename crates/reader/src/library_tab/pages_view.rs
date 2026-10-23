//! Concept pages in the library tab: the pages written and topics
//! suggested for new ones (frame 04p), and one page read with its
//! citations as chips, its sources and what links to it (04q). Pages are
//! edited outside, in the user's editor.

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};
use glyphst_library::PageRecord;
use glyphst_ui::{button::Button, motif, text};
use glyphst_vault::index::Owner;

use super::{LibraryTab, LibraryTabAction, TabContext, View, empty_line, flush_rule, link_row};
use crate::{
    knowledge::{PageAsk, TopicKind},
    theme::{self, Tokens},
};

const ROW: f32 = 32.0;
const SIDE: f32 = 300.0;

impl LibraryTab {
    /// The pages view in `rect` (the centre, with no item pane).
    pub(super) fn pages_part(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        rect: Rect,
    ) -> Option<LibraryTabAction> {
        let inner = rect.shrink2(vec2(28.0, 24.0));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut ui;
        match self.view {
            View::Page(id) => match context.library.page(id).cloned() {
                Some(page) => self.page(ui, tokens, context, &page),
                None => {
                    self.view = View::Pages;
                    None
                }
            },
            _ => self.page_list(ui, tokens, context),
        }
    }

    fn page_list(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let width = ui.available_width();
        let mut action = None;
        let pages = library.pages().cloned().collect::<Vec<_>>();
        let line = heading(
            ui,
            tokens,
            "library / pages",
            "pages",
            &format!("{} pages · notes/concepts", pages.len()),
        );
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(line)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if buttons.add(Button::bracket("write a page…")).clicked() {
            action = Some(LibraryTabAction::WritePage(PageAsk::Asked(String::new())));
        }
        caption(
            ui,
            tokens,
            "written by the ai from your papers, each claim cited · edit them in obsidian or any editor",
        );
        egui::ScrollArea::vertical()
            .id_salt("library-pages")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.add_space(8.0);
                flush_rule(ui, "your pages", None);
                ui.add_space(2.0);
                // Pages being written for the first time have no record yet.
                for (id, title) in context.writing {
                    if library.page(*id).is_none() {
                        page_row(ui, tokens, title, "◌ writing…", "", "", false);
                    }
                }
                if pages.is_empty() && context.writing.is_empty() {
                    empty_line(ui, "no pages yet · write one about a topic below");
                }
                for page in &pages {
                    let mark = if context.writing.iter().any(|(id, _)| *id == page.id) {
                        "◌ writing again…"
                    } else if page.unreviewed {
                        "new · keep or undo"
                    } else {
                        ""
                    };
                    let links = context
                        .vault_index
                        .map(|index| index.backlinks(Owner::Page(page.id)).len())
                        .filter(|count| *count > 0)
                        .map(|count| format!("← {count}"))
                        .unwrap_or_default();
                    if page_row(
                        ui,
                        tokens,
                        &page.title,
                        mark,
                        &page_meta(page),
                        &links,
                        page.unreviewed,
                    )
                    .clicked()
                    {
                        self.view = View::Page(page.id);
                    }
                }
                ui.add_space(12.0);
                flush_rule(
                    ui,
                    "suggested topics · nothing is written until you choose",
                    None,
                );
                ui.add_space(6.0);
                if context.topics.is_empty() {
                    empty_line(
                        ui,
                        "no topics yet · tag a few papers alike, or turn on search by meaning",
                    );
                    return;
                }
                let card_width = ((width - 24.0) / 3.0).floor();
                for row in context.topics.chunks(3) {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        for topic in row {
                            let (rect, _) =
                                ui.allocate_exact_size(vec2(card_width, 110.0), Sense::hover());
                            let mut card = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(rect.shrink2(vec2(12.0, 10.0)))
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            ui.painter().rect_stroke(
                                rect,
                                tokens.ctl_radius(),
                                egui::Stroke::new(1.0, tokens.rule),
                                egui::StrokeKind::Inside,
                            );
                            card.spacing_mut().item_spacing.y = 4.0;
                            let inner = card.available_width();
                            let (top, _) =
                                card.allocate_exact_size(vec2(inner, 18.0), Sense::hover());
                            text::row(
                                &card,
                                pos2(top.left(), top.center().y),
                                &topic.title,
                                theme::mono(13.0),
                                tokens.text,
                                inner - 24.0,
                            );
                            let dismiss = card
                                .interact(
                                    Rect::from_center_size(
                                        pos2(top.right() - 6.0, top.center().y),
                                        vec2(16.0, 16.0),
                                    ),
                                    card.scope_id().with(("dismiss", &topic.key)),
                                    Sense::click(),
                                )
                                .on_hover_text("not this topic")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            card.painter().text(
                                pos2(top.right() - 6.0, top.center().y),
                                Align2::CENTER_CENTER,
                                "×",
                                theme::mono(12.0),
                                if dismiss.hovered() {
                                    tokens.text
                                } else {
                                    tokens.faint
                                },
                            );
                            if dismiss.clicked() {
                                action = Some(LibraryTabAction::DismissTopic(topic.key.clone()));
                            }
                            let kind = match topic.kind {
                                TopicKind::Cluster => {
                                    format!("◇ {} papers close in meaning", topic.items.len())
                                }
                                TopicKind::Tag(_) => format!("# tag · {} items", topic.items.len()),
                            };
                            caption(&mut card, tokens, &kind);
                            let titles = topic
                                .items
                                .iter()
                                .filter_map(|id| library.library_item(*id))
                                .map(|item| item.title.as_str())
                                .collect::<Vec<_>>()
                                .join(" · ");
                            let (line, _) =
                                card.allocate_exact_size(vec2(inner, 18.0), Sense::hover());
                            text::row(
                                &card,
                                pos2(line.left(), line.center().y),
                                titles,
                                theme::regular(12.0),
                                tokens.text_2,
                                inner,
                            );
                            if card.add(Button::bracket("write…")).clicked() {
                                action = Some(LibraryTabAction::WritePage(PageAsk::Topic(
                                    topic.clone(),
                                )));
                            }
                        }
                    });
                    ui.add_space(12.0);
                }
            });
        action
    }

    fn page(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        page: &PageRecord,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let mut action = None;
        let path = context.vault.map(|root| root.join(&page.relative_path));
        let line = heading(ui, tokens, "library / pages", &page.title, &page_meta(page));
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(line)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if let Some(path) = &path
            && buttons
                .add(Button::bracket_muted("open in editor"))
                .clicked()
        {
            action = Some(LibraryTabAction::OpenNote(path.clone()));
        }
        caption(ui, tokens, &format!("notes/{}", page.relative_path));
        let writing = context.writing.iter().any(|(id, _)| *id == page.id);
        if writing || page.unreviewed {
            ui.add_space(6.0);
            let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
            ui.painter().rect_filled(bar, 0, tokens.accent_soft);
            let message = if writing {
                "◌ writing it again · the page stays as it is until then".to_string()
            } else {
                format!(
                    "written from {} sources · {} in · {} out",
                    page.sources.len(),
                    crate::library_dialogs::short_count(page.tokens_in),
                    crate::library_dialogs::short_count(page.tokens_out)
                )
            };
            text::row(
                ui,
                pos2(bar.left() + 12.0, bar.center().y),
                message,
                theme::mono(11.0),
                tokens.accent_text,
                bar.width() - 180.0,
            );
            if !writing {
                let mut buttons = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(bar.shrink2(vec2(8.0, 0.0)))
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                );
                if buttons.add(Button::bracket("keep")).clicked() {
                    action = Some(LibraryTabAction::KeepPage(page.id));
                }
                if buttons.add(Button::bracket_muted("undo")).clicked() {
                    action = Some(LibraryTabAction::UndoPage(page.id));
                }
            }
        }
        ui.add_space(12.0);
        // The page at the left, its sources and backlinks at the right.
        let area = ui.available_rect_before_wrap();
        let side = Rect::from_min_max(pos2(area.right() - SIDE, area.top()), area.max);
        let body = Rect::from_min_max(area.min, pos2(side.left() - 24.0, area.bottom()));
        // The page's text, read again when the file changes.
        let modified = path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok());
        let notes = context.vault_index.map_or(0, |index| index.notes.len());
        let key = (page.id, modified, notes);
        if self.page_text.as_ref().map(|(cached, _)| cached) != Some(&key) {
            let text = path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|note| {
                    crate::knowledge::page_markdown(
                        &note,
                        context.vault_index.unwrap_or(&Default::default()),
                        context.vault,
                    )
                })
                .unwrap_or_else(|| "*The page's file is not in the vault.*".to_string());
            self.page_text = Some((key, text));
        }
        {
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(body)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            egui::ScrollArea::vertical()
                .id_salt(("library-page", page.id))
                .auto_shrink(false)
                .show(&mut ui, |ui| {
                    ui.set_width(body.width());
                    if let Some((_, text)) = &self.page_text {
                        let style = glyphst_markdown::MarkdownStyle::new(tokens, 14.0);
                        glyphst_markdown::show(ui, &mut self.markdown, &style, text);
                    }
                });
        }
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(side)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.spacing_mut().item_spacing.y = 4.0;
        flush_rule(&mut ui, "sources", None);
        for id in &page.sources {
            let Some(item) = library.library_item(*id) else {
                continue;
            };
            let year = item.year.map(|year| year.to_string()).unwrap_or_default();
            if link_row(&mut ui, tokens, "■", &item.title, &year, tokens.text_2).clicked() {
                action = Some(LibraryTabAction::ShowInLibrary(item.id));
            }
        }
        if let Some(index) = context.vault_index {
            let backlinks = index.backlinks(Owner::Page(page.id));
            if !backlinks.is_empty() {
                ui.add_space(10.0);
                flush_rule(&mut ui, "linked from", None);
                for note in backlinks {
                    if let Some(chosen) = backlink_row(&mut ui, tokens, note, context.vault) {
                        action = Some(chosen);
                    }
                }
            }
        }
        ui.add_space(14.0);
        if !writing && ui.add(Button::bracket("write again…")).clicked() {
            action = Some(LibraryTabAction::WritePage(PageAsk::Again(page.id)));
        }
        action
    }
}

/// A note that links here: a page opens in Glyphst, an item shows in the
/// library, the user's note opens in their editor.
pub(super) fn backlink_row(
    ui: &mut Ui,
    tokens: &Tokens,
    note: &glyphst_vault::index::IndexedNote,
    root: Option<&std::path::Path>,
) -> Option<LibraryTabAction> {
    let (detail, chosen) = match note.owner {
        Owner::Page(id) => ("page".to_string(), LibraryTabAction::ShowPage(id)),
        Owner::Item(id) => (
            "note".to_string(),
            LibraryTabAction::ShowInLibrary(glyphst_library::DocumentId(id)),
        ),
        Owner::User => (
            note.path
                .rsplit_once('/')
                .map(|(folder, _)| format!("{folder}/"))
                .unwrap_or_default(),
            LibraryTabAction::OpenNote(root?.join(&note.path)),
        ),
    };
    link_row(ui, tokens, "←", &note.title, &detail, tokens.text_2)
        .clicked()
        .then_some(chosen)
}

/// `── kicker ──`, the heading and its counts; returns the heading's line
/// for buttons at its right.
fn heading(ui: &mut Ui, tokens: &Tokens, kicker: &str, title: &str, counts: &str) -> Rect {
    let width = ui.available_width();
    let (head, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    motif::paint_rule_title(
        ui,
        Rect::from_min_size(head.min, vec2(width.min(420.0), 18.0)),
        kicker,
        None,
        true,
    );
    ui.add_space(6.0);
    let (line, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
    let title = text::line(
        ui,
        pos2(line.left(), line.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::mono(22.0),
        tokens.text,
        width * 0.55,
    );
    text::line(
        ui,
        pos2(title.right() + 12.0, line.center().y + 4.0),
        Align2::LEFT_CENTER,
        counts,
        theme::mono(11.0),
        tokens.muted,
        (line.right() - title.right() - 220.0).max(0.0),
    );
    line
}

fn caption(ui: &mut Ui, tokens: &Tokens, line: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    text::row(
        ui,
        pos2(rect.left(), rect.center().y),
        line,
        theme::mono(11.0),
        tokens.faint,
        rect.width(),
    );
}

/// `4 sources · haiku-5.5 · oct 20`.
fn page_meta(page: &PageRecord) -> String {
    format!(
        "{} sources · {} · {}",
        page.sources.len(),
        page.model,
        super::short_date(page.written_at)
    )
}

fn page_row(
    ui: &mut Ui,
    tokens: &Tokens,
    title: &str,
    mark: &str,
    meta: &str,
    links: &str,
    active: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if active {
        ui.painter().rect_filled(rect, 0, tokens.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0, tokens.hover);
    }
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    glyphst_ui::icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 14.0, y), vec2(13.0, 13.0)),
        "file-text",
        13.0,
        if active {
            tokens.accent_text
        } else {
            tokens.muted
        },
    );
    text::line(
        ui,
        pos2(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        links,
        theme::mono(11.0),
        tokens.faint,
        60.0,
    );
    let meta_rect = text::line(
        ui,
        pos2(rect.right() - 76.0, y),
        Align2::RIGHT_CENTER,
        meta,
        theme::mono(11.0),
        tokens.muted,
        260.0,
    );
    let mark_rect = text::line(
        ui,
        pos2(meta_rect.left() - 16.0, y),
        Align2::RIGHT_CENTER,
        mark,
        theme::mono(11.0),
        tokens.accent_text,
        200.0,
    );
    text::row(
        ui,
        pos2(rect.left() + 30.0, y),
        title,
        theme::regular(13.0),
        tokens.text,
        (mark_rect.left() - rect.left() - 46.0).max(0.0),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
