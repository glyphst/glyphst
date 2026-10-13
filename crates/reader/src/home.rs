//! The home screen, a reading landing page in pdfcraft's component style:
//! the book to continue, recent books as cover cards and the library, in a
//! centred column. Covers are generated from the title (a gradient picked
//! from its hash, its initials and the file type); real covers are not
//! rendered.

use std::path::{Path, PathBuf};

use egui::{
    Align2, Color32, CornerRadius, Mesh, Pos2, Rect, Sense, Stroke, StrokeKind,
    epaint::{CornerRadiusF32, Hsva, tessellator::path::rounded_rectangle},
    pos2, vec2,
};
use glyphst_library::LibraryStore;

use crate::{
    icons,
    theme::{self, Tokens},
    widgets,
};

const RECENT_LIMIT: usize = 24;
/// The content column's widest.
const COLUMN: f32 = 960.0;
const CARD_COVER: egui::Vec2 = vec2(132.0, 176.0);

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
        let full = ui.max_rect();
        let mut entries: Vec<_> = library.history_entries().iter().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                let width = ui.available_width();
                let column = (width - 48.0).clamp(240.0, COLUMN);
                ui.horizontal(|ui| {
                    ui.add_space(((width - column) / 2.0).max(0.0));
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        ui.add_space(36.0);
                        if header(ui, &tokens, picking) {
                            action = Some(HomeAction::OpenDialog);
                        }
                        if let Some(error) = library.startup_error() {
                            ui.add_space(16.0);
                            notice(ui, &tokens, error);
                        }
                        let continuing = entries.iter().find(|entry| entry.path.exists());
                        if let Some(entry) = continuing {
                            ui.add_space(24.0);
                            if hero(ui, &tokens, &entry.path, entry.position.page.max(1)) {
                                action = Some(HomeAction::Open(entry.path.clone()));
                            }
                        }
                        let recent: Vec<_> = entries
                            .iter()
                            .filter(|entry| Some(&entry.path) != continuing.map(|c| &c.path))
                            .take(RECENT_LIMIT)
                            .collect();
                        if !recent.is_empty() {
                            ui.add_space(12.0);
                            widgets::section_title(ui, "Recent");
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = vec2(16.0, 16.0);
                                for entry in recent {
                                    let page = entry.position.page.max(1);
                                    if cover_card(ui, &tokens, &entry.path, page).clicked() {
                                        action = Some(HomeAction::Open(entry.path.clone()));
                                    }
                                }
                            });
                        }
                        if let Some(open) = self.library(ui, library, &tokens) {
                            action = Some(open);
                        }
                        if entries.is_empty() && library.library_items().is_empty() {
                            ui.add_space(48.0);
                            empty(ui, &tokens);
                        }
                        ui.add_space(48.0);
                    });
                });
            });
        drop_hint(ui, &tokens, full);
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
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            widgets::section_title(ui, "Library");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::Frame::new()
                    .fill(tokens.field)
                    .stroke(Stroke::new(1.0, tokens.border))
                    .corner_radius(15)
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(icons::image("search", 14.0, tokens.text_muted));
                            ui.add(
                                egui::TextEdit::singleline(&mut self.filter)
                                    .hint_text("Filter by title or author")
                                    .frame(egui::Frame::NONE)
                                    .desired_width(200.0),
                            );
                        });
                    });
            });
        });
        ui.add_space(4.0);
        let filter = self.filter.to_lowercase();
        ui.spacing_mut().item_spacing.y = 2.0;
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
            if library_row(ui, tokens, &item.title, &detail, &path) {
                action = Some(HomeAction::Open(path));
            }
        }
        action
    }
}

/// The title and the Open button; returns whether Open was clicked.
fn header(ui: &mut egui::Ui, tokens: &Tokens, picking: bool) -> bool {
    let mut open = false;
    ui.horizontal(|ui| {
        ui.add(icons::image("book-open-text", 26.0, tokens.accent));
        ui.vertical(|ui| {
            ui.label(egui::RichText::new("Glyphst Reader").font(theme::semibold(22.0)));
            ui.label(
                egui::RichText::new("Drop PDF, EPUB and other documents anywhere in this window.")
                    .color(tokens.text_muted),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            open = ui
                .add_enabled_ui(!picking, |ui| {
                    widgets::pill_button(ui, Some("folder-open"), "Open documents…", true)
                })
                .inner
                .clicked();
        });
    });
    open
}

fn notice(ui: &mut egui::Ui, tokens: &Tokens, error: &str) {
    egui::Frame::new()
        .fill(tokens.card)
        .stroke(Stroke::new(1.0, tokens.border))
        .corner_radius(10)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(icons::image("triangle-alert", 16.0, tokens.danger));
                ui.label(
                    egui::RichText::new("The library is unavailable").font(theme::semibold(13.0)),
                );
            });
            ui.label(
                egui::RichText::new(format!(
                    "Reading positions are not remembered this session. \
                     Another Glyphst Reader may be running and holding it.\n{error}"
                ))
                .color(tokens.text_muted),
            );
        });
}

