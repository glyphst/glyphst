//! What the glyph design draws around the pages (frame 02a): corner ticks
//! and a meta line (`p.4 — 612 × 792 pt — page 4 of 15`) on the page being
//! read, and a column of page ticks at the pane's right edge, the current
//! one long and numbered, to jump with.

use egui::{Align2, Rect, Sense, pos2};
use glyphst_ui::{Tip, motif, text as ui_text};

use super::*;
use crate::text_overlay::PageMap;

/// The ticks' column at the pane's right edge.
const MINIMAP_WIDTH: f32 = 22.0;
/// Room kept clear of pages at a pane's right for the ticks.
pub(super) const MINIMAP_GUTTER: f32 = 26.0;
/// Panes narrower than this have no ticks.
pub(super) const MINIMAP_FROM: f32 = 360.0;
/// At most this many ticks; longer documents are sampled.
const MAX_TICKS: usize = 120;

/// How far the page under the meta line has loaded.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// Its upload to the GPU: the share written, and seconds since it was
    /// asked for.
    Uploading(f32, f32),
    /// Its loading animation runs, for this many seconds.
    Generating(f32),
    /// Seconds since it was asked for; its preview may show.
    Loading(f32),
    Loaded,
}

/// The meta line of page `page` (0-based) of `size` points, of `pages`.
fn meta_line(page: usize, [width, height]: [f64; 2], pages: usize, stage: Stage) -> String {
    let p = page + 1;
    let size = format!("{width:.0} × {height:.0} pt");
    match stage {
        Stage::Uploading(written, seconds) => format!(
            "p.{p} — uploading {:.0}% — {size} — {seconds:.1} s",
            written * 100.0
        ),
        Stage::Generating(seconds) => format!("p.{p} — generating — {size} — {seconds:.2} s"),
        Stage::Loading(seconds) => format!("p.{p} — loading — {size} — {seconds:.1} s"),
        Stage::Loaded => format!("p.{p} — {size} — page {p} of {pages}"),
    }
}

