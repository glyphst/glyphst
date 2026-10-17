//! Presenting (frames 02g–j): the window going fullscreen, the hidden
//! cursor locked at the centre, the page's input while presenting (page
//! turns, the tool ring, ink, the laser) and what is drawn over it: the
//! corner chip, the progress line, the toolbox, the ring, the cursors and
//! the blank screen.

use std::time::{Duration, Instant};

use egui::{Align2, Color32, CursorIcon, PointerButton, Pos2, Rect, Shape, Stroke, pos2, vec2};
use glyphst_ui::{motion, ring, toolbox};

use super::ReaderApp;
use crate::{
    commands::{Category, Command},
    document_tab::ViewAction,
    present::{
        self, Blank, Clock, ERASER_RADIUS, HIGHLIGHTER, HIGHLIGHTER_WIDTH, INK, LASER, Mode,
        PEN_WIDTH, Pace, Presenting, RingGesture, TRAIL,
    },
    text_overlay::{PageMap, page_at},
    theme::{self, Tokens},
};

/// How long the toolbox stays once the pointer stops.
const TOOLBOX_IDLE: Duration = Duration::from_secs(3);
/// The corners' gutter (frame 02g).
const GUTTER: f32 = 16.0;

/// Whether starting a presentation makes the window fullscreen: when the
/// preference asks, the run is the user's, and it isn't already.
fn goes_fullscreen(preference: bool, persist: bool, fullscreen: bool) -> bool {
    preference && persist && !fullscreen
}

