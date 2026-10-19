//! The 24pt status bar (frame 02a, pen component `status-seg`): it
//! replaces the floating dock and the progress line. Reading at the left
//! (back and forward, the page, the section, a progress meter to scrub);
//! the view at the right (find, zoom, fit, spread, rotation, translation,
//! the assistant, present). On home it shows the library. Narrow windows
//! drop the least used segments; they stay in the palette and on keys.

use egui::{Align2, Rect, Sense, pos2, vec2};
use glyphst_ui::{
    Tip, menu as ui_menu,
    meter::Meter,
    status::{self, Segment, Side},
};

use super::*;
use crate::shell;

/// What the bar's segments asked for.
#[derive(Default)]
struct Requests {
    actions: Vec<ViewAction>,
    run: Option<Command>,
    find: bool,
    assistant: bool,
    font_step: f32,
    language: Option<String>,
    translate: bool,
    show_translated: Option<bool>,
    scrub: Option<usize>,
    scrub_stopped: bool,
    page: Option<usize>,
}

impl ReaderApp {
    /// The status bar under everything.
    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        match self.active {
            Some(active) => self.document_status(ui, active),
            None => self.home_status(ui),
        }
    }

    fn home_status(&mut self, ui: &mut egui::Ui) {
        // The documents and their size on disk, read again when the count
        // changes.
        let mut paths: std::collections::HashSet<&std::path::Path> = self
            .library
            .history_entries()
            .iter()
            .map(|entry| entry.path.as_path())
            .collect();
        paths.extend(
            self.library
                .library_attachments()
                .iter()
                .map(|attachment| attachment.path.as_path()),
        );
        let count = paths.len();
        if self.library_size.is_none_or(|(known, _)| known != count) {
            let bytes = paths
                .iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .sum();
            self.library_size = Some((count, bytes));
        }
        let bytes = self.library_size.map_or(0, |(_, bytes)| bytes);
        let highlights = self.library.highlight_count();
        // Frame 04g: an import under way, or the last one's tally.
        let import = self.importer.progress().map(|progress| {
            let mut text = if progress.finished {
                format!("added {}", progress.added.len())
            } else {
                format!(
                    "adding {} / {}",
                    progress.processed,
                    progress.total.max(progress.processed)
                )
            };
            if !progress.duplicates.is_empty() {
                text.push_str(&format!(" · {} duplicates", progress.duplicates.len()));
            }
            if !progress.failed.is_empty() {
                text.push_str(&format!(" · {} failed", progress.failed.len()));
            }
            let tip = if progress.finished {
                "dismiss"
            } else {
                "cancel the import (files added stay)"
            };
            (text, progress.finished, tip)
        });
        let mut segments = vec![
            Segment::label(
                "library",
                Side::Left,
                Some("library"),
                format!("library · {count} docs"),
            )
            .static_text(),
            Segment::label("size", Side::Left, None, sidebar::format_size(bytes))
                .static_text()
                .priority(2),
            Segment::label(
                "highlights",
                Side::Left,
                Some("highlighter"),
                format!("{highlights} highlights"),
            )
            .static_text()
            .priority(3),
            Segment::label(
                "version",
                Side::Right,
                None,
                format!("v{} · gpu-vector", env!("CARGO_PKG_VERSION")),
            )
            .static_text()
            .priority(2),
            Segment::label("drop", Side::Right, Some("upload"), "drop files anywhere")
                .static_text()
                .priority(1),
        ];
        // The library's background work (metadata, the AI pass).
        let queued = self.library.library_jobs().len();
        if queued > 0 || self.library_jobs.running() > 0 {
            let text = if self.library_jobs.is_paused() {
                format!("◆ paused · {queued} queued")
            } else {
                format!("◆ working · {queued} queued")
            };
            segments.insert(
                1,
                Segment::label("jobs", Side::Left, Some("sparkles"), text)
                    .accent(!self.library_jobs.is_paused())
                    .static_text()
                    .priority(1),
            );
        }
        // Reading the library's text for search.
        if let Some(search) = &self.library_search {
            let status = search.status();
            let waiting = status.waiting();
            if waiting > 0 {
                let text = if search.is_paused() {
                    format!("text index paused · {waiting} to read")
                } else {
                    let cells = 8;
                    let filled = (status.done * cells / status.docs.max(1)).min(cells);
                    format!(
                        "reading text {}{} {} / {}",
                        "━".repeat(filled),
                        "─".repeat(cells - filled),
                        status.done,
                        status.docs
                    )
                };
                segments.insert(
                    1,
                    Segment::label("index", Side::Left, Some("file-search"), text)
                        .tooltip(match crate::library_search::time_left(&status) {
                            Some(left) => format!("reading the library's text · {left}"),
                            None => "reading the library's text · settings › library".to_string(),
                        })
                        .priority(1),
                );
            }
        }
        if let Some((text, finished, tip)) = import {
            segments.insert(
                1,
                Segment::label("import", Side::Left, Some("download"), text)
                    .accent(!finished)
                    .tooltip(tip),
            );
        }
        for (id, response) in status::bar(ui, segments) {
            if id == "index" && response.clicked() {
                self.settings_window
                    .show_section(crate::settings_window::Section::Library);
            }
            if id == "import" && response.clicked() {
                if self.importer.is_running() {
                    self.importer.cancel();
                } else {
                    self.importer.dismiss();
                }
            }
        }
    }

    fn document_status(&mut self, ui: &mut egui::Ui, active: usize) {
        let tokens = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let tab = &self.tabs[active];
        let pages = tab.layout.len();
        let current = tab.current_page();
        let section = shell::section_path(
            tab.outline
                .iter()
                .map(|entry| (entry.depth, entry.page, entry.title.as_str())),
            current,
        )
        .last()
        .map(|title| title.to_lowercase())
        .unwrap_or_default();
        let ticks = shell::chapter_ticks(
            tab.outline.iter().map(|entry| (entry.depth, entry.page)),
            pages,
        );
        let chapters: Vec<(usize, String)> = tab
            .outline
            .iter()
            .filter_map(|entry| Some((entry.page?, entry.title.clone())))
            .collect();
        let zoom = tab.view.scale / tab.fit_scale() * 100.0;
        let fraction = shell::read_fraction(current, pages);
        let (can_back, can_forward) = (tab.can_go_back(), tab.can_go_forward());
        let reflow = match tab.document_layout {
            DocumentLayout::Reflow(layout) => Some(layout.font_size),
            _ => None,
        };
        let spread = tab.layout.arrangement.spread;
        let rotation = u32::from(tab.layout.arrangement.rotation) * 90;
        let finding = tab.find.open;
        let presenting = tab.present.is_some();
        let translation = tab
            .translation
            .as_ref()
            .map(|session| session.show_translated);
        let target = self
            .translator
            .target_language(self.prefs.target_language.as_deref());
        let source = self
            .user_settings
            .get()
            .and_then(|settings| settings.translation.source_language.clone())
            .unwrap_or_else(|| "auto".into());
        let short = |code: &str| code.split(['-', '_']).next().unwrap_or(code).to_lowercase();
        let language = format!(
            "{} → {}",
            short(&source),
            target.as_deref().map(short).unwrap_or_else(|| "?".into())
        );
        let assistant_open = self.assistant.open;
        let keymap = &self.keymap;
        let tips: Vec<(&str, String)> = [
            ("back", keymap.tip("back", Command::Back)),
            ("forward", keymap.tip("forward", Command::Forward)),
            ("find", keymap.tip("find", Command::Find)),
            ("zoom-out", keymap.tip("zoom out", Command::ZoomOut)),
            ("zoom-in", keymap.tip("zoom in", Command::ZoomIn)),
            (
                "fit",
                keymap.tip("fit width (again: edge to edge)", Command::FitWidth),
            ),
            (
                "zoom-reset",
                "back to 100% (fit width with margins)".to_string(),
            ),
            (
                "spread",
                keymap.tip("two-page spread", Command::ToggleSpread),
            ),
            (
                "rotate",
                keymap.tip("rotate clockwise", Command::RotateClockwise),
            ),
            (
                "assistant",
                keymap.tip("ask about this document", Command::Assistant),
            ),
            ("present", keymap.tip("present", Command::Present)),
            ("smaller", keymap.tip("smaller text", Command::SmallerText)),
            ("larger", keymap.tip("larger text", Command::LargerText)),
        ]
        .into_iter()
        .collect();
        let tip = |id: &str| {
            tips.iter()
                .find(|(key, _)| *key == id)
                .map_or_else(String::new, |(_, tip)| tip.clone())
        };
        // The segments' closures all report here, one after another.
        let requests = std::cell::RefCell::new(Requests::default());
        let report = &requests;
        let page_input = &mut self.page_input;
        let scrubbing = &mut self.chrome.scrub;
        let mut segments = Vec::new();
        let (back_tip, forward_tip) = (tip("back"), tip("forward"));
        let nav = Segment::custom("nav", Side::Left, 48.0, move |ui, rect| {
            let step = |ui: &mut egui::Ui, x: f32, icon: &str, enabled: bool, tip: &str| {
                let cell = Rect::from_center_size(pos2(x, rect.center().y), vec2(18.0, 20.0));
                let response = ui
                    .interact(cell, ui.scope_id().with(icon), Sense::click())
                    .tip(tip);
                let color = if !enabled {
                    tokens.rule_strong
                } else if response.hovered() {
                    tokens.text
                } else {
                    tokens.faint
                };
                icons::paint(ui, cell, icon, 12.0, color);
                enabled && response.clicked()
            };
            if step(ui, rect.left() + 14.0, "arrow-left", can_back, &back_tip) {
                report.borrow_mut().actions.push(ViewAction::Back);
            }
            if step(
                ui,
                rect.left() + 32.0,
                "arrow-right",
                can_forward,
                &forward_tip,
            ) {
                report.borrow_mut().actions.push(ViewAction::Forward);
            }
        })
        .priority(4);
        segments.push(nav);
        // `p 23 / 623`: the field as wide as its digits, the segment as
        // wide as the longest page number needs.
        let page_label = format!("/ {pages}");
        let font = theme::mono(11.0);
        let digit = glyphst_ui::text::width(ui, "0", &font);
        let widest = digit * pages.max(1).to_string().len() as f32;
        let page_width =
            38.0 + widest + 8.0 + glyphst_ui::text::width(ui, &page_label, &font) + 10.0;
        segments.push(
            Segment::custom("page", Side::Left, page_width, move |ui, rect| {
                let y = rect.center().y;
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 16.0, y), vec2(12.0, 12.0)),
                    "file-text",
                    12.0,
                    tokens.faint,
                );
                ui.painter().text(
                    pos2(rect.left() + 28.0, y),
                    Align2::LEFT_CENTER,
                    "p",
                    font.clone(),
                    tokens.muted,
                );
                let shown = if page_input.is_empty() {
                    (current + 1).to_string()
                } else {
                    page_input.clone()
                };
                let width = glyphst_ui::text::width(ui, &shown, &font).clamp(digit, widest) + 2.0;
                let field = Rect::from_min_size(
                    pos2(rect.left() + 38.0, rect.top() + 3.0),
                    vec2(width, rect.height() - 6.0),
                );
                let response = glyphst_ui::text::fitted(ui, |ui| {
                    ui.put(
                        field,
                        egui::TextEdit::singleline(page_input)
                            .font(font.clone())
                            .frame(egui::Frame::NONE)
                            .margin(egui::Margin::ZERO)
                            .desired_width(width)
                            .align(egui::Align2::LEFT_CENTER)
                            .hint_text(
                                egui::RichText::new((current + 1).to_string())
                                    .font(font.clone())
                                    .color(tokens.text),
                            ),
                    )
                });
                if response.has_focus() {
                    ui.painter().hline(
                        field.x_range(),
                        field.bottom(),
                        egui::Stroke::new(1.0, tokens.accent),
                    );
                }
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Ok(page) = page_input.trim().parse::<usize>() {
                        report.borrow_mut().page = Some(page.saturating_sub(1));
                    }
                    page_input.clear();
                }
                ui.painter().text(
                    pos2(field.right() + 6.0, y),
                    Align2::LEFT_CENTER,
                    page_label,
                    font,
                    tokens.muted,
                );
            })
            .priority(9)
            .tooltip("go to page"),
        );
        if !section.is_empty() {
            segments.push(
                Segment::label("section", Side::Left, None, truncate(&section, 28))
                    .static_text()
                    .priority(2),
            );
        }
        // The meter takes what is left of the bar, within limits.
        let meter_width = (ui.max_rect().width() * 0.25).clamp(140.0, 380.0);
        segments.push(
            Segment::custom("progress", Side::Left, meter_width, move |ui, rect| {
                let track = rect.shrink2(vec2(10.0, 0.0));
                let response =
                    ui.interact(track, ui.scope_id().with("scrub"), Sense::click_and_drag());
                let label = format!("{:.0}%", fraction * 100.0);
                let meter = track.with_max_x(track.right() - 2.0);
                // The bar fills smoothly as the page changes.
                let shown = glyphst_ui::motion::smooth(
                    ui.ctx(),
                    ui.scope_id().with("progress-fill"),
                    fraction,
                    glyphst_ui::motion::PANEL,
                );
                Meter::new(shown).bar().label(&label).paint(ui, meter);
                let bar_right =
                    meter.right() - 8.0 - glyphst_ui::text::width(ui, &label, &theme::mono(11.0));
                if response.hovered() || response.dragged() {
                    for tick in &ticks {
                        let x = meter.left() + (bar_right - meter.left()) * tick;
                        ui.painter().vline(
                            x,
                            (rect.center().y - 4.0)..=(rect.center().y + 4.0),
                            egui::Stroke::new(1.0, tokens.muted),
                        );
                    }
                }
                if let Some(pos) = response.hover_pos() {
                    let page = shell::page_at(pos.x, meter.left(), bar_right, pages);
                    let chapter = chapters
                        .iter()
                        .rfind(|(start, _)| *start <= page)
                        .map(|(_, title)| format!("{} · ", title.to_lowercase()))
                        .unwrap_or_default();
                    response
                        .clone()
                        .on_hover_text_at_pointer(format!("{chapter}page {} of {pages}", page + 1));
                }
                if (response.clicked() || response.dragged())
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    report.borrow_mut().scrub =
                        Some(shell::page_at(pos.x, meter.left(), bar_right, pages));
                }
                if response.drag_stopped() {
                    report.borrow_mut().scrub_stopped = true;
                }
            })
            .priority(3),
        );
        // The right side, from the window's edge inwards.
        segments.push(
            Segment::label("present", Side::Right, Some("presentation"), "present")
                .accent(presenting)
                .priority(5)
                .tooltip(tip("present")),
        );
        segments.push(
            Segment::label("assistant", Side::Right, Some("sparkles"), "assistant")
                .accent(assistant_open)
                .priority(8)
                .tooltip(tip("assistant")),
        );
        segments.push(
            Segment::label("language", Side::Right, Some("languages"), language)
                .accent(translation == Some(true))
                .priority(4)
                .tooltip("translation"),
        );
        if reflow.is_none() {
            segments.push(
                Segment::label(
                    "rotate",
                    Side::Right,
                    Some("rotate-cw"),
                    format!("{rotation}°"),
                )
                .priority(1)
                .tooltip(tip("rotate")),
            );
            segments.push(
                Segment::label(
                    "spread",
                    Side::Right,
                    Some("book-open"),
                    if spread { "spread" } else { "single" },
                )
                .priority(1)
                .tooltip(tip("spread")),
            );
        }
        if let Some(size) = reflow {
            let (smaller, larger) = (tip("smaller"), tip("larger"));
            segments.push(
                Segment::custom("text-size", Side::Right, 92.0, move |ui, rect| {
                    let y = rect.center().y;
                    let step = |x: f32, icon: &str, tip: &str| {
                        let cell = Rect::from_center_size(pos2(x, y), vec2(18.0, 20.0));
                        let response = ui
                            .interact(cell, ui.scope_id().with(icon), Sense::click())
                            .tip(tip);
                        icons::paint(
                            ui,
                            cell,
                            icon,
                            12.0,
                            if response.hovered() {
                                tokens.text
                            } else {
                                tokens.faint
                            },
                        );
                        response.clicked()
                    };
                    if step(rect.left() + 16.0, "a-arrow-down", &smaller) {
                        report.borrow_mut().font_step = -1.0;
                    }
                    if step(rect.right() - 16.0, "a-arrow-up", &larger) {
                        report.borrow_mut().font_step = 1.0;
                    }
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        format!("{size:.0} pt"),
                        theme::mono(11.0),
                        tokens.muted,
                    );
                })
                .priority(3),
            );
        }
        segments.push(
            Segment::label("fit", Side::Right, Some("scan"), "fit width")
                .priority(2)
                .tooltip(tip("fit")),
        );
        let (zoom_out, zoom_in, zoom_reset) = (tip("zoom-out"), tip("zoom-in"), tip("zoom-reset"));
        segments.push(
            Segment::custom("zoom", Side::Right, 108.0, move |ui, rect| {
                let y = rect.center().y;
                icons::paint(
                    ui,
                    Rect::from_center_size(pos2(rect.left() + 16.0, y), vec2(12.0, 12.0)),
                    "zoom-in",
                    12.0,
                    tokens.faint,
                );
                let step = |x: f32, glyph: &str, tip: &str| {
                    let cell = Rect::from_center_size(pos2(x, y), vec2(14.0, 20.0));
                    let response = ui
                        .interact(cell, ui.scope_id().with(glyph), Sense::click())
                        .tip(tip);
                    ui.painter().text(
                        cell.center(),
                        Align2::CENTER_CENTER,
                        glyph,
                        theme::mono(11.0),
                        if response.hovered() {
                            tokens.text
                        } else {
                            tokens.muted
                        },
                    );
                    response.clicked()
                };
                if step(rect.left() + 34.0, "-", &zoom_out) {
                    report.borrow_mut().actions.push(ViewAction::ZoomOut);
                }
                if step(rect.right() - 12.0, "+", &zoom_in) {
                    report.borrow_mut().actions.push(ViewAction::ZoomIn);
                }
                // The percentage between the steps resets the zoom.
                let reset = Rect::from_x_y_ranges(
                    (rect.left() + 42.0)..=(rect.right() - 20.0),
                    (y - 10.0)..=(y + 10.0),
                );
                let response = ui
                    .interact(reset, ui.scope_id().with("zoom-reset"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .tip(&zoom_reset);
                if response.clicked() {
                    report.borrow_mut().actions.push(ViewAction::FitMargins);
                }
                ui.painter().text(
                    reset.center(),
                    Align2::CENTER_CENTER,
                    format!("{zoom:.0}%"),
                    theme::mono(11.0),
                    if response.hovered() {
                        tokens.text
                    } else {
                        tokens.muted
                    },
                );
            })
            .priority(6),
        );
        segments.push(
            Segment::label("find", Side::Right, Some("search"), "find")
                .accent(finding)
                .priority(7)
                .tooltip(tip("find")),
        );
        let responses = status::bar(ui, segments);
        let mut requests = requests.into_inner();
        for (id, response) in &responses {
            match *id {
                "present" if response.clicked() => {
                    requests.actions.push(ViewAction::TogglePresent);
                }
                "assistant" if response.clicked() => requests.assistant = true,
                "rotate" if response.clicked() => requests.run = Some(Command::RotateClockwise),
                "spread" if response.clicked() => requests.run = Some(Command::ToggleSpread),
                "fit" if response.clicked() => requests.actions.push(ViewAction::FitWidth),
                "find" if response.clicked() => requests.find = true,
                "language" => {
                    egui::Popup::menu(response).show(|ui| {
                        ui_menu::style(ui);
                        match translation {
                            Some(shown) => {
                                if ui_menu::Item::new("show translated pages")
                                    .icon("globe")
                                    .checked(shown)
                                    .show(ui)
                                    .clicked()
                                {
                                    requests.show_translated = Some(!shown);
                                }
                            }
                            None => {
                                if ui_menu::Item::new("translate this document")
                                    .icon("globe")
                                    .show(ui)
                                    .clicked()
                                {
                                    requests.translate = true;
                                }
                            }
                        }
                        ui_menu::separator(ui);
                        ui_menu::group(ui, "translate into");
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                for choice in glyphst_translation::TARGET_LANGUAGES {
                                    let chosen = target.as_deref() == Some(choice.code);
                                    if ui_menu::Item::new(choice.label)
                                        .checked(chosen)
                                        .show(ui)
                                        .clicked()
                                    {
                                        requests.language = Some(choice.code.to_string());
                                    }
                                }
                            });
                    });
                }
                _ => {}
            }
        }
        if let Some(page) = requests.scrub
            && *scrubbing != Some(page)
        {
            *scrubbing = Some(page);
            requests.actions.push(ViewAction::GoToPage(page));
        }
        if requests.scrub_stopped {
            *scrubbing = None;
        }
        if let Some(page) = requests.page {
            requests.actions.push(ViewAction::GoToPage(page));
        }
        self.pending_actions.extend(requests.actions);
        if requests.find {
            let tab = &mut self.tabs[active];
            if tab.find.open {
                tab.find.close();
            } else {
                let page = tab.current_page();
                tab.find.show(page);
            }
        }
        if requests.font_step != 0.0 {
            self.change_font_size(requests.font_step);
        }
        if let Some(language) = requests.language {
            self.prefs.target_language = Some(language);
        }
        if requests.translate {
            self.start_document_translation(&ctx);
        }
        if let Some(shown) = requests.show_translated
            && let Some(session) = self.tabs[active].translation.as_mut()
        {
            session.show_translated = shown;
        }
        if requests.assistant {
            self.toggle_assistant();
        }
        if let Some(command) = requests.run {
            self.run_or_notify(command, &ctx);
        }
    }
}