/// The book to continue; returns whether Continue was clicked.
fn hero(ui: &mut egui::Ui, tokens: &Tokens, path: &Path, page: usize) -> bool {
    let mut open = false;
    egui::Frame::new()
        .fill(tokens.card)
        .stroke(Stroke::new(1.0, tokens.border))
        .corner_radius(16)
        .shadow(tokens.card_shadow())
        .inner_margin(20)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, cover_response) =
                    ui.allocate_exact_size(vec2(120.0, 160.0), Sense::click());
                paint_cover(ui, rect, &title_of(path), kind_of(path));
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    widgets::section_title(ui, "Continue reading");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(title_of(path)).font(theme::semibold(20.0)),
                        )
                        .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(folder(path)).color(tokens.text_muted),
                        )
                        .truncate(),
                    );
                    ui.label(egui::RichText::new(format!("Page {page}")).color(tokens.text_faint));
                    ui.add_space(12.0);
                    open = widgets::pill_button(ui, Some("book-open"), "Continue", true).clicked();
                });
                open |= cover_response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked();
            });
        });
    open
}

/// A recent book: its cover, title and page.
fn cover_card(ui: &mut egui::Ui, tokens: &Tokens, path: &Path, page: usize) -> egui::Response {
    let exists = path.exists();
    let size = vec2(CARD_COVER.x, CARD_COVER.y + 44.0);
    let (rect, response) = ui.allocate_exact_size(
        size,
        if exists {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let title = title_of(path);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, exists, &title));
    let cover = Rect::from_min_size(rect.min, CARD_COVER);
    let lift = if response.hovered() && exists {
        -3.0
    } else {
        0.0
    };
    let cover = cover.translate(vec2(0.0, lift));
    ui.painter().add(
        egui::Shadow {
            offset: [0, if lift < 0.0 { 8 } else { 4 }],
            blur: if lift < 0.0 { 20 } else { 12 },
            spread: 0,
            color: tokens.page_shadow,
        }
        .as_shape(cover, 8),
    );
    paint_cover(ui, cover, &title, kind_of(path));
    if !exists {
        ui.painter()
            .rect_filled(cover, 8, tokens.pasteboard.gamma_multiply(0.7));
    }
    let color = if exists {
        tokens.text
    } else {
        tokens.text_faint
    };
    widgets::row_text(
        ui,
        pos2(rect.left(), cover.bottom() + 16.0),
        title.as_str(),
        theme::medium(13.0),
        color,
        CARD_COVER.x,
    );
    widgets::row_text(
        ui,
        pos2(rect.left(), cover.bottom() + 34.0),
        if exists {
            format!("Page {page}")
        } else {
            "Missing".to_string()
        },
        theme::regular(11.5),
        tokens.text_faint,
        CARD_COVER.x,
    );
    let response = response.on_hover_text(path.display().to_string());
    if exists {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A library item: a small cover, its title and authors. Returns whether
/// it was clicked; missing files do nothing.
fn library_row(ui: &mut egui::Ui, tokens: &Tokens, title: &str, detail: &str, path: &Path) -> bool {
    let exists = path.exists();
    let (rect, response) = widgets::list_row(ui, 48.0, false);
    let cover = Rect::from_min_size(rect.min + vec2(8.0, 6.0), vec2(27.0, 36.0));
    paint_cover(ui, cover, title, kind_of(path));
    let left = cover.right() + 12.0;
    let width = rect.right() - left - 8.0;
    widgets::row_text(
        ui,
        pos2(left, rect.top() + 16.0),
        title,
        theme::medium(13.0),
        if exists {
            tokens.text
        } else {
            tokens.text_faint
        },
        width,
    );
    widgets::row_text(
        ui,
        pos2(left, rect.top() + 33.0),
        if exists {
            detail
        } else {
            "The file is missing"
        },
        theme::regular(11.5),
        tokens.text_faint,
        width,
    );
    exists && response.clicked()
}

fn empty(ui: &mut egui::Ui, tokens: &Tokens) {
    ui.vertical_centered(|ui| {
        ui.add(icons::image("library", 40.0, tokens.text_faint));
        ui.add_space(8.0);
        ui.label(egui::RichText::new("No books yet").font(theme::semibold(15.0)));
        ui.label(
            egui::RichText::new("Books you open appear here, ready to continue.")
                .color(tokens.text_muted),
        );
    });
}

/// An accent outline over the window while files are dragged over it.
fn drop_hint(ui: &egui::Ui, tokens: &Tokens, rect: Rect) {
    if ui.ctx().input(|i| i.raw.hovered_files.is_empty()) {
        return;
    }
    let area = rect.shrink(12.0);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("drop-hint"),
    ));
    painter.rect(
        area,
        16,
        tokens.accent_soft.gamma_multiply(0.85),
        Stroke::new(2.0, tokens.accent),
        StrokeKind::Inside,
    );
    painter.text(
        area.center(),
        Align2::CENTER_CENTER,
        "Drop to open",
        theme::semibold(20.0),
        tokens.accent_text,
    );
}