impl ReaderApp {
    /// Starts the presentation's state when presenting starts, and puts
    /// the window back when it ends.
    pub(super) fn presenting_edges(&mut self, ctx: &egui::Context, presenting: bool, now: Instant) {
        match (presenting, self.presenting.is_some()) {
            (true, false) => {
                let mut state = Presenting::new(now);
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if goes_fullscreen(self.prefs.present_fullscreen, self.persist, fullscreen) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                    state.made_fullscreen = true;
                }
                self.presenting = Some(state);
                self.notices
                    .push("Presenting · Esc to leave · hold the right button for tools");
            }
            (false, true) => {
                if let Some(state) = self.presenting.take() {
                    if state.locked {
                        unlock(ctx);
                    }
                    if state.made_fullscreen {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    }
                }
            }
            _ => {}
        }
    }

    /// The ring's commands from the preferences, from up clockwise.
    pub(super) fn ring_commands(&self) -> [Option<Command>; ring::SLOTS] {
        self.prefs.present_ring.clone().map(|id| {
            id.as_deref()
                .and_then(Command::from_id)
                .filter(|command| command.category() == Category::Present)
        })
    }

    /// Escape while presenting: closes the ring, ends the blank screen or
    /// goes back to the hidden cursor (from the laser or a tool), in that
    /// order; whether it did any.
    pub(super) fn present_cancel(&mut self) -> bool {
        let Some(state) = self.presenting.as_mut() else {
            return false;
        };
        if state.ring.take().is_some() {
            return true;
        }
        if state.blank.take().is_some() {
            return true;
        }
        if state.mode != Mode::Hidden {
            state.toggle_mode(state.mode);
            return true;
        }
        false
    }

    /// Runs a presenting command.
    pub(super) fn run_present(&mut self, command: Command, now: Instant) {
        let Some(active) = self.active else { return };
        let page = self.tabs[active].current_page();
        let Some(state) = self.presenting.as_mut() else {
            return;
        };
        let ink = &mut self.tabs[active].ink;
        let mode = match command {
            Command::PresentDraw => Some(Mode::Pen),
            Command::PresentHighlight => Some(Mode::Highlighter),
            Command::PresentErase => Some(Mode::Eraser),
            Command::PresentCursor => Some(Mode::Laser),
            _ => None,
        };
        if let Some(mode) = mode {
            state.toggle_mode(mode);
            state.moved = now;
            return;
        }
        match command {
            Command::PresentClear => {
                ink.clear(page);
            }
            Command::PresentUndo => {
                ink.undo(page);
            }
            Command::PresentRedo => {
                ink.redo(page);
            }
            Command::PresentBlack => state.toggle_blank(Blank::Black),
            Command::PresentWhite => state.toggle_blank(Blank::White),
            Command::PresentTimer => state.timer(now),
            Command::PresentTimerStop => state.clock = None,
            _ => {}
        }
    }

    /// Sets out the tools for the 02h screenshot: a loop, an underline and
    /// a highlight on the page, the laser's toolbox with the pen and its
    /// colours, and the ring open.
    pub(crate) fn seed_present_tools(&mut self, ctx: &egui::Context) {
        let Some(active) = self.active else { return };
        let Some(state) = self.presenting.as_mut() else {
            eprintln!("--screenshot-present-tools: not presenting");
            return;
        };
        let now = Instant::now();
        state.mode = Mode::Pen;
        state.moved = now;
        state.clock = Some(Clock::new(
            now.checked_sub(Duration::from_secs(761)).unwrap_or(now),
        ));
        let screen = ctx.content_rect();
        let at = pos2(
            screen.left() + screen.width() * 0.74,
            screen.top() + screen.height() * 0.32,
        );
        state.ring = Some(RingGesture::Open {
            at,
            center: ring::center_within(at, screen),
        });
        let tab = &mut self.tabs[active];
        let page = tab.current_page();
        let [width, height] = tab.layout.page_size(page).map(|side| side as f32);
        let at = |x: f32, y: f32| [x * width, y * height];
        let loop_points = (0..=40)
            .map(|step| {
                let turn = step as f32 / 40.0 * std::f32::consts::TAU * 1.08;
                at(0.5 + 0.22 * turn.cos(), 0.56 + 0.035 * turn.sin())
            })
            .collect();
        let strokes = [
            (loop_points, INK[0].1, PEN_WIDTH),
            (
                vec![
                    at(0.14, 0.47),
                    at(0.3, 0.468),
                    at(0.45, 0.471),
                    at(0.6, 0.467),
                ],
                INK[0].1,
                PEN_WIDTH,
            ),
            (
                vec![at(0.12, 0.385), at(0.86, 0.385)],
                HIGHLIGHTER,
                HIGHLIGHTER_WIDTH,
            ),
        ];
        for (points, color, width) in strokes {
            tab.ink.start(
                page,
                present::Stroke {
                    points,
                    color,
                    width,
                },
            );
        }
    }

    /// Labels that follow the presentation's state, for the palette.
    pub(super) fn present_labels(&self) -> std::collections::HashMap<Command, &'static str> {
        let Some(state) = &self.presenting else {
            return Default::default();
        };
        Command::ALL
            .into_iter()
            .filter_map(|command| Some((command, state.label(command)?)))
            .collect()
    }

    /// Pointer input on the page while presenting, in place of the text's:
    /// a click turns to the next page and a right-click to the previous
    /// one, the wheel turns pages, a held or dragged right button opens
    /// the ring, and a drag draws or erases with the tool in hand.
    pub(super) fn present_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        maps: &[PageMap],
        active: usize,
        actions: &mut Vec<ViewAction>,
        now: Instant,
    ) {
        let ring_commands = self.ring_commands();
        let Some(state) = self.presenting.as_mut() else {
            return;
        };
        let (primary_pressed, primary_down, secondary_pressed, secondary_released, wheel, motion) =
            ctx.input(|i| {
                let motion = i
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::MouseMoved(delta) => Some(*delta),
                        _ => None,
                    })
                    .fold(egui::Vec2::ZERO, |sum, delta| sum + delta);
                (
                    i.pointer.button_pressed(PointerButton::Primary),
                    i.pointer.primary_down(),
                    i.pointer.button_pressed(PointerButton::Secondary),
                    i.pointer.button_released(PointerButton::Secondary),
                    i.smooth_scroll_delta.y,
                    motion / i.pixels_per_point,
                )
            });
        let moved =
            motion != egui::Vec2::ZERO || ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
        let over = response.contains_pointer();
        let latest = ctx.pointer_latest_pos();
        if moved {
            state.moved = now;
        }
        // The blank screen: any click comes back to the slide.
        if state.blank.is_some() {
            if over && (primary_pressed || secondary_pressed) {
                state.blank = None;
            }
            state.ring = None;
            state.drawing = None;
            return;
        }

        let mut picked = None;
        let screen = ctx.content_rect();
        if secondary_pressed
            && over
            && let Some(at) = latest
        {
            // Hidden, the cursor waits at the centre and the ring opens
            // there; shown, it opens where the button went down.
            let center = match state.mode {
                Mode::Hidden => screen.center(),
                _ => ring::center_within(at, screen),
            };
            state.ring = Some(RingGesture::Pressed {
                at,
                center,
                since: now,
            });
            state.ring_motion = None;
        }
        if state.ring.is_some() && state.locked && motion != egui::Vec2::ZERO {
            state.ring_motion = Some(state.ring_motion.unwrap_or_default() + motion);
        }
        match state.ring {
            Some(RingGesture::Pressed { at, center, since }) => {
                let held = now.saturating_duration_since(since);
                let pointer =
                    present::virtual_pointer(center, state.ring_motion, at, latest.unwrap_or(at));
                if secondary_released {
                    state.ring = None;
                    actions.push(ViewAction::PreviousPage);
                } else if ring::opens((pointer - center).length(), held) {
                    state.ring = Some(RingGesture::Open { at, center });
                    state.drawing = None;
                } else {
                    ctx.request_repaint_after(ring::OPEN_HOLD.saturating_sub(held));
                }
            }
            // Releasing picks what is under the pointer; till then (or
            // Esc) the ring stays.
            Some(RingGesture::Open { at, center }) if secondary_released => {
                state.ring = None;
                let pointer =
                    present::virtual_pointer(center, state.ring_motion, at, latest.unwrap_or(at));
                if let ring::Pick::Slot(slot) = ring::pick(center, pointer) {
                    picked = ring_commands[slot];
                }
            }
            Some(RingGesture::Open { .. }) | None => {}
        }

        if state.ring.is_none() {
            if over && let Some(forward) = state.wheel_turn(-wheel) {
                actions.push(if forward {
                    ViewAction::NextPage
                } else {
                    ViewAction::PreviousPage
                });
            }
            let ink = &mut self.tabs[active].ink;
            match state.mode {
                tool @ (Mode::Pen | Mode::Highlighter) => {
                    if primary_pressed
                        && over
                        && let Some(pos) = latest
                        && let Some(map) = page_at(maps, pos)
                    {
                        let (color, width) = match tool {
                            Mode::Highlighter => (HIGHLIGHTER, HIGHLIGHTER_WIDTH),
                            _ => (INK[state.color].1, PEN_WIDTH),
                        };
                        ink.start(
                            map.page,
                            present::Stroke {
                                points: vec![map.page_point(pos)],
                                color,
                                width,
                            },
                        );
                        state.drawing = Some(map.page);
                    } else if primary_down
                        && let Some(page) = state.drawing
                        && let Some(map) = maps.iter().find(|map| map.page == page)
                        && let Some(pos) = latest
                    {
                        ink.extend(page, map.page_point(pos), map.points(0.5));
                    }
                    if !primary_down {
                        state.drawing = None;
                    }
                }
                Mode::Eraser => {
                    if primary_down
                        && (over || state.drawing.is_some())
                        && let Some(pos) = latest
                        && let Some(map) = page_at(maps, pos)
                    {
                        state.drawing = Some(map.page);
                        let point = map.page_point(pos);
                        if ink.erase(map.page, point, map.points(ERASER_RADIUS), !state.erased) {
                            state.erased = true;
                        }
                    }
                    if !primary_down {
                        state.drawing = None;
                        state.erased = false;
                    }
                }
                Mode::Hidden | Mode::Laser => {
                    if response.clicked() {
                        actions.push(ViewAction::NextPage);
                    }
                }
            }
        }
        if let Some(command) = picked {
            self.run_or_notify(command, ctx);
        }
    }

    /// The page's ink, under the presentation's overlay; hidden while the
    /// page turns.
    pub(super) fn paint_ink(&self, painter: &egui::Painter, maps: &[PageMap], active: usize) {
        let tab = &self.tabs[active];
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.tab == tab.id)
        {
            return;
        }
        for map in maps {
            let strokes = tab.ink.page(map.page);
            if strokes.is_empty() {
                continue;
            }
            let painter = painter.with_clip_rect(painter.clip_rect().intersect(map.paper));
            // Screen points per page point.
            let scale = 1.0 / map.points(1.0);
            for stroke in strokes {
                let points: Vec<Pos2> = stroke
                    .points
                    .iter()
                    .map(|&point| map.screen_pos(point))
                    .collect();
                let width = stroke.width * scale;
                match points.as_slice() {
                    [] => {}
                    [only] => {
                        painter.circle_filled(*only, width / 2.0, stroke.color);
                    }
                    _ => {
                        painter.add(Shape::line(points, Stroke::new(width, stroke.color)));
                    }
                }
            }
        }
    }

    /// Everything over the presented page (frames 02g, 02h): the blank
    /// screen, or the progress line, the clock chip, the toolbox while the
    /// cursor shows, the ring and the cursor itself.
    pub(super) fn present_overlay(&mut self, ctx: &egui::Context, active: usize, now: Instant) {
        let tokens = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let ring_commands = self.ring_commands();
        // A dialog over the presentation gets the system's pointer.
        let dialog = self.palette.open || self.settings_window.open;
        let tab = &self.tabs[active];
        let pages = tab.layout.len().max(1);
        let current = tab.current_page();
        let talk = (self.prefs.talk_minutes > 0)
            .then(|| Duration::from_secs(u64::from(self.prefs.talk_minutes) * 60));
        let page_number = self.prefs.present_page_number;
        let lockable = self.persist;
        let Some(state) = self.presenting.as_mut() else {
            return;
        };
        let hover = ctx.pointer_hover_pos().filter(|_| !dialog);
        let over_toolbox =
            hover.is_some_and(|pos| state.toolbox.iter().any(|rect| rect.contains(pos)));
        let passed: Vec<Pos2> = if dialog || over_toolbox {
            Vec::new()
        } else {
            ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|event| match event {
                        egui::Event::PointerMoved(pos) => Some(*pos),
                        _ => None,
                    })
                    .collect()
            })
        };
        state.trail_to(&passed, now);
        let clock = state
            .clock
            .map(|clock| present::clock_text(clock.elapsed(now), talk));
        let slots = ring_commands.map(|command| {
            command.map(|command| {
                let (label, icon) = state.slot(command);
                ring::Slot { icon, label }
            })
        });

        egui::Area::new(egui::Id::new("present-overlay"))
            .order(egui::Order::Foreground)
            .fixed_pos(screen.min)
            .interactable(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                let painter = ui.painter();
                if let Some(blank) = state.blank {
                    let fill = match blank {
                        Blank::Black => Color32::BLACK,
                        Blank::White => Color32::WHITE,
                    };
                    painter.rect_filled(screen, 0, fill);
                } else {
                    let read = motion::smooth(
                        ctx,
                        egui::Id::new("present-progress"),
                        crate::shell::read_fraction(current, pages),
                        motion::PANEL,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(
                            pos2(screen.left(), screen.bottom() - 2.0),
                            vec2(screen.width() * read, 2.0),
                        ),
                        0,
                        tokens.accent,
                    );
                    if state.chip_shows(page_number) {
                        corner_chip(ui, screen, current, pages, clock.as_ref(), &tokens);
                    }
                }
                // The ring fades in where it opens.
                let ring_shown = motion::smooth_bool(
                    ctx,
                    egui::Id::new("present-ring-shown"),
                    state.ring.is_some(),
                    motion::HOVER,
                );
                if let Some(RingGesture::Open { at, center }) = state.ring {
                    let latest = ctx.pointer_latest_pos().unwrap_or(at);
                    let pointer = present::virtual_pointer(center, state.ring_motion, at, latest);
                    let mut ring_ui = ui.new_child(egui::UiBuilder::new());
                    ring_ui.multiply_opacity(ring_shown);
                    ring::paint(&ring_ui, center, &slots, pointer);
                }
                if let Some(pos) = hover.filter(|_| !over_toolbox && state.ring.is_none()) {
                    paint_cursor(ui, state, pos, now);
                }
            });

        // The toolbox shows with the cursor, and fades once it rests.
        let idle = now.saturating_duration_since(state.moved) > TOOLBOX_IDLE;
        let awake = !idle || over_toolbox;
        let shown = motion::smooth_bool(
            ctx,
            egui::Id::new("present-toolbox-shown"),
            awake,
            motion::FADE,
        );
        let opacity = shown
            * motion::smooth(
                ctx,
                egui::Id::new("present-toolbox-hover"),
                if over_toolbox { 1.0 } else { 0.6 },
                motion::HOVER,
            );
        let mut clicked = Vec::new();
        state.toolbox.clear();
        if state.mode != Mode::Hidden && state.blank.is_none() && !dialog && shown > 0.0 {
            let mode = state.mode;
            let area = egui::Area::new(egui::Id::new("present-toolbox"))
                .order(egui::Order::Foreground)
                .fade_in(false)
                .interactable(awake)
                .pivot(Align2::LEFT_BOTTOM)
                .fixed_pos(pos2(screen.left() + GUTTER, screen.bottom() - GUTTER))
                .show(ctx, |ui| {
                    ui.set_opacity(opacity);
                    toolbox::frame(ctx)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                let mut pen = None;
                                for (index, group) in TOOLBOX.iter().enumerate() {
                                    if index > 0 {
                                        toolbox::separator(ui);
                                    }
                                    for &(icon, command) in group.iter() {
                                        let on = match command {
                                            Some(Command::PresentCursor) => mode == Mode::Laser,
                                            Some(Command::PresentDraw) => mode == Mode::Pen,
                                            Some(Command::PresentHighlight) => {
                                                mode == Mode::Highlighter
                                            }
                                            Some(Command::PresentErase) => mode == Mode::Eraser,
                                            _ => false,
                                        };
                                        let tip = command
                                            .map(|command| {
                                                state.label(command).unwrap_or(command.label())
                                            })
                                            .unwrap_or(match icon {
                                                "chevron-left" => "Previous page",
                                                "chevron-right" => "Next page",
                                                _ => "Commands",
                                            });
                                        let key =
                                            command.and_then(|command| self.keymap.label(command));
                                        let response =
                                            toolbox::button(ui, icon, on, tip, key.as_deref());
                                        if command == Some(Command::PresentDraw) {
                                            pen = Some(response.rect);
                                        }
                                        if response.clicked() {
                                            clicked.push(match (icon, command) {
                                                (_, Some(command)) => ToolboxClick::Run(command),
                                                ("chevron-left", None) => ToolboxClick::Previous,
                                                ("chevron-right", None) => ToolboxClick::Next,
                                                _ => ToolboxClick::Run(Command::Palette),
                                            });
                                        }
                                    }
                                }
                                pen
                            })
                            .inner
                        })
                        .inner
                });
            state.toolbox.push(area.response.rect);
            // The pen's colours, over its button.
            if mode == Mode::Pen
                && let Some(pen) = area.inner
            {
                let colors = egui::Area::new(egui::Id::new("present-colors"))
                    .order(egui::Order::Foreground)
                    .fade_in(false)
                    .interactable(awake)
                    .pivot(Align2::LEFT_BOTTOM)
                    .fixed_pos(pos2(pen.left(), area.response.rect.top() - 6.0))
                    .show(ctx, |ui| {
                        ui.set_opacity(shown);
                        glyphst_ui::pane::popover(ctx)
                            .inner_margin(egui::Margin::symmetric(8, 7))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 8.0;
                                    for (index, (name, color)) in INK.iter().enumerate() {
                                        if swatch(ui, *color, index == state.color, name).clicked()
                                        {
                                            clicked.push(ToolboxClick::Color(index));
                                        }
                                    }
                                });
                            });
                    });
                state.toolbox.push(colors.response.rect);
            }
        }
        for click in clicked {
            match click {
                ToolboxClick::Run(command) => self.run_or_notify(command, ctx),
                ToolboxClick::Previous => self.pending_actions.push(ViewAction::PreviousPage),
                ToolboxClick::Next => self.pending_actions.push(ViewAction::NextPage),
                ToolboxClick::Color(index) => {
                    if let Some(state) = self.presenting.as_mut() {
                        state.color = index;
                    }
                }
            }
        }

        // The system's pointer only over the toolbox and dialogs; on the
        // page the cursor is hidden or drawn.
        if !dialog && !over_toolbox {
            ctx.set_cursor_icon(CursorIcon::None);
        }
        let Some(state) = self.presenting.as_mut() else {
            return;
        };
        // Hidden, the cursor is locked where it waits, at the centre;
        // shown again, it comes back there.
        let lock = lockable && state.mode == Mode::Hidden && !dialog;
        if lock != state.locked {
            if lock {
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(
                    egui::viewport::CursorGrab::Locked,
                ));
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CursorPosition(screen.center()));
                unlock(ctx);
            }
            state.locked = lock;
        }
        if !state.trail.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if state.clock.is_some_and(|clock| !clock.is_paused()) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if state.mode != Mode::Hidden && !idle {
            // The toolbox fades once the pointer rests.
            ctx.request_repaint_after(
                TOOLBOX_IDLE.saturating_sub(now.saturating_duration_since(state.moved)),
            );
        }
    }
}

