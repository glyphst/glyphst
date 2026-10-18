//! The book switcher on screen (frames 10a–f): its commands, the held
//! modifier and the keys while it is open, the cards, and the order books
//! were read in. The state itself is `crate::switcher`.

use std::time::Instant;

use egui::{
    Color32, Id, Mesh, Pos2, Rect, Sense, Shape, Stroke, Vec2, epaint::TextShape, pos2,
    text::LayoutJob, vec2,
};
use glyphst_ui::{
    key, motion,
    theme::{self, Tokens},
};

use super::{ReaderApp, Switch};
use crate::{
    commands::Command,
    switcher::{
        HOVER_DELAY, Mode, PEEK_SPEED, Step,
        layout::{self, CARD, Layout, Place, THUMB_SHARE},
    },
};

/// The widest a card's image is kept, in pixels.
const CARD_PIXELS: u32 = 640;
/// Gamepad mode's cards, larger to read from the couch.
const COUCH_ZOOM: f32 = 1.39;

/// What a card shows of a book.
struct CardInfo {
    tab: u64,
    title: String,
    page: String,
    progress: f32,
    tint: Color32,
    /// The book's last view, or its cover: a texture and its aspect.
    image: Option<(egui::TextureId, f32)>,
}

impl ReaderApp {
    /// Ctrl+Tab and Ctrl+Shift+Tab (held: letting go keeps the book; run
    /// without a modifier, from the palette, they switch at once), and the
    /// book switcher command, which stays open.
    pub(super) fn switch_book(&mut self, command: Command, ctx: &egui::Context) {
        let back = command == Command::SwitchBookBack;
        if self.switcher.is_open() {
            let step = self.switcher.step(if back { -1 } else { 1 });
            self.apply_switch(step);
            return;
        }
        let held = ctx.input(|i| i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt);
        // Up to date even before the first frame has settled it.
        self.settle_switcher();
        let origin = self.active.map(|index| self.tabs[index].id);
        if let Some(origin) = origin {
            self.capture_card(origin);
        }
        let mode = if command == Command::BookSwitcher {
            Mode::Sticky
        } else {
            Mode::Held
        };
        let step = self.switcher.open(mode, origin, back, Instant::now());
        self.apply_switch(step);
        if mode == Mode::Held && !held {
            let step = self.switcher.keep();
            self.apply_switch(step);
        }
    }

    fn tab_index(&self, tab: u64) -> Option<usize> {
        self.tabs.iter().position(|open| open.id == tab)
    }

    fn apply_switch(&mut self, step: Option<Step>) {
        let Some(step) = step else {
            return;
        };
        match step {
            Step::Peek { tab, direction } => {
                let Some(index) = self.tab_index(tab) else {
                    return;
                };
                let origin = self
                    .switcher_places
                    .iter()
                    .find(|(card, _)| *card == tab)
                    .map(|(_, at)| *at);
                self.activate_with(
                    Some(index),
                    Switch {
                        origin,
                        direction: Some(direction),
                        peek: true,
                        speed: PEEK_SPEED,
                    },
                );
            }
            Step::Keep(tab) => {
                self.couch.book_opened = true;
                if let Some(index) = self.tab_index(tab)
                    && self.active != Some(index)
                {
                    self.activate(Some(index));
                }
            }
            Step::Back(Some(tab)) => {
                if let Some(index) = self.tab_index(tab) {
                    self.activate_with(
                        Some(index),
                        Switch {
                            direction: Some(-1.0),
                            peek: true,
                            speed: PEEK_SPEED,
                            ..Switch::default()
                        },
                    );
                }
            }
            Step::Back(None) => self.activate(None),
        }
    }

