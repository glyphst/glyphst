//! Gamepad mode's home (frame 09c): the focused book's hero (a glow of
//! its cover's colour and the cover itself, large, fading into `bg`), its
//! title and progress, a shelf of the recent books where the focused one
//! is a wide card, the clock and the pad's battery at the top right, and
//! the hint bar.

use std::path::PathBuf;

use egui::{Align2, Color32, Rect, Sense, TextureId, pos2, vec2};
use glyphst_ui::{
    icons, motion,
    pad_glyph::{Face, Glyph, Hint},
    theme::{self, Tokens},
};

use super::{Couch, Nav, draw};
use crate::{
    covers::Covers,
    home::Doc,
    pad::{PadButton, PadEvent},
};

pub struct HomeView<'a> {
    pub docs: &'a [Doc],
    pub covers: &'a mut Covers,
    /// `21:30`.
    pub clock: String,
    /// The pad's battery in percent, when it says.
    pub battery: Option<u8>,
    pub pad: bool,
    /// Whether home takes keys and buttons (not under a prompt or window).
    pub input: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeAction {
    Open(PathBuf),
    /// Opens the book on its contents (Y).
    Contents(PathBuf),
    Menu,
    /// B or Escape: leave gamepad mode (once confirmed).
    Leave,
}

const CARD: [f32; 2] = [520.0, 270.0];
const COVER: [f32; 2] = [190.0, 254.0];
const GAP: f32 = 20.0;
const GUTTER: f32 = 48.0;

pub fn show(ui: &mut egui::Ui, couch: &mut Couch, view: HomeView<'_>) -> Option<HomeAction> {
    let ctx = ui.ctx().clone();
    let t = Tokens::get(&ctx);
    let screen = ui.max_rect();
    let (_, response) = ui.allocate_exact_size(screen.size(), Sense::click());
    let books = view.docs.len();
    let mut action = None;
    let navs = if view.input {
        Nav::take(&ctx)
    } else {
        Vec::new()
    };
    let events = if view.input {
        crate::pad::frame_events(&ctx)
    } else {
        Vec::new()
    };
    for nav in navs {
        match nav {
            Nav::Left | Nav::Previous => couch.move_shelf(-1, books),
            Nav::Right | Nav::Next => couch.move_shelf(1, books),
            Nav::Enter => {
                action = view
                    .docs
                    .get(couch.shelf)
                    .map(|doc| HomeAction::Open(doc.path.clone()));
            }
            Nav::Back => action = Some(HomeAction::Leave),
            _ => {}
        }
    }
    for event in events {
        match event {
            PadEvent::Pressed(PadButton::Start) => action = Some(HomeAction::Menu),
            PadEvent::Pressed(PadButton::North) => {
                action = view
                    .docs
                    .get(couch.shelf)
                    .map(|doc| HomeAction::Contents(doc.path.clone()));
            }
            _ => {}
        }
    }
    couch.shelf = couch.shelf.min(books.saturating_sub(1));
    let painter = ui.painter().clone();
    painter.rect_filled(screen, 0, t.bg);

    let focused = view.docs.get(couch.shelf);
    if let Some(doc) = focused {
        hero(ui, screen, doc, view.covers, &t);
    }
    top_bar(ui, screen, &view, books, &t);
    let Some(doc) = focused else {
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            "No books yet: open some in the desktop Reader (select + start), then come back.",
            theme::regular(20.0),
            t.muted,
        );
        let exit = [Glyph::Face(Face::East)];
        draw::hint_bar(
            ui,
            screen,
            &menu_hint(),
            &[Hint::new(&exit, "exit").couch()],
        );
        return action;
    };
    hero_text(ui, screen, doc, &t);

    // The shelf: its title, then the books scrolled so the focused one
    // leads.
    let shelf_top = screen.bottom() - draw::BAR - 20.0 - CARD[1];
    let title_y = shelf_top - 22.0;
    painter.text(
        pos2(screen.left() + GUTTER, title_y),
        Align2::LEFT_CENTER,
        "recent",
        theme::mono(15.0),
        t.text_2,
    );
    let count = format!("{} / {}", couch.shelf + 1, books);
    let count_rect = painter.text(
        pos2(screen.right() - GUTTER, title_y),
        Align2::RIGHT_CENTER,
        count,
        theme::mono(15.0),
        t.faint,
    );
    painter.hline(
        (screen.left() + GUTTER + 76.0)..=(count_rect.left() - 16.0),
        title_y,
        egui::Stroke::new(1.0, t.rule),
    );
    let offset = motion::smooth(
        &ctx,
        egui::Id::new("couch-shelf"),
        couch.shelf as f32 * (COVER[0] + GAP),
        motion::PANEL,
    );
    let shelf = painter.with_clip_rect(Rect::from_min_max(
        pos2(screen.left(), shelf_top - 8.0),
        pos2(screen.right(), shelf_top + CARD[1] + 8.0),
    ));
    let mut x = screen.left() + GUTTER - offset;
    for (index, doc) in view.docs.iter().enumerate() {
        let at = |w: f32, h: f32, x: f32| {
            Rect::from_min_size(pos2(x, shelf_top + (CARD[1] - h).min(0.0)), vec2(w, h))
        };
        if index == couch.shelf {
            // The focused one starts where the others would.
            let rect = at(CARD[0], CARD[1], x.max(screen.left() + GUTTER));
            focused_card(ui, &shelf, rect, doc, view.covers, &t);
            x = rect.right() + GAP;
        } else {
            let rect = at(COVER[0], COVER[1], x);
            if rect.right() > screen.left() && rect.left() < screen.right() {
                let cover = view.covers.get(&doc.path);
                page_image(&shelf, rect, cover, &t);
            }
            x = rect.right() + GAP;
        }
    }

    let read = [Glyph::Face(Face::South)];
    let contents = [Glyph::Face(Face::North)];
    let exit = [Glyph::Face(Face::East)];
    draw::hint_bar(
        ui,
        screen,
        &menu_hint(),
        &[
            Hint::new(&contents, "contents").couch(),
            Hint::new(&read, "read").couch(),
            Hint::new(&exit, "exit").couch(),
        ],
    );
    if response.clicked() {
        response.request_focus();
    }
    action
}