/// A click on the toolbox, run once its frame is drawn.
enum ToolboxClick {
    Run(Command),
    Previous,
    Next,
    Color(usize),
}

/// The toolbox's buttons in groups (frame 02h); `None` are the page turns
/// and the palette.
const TOOLBOX: [&[(&str, Option<Command>)]; 4] = [
    &[("chevron-left", None), ("chevron-right", None)],
    &[
        ("mouse-pointer-2", Some(Command::PresentCursor)),
        ("pencil", Some(Command::PresentDraw)),
        ("highlighter", Some(Command::PresentHighlight)),
        ("eraser", Some(Command::PresentErase)),
    ],
    &[
        ("undo-2", Some(Command::PresentUndo)),
        ("redo-2", Some(Command::PresentRedo)),
        ("trash-2", Some(Command::PresentClear)),
        ("square", Some(Command::PresentBlack)),
    ],
    &[("ellipsis", None)],
];

/// The cursor back to the system.
fn unlock(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(
        egui::viewport::CursorGrab::None,
    ));
}

/// The chip in the bottom-right corner (frames 02g, 02h): the page, and
/// the clock once the timer tool started it.
fn corner_chip(
    ui: &egui::Ui,
    screen: Rect,
    current: usize,
    pages: usize,
    clock: Option<&(String, Pace)>,
    tokens: &Tokens,
) {
    let painter = ui.painter();
    let font = theme::mono(11.0);
    let width = pages.to_string().len().max(2);
    let page = format!("{:0width$} / {pages:0width$}", current + 1);
    let mut galleys = vec![painter.layout_no_wrap(page, font.clone(), tokens.muted)];
    if let Some((clock, pace)) = clock {
        let color = match pace {
            Pace::OnTime => tokens.muted,
            Pace::Closing => tokens.accent_text,
            Pace::Over => tokens.err,
        };
        galleys.push(painter.layout_no_wrap("·".into(), font.clone(), tokens.faint));
        galleys.push(painter.layout_no_wrap(clock.clone(), font, color));
    }
    let gap = 8.0;
    let inner: f32 = galleys.iter().map(|galley| galley.size().x).sum::<f32>()
        + gap * (galleys.len() - 1) as f32;
    let height = galleys[0].size().y + 8.0;
    let chip = Rect::from_min_size(
        pos2(
            screen.right() - GUTTER - inner - 16.0,
            screen.bottom() - GUTTER - height,
        ),
        vec2(inner + 16.0, height),
    );
    painter.rect(
        chip,
        tokens.ctl_radius(),
        tokens.surface,
        tokens.hairline(),
        egui::StrokeKind::Inside,
    );
    let mut x = chip.left() + 8.0;
    for galley in galleys {
        let size = galley.size();
        painter.galley(
            pos2(x, chip.center().y - size.y / 2.0),
            galley,
            Color32::WHITE,
        );
        x += size.x + gap;
    }
}

