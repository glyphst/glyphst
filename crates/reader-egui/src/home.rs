//! The home tab, following pdfcraft's `home.rs`: an open card, recent
//! documents (from the library's history) and the library's items.

use std::path::{Path, PathBuf};

use glyphst_library::LibraryStore;

use crate::theme::Tokens;

const RECENT_LIMIT: usize = 24;

pub enum HomeAction {
    Open(PathBuf),
    OpenDialog,
}

#[derive(Default)]
pub struct Home {
    filter: String,
}

impl Home {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        library: &LibraryStore,
        picking: bool,
    ) -> Option<HomeAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    ui.heading("Glyphst Reader");
                    ui.add_space(8.0);
                    card(ui, &tokens, |ui| {
                        ui.horizontal(|ui| {
                            let open =
                                ui.add_enabled(!picking, egui::Button::new("Open documents…"));
                            if open.clicked() {
                                action = Some(HomeAction::OpenDialog);
                            }
                            ui.label(
                                egui::RichText::new(
                                    "or drop PDF, EPUB and other documents on the window",
                                )
                                .color(tokens.text_muted),
                            );
                        });
                    });
                    if let Some(error) = library.startup_error() {
                        ui.add_space(8.0);
                        card(ui, &tokens, |ui| {
                            ui.label(egui::RichText::new("The library is unavailable").strong());
                            ui.label(
                                egui::RichText::new(format!(
                                    "Reading positions are not remembered this session. \
                                     The GPUI Reader may be running and holding it.\n{error}"
                                ))
                                .color(tokens.text_muted),
                            );
                        });
                    }
                    ui.add_space(16.0);
                    action = action.take().or_else(|| recent(ui, library, &tokens));
                    ui.add_space(16.0);
                    action = action.take().or_else(|| self.library(ui, library, &tokens));
                });
            });
        });
        action
    }

    fn library(
        &mut self,
        ui: &mut egui::Ui,
        library: &LibraryStore,
        tokens: &Tokens,
    ) -> Option<HomeAction> {
        let items = library.library_items();
        if items.is_empty() {
            return None;
        }
        let mut action = None;
        ui.horizontal(|ui| {
            ui.heading("Library");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter).hint_text("Filter by title or author"),
            );
        });
        let filter = self.filter.to_lowercase();
        for item in items {
            let creators: Vec<&str> = item
                .creators
                .iter()
                .map(|creator| creator.name.as_str())
                .collect();
            let matches = filter.is_empty()
                || item.title.to_lowercase().contains(&filter)
                || creators
                    .iter()
                    .any(|name| name.to_lowercase().contains(&filter));
            if !matches {
                continue;
            }
            let Some(path) = library.primary_attachment(item.id).map(|a| a.path.clone()) else {
                continue;
            };
            let mut detail = creators.join(", ");
            if let Some(year) = item.year {
                detail = if detail.is_empty() {
                    year.to_string()
                } else {
                    format!("{detail} · {year}")
                };
            }
            if row(ui, tokens, &item.title, &detail, path.exists()).clicked() {
                action = Some(HomeAction::Open(path));
            }
        }
        action
    }
}

fn recent(ui: &mut egui::Ui, library: &LibraryStore, tokens: &Tokens) -> Option<HomeAction> {
    let mut entries: Vec<_> = library.history_entries().iter().collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
    ui.heading("Recent");
    let mut action = None;
    for entry in entries.into_iter().take(RECENT_LIMIT) {
        let detail = format!("{} · page {}", folder(&entry.path), entry.position.page + 1);
        if row(
            ui,
            tokens,
            &file_name(&entry.path),
            &detail,
            entry.path.exists(),
        )
        .clicked()
        {
            action = Some(HomeAction::Open(entry.path.clone()));
        }
    }
    action
}

/// A clickable list row; missing files are shown dimmed and do nothing.
fn row(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    title: &str,
    detail: &str,
    exists: bool,
) -> egui::Response {
    let text_color = if exists {
        tokens.text
    } else {
        tokens.text_muted
    };
    let response = ui
        .add_enabled_ui(exists, |ui| {
            ui.add(
                egui::Button::new(two_lines(title, text_color, detail, tokens))
                    .frame(false)
                    .min_size(egui::vec2(ui.available_width().min(720.0), 0.0)),
            )
        })
        .inner;
    if !exists {
        return response.on_disabled_hover_text("The file is missing");
    }
    response
}

/// A title line with a smaller, muted detail line under it.
fn two_lines(
    title: &str,
    title_color: egui::Color32,
    detail: &str,
    tokens: &Tokens,
) -> egui::WidgetText {
    let style = egui::Style::default();
    let mut job = egui::text::LayoutJob::default();
    for text in [
        egui::RichText::new(format!("{title}\n")).color(title_color),
        egui::RichText::new(detail).small().color(tokens.text_muted),
    ] {
        text.append_to(
            &mut job,
            &style,
            egui::FontSelection::Default,
            egui::Align::LEFT,
        );
    }
    job.into()
}

fn card(ui: &mut egui::Ui, tokens: &Tokens, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(tokens.card)
        .stroke(egui::Stroke::new(1.0, tokens.border))
        .corner_radius(tokens.radius)
        .inner_margin(12.0)
        .show(ui, add);
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn folder(path: &Path) -> String {
    path.parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default()
}
