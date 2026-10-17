//! Gamepad mode over a book (frames 09d–i): the section and page chips,
//! the hint bar that fades a few seconds after the last input, and the
//! contents, go-to-page and menu sheets.

use egui::{Align2, Color32, Rect, pos2, vec2};
use glyphst_ui::{
    icons, motion,
    pad_glyph::{Arm, Face, Glyph, Hint},
    theme::{self, Tokens},
};

use super::{Couch, MenuItem, Overlay, draw};
use crate::document::OutlineEntry;

pub struct ReaderView<'a> {
    pub title: &'a str,
    pub section: String,
    /// The page being read, from 0.
    pub page: usize,
    pub pages: usize,
    pub outline: &'a [OutlineEntry],
    /// The entry being read.
    pub current: Option<usize>,
    pub overview: bool,
    /// The menu's values.
    pub text_size: f32,
    pub theme: &'static str,
    pub smooth: bool,
    pub fit_page: bool,
    /// Gamepad mode: the chips and the reading hints show (the go-to sheet
    /// also works outside it).
    pub couch: bool,
}

/// `1.15×`, `1×`.
fn text_size(size: f32) -> String {
    let text = format!("{size:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}×")
}

const ROW: f32 = 52.0;

pub fn show(ctx: &egui::Context, couch: &Couch, view: &ReaderView<'_>) {
    let screen = ctx.content_rect();
    let now = ctx.input(|input| input.time);
    let hints = motion::smooth_bool(
        ctx,
        egui::Id::new("couch-hints"),
        now < couch.hints_until || couch.overlay.is_some() || view.overview,
        motion::PANEL,
    );
    if now < couch.hints_until {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(couch.hints_until - now));
    }
    egui::Area::new(egui::Id::new("couch-reader"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_clip_rect(screen);
            let t = Tokens::get(ctx);
            if couch.overlay.is_some() {
                ui.painter().rect_filled(screen, 0, t.scrim);
            }
            if view.couch && !view.overview {
                let mut chips = ui.new_child(egui::UiBuilder::new());
                chips.multiply_opacity(hints.max(0.35));
                if !view.section.is_empty() {
                    draw::chip(
                        &chips,
                        screen.left_top() + vec2(24.0, 20.0),
                        Align2::LEFT_TOP,
                        &view.section,
                        None,
                    );
                }
                let fraction = crate::shell::read_fraction(view.page, view.pages);
                draw::chip(
                    &chips,
                    screen.right_top() + vec2(-24.0, 20.0),
                    Align2::RIGHT_TOP,
                    &format!("{} / {}", view.page + 1, view.pages),
                    Some(&format!("{:.0}%", fraction * 100.0)),
                );
            }
            match couch.overlay {
                Some(Overlay::Contents { cursor }) => contents(ui, screen, view, cursor, &t),
                Some(Overlay::GoTo { page }) => go_to(ui, screen, view, page, &t),
                Some(Overlay::Menu { cursor }) => menu(ui, screen, view, cursor, true, &t),
                None => {}
            }
            if view.couch || couch.overlay.is_some() {
                let mut bar = ui.new_child(egui::UiBuilder::new());
                bar.multiply_opacity(hints);
                hint_bar(&bar, screen, couch.overlay, view.overview);
            }
        });
}