/// "PDF", "EPUB"… from the file's extension.
fn kind_of(path: &Path) -> String {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_uppercase())
        .unwrap_or_default()
}

/// The file name without its extension, as a title.
fn title_of(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().replace(['_', '-'], " "))
        .unwrap_or_else(|| path.display().to_string())
}

fn folder(path: &Path) -> String {
    path.parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default()
}

/// The two colours of a generated cover, picked from the title's hash.
fn cover_colors(title: &str) -> (Color32, Color32) {
    // FNV-1a: stable across runs and platforms.
    let hash = title.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    let hue = (hash % 360) as f32 / 360.0;
    let top = Hsva::new(hue, 0.50, 0.82, 1.0);
    let bottom = Hsva::new((hue + 0.08) % 1.0, 0.72, 0.42, 1.0);
    (top.into(), bottom.into())
}

/// The first letters of the title's first two words.
fn initials(title: &str) -> String {
    title
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Paints a generated cover in `rect`: a diagonal gradient, the initials
/// and the file type.
fn paint_cover(ui: &egui::Ui, rect: Rect, title: &str, kind: String) {
    let (top, bottom) = cover_colors(title);
    let radius = (rect.width() * 0.06).clamp(3.0, 8.0);
    let mut outline = Vec::new();
    rounded_rectangle(&mut outline, rect, CornerRadiusF32::same(radius));
    let color_at = |point: Pos2| {
        let t = ((point.x - rect.left()) / rect.width() * 0.35
            + (point.y - rect.top()) / rect.height() * 0.65)
            .clamp(0.0, 1.0);
        top.lerp_to_gamma(bottom, t)
    };
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.center(), color_at(rect.center()));
    for point in &outline {
        mesh.colored_vertex(*point, color_at(*point));
    }
    let count = outline.len() as u32;
    for index in 0..count {
        mesh.add_triangle(0, 1 + index, 1 + (index + 1) % count);
    }
    let painter = ui.painter();
    painter.add(mesh);
    if rect.width() >= 60.0 {
        painter.text(
            rect.left_top() + vec2(rect.width() * 0.12, rect.height() * 0.12),
            Align2::LEFT_TOP,
            initials(title),
            theme::semibold(rect.width() * 0.26),
            Color32::WHITE,
        );
        let badge_font = theme::semibold(10.0);
        let galley = painter.layout_no_wrap(kind, badge_font, Color32::WHITE);
        let badge = Rect::from_min_size(
            rect.left_bottom() + vec2(rect.width() * 0.12, -rect.height() * 0.1 - 18.0),
            galley.size() + vec2(12.0, 6.0),
        );
        painter.rect_filled(badge, CornerRadius::same(9), Color32::from_black_alpha(70));
        painter.galley(badge.min + vec2(6.0, 3.0), galley, Color32::WHITE);
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            initials(title),
            theme::semibold(rect.width() * 0.4),
            Color32::WHITE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covers_are_stable_per_title_and_differ_between_titles() {
        assert_eq!(cover_colors("War and Peace"), cover_colors("War and Peace"));
        assert_ne!(
            cover_colors("War and Peace"),
            cover_colors("Pride and Prejudice")
        );
        assert_eq!(initials("pride and prejudice"), "PA");
        assert_eq!(initials("“Murietta” level 1"), "ML");
        assert_eq!(
            title_of(Path::new("/books/zero2prod_20240903.pdf")),
            "zero2prod 20240903"
        );
        assert_eq!(kind_of(Path::new("/books/a.epub")), "EPUB");
    }
}