fn menu_hint() -> [Hint<'static>; 1] {
    const MENU: [Glyph; 1] = [Glyph::Button("≡")];
    [Hint::new(&MENU, "menu").couch()]
}

/// The glow of the cover's colour and the cover large at the right,
/// fading into `bg` below and toward the text.
fn hero(ui: &egui::Ui, screen: Rect, doc: &Doc, covers: &mut Covers, t: &Tokens) {
    let painter = ui.painter();
    let tint = covers.tint(&doc.path).unwrap_or(t.accent);
    let anchor = pos2(screen.right() - 380.0, screen.top() + 110.0);
    draw::glow(painter, anchor, 620.0, 1.25, tint.gamma_multiply(0.55));
    if let Some((texture, aspect)) = covers.get(&doc.path) {
        let height = 560.0_f32.min(screen.height() * 0.62);
        let art = Rect::from_min_size(
            pos2(
                screen.right() - 160.0 - height * aspect,
                screen.top() - 40.0,
            ),
            vec2(height * aspect, height),
        );
        draw::image_fading(painter, art, texture, 0.45, 0.92);
    }
    let fade_top = screen.top() + screen.height() * 0.24;
    let fade = Rect::from_min_max(
        pos2(screen.left(), fade_top),
        pos2(screen.right(), screen.top() + screen.height() * 0.7),
    );
    draw::vertical_fade(painter, fade, Color32::TRANSPARENT, t.bg);
    painter.rect_filled(
        Rect::from_min_max(pos2(screen.left(), fade.bottom()), screen.max),
        0,
        t.bg,
    );
    let side = Rect::from_min_max(
        screen.min,
        pos2(screen.left() + screen.width() * 0.62, fade.bottom()),
    );
    draw::horizontal_fade(painter, side, Tokens::fade(t.bg, 0.7), Color32::TRANSPARENT);
    // The status at the top right stays legible over a light cover.
    let band = Rect::from_min_max(screen.min, pos2(screen.right(), screen.top() + 120.0));
    draw::vertical_fade(
        painter,
        band,
        Tokens::fade(t.bg, 0.85),
        Color32::TRANSPARENT,
    );
}