    /// Draws `tab`'s last view into its card, when it has one.
    pub(super) fn capture_card(&mut self, tab: u64) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        let Some(Some((view, size, new))) =
            self.with_gpu(|gpu| gpu.capture_card(&device, &queue, tab, CARD_PIXELS))
        else {
            return;
        };
        let mut renderer = self.render_state.renderer.write();
        let id = match self.switcher_cards.get(&tab) {
            Some((id, _)) if !new => *id,
            Some((id, _)) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    &device,
                    &view,
                    wgpu::FilterMode::Linear,
                    *id,
                );
                *id
            }
            None => renderer.register_native_texture(&device, &view, wgpu::FilterMode::Linear),
        };
        self.switcher_cards.insert(tab, (id, size));
    }

    /// Forgets a closed tab's card.
    pub(super) fn drop_card(&mut self, tab: u64) {
        if let Some((id, _)) = self.switcher_cards.remove(&tab) {
            self.render_state.renderer.write().free_texture(&id);
        }
    }

    /// Before the keymap: letting go of the held modifier (or the window
    /// losing focus) keeps the book shown; Tab, arrows, Enter and Escape
    /// work the open switcher.
    pub(super) fn switcher_input(&mut self, ctx: &egui::Context) {
        let Some(mode) = self.switcher.mode() else {
            return;
        };
        // Held, the cards show after a moment.
        ctx.request_repaint();
        let (held, focused) = ctx.input(|i| {
            (
                i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt,
                i.focused,
            )
        });
        let mut steps = Vec::new();
        ctx.input_mut(|i| {
            i.events.retain(|event| {
                let egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } = event
                else {
                    return true;
                };
                let step = match key {
                    egui::Key::Tab if modifiers.shift => Some(Intent::Move(-1)),
                    egui::Key::Tab | egui::Key::ArrowRight | egui::Key::ArrowDown => {
                        Some(Intent::Move(1))
                    }
                    egui::Key::ArrowLeft | egui::Key::ArrowUp => Some(Intent::Move(-1)),
                    egui::Key::Enter => Some(Intent::Keep),
                    egui::Key::Escape => Some(Intent::Back),
                    _ => None,
                };
                match step {
                    Some(step) => {
                        if *pressed {
                            steps.push(step);
                        }
                        false
                    }
                    None => true,
                }
            });
        });
        for intent in steps {
            let step = match intent {
                Intent::Move(delta) => self.switcher.step(delta),
                Intent::Keep => self.switcher.keep(),
                Intent::Back => self.switcher.back(),
            };
            self.apply_switch(step);
        }
        if self.switcher.is_open() && mode == Mode::Held && (!held || !focused) {
            let step = self.switcher.keep();
            self.apply_switch(step);
        }
    }

    /// Once a frame, after everything: the book shown is the most recent
    /// unless the switcher is only peeking at it.
    pub(super) fn settle_switcher(&mut self) {
        let open: Vec<u64> = self.tabs.iter().map(|tab| tab.id).collect();
        let active = self.active.map(|index| self.tabs[index].id);
        self.switcher.settle(active, &open);
    }

    fn card_info(&mut self, tab: u64) -> Option<CardInfo> {
        let index = self.tab_index(tab)?;
        let (path, title, page, pages) = {
            let open = &self.tabs[index];
            let title = open
                .path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            (
                open.path.clone(),
                title,
                open.current_page() + 1,
                open.layout.len().max(1),
            )
        };
        let tokens_tint = self.covers.tint(&path);
        let image = self
            .switcher_cards
            .get(&tab)
            .map(|(id, size)| (*id, size[0] as f32 / size[1].max(1) as f32))
            .or_else(|| self.covers.get(&path));
        Some(CardInfo {
            tab,
            title,
            page: format!("p. {page} / {pages}"),
            progress: page as f32 / pages as f32,
            tint: tokens_tint.unwrap_or(Color32::from_gray(120)),
            image,
        })
    }

    /// The switcher over everything while it shows.
    pub(super) fn switcher_overlay(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let shows = self.switcher.shows(now);
        let shown = motion::smooth_bool(ctx, Id::unique("book-switcher"), shows, motion::PANEL);
        if !shows {
            self.switcher_places.clear();
            self.switcher_hover = None;
            return;
        }
        let list = self.switcher.list().to_vec();
        let Some(highlight) = self.switcher.highlight() else {
            return;
        };
        let origin = self.switcher.origin();
        let cards: Vec<CardInfo> = list.iter().filter_map(|tab| self.card_info(*tab)).collect();
        if cards.len() != list.len() {
            return;
        }
        let couch = self.prefs.gamepad_mode;
        let screen = ctx.content_rect();
        let layout_kind = self.prefs.switcher_layout;
        let targets = layout::place(
            layout_kind,
            cards.len(),
            highlight,
            screen,
            if couch { COUCH_ZOOM } else { 1.0 },
        );
        // Each card glides to its place.
        let places: Vec<Place> = targets
            .iter()
            .zip(&cards)
            .map(|(target, card)| {
                let smooth = |field: &str, value: f32| {
                    motion::smooth(
                        ctx,
                        Id::unique(("switcher-card", card.tab, field)),
                        value,
                        motion::PANEL,
                    )
                };
                Place {
                    center: pos2(smooth("x", target.center.x), smooth("y", target.center.y)),
                    size: vec2(smooth("w", target.size.x), smooth("h", target.size.y)),
                    angle: smooth("angle", target.angle),
                    skew: target.skew,
                    opacity: smooth("opacity", target.opacity),
                    order: target.order,
                }
            })
            .collect();
        self.switcher_places = cards
            .iter()
            .zip(&places)
            .map(|(card, place)| (card.tab, place.center))
            .collect();
        let tokens = Tokens::get(ctx);
        let pad_hints = couch || self.last_input == glyphst_ui::pad_glyph::InputDevice::Pad;
        let mut pointed = None;
        let mut clicked_empty = false;
        egui::Area::new(Id::unique("book-switcher-layer"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, Sense::click());
                let painter = ui.painter();
                painter.rect_filled(
                    screen,
                    0,
                    Tokens::fade(Color32::from_rgb(14, 15, 17), 0.58 * shown),
                );
                let mut order: Vec<usize> = (0..cards.len()).collect();
                order.sort_by_key(|index| places[*index].order);
                let pointer = ui.input(|i| i.pointer.hover_pos());
                if let Some(pointer) = pointer {
                    pointed = order.iter().rev().copied().find(|index| {
                        places[*index].opacity > 0.05 && places[*index].contains(pointer)
                    });
                }
                for index in order {
                    paint_card(
                        ui,
                        &places[index],
                        &cards[index],
                        CardLook {
                            highlighted: index == highlight,
                            origin: origin == Some(cards[index].tab),
                            slices: layout_kind == Layout::Slices,
                            shown,
                            pad_hints,
                        },
                        &tokens,
                    );
                }
                paint_strip(ui, screen, &cards, highlight, origin, shown, &tokens);
                let origin_title = origin
                    .and_then(|tab| cards.iter().find(|card| card.tab == tab))
                    .map_or("home", |card| card.title.as_str());
                paint_hints(
                    ui,
                    screen,
                    highlight,
                    cards.len(),
                    origin_title,
                    pad_hints,
                    shown,
                    &tokens,
                );
                clicked_empty = response.clicked() && pointed.is_none();
                if response.clicked()
                    && let Some(index) = pointed
                {
                    self.switcher_click = Some(index);
                }
            });
        // Pointing at a card a moment highlights it; a click keeps it.
        if let Some(index) = self.switcher_click.take() {
            let step = self.switcher.point(index);
            self.apply_switch(step);
            let step = self.switcher.keep();
            self.apply_switch(step);
            return;
        }
        match pointed {
            Some(index) if index != highlight => match self.switcher_hover {
                Some((hovered, since)) if hovered == index => {
                    if now.saturating_duration_since(since) >= HOVER_DELAY {
                        let step = self.switcher.point(index);
                        self.apply_switch(step);
                    }
                }
                _ => self.switcher_hover = Some((index, now)),
            },
            _ => self.switcher_hover = None,
        }
        if clicked_empty && self.switcher.mode() == Some(Mode::Sticky) {
            let step = self.switcher.back();
            self.apply_switch(step);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Intent {
    Move(i32),
    Keep,
    Back,
}

#[derive(Clone, Copy, Debug)]
struct CardLook {
    highlighted: bool,
    /// The book the switcher started from.
    origin: bool,
    slices: bool,
    /// How far the switcher has faded in.
    shown: f32,
    pad_hints: bool,
}

/// Text along a card: `text` from card-local `at`, at most `width` wide
/// (ending in an ellipsis), turned with the card.
#[allow(clippy::too_many_arguments)]
fn card_text(
    ui: &egui::Ui,
    place: &Place,
    at: Vec2,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
) {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    ui.painter()
        .add(TextShape::new(place.at(at), galley, color).with_angle(place.angle));
}

/// A quad of the card from card-local `min` to `max`.
fn card_quad(place: &Place, min: Vec2, max: Vec2) -> [Pos2; 4] {
    [
        place.at(min),
        place.at(vec2(max.x, min.y)),
        place.at(max),
        place.at(vec2(min.x, max.y)),
    ]
}

/// The book's image over `quad`, cropped to fill it (from the top, where a
/// page starts); `bounds` is the unturned box the crop is worked out in
/// and `local` where each corner lies in it, from 0 to 1.
fn paint_image(
    ui: &egui::Ui,
    quad: [Pos2; 4],
    bounds: Vec2,
    local: [Vec2; 4],
    image: (egui::TextureId, f32),
    alpha: f32,
) {
    let (texture, aspect) = image;
    let box_aspect = bounds.x / bounds.y.max(1.0);
    let (span, start) = if aspect > box_aspect {
        let span = vec2(box_aspect / aspect, 1.0);
        (span, vec2((1.0 - span.x) / 2.0, 0.0))
    } else {
        (vec2(1.0, aspect / box_aspect), Vec2::ZERO)
    };
    let tint = Color32::WHITE.gamma_multiply(alpha);
    let mut mesh = Mesh::with_texture(texture);
    for (corner, local) in quad.into_iter().zip(local) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: corner,
            uv: (start + local * span).to_pos2(),
            color: tint,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(Shape::mesh(mesh));
}

fn paint_card(ui: &egui::Ui, place: &Place, card: &CardInfo, look: CardLook, tokens: &Tokens) {
    let alpha = place.opacity * look.shown * if look.highlighted { 1.0 } else { 0.9 };
    if alpha <= 0.01 {
        return;
    }
    let painter = ui.painter();
    let scale = if look.slices {
        1.0
    } else {
        place.size.x / CARD.x
    };
    let size = place.size;
    let thumb_height = if look.slices {
        size.y
    } else {
        size.y * THUMB_SHARE
    };
    let outline = place.corners().to_vec();
    painter.add(Shape::convex_polygon(
        outline.clone(),
        Tokens::fade(tokens.surface, alpha),
        Stroke::NONE,
    ));
    // The image: the book's last view (or its cover) over the backdrop.
    let thumb = card_quad(place, Vec2::ZERO, vec2(size.x, thumb_height));
    painter.add(Shape::convex_polygon(
        thumb.to_vec(),
        Tokens::fade(tokens.bg, alpha),
        Stroke::NONE,
    ));
    if let Some(image) = card.image {
        if look.slices {
            // Slices crop the image upright, as if seen through a slot.
            let bounds = Rect::from_points(&thumb);
            let local = thumb.map(|corner| (corner - bounds.min) / bounds.size());
            paint_image(ui, thumb, bounds.size(), local, image, alpha);
        } else {
            let local = [Vec2::ZERO, vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0)];
            paint_image(ui, thumb, vec2(size.x, thumb_height), local, image, alpha);
        }
    }
    // The cover's tint along the left edge.
    let tint = card_quad(place, Vec2::ZERO, vec2(3.0 * scale, thumb_height));
    painter.add(Shape::convex_polygon(
        tint.to_vec(),
        Tokens::fade(card.tint, alpha),
        Stroke::NONE,
    ));
    let stroke = if look.highlighted {
        Stroke::new(1.5, Tokens::fade(tokens.accent, alpha))
    } else {
        Stroke::new(1.0, Tokens::fade(tokens.rule_strong, alpha))
    };
    painter.add(Shape::closed_line(outline, stroke));

    if look.highlighted {
        let label = if look.pad_hints {
            "A open"
        } else {
            "enter open"
        };
        chip(
            ui,
            place,
            vec2(size.x - 8.0 * scale, 8.0 * scale),
            label,
            true,
            scale,
            alpha,
            tokens,
        );
    }
    if look.origin {
        chip(
            ui,
            place,
            vec2(8.0 * scale, 8.0 * scale),
            "esc back",
            false,
            scale,
            alpha,
            tokens,
        );
    }
    if look.slices {
        if look.highlighted {
            slice_meta(ui, place, card, alpha, tokens);
        }
        return;
    }
    // Title, page and progress under the image.
    let pad = 10.0 * scale;
    let text_width = size.x - 2.0 * pad;
    let title_color = if look.highlighted {
        tokens.accent_text
    } else {
        tokens.text
    };
    card_text(
        ui,
        place,
        vec2(pad, thumb_height + 8.0 * scale),
        &card.title,
        theme::mono(11.0 * scale),
        Tokens::fade(title_color, alpha),
        text_width,
    );
    card_text(
        ui,
        place,
        vec2(pad, thumb_height + 26.0 * scale),
        &format!("{}  {:.0}%", card.page, card.progress * 100.0),
        theme::mono(9.0 * scale),
        Tokens::fade(tokens.faint, alpha),
        text_width,
    );
    let filled = ((card.progress * 15.0).round() as usize).clamp(1, 15);
    card_text(
        ui,
        place,
        vec2(pad, thumb_height + 40.0 * scale),
        &"━".repeat(filled),
        theme::mono(9.0 * scale),
        Tokens::fade(tokens.accent, alpha),
        text_width,
    );
    let bar = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap("━".repeat(filled), theme::mono(9.0 * scale), Color32::WHITE)
            .size()
            .x
    });
    card_text(
        ui,
        place,
        vec2(pad + bar, thumb_height + 40.0 * scale),
        &"─".repeat(15 - filled),
        theme::mono(9.0 * scale),
        Tokens::fade(tokens.rule_strong, alpha),
        text_width - bar,
    );
}