/// The hint bar for what shows.
fn hint_bar(ui: &egui::Ui, screen: Rect, overlay: Option<Overlay>, overview: bool) {
    const MENU: [Glyph; 1] = [Glyph::Button("≡")];
    const DPAD: [Glyph; 1] = [Glyph::Dpad(None)];
    const SIDEWAYS: [Glyph; 1] = [Glyph::Dpad(Some(Arm::Right))];
    const A: [Glyph; 1] = [Glyph::Face(Face::South)];
    const B: [Glyph; 1] = [Glyph::Face(Face::East)];
    const X: [Glyph; 1] = [Glyph::Face(Face::West)];
    const BUMPERS: [Glyph; 2] = [Glyph::Button("lb"), Glyph::Button("rb")];
    const TRIGGERS: [Glyph; 2] = [Glyph::Button("lt"), Glyph::Button("rt")];
    const STICK: [Glyph; 1] = [Glyph::Stick];
    let menu = [Hint::new(&MENU, "menu").couch()];
    let (left, right): (Vec<Hint<'_>>, Vec<Hint<'_>>) = match (overlay, overview) {
        (Some(Overlay::Contents { .. }), _) => (
            Vec::new(),
            vec![
                Hint::new(&DPAD, "move").couch(),
                Hint::new(&A, "go to section").couch(),
                Hint::new(&B, "close").couch(),
            ],
        ),
        (Some(Overlay::GoTo { .. }), _) => (
            vec![Hint::new(&BUMPERS, "± 10 pages").couch()],
            vec![
                Hint::new(&SIDEWAYS, "page").couch(),
                Hint::new(&A, "go").couch(),
                Hint::new(&B, "cancel").couch(),
            ],
        ),
        (Some(Overlay::Menu { .. }), _) => (
            Vec::new(),
            vec![
                Hint::new(&SIDEWAYS, "change").couch(),
                Hint::new(&A, "select").couch(),
                Hint::new(&B, "close").couch(),
            ],
        ),
        (None, true) => (
            menu.to_vec(),
            vec![
                Hint::new(&DPAD, "move").couch(),
                Hint::new(&TRIGGERS, "size").couch(),
                Hint::new(&A, "open page").couch(),
                Hint::new(&B, "close").couch(),
            ],
        ),
        (None, false) => (
            vec![
                menu[0],
                Hint::new(&BUMPERS, "page").couch(),
                Hint::new(&STICK, "scroll · ring").couch(),
                Hint::new(&TRIGGERS, "zoom").couch(),
            ],
            vec![
                Hint::new(&X, "bookmark").couch(),
                Hint::new(&B, "library").couch(),
            ],
        ),
    };
    draw::hint_bar(ui, screen, &left, &right);
}

/// A sheet's title: a kicker and the book's title (Inter 24–26).
fn sheet_head(ui: &egui::Ui, at: egui::Pos2, kicker: &str, title: &str, width: f32, t: &Tokens) {
    let painter = ui.painter();
    painter.text(at, Align2::LEFT_TOP, kicker, theme::mono(15.0), t.muted);
    let mut job =
        egui::text::LayoutJob::simple(title.to_string(), theme::semibold(26.0), t.text, width);
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    painter.galley(at + vec2(0.0, 28.0), painter.layout_job(job), t.text);
}

/// A focusable row: `accent-soft` with a 4px `accent` edge when focused.
fn focus_row(painter: &egui::Painter, rect: Rect, on: bool, t: &Tokens) {
    if on {
        painter.rect_filled(rect, 0, t.accent_soft);
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(4.0, rect.height())),
            0,
            t.accent,
        );
    }
}

/// The outline as a sheet at the left (frame 09f).
fn contents(ui: &egui::Ui, screen: Rect, view: &ReaderView<'_>, cursor: usize, t: &Tokens) {
    let painter = ui.painter();
    let sheet = Rect::from_min_max(
        screen.min,
        pos2(
            screen.left() + 600.0_f32.min(screen.width()),
            screen.bottom() - draw::BAR,
        ),
    );
    painter.rect_filled(sheet, 0, t.surface);
    painter.vline(sheet.right() - 0.5, sheet.y_range(), t.hairline());
    let kicker = format!("contents · {}", view.outline.len());
    sheet_head(
        ui,
        sheet.left_top() + vec2(40.0, 36.0),
        &kicker,
        view.title,
        sheet.width() - 80.0,
        t,
    );
    let top = sheet.top() + 128.0;
    painter.hline(sheet.x_range(), top - 0.5, t.hairline());
    if view.outline.is_empty() {
        painter.text(
            pos2(sheet.left() + 40.0, top + 30.0),
            Align2::LEFT_TOP,
            "This book has no contents.",
            theme::regular(18.0),
            t.muted,
        );
        return;
    }
    let fit = (((sheet.bottom() - top - 20.0) / ROW).floor() as usize).max(1);
    let first = cursor
        .saturating_sub(fit / 2)
        .min(view.outline.len().saturating_sub(fit));
    let clip = painter.with_clip_rect(Rect::from_min_max(pos2(sheet.left(), top), sheet.max));
    for (row, (index, entry)) in view
        .outline
        .iter()
        .enumerate()
        .skip(first)
        .take(fit)
        .enumerate()
    {
        let rect = Rect::from_min_size(
            pos2(sheet.left(), top + 10.0 + row as f32 * ROW),
            vec2(sheet.width() - 1.0, ROW),
        );
        let on = index == cursor;
        focus_row(&clip, rect, on, t);
        let mut x = rect.left() + 40.0 + entry.depth.min(4) as f32 * 28.0;
        if view.current == Some(index) {
            clip.circle_filled(pos2(x + 4.0, rect.center().y), 4.0, t.accent);
            x += 20.0;
        }
        let page = entry
            .page
            .map(|page| (page + 1).to_string())
            .unwrap_or_default();
        let page_rect = clip.text(
            pos2(rect.right() - 40.0, rect.center().y),
            Align2::RIGHT_CENTER,
            page,
            theme::mono(15.0),
            if on { t.accent_text } else { t.faint },
        );
        let (font, color) = if entry.depth == 0 {
            (theme::semibold(19.0), t.text)
        } else {
            (theme::regular(18.0), t.text_2)
        };
        let mut job = egui::text::LayoutJob::simple(
            entry.title.clone(),
            font,
            if on { t.accent_text } else { color },
            (page_rect.left() - 16.0 - x).max(40.0),
        );
        job.wrap.max_rows = 1;
        job.wrap.overflow_character = Some('…');
        let galley = clip.layout_job(job);
        let y = rect.center().y - galley.size().y / 2.0;
        clip.galley(pos2(x, y), galley, color);
    }
}