/// The cursor drawn on the page: the laser with its tail, the pencil
/// with its tip at the pointer, or the eraser's circle.
fn paint_cursor(ui: &egui::Ui, state: &Presenting, pos: Pos2, now: Instant) {
    let painter = ui.painter();
    match state.mode {
        Mode::Eraser => {
            painter.circle(
                pos,
                ERASER_RADIUS,
                Color32::from_white_alpha(20),
                Stroke::new(1.5, Color32::from_gray(40)),
            );
            painter.circle_stroke(pos, ERASER_RADIUS + 1.5, Stroke::new(1.0, Color32::WHITE));
        }
        tool @ (Mode::Pen | Mode::Highlighter) => {
            let (icon, color) = match tool {
                Mode::Pen => ("pencil", INK[state.color].1),
                _ => ("highlighter", Color32::from_rgb(0xC9, 0xA2, 0x00)),
            };
            // The pencil's tip is its bottom-left corner.
            let rect = Rect::from_min_size(pos - vec2(0.0, 20.0), vec2(20.0, 20.0));
            glyphst_ui::icons::paint(
                ui,
                rect.translate(vec2(1.0, 1.0)),
                icon,
                20.0,
                Color32::WHITE,
            );
            glyphst_ui::icons::paint(ui, rect, icon, 20.0, color);
        }
        Mode::Laser => {
            painter.add(meteor(&state.trail, pos, now));
            painter.circle_filled(pos, 13.0, LASER.gamma_multiply(0.25));
            painter.circle(pos, 5.0, LASER, Stroke::new(1.0, Color32::WHITE));
        }
        Mode::Hidden => {}
    }
}