/// A small label on a card: `enter open` on the highlighted one (filled
/// with the accent), `esc back` on the one it started from. `at` is its top right
/// when `filled`, else its top left.
#[allow(clippy::too_many_arguments)]
fn chip(
    ui: &egui::Ui,
    place: &Place,
    at: Vec2,
    label: &str,
    filled: bool,
    scale: f32,
    alpha: f32,
    tokens: &Tokens,
) {
    let font = if filled {
        theme::mono_bold(9.0 * scale)
    } else {
        theme::mono(9.0 * scale)
    };
    let width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE)
            .size()
            .x
    }) + 10.0 * scale;
    let height = 14.0 * scale;
    let min = if filled { at - vec2(width, 0.0) } else { at };
    let quad = card_quad(place, min, min + vec2(width, height));
    let (fill, stroke, color) = if filled {
        (tokens.accent, Stroke::NONE, tokens.on_accent)
    } else {
        (
            Color32::from_rgba_unmultiplied(14, 15, 17, 217),
            Stroke::new(1.0, Tokens::fade(tokens.rule_strong, alpha)),
            tokens.text_2,
        )
    };
    ui.painter().add(Shape::convex_polygon(
        quad.to_vec(),
        Tokens::fade(fill, alpha),
        stroke,
    ));
    card_text(
        ui,
        place,
        min + vec2(5.0 * scale, 2.0 * scale),
        label,
        font,
        Tokens::fade(color, alpha),
        width,
    );
}