/// The bottom sheet for choosing a page (frame 09h).
fn go_to(ui: &egui::Ui, screen: Rect, view: &ReaderView<'_>, page: usize, t: &Tokens) {
    let painter = ui.painter();
    let sheet = Rect::from_min_max(
        pos2(screen.left(), screen.bottom() - draw::BAR - 380.0),
        pos2(screen.right(), screen.bottom() - draw::BAR),
    );
    painter.rect_filled(sheet, 0, t.surface);
    painter.hline(sheet.x_range(), sheet.top() + 0.5, t.hairline());
    let left = sheet.left() + 96.0;
    let right = sheet.right() - 96.0;
    let number = painter.text(
        pos2(left, sheet.top() + 28.0),
        Align2::LEFT_TOP,
        (page + 1).to_string(),
        theme::semibold(64.0),
        t.accent_text,
    );
    let of = painter.text(
        pos2(number.right() + 16.0, number.bottom() - 10.0),
        Align2::LEFT_BOTTOM,
        format!("/ {}", view.pages),
        theme::mono(22.0),
        t.muted,
    );
    let section = crate::shell::section_path(
        view.outline
            .iter()
            .map(|entry| (entry.depth, entry.page, entry.title.as_str())),
        page,
    )
    .last()
    .map(|title| (*title).to_string())
    .unwrap_or_default();
    painter.text(
        pos2(of.right() + 32.0, of.bottom()),
        Align2::LEFT_BOTTOM,
        section,
        theme::regular(22.0),
        t.text_2,
    );
    painter.text(
        pos2(right, of.bottom()),
        Align2::RIGHT_BOTTOM,
        format!("from p. {}", view.page + 1),
        theme::mono(15.0),
        t.faint,
    );
    // The pages about the chosen one, it larger.
    let strip_bottom = sheet.top() + 290.0;
    let center = sheet.center().x;
    for offset in -4isize..=4 {
        let Some(at) = page
            .checked_add_signed(offset)
            .filter(|&at| at < view.pages)
        else {
            continue;
        };
        let on = offset == 0;
        let size = if on {
            vec2(125.0, 162.0)
        } else {
            vec2(100.0, 130.0)
        };
        let x = center
            + offset as f32 * 118.0
            + if offset > 0 {
                13.0
            } else if offset < 0 {
                -13.0
            } else {
                0.0
            };
        let rect = Rect::from_center_size(pos2(x, strip_bottom - size.y / 2.0), size);
        let alpha = if on { 1.0 } else { 0.75 };
        painter.rect_filled(rect, 0, t.page.gamma_multiply(alpha));
        for line in 0..9 {
            let y = rect.top() + 12.0 + line as f32 * (rect.height() - 24.0) / 9.0;
            let width =
                (rect.width() - 18.0) * [0.75, 1.0, 1.0, 0.92, 1.0, 0.55, 1.0, 0.95, 0.8][line];
            painter.rect_filled(
                Rect::from_min_size(pos2(rect.left() + 9.0, y), vec2(width, 3.0)),
                0,
                Color32::from_black_alpha(48),
            );
        }
        if on {
            painter.rect_stroke(
                rect,
                0,
                egui::Stroke::new(3.0, t.accent),
                egui::StrokeKind::Outside,
            );
        }
        painter.text(
            pos2(rect.center().x, rect.bottom() + 16.0),
            Align2::CENTER_CENTER,
            (at + 1).to_string(),
            theme::mono(14.0),
            if on { t.accent_text } else { t.muted },
        );
    }
    // The track: read in `accent`, the rest a hairline.
    let y = sheet.bottom() - 34.0;
    let fraction = page as f32 / view.pages.saturating_sub(1).max(1) as f32;
    let at = left + (right - left) * fraction;
    painter.hline(left..=right, y, egui::Stroke::new(1.0, t.rule_strong));
    painter.rect_filled(
        Rect::from_min_max(pos2(left, y - 1.5), pos2(at, y + 1.5)),
        0,
        t.accent,
    );
    painter.circle_filled(pos2(at, y), 7.0, t.accent);
}