/// The laser's tail (frame 02j, L1): one ribbon along the pointer's path
/// to `head`, thinning and fading toward its end and as it ages.
fn meteor(trail: &std::collections::VecDeque<(Pos2, Instant)>, head: Pos2, now: Instant) -> Shape {
    let mut points: Vec<(Pos2, f32)> = trail
        .iter()
        .map(|(at, when)| {
            let age = now.saturating_duration_since(*when).as_secs_f32() / TRAIL.as_secs_f32();
            (*at, (1.0 - age).clamp(0.0, 1.0))
        })
        .collect();
    if points.last().is_none_or(|(last, _)| *last != head) {
        points.push((head, 1.0));
    }
    if points.len() < 2 {
        return Shape::Noop;
    }
    let mut along = vec![0.0f32; points.len()];
    for index in 1..points.len() {
        along[index] = along[index - 1] + (points[index].0 - points[index - 1].0).length();
    }
    let total = along[points.len() - 1].max(1.0);
    let mut mesh = egui::Mesh::default();
    for (index, &(at, life)) in points.iter().enumerate() {
        let before = points[index.saturating_sub(1)].0;
        let after = points[(index + 1).min(points.len() - 1)].0;
        let direction = (after - before).normalized();
        let normal = vec2(-direction.y, direction.x);
        // 0 at the tail's end, 1 at the head.
        let t = along[index] / total;
        let half = 0.4 + 4.2 * t.powf(1.6) * life.sqrt();
        let color = LASER.gamma_multiply(t.powf(1.3) * life * 0.9);
        mesh.colored_vertex(at + normal * half, color);
        mesh.colored_vertex(at - normal * half, color);
    }
    for index in 0..points.len() as u32 - 1 {
        let i = index * 2;
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i + 1, i + 3, i + 2);
    }
    Shape::mesh(mesh)
}

/// A colour of the pen's popover; the chosen one is ringed.
fn swatch(ui: &mut egui::Ui, color: Color32, chosen: bool, name: &str) -> egui::Response {
    let tokens = Tokens::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::click());
    ui.painter().circle_filled(rect.center(), 7.0, color);
    if chosen {
        ui.painter()
            .circle_stroke(rect.center(), 9.0, Stroke::new(2.0, tokens.text));
    } else if response.hovered() {
        ui.painter()
            .circle_stroke(rect.center(), 8.5, Stroke::new(1.0, tokens.rule_strong));
    }
    glyphst_ui::overlay::tooltip(response, name, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presenting_goes_fullscreen_only_when_asked_and_not_already() {
        assert!(goes_fullscreen(true, true, false));
        assert!(!goes_fullscreen(false, true, false));
        assert!(
            !goes_fullscreen(true, false, false),
            "scripted runs keep their size"
        );
        assert!(
            !goes_fullscreen(true, true, true),
            "already fullscreen: leaving keeps it"
        );
    }
}