/// The highlighted slice's title, page and progress, in a chip under it.
fn slice_meta(ui: &egui::Ui, place: &Place, card: &CardInfo, alpha: f32, tokens: &Tokens) {
    let bounds = Rect::from_points(&place.corners());
    let rect = Rect::from_min_size(
        pos2(bounds.left(), bounds.bottom() + 14.0),
        vec2(place.size.x, 30.0),
    );
    let painter = ui.painter();
    painter.rect(
        rect,
        tokens.ctl_radius(),
        Tokens::fade(Color32::from_rgba_unmultiplied(14, 15, 17, 217), alpha),
        Stroke::new(1.0, Tokens::fade(tokens.rule, alpha)),
        egui::StrokeKind::Inside,
    );
    let left = painter.text(
        rect.left_center() + vec2(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &card.title,
        theme::mono(12.0),
        Tokens::fade(tokens.accent_text, alpha),
    );
    painter.text(
        pos2(left.right() + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} · {:.0}%", card.page, card.progress * 100.0),
        theme::mono(10.0),
        Tokens::fade(tokens.faint, alpha),
    );
}

/// One segment per book across the top, in recent order: the cover's
/// tint, the highlighted one full height and outlined, a dot under the book
/// it started from.
fn paint_strip(
    ui: &egui::Ui,
    screen: Rect,
    cards: &[CardInfo],
    highlight: usize,
    origin: Option<u64>,
    shown: f32,
    tokens: &Tokens,
) {
    let painter = ui.painter();
    let (left, right, top) = (
        screen.left() + 40.0,
        screen.right() - 40.0,
        screen.top() + 16.0,
    );
    let gap = 3.0;
    let count = cards.len() as f32;
    let width = ((right - left) - gap * (count - 1.0)) / count;
    for (index, card) in cards.iter().enumerate() {
        let x = left + index as f32 * (width + gap);
        let highlighted = index == highlight;
        let height = if highlighted { 6.0 } else { 4.0 };
        let rect = Rect::from_min_size(pos2(x, top + (6.0 - height) / 2.0), vec2(width, height));
        let alpha = if highlighted { 1.0 } else { 0.55 };
        painter.rect_filled(rect, 1.0, Tokens::fade(card.tint, alpha * shown));
        if highlighted {
            painter.rect_stroke(
                rect.expand(2.0),
                1.0,
                Stroke::new(1.0, Tokens::fade(tokens.text, shown)),
                egui::StrokeKind::Inside,
            );
        }
        if origin == Some(card.tab) {
            painter.circle_filled(
                pos2(rect.center().x, top + 12.0),
                2.0,
                Tokens::fade(tokens.text_2, shown),
            );
        }
    }
}

/// The keys, in a chip under the strip.
#[allow(clippy::too_many_arguments)]
fn paint_hints(
    ui: &egui::Ui,
    screen: Rect,
    highlight: usize,
    count: usize,
    origin_title: &str,
    pad_hints: bool,
    shown: f32,
    tokens: &Tokens,
) {
    let font = theme::mono(10.0);
    let stay = format!("stay on {origin_title}");
    let parts: Vec<(Option<&str>, &str)> = if pad_hints {
        vec![
            (Some("lb rb"), "move"),
            (Some("A"), "open"),
            (Some("B"), &stay),
        ]
    } else {
        vec![
            (Some("tab"), "next"),
            (Some("shift tab"), "back"),
            (Some("enter"), "open"),
            (Some("esc"), &stay),
        ]
    };
    let count_text = format!("{} / {count}", highlight + 1);
    let width_of = |text: &str| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    let gap = 8.0;
    let separator = width_of("·");
    let mut width = width_of(&count_text);
    for (key_label, label) in &parts {
        width += gap * 2.0 + separator;
        if let Some(key_label) = key_label {
            width += key::size(ui, key_label).x + gap;
        }
        width += width_of(label);
    }
    let rect = Rect::from_center_size(
        pos2(screen.center().x, screen.top() + 52.0),
        vec2(width + 20.0, 25.0),
    );
    let painter = ui.painter();
    painter.rect(
        rect,
        tokens.ctl_radius(),
        Tokens::fade(Color32::from_rgba_unmultiplied(14, 15, 17, 217), shown),
        Stroke::new(1.0, Tokens::fade(tokens.rule, shown)),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 10.0;
    let y = rect.center().y;
    let text = |x: &mut f32, value: &str, color: Color32| {
        let at = painter.text(
            pos2(*x, y),
            egui::Align2::LEFT_CENTER,
            value,
            font.clone(),
            Tokens::fade(color, shown),
        );
        *x = at.right();
    };
    text(&mut x, &count_text, tokens.accent_text);
    for (key_label, label) in &parts {
        x += gap;
        text(&mut x, "·", tokens.faint);
        x += gap;
        if let Some(key_label) = key_label {
            let size = key::size(ui, key_label);
            key::paint(
                ui,
                Rect::from_min_size(pos2(x, y - size.y / 2.0), size),
                key_label,
                None,
            );
            x += size.x + gap;
        }
        text(&mut x, label, tokens.text_2);
    }
}