/// The menu as a sheet at the right (frame 09i).
pub fn menu(
    ui: &egui::Ui,
    screen: Rect,
    view: &ReaderView<'_>,
    cursor: usize,
    reading: bool,
    t: &Tokens,
) {
    let painter = ui.painter();
    let width = 520.0_f32.min(screen.width());
    let sheet = Rect::from_min_max(
        pos2(screen.right() - width, screen.top()),
        pos2(screen.right(), screen.bottom() - draw::BAR),
    );
    painter.rect_filled(sheet, 0, t.surface);
    painter.vline(sheet.left() + 0.5, sheet.y_range(), t.hairline());
    sheet_head(
        ui,
        sheet.left_top() + vec2(40.0, 36.0),
        "menu",
        view.title,
        width - 80.0,
        t,
    );
    let top = sheet.top() + 128.0;
    painter.hline(sheet.x_range(), top - 0.5, t.hairline());
    let rows = MenuItem::rows(reading);
    let cursor = cursor.min(rows.len() - 1);
    let mut y = top + 10.0;
    for (index, &item) in rows.iter().enumerate() {
        if item == MenuItem::Library || (!reading && item == MenuItem::Exit) {
            painter.hline(sheet.x_range(), y + 0.5, t.hairline());
            y += 1.0;
        }
        let rect = Rect::from_min_size(pos2(sheet.left() + 1.0, y), vec2(width - 1.0, 60.0));
        let on = index == cursor;
        focus_row(painter, rect, on, t);
        let color = if on { t.accent_text } else { t.text };
        let icon =
            Rect::from_center_size(pos2(rect.left() + 51.0, rect.center().y), vec2(22.0, 22.0));
        icons::paint(
            ui,
            icon,
            item.icon(),
            22.0,
            if on { t.accent_text } else { t.muted },
        );
        painter.text(
            pos2(rect.left() + 76.0, rect.center().y),
            Align2::LEFT_CENTER,
            item.label(),
            theme::regular(19.0),
            color,
        );
        let value = match item {
            MenuItem::TextSize => Some(text_size(view.text_size)),
            MenuItem::Theme => Some(view.theme.to_string()),
            MenuItem::Fit => Some(if view.fit_page { "page" } else { "width" }.to_string()),
            MenuItem::Smooth => Some(if view.smooth { "on" } else { "off" }.to_string()),
            MenuItem::Library | MenuItem::Exit => None,
        };
        if let Some(value) = value {
            let arrows = if on { t.accent_text } else { t.faint };
            let right = rect.right() - 40.0;
            let next = painter.text(
                pos2(right, rect.center().y),
                Align2::RIGHT_CENTER,
                "›",
                theme::mono(20.0),
                arrows,
            );
            let shown = painter.text(
                pos2(next.left() - 12.0, rect.center().y),
                Align2::RIGHT_CENTER,
                value,
                theme::mono(16.0),
                if on { t.accent_text } else { t.text_2 },
            );
            painter.text(
                pos2(shown.left() - 12.0, rect.center().y),
                Align2::RIGHT_CENTER,
                "‹",
                theme::mono(20.0),
                arrows,
            );
        }
        y += 60.0;
    }
}