/// The kicker, title, meta line and progress of the focused book.
fn hero_text(ui: &egui::Ui, screen: Rect, doc: &Doc, t: &Tokens) {
    let painter = ui.painter();
    let left = screen.left() + 96.0;
    let width = (screen.width() - 96.0 - 620.0).clamp(360.0, 720.0);
    let mut y = screen.top() + 150.0;
    let kicker = if doc.opened.is_some() {
        "continue reading"
    } else {
        "in your library"
    };
    painter.text(
        pos2(left, y),
        Align2::LEFT_TOP,
        kicker,
        theme::mono(15.0),
        t.accent_text,
    );
    y += 32.0;
    let mut job =
        egui::text::LayoutJob::simple(doc.title.clone(), theme::semibold(44.0), t.text, width);
    job.wrap.max_rows = 2;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    let height = galley.size().y;
    painter.galley(pos2(left, y), galley, t.text);
    y += height + 14.0;
    let pages = doc.pages();
    let mut meta = vec![];
    if !doc.authors.is_empty() {
        meta.push(doc.authors.clone());
    }
    meta.push(doc.kind.clone());
    if let Some(pages) = pages {
        meta.push(format!("{pages} pages"));
    }
    painter.text(
        pos2(left, y),
        Align2::LEFT_TOP,
        meta.join(" · "),
        theme::mono(16.0),
        t.text_2,
    );
    y += 40.0;
    if let Some(progress) = doc.progress() {
        let (done, rest) = draw::meter(progress, 30);
        let done_rect = painter.text(
            pos2(left, y),
            Align2::LEFT_TOP,
            &done,
            theme::mono(16.0),
            t.accent,
        );
        let rest_rect = painter.text(
            done_rect.right_top(),
            Align2::LEFT_TOP,
            &rest,
            theme::mono(16.0),
            t.rule_strong,
        );
        let page = format!(
            "p. {} of {} · {:.0}%",
            doc.page(),
            pages.unwrap_or(0),
            progress * 100.0
        );
        painter.text(
            pos2(rest_rect.right() + 16.0, y + 1.0),
            Align2::LEFT_TOP,
            page,
            theme::mono(15.0),
            t.muted,
        );
    }
}

/// The brand and the library at the left; the pad, its battery and the
/// clock at the right.
fn top_bar(ui: &egui::Ui, screen: Rect, view: &HomeView<'_>, books: usize, t: &Tokens) {
    let painter = ui.painter();
    let y = screen.top() + 36.0;
    let mut x = screen.left() + GUTTER;
    let g = painter.text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        "g",
        theme::mono_bold(20.0),
        t.text,
    );
    painter.rect_filled(
        Rect::from_min_size(pos2(g.right() + 2.0, y - 11.0), vec2(10.0, 22.0)),
        0,
        t.accent,
    );
    x = g.right() + 36.0;
    painter.text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        format!("library · {books} books"),
        theme::mono(15.0),
        t.muted,
    );
    let mut right = screen.right() - GUTTER;
    let clock = painter.text(
        pos2(right, y),
        Align2::RIGHT_CENTER,
        &view.clock,
        theme::mono_strong(18.0),
        t.text,
    );
    right = clock.left() - 22.0;
    if view.pad {
        if let Some(percent) = view.battery {
            let level = painter.text(
                pos2(right, y),
                Align2::RIGHT_CENTER,
                format!("{percent}%"),
                theme::mono(15.0),
                t.text_2,
            );
            let icon = match percent {
                0..=20 => "battery-low",
                21..=70 => "battery-medium",
                _ => "battery-full",
            };
            let rect = Rect::from_center_size(pos2(level.left() - 18.0, y), vec2(22.0, 22.0));
            icons::paint(ui, rect, icon, 22.0, t.text_2);
            right = rect.left() - 8.0;
        }
        let rect = Rect::from_center_size(pos2(right - 10.0, y), vec2(20.0, 20.0));
        icons::paint(ui, rect, "gamepad-2", 20.0, t.text_2);
    }
}