impl ReaderApp {
    /// Corner ticks and the meta line on the current page of tab `active`,
    /// and the page ticks of the pane at `rect`.
    pub(super) fn page_chrome(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        rect: Rect,
        maps: &[PageMap],
        active: usize,
        now: Instant,
    ) {
        let tokens = Tokens::get(ui.ctx());
        let tab = &self.tabs[active];
        if tab.present.is_some() || self.zen {
            return;
        }
        // The current page, once it is found wholly on the GPU.
        let mut finished = None;
        let pages = tab.layout.len();
        let current = tab.current_page();
        if let Some(map) = maps.iter().find(|map| map.page == current)
            && map.paper.width() > 160.0
        {
            // The room around the page: neighbours close by leave less.
            let room = |side: f32| {
                maps.iter()
                    .filter(|other| other.page != current)
                    .filter(|other| other.paper.x_range().intersects(map.paper.x_range()))
                    .map(|other| {
                        if side < 0.0 {
                            map.paper.top() - other.paper.bottom()
                        } else {
                            other.paper.top() - map.paper.bottom()
                        }
                    })
                    .filter(|gap| *gap >= 0.0)
                    .fold(f32::INFINITY, f32::min)
            };
            let (above, below) = (room(-1.0), room(1.0));
            let offset = (above.min(below) / 2.0 - 1.0).min(10.0);
            if offset >= 3.0 {
                motif::corner_ticks(ui, map.paper, offset, 12.0, tokens.accent);
            }
            let [width, height] = tab.layout.page_size(current);
            let key = PageKey {
                tab: tab.id,
                page: current,
            };
            let since = tab
                .loading_since(current)
                .map(|since| now.saturating_duration_since(since).as_secs_f32());
            let (complete, upload, previewing) = self
                .with_gpu(|gpu| {
                    (
                        gpu.is_complete(key),
                        gpu.upload_progress(key),
                        gpu.is_previewing(key),
                    )
                })
                .unwrap_or((false, None, false));
            if complete && since.is_some() {
                finished = Some(current);
            }
            // Until the page is wholly on the GPU the line says how far it
            // is, in the accent (frame 02b).
            let stage = match (upload, self.generations.loading(key, now)) {
                (Some(written), _) => Stage::Uploading(written, since.unwrap_or_default()),
                (None, Some(seconds)) => Stage::Generating(seconds),
                (None, None) if !complete && (previewing || since.is_some()) => {
                    Stage::Loading(since.unwrap_or_default())
                }
                (None, None) => Stage::Loaded,
            };
            let loading = stage != Stage::Loaded;
            if loading {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            let meta = match tab.translation.as_ref().filter(|session| {
                session.show_translated && session.previews.contains_key(&current)
            }) {
                Some(session) => format!(
                    "p.{} — translated → {} — {}",
                    current + 1,
                    session.target.to_lowercase(),
                    self.user_settings.get().map_or("ai", |settings| settings
                        .translation
                        .active_provider
                        .as_str()),
                ),
                None => meta_line(current, [width, height], pages, stage),
            };
            let top = map.paper.top() - 13.0;
            if top - 6.0 > rect.top() && above >= 26.0 {
                let line = Rect::from_min_max(
                    pos2(map.paper.left() + 18.0, top - 7.0),
                    pos2(map.paper.right() - 18.0, top + 7.0),
                );
                let painter = painter.with_clip_rect(rect);
                let color = if loading {
                    tokens.accent_text
                } else {
                    tokens.faint
                };
                let galley = painter.layout_no_wrap(meta, theme::mono(11.0), color);
                if galley.size().x <= line.width() {
                    painter.galley(
                        pos2(line.left(), line.center().y - galley.size().y / 2.0),
                        galley,
                        color,
                    );
                }
            }
        }
        if let Some(page) = finished {
            self.tabs[active].finished_loading(page);
        }
        self.minimap(ui, rect, active, pages, current);
    }

    /// The page ticks at the right edge of the pane at `rect`; a click
    /// goes to a page.
    fn minimap(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        active: usize,
        pages: usize,
        current: usize,
    ) {
        if pages < 2 || rect.height() < 160.0 || rect.width() < MINIMAP_FROM {
            return;
        }
        let tokens = Tokens::get(ui.ctx());
        let column = Rect::from_min_max(
            pos2(rect.right() - MINIMAP_WIDTH, rect.top() + 24.0),
            pos2(rect.right() - 4.0, rect.bottom() - 24.0),
        );
        let ticks = pages.min(MAX_TICKS);
        // Ticks 7 points apart at most: short documents keep to the top.
        let step = (column.height() / ticks as f32).min(7.0);
        let y_of = |tick: usize| column.top() + step * (tick as f32 + 0.5);
        let tick_of = |page: usize| page * ticks / pages;
        let response = ui
            .interact(
                Rect::from_min_max(column.min, pos2(column.right(), y_of(ticks - 1) + step)),
                ui.id().with(("minimap", self.tabs[active].id)),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let painter = ui.painter_at(rect);
        let hover = response.hover_pos().map(|pos| {
            let tick = (((pos.y - column.top()) / step).floor().max(0.0) as usize).min(ticks - 1);
            (tick * pages / ticks).min(pages - 1)
        });
        let x = column.right() - 4.0;
        for tick in 0..ticks {
            let y = y_of(tick);
            let (length, color) = if tick == tick_of(current) {
                continue;
            } else if hover.is_some_and(|page| tick_of(page) == tick) {
                (8.0, tokens.text_2)
            } else {
                (5.0, tokens.rule_strong)
            };
            painter.hline((x - length)..=x, y, egui::Stroke::new(1.0, color));
        }
        let y = y_of(tick_of(current));
        painter.hline((x - 10.0)..=x, y, egui::Stroke::new(1.5, tokens.accent));
        ui_text::line(
            ui,
            pos2(x - 14.0, y),
            Align2::RIGHT_CENTER,
            (current + 1).to_string(),
            theme::mono(9.0),
            tokens.accent_text,
            40.0,
        );
        if let Some(page) = hover {
            let response = response.clone().tip(&format!("page {}", page + 1));
            if (response.clicked() || response.dragged()) && page != current {
                self.pending_actions.push(ViewAction::GoToPage(page));
            }
        }
    }

    /// The presentation's bar (frame 02g): the keys at the left, the page
    /// meter in the middle, the document, section and clock at the right,
    /// and the progress along the window's bottom edge.
    pub(super) fn present_overlay(&self, ctx: &egui::Context, active: usize, now: Instant) {
        let tokens = Tokens::get(ctx);
        let tab = &self.tabs[active];
        let pages = tab.layout.len().max(1);
        let current = tab.current_page();
        let screen = ctx.content_rect();
        let bar = Rect::from_min_max(pos2(screen.left(), screen.bottom() - 40.0), screen.max);
        let elapsed = self.present_started.map_or(0, |started| {
            now.saturating_duration_since(started).as_secs()
        });
        let clock = format!(
            "{:02}:{:02}:{:02}",
            elapsed / 3600,
            elapsed / 60 % 60,
            elapsed % 60
        );
        let file = tab
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let section = crate::shell::section_path(
            tab.outline
                .iter()
                .map(|entry| (entry.depth, entry.page, entry.title.as_str())),
            current,
        )
        .last()
        .map(|title| format!(" · {}", title.to_lowercase()))
        .unwrap_or_default();
        let back = self
            .keymap
            .label(Command::Present)
            .unwrap_or_else(|| "esc".into());
        egui::Area::new(egui::Id::new("present-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(bar.min)
            .interactable(false)
            .show(ctx, |ui| {
                let painter = ui.painter();
                let font = theme::mono(11.0);
                let y = bar.center().y;
                painter.text(
                    pos2(bar.left() + 24.0, y),
                    Align2::LEFT_CENTER,
                    format!(
                        "[esc] exit   [←][→] page   [{}] present",
                        glyphst_ui::key::display(&back)
                    ),
                    font.clone(),
                    tokens.faint,
                );
                let meter = Rect::from_center_size(bar.center(), egui::vec2(180.0, 12.0));
                let width = pages.to_string().len().max(2);
                painter.text(
                    pos2(meter.left() - 10.0, y),
                    Align2::RIGHT_CENTER,
                    format!("{:0width$}", current + 1),
                    theme::mono(12.0),
                    tokens.accent_text,
                );
                glyphst_ui::meter::Meter::new(crate::shell::read_fraction(current, pages))
                    .bar()
                    .color(tokens.muted)
                    .paint(ui, meter);
                painter.text(
                    pos2(meter.right() + 10.0, y),
                    Align2::LEFT_CENTER,
                    format!("{pages:0width$}"),
                    theme::mono(12.0),
                    tokens.faint,
                );
                // Cut to the room right of the meter.
                ui_text::line(
                    ui,
                    pos2(bar.right() - 24.0, y),
                    Align2::RIGHT_CENTER,
                    format!("{file}{section} · {clock}"),
                    font,
                    tokens.faint,
                    (bar.right() - 24.0 - meter.right() - 60.0).max(0.0),
                );
                let read = crate::shell::read_fraction(current, pages);
                painter.rect_filled(
                    Rect::from_min_size(
                        pos2(bar.left(), bar.bottom() - 2.0),
                        egui::vec2(bar.width() * read, 2.0),
                    ),
                    0,
                    tokens.accent,
                );
            });
        // The clock ticks.
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meta_line_says_how_far_the_page_has_loaded() {
        let line = |stage| meta_line(0, [3456.0, 2592.4], 12, stage);
        assert_eq!(
            line(Stage::Generating(1.416)),
            "p.1 — generating — 3456 × 2592 pt — 1.42 s"
        );
        assert_eq!(
            line(Stage::Loading(4.23)),
            "p.1 — loading — 3456 × 2592 pt — 4.2 s"
        );
        assert_eq!(
            line(Stage::Uploading(0.634, 11.06)),
            "p.1 — uploading 63% — 3456 × 2592 pt — 11.1 s"
        );
        assert_eq!(line(Stage::Loaded), "p.1 — 3456 × 2592 pt — page 1 of 12");
    }
}