/// The focused book: its cover at the left, its title, meta line, meter
/// and hints at the right, ringed in `accent`.
fn focused_card(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: Rect,
    doc: &Doc,
    covers: &mut Covers,
    t: &Tokens,
) {
    painter.rect_filled(rect, t.pop_radius(), t.raised);
    let cover = Rect::from_min_size(rect.min, vec2(196.0, rect.height()));
    page_image(painter, cover, covers.get(&doc.path), t);
    let left = cover.right() + 24.0;
    let width = rect.right() - 24.0 - left;
    let mut job =
        egui::text::LayoutJob::simple(doc.title.clone(), theme::semibold(22.0), t.text, width);
    job.wrap.max_rows = 3;
    job.wrap.overflow_character = Some('…');
    let galley = painter.layout_job(job);
    let title_height = galley.size().y;
    painter.galley(pos2(left, rect.top() + 22.0), galley, t.text);
    let mut meta = vec![doc.kind.clone()];
    if let Some(pages) = doc.pages() {
        meta.push(format!("{pages}p"));
    }
    if let Some(opened) = doc.opened {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        meta.push(crate::home::day(opened, now));
    }
    painter.text(
        pos2(left, rect.top() + 22.0 + title_height + 10.0),
        Align2::LEFT_TOP,
        meta.join(" · "),
        theme::mono(14.0),
        t.muted,
    );
    let hints_y = rect.bottom() - 34.0;
    if let Some(progress) = doc.progress() {
        let (done, rest) = draw::meter(progress, 18);
        let at = pos2(left, hints_y - 34.0);
        let done = painter.text(at, Align2::LEFT_CENTER, done, theme::mono(14.0), t.accent);
        let rest = painter.text(
            done.right_center(),
            Align2::LEFT_CENTER,
            rest,
            theme::mono(14.0),
            t.rule_strong,
        );
        painter.text(
            pos2(rest.right() + 10.0, at.y),
            Align2::LEFT_CENTER,
            format!("{:.0}%", progress * 100.0),
            theme::mono(14.0),
            t.accent_text,
        );
    }
    let read = [Glyph::Face(Face::South)];
    let contents = [Glyph::Face(Face::North)];
    let read = Hint::new(&read, "read").couch().color(t.text);
    read.paint(ui, left, hints_y);
    Hint::new(&contents, "contents")
        .couch()
        .paint(ui, left + read.size(ui).x + 18.0, hints_y);
    painter.rect_stroke(
        rect,
        t.pop_radius(),
        egui::Stroke::new(3.0, t.accent),
        egui::StrokeKind::Outside,
    );
}

/// A book's first page filling `rect` (cropped to it), or a plain page
/// while it is rendered.
fn page_image(painter: &egui::Painter, rect: Rect, cover: Option<(TextureId, f32)>, t: &Tokens) {
    match cover {
        Some((texture, aspect)) => {
            let wanted = rect.width() / rect.height();
            let uv = if aspect > wanted {
                let w = wanted / aspect;
                Rect::from_min_max(pos2(0.5 - w / 2.0, 0.0), pos2(0.5 + w / 2.0, 1.0))
            } else {
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, aspect / wanted))
            };
            painter.image(texture, rect, uv, Color32::WHITE);
        }
        None => {
            painter.rect_filled(rect, 0, t.page);
        }
    }
    painter.rect_stroke(rect, 0, t.hairline(), egui::StrokeKind::Inside);
}
