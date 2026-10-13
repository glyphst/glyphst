//! The reading stage's chrome: the backdrop, the rail, the drawers, the
//! dock and the progress line. `stage.rs` has the geometry; this draws it.
//! Floating parts are egui Areas above the document view, so the pointer
//! over them never reaches the pages.

use egui::{Align2, CornerRadius, Id, Order, Rect, Stroke, pos2, vec2};

use super::*;
use crate::{
    book_effects::{Defaults, Resolved, resolve},
    gpu::BackdropCallback,
    overview,
    panes::{self, Divider},
    stage::{self, ChromeFade, StageInput, StageLayout},
};

/// The chrome's state between frames.
#[derive(Default)]
pub(super) struct StageChrome {
    pub fade: ChromeFade,
    /// Where the rail and the dock were drawn last frame.
    rail: Option<Rect>,
    dock: Option<Rect>,
    /// The page last asked for while scrubbing the progress line.
    scrub: Option<usize>,
    /// Where the panes were drawn last frame, for dropping a tab dragged
    /// from the top bar.
    pub panes: Vec<(u64, Rect)>,
    /// The tab being dragged from the top bar.
    pub tab_drag: Option<u64>,
}

impl ReaderApp {
    /// The stage of the active document: backdrop, pages and chrome.
    /// Returns whether the view animates and whether every page is drawn.
    pub(super) fn stage(
        &mut self,
        ui: &mut egui::Ui,
        actions: Vec<ViewAction>,
        pad: &GamepadInput,
        seconds: f64,
        now: Instant,
    ) -> (bool, bool) {
        let Some(active) = self.active else {
            return (false, true);
        };
        let ctx = ui.ctx().clone();
        let tokens = Tokens::get(&ctx);
        let rect = ui.max_rect();
        let presenting = self.tabs[active].present.is_some();
        let layout = StageLayout::compute(StageInput {
            stage: rect,
            rail_buttons: SidebarTab::ALL.len(),
            drawer: self.prefs.show_sidebar.then_some(self.prefs.drawer_width),
            assistant: self.assistant.open.then_some(self.prefs.assistant_width),
            present: presenting || self.zen,
        });
        let ppp = ctx.pixels_per_point();
        let pointer = ctx
            .pointer_hover_pos()
            .filter(|pos| rect.contains(*pos))
            .map(|pos| [(pos.x - rect.min.x) * ppp, (pos.y - rect.min.y) * ppp]);
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            BackdropCallback {
                rect,
                background: self.background_effect(&tokens),
                time: self.started.elapsed().as_secs_f32(),
                pointer,
            },
        ));
        // The focused pane's tab is always on the stage.
        let focused_id = self.tabs[active].id;
        if !self.panes.contains(focused_id) {
            self.panes.show(focused_id);
        }
        let (panes, dividers) = if presenting {
            (vec![(focused_id, layout.reading)], Vec::new())
        } else {
            self.panes.rects(layout.reading)
        };
        let mut actions = Some(actions);
        let (mut animating, mut drawn) = (false, true);
        let mut keep = Vec::new();
        let split = panes.len() > 1 && !self.zen;
        for &(id, pane) in &panes {
            // Split panes keep a strip at the top for their title chip.
            let rect = if split {
                pane.with_min_y((pane.top() + PANE_HEADER).min(pane.bottom()))
            } else {
                pane
            };
            let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                continue;
            };
            let focused = index == active;
            let mut view = PaneView {
                rect,
                index,
                focused,
                actions: if focused {
                    actions.take().unwrap_or_default()
                } else {
                    Vec::new()
                },
                pad: focused.then_some(pad),
                seconds,
                now,
            };
            // The page overview stands in for the pages.
            if self.tabs[index].overview.is_some() {
                let tab = &mut self.tabs[index];
                for action in std::mem::take(&mut view.actions) {
                    tab.apply(action, false);
                }
                if tab.overview.is_some() {
                    if let Some(page) = overview::show(ui, rect, tab) {
                        tab.overview = None;
                        tab.apply(ViewAction::GoToPage(page), false);
                    }
                    continue;
                }
            }
            let (pane_animating, pane_drawn) = self.document_view(ui, view, &mut keep);
            animating |= pane_animating;
            drawn &= pane_drawn;
        }
        if let Some(dropped) = self.with_gpu(|gpu| gpu.evict(&keep, PAGE_BUDGET_BYTES)) {
            for key in dropped {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == key.tab) {
                    tab.forget(key.page);
                }
            }
        }
        self.chrome.panes = panes.clone();
        self.note_popover(&ctx);
        self.translation_card(&ctx);
        self.quick_ask_ui(&ctx, layout.reading);
        if presenting || self.zen {
            return (animating, drawn);
        }
        if panes.len() > 1 {
            self.pane_chrome(ui, &panes, &dividers);
        }
        self.tab_drop(&ctx);
        let active = self.active.unwrap_or(active);
        let visible = self.controls_visible(&ctx, active);
        let opacity = ctx.animate_bool_with_time(Id::new("stage-controls"), visible, 0.25);
        self.rail(&ctx, &layout, opacity);
        self.drawers(&ctx, &layout, active, now);
        self.progress_line(&ctx, &layout, active);
        self.dock(&ctx, &layout, active, opacity);
        (animating, drawn)
    }

    /// With two or more panes: each pane's title chip (with a button that
    /// closes the pane), the focused pane's outline, and the dividers.
    fn pane_chrome(&mut self, ui: &mut egui::Ui, panes: &[(u64, Rect)], dividers: &[Divider]) {
        let ctx = ui.ctx().clone();
        let tokens = Tokens::get(&ctx);
        let focused = self.active.map(|active| self.tabs[active].id);
        let mut close = None;
        let mut focus = None;
        for &(id, rect) in panes {
            let Some(tab) = self.tabs.iter().find(|tab| tab.id == id) else {
                continue;
            };
            let is_focused = focused == Some(id);
            if is_focused {
                ui.painter().rect_stroke(
                    rect.shrink(1.0),
                    CornerRadius::same(10),
                    Stroke::new(1.5, tokens.accent),
                    egui::StrokeKind::Inside,
                );
            }
            let icon = if tab.is_reflowed() {
                "book-open"
            } else {
                "file-text"
            };
            let title = truncate(&tab.title, 40);
            egui::Area::new(Id::new(("pane-chip", id)))
                .order(Order::Middle)
                .fixed_pos(rect.min + vec2(4.0, 5.0))
                .show(&ctx, |ui| {
                    let frame = egui::Frame::new()
                        .fill(if is_focused {
                            tokens.accent_soft
                        } else {
                            tokens.card
                        })
                        .stroke(Stroke::new(1.0, tokens.border))
                        .corner_radius(CornerRadius::same(8))
                        .shadow(tokens.card_shadow())
                        .inner_margin(egui::Margin {
                            left: 8,
                            right: 2,
                            top: 2,
                            bottom: 2,
                        });
                    let response = frame
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.horizontal(|ui| {
                                let color = if is_focused {
                                    tokens.accent_text
                                } else {
                                    tokens.text_muted
                                };
                                ui.add(icons::image(icon, 14.0, color));
                                ui.label(
                                    egui::RichText::new(title)
                                        .font(theme::medium(12.5))
                                        .color(color),
                                );
                                if icons::button(ui, "x", 22.0, false, "Close the pane").clicked() {
                                    close = Some(id);
                                }
                            });
                        })
                        .response
                        .interact(egui::Sense::click());
                    if response.clicked() {
                        focus = Some(id);
                    }
                });
        }
        for divider in dividers {
            let horizontal = divider.axis == craft_ui::layout::SplitAxis::Horizontal;
            let style = craft_ui::layout::ResizeHandleStyle {
                hovered: tokens.accent.gamma_multiply(0.5),
                dragged: tokens.accent,
                focused: tokens.accent,
            };
            let handle = if horizontal {
                Rect::from_center_size(
                    divider.rect.center(),
                    vec2(4.0, divider.rect.height() - 28.0),
                )
            } else {
                Rect::from_center_size(
                    divider.rect.center(),
                    vec2(divider.rect.width() - 28.0, 4.0),
                )
            };
            let delta = craft_ui::layout::ResizeHandle::new(
                Id::new(("pane-divider", divider.path_bits, divider.depth)),
                divider.axis,
                handle,
                "Resize panes",
                style,
            )
            .value(divider.first, 0.0..=divider.available)
            .show(ui)
            .delta;
            if delta != 0.0 {
                let first = (divider.first + delta).clamp(
                    panes::MIN_PANE.min(divider.available / 2.0),
                    (divider.available - panes::MIN_PANE).max(divider.available / 2.0),
                );
                self.panes.resize(
                    divider.path(),
                    craft_ui::layout::SplitSize::Ratio(first / divider.available.max(1.0)),
                );
            }
        }
        if let Some(id) = close
            && self.panes.close_pane(id)
            && let Some(focused) = self.panes.focused()
        {
            self.focus_tab(focused);
        } else if let Some(id) = focus {
            self.focus_tab(id);
        }
    }

    /// While a tab is dragged from the top bar over the stage: where it
    /// would go (beside a pane, or in it), and putting it there on release.
    fn tab_drop(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.chrome.tab_drag else {
            return;
        };
        let (pointer, released) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.any_released()));
        let target = pointer.and_then(|pos| {
            self.chrome
                .panes
                .iter()
                .find(|(_, rect)| rect.contains(pos))
                .and_then(|&(pane, rect)| {
                    let zone = craft_ui::docking::drop_zone(rect, 0.0, pos)?;
                    Some((pane, zone, craft_ui::docking::zone_rect(rect, zone)?))
                })
        });
        if released {
            self.chrome.tab_drag = None;
            if let Some((pane, zone, _)) = target {
                self.drop_tab(tab, pane, zone);
            }
            return;
        }
        if let Some((pane, _, preview)) = target
            && pane != tab
        {
            let tokens = Tokens::get(ctx);
            let painter = ctx.layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::new("tab-drop-preview"),
            ));
            painter.rect(
                preview.shrink(4.0),
                CornerRadius::same(12),
                tokens.accent.gamma_multiply(0.18),
                Stroke::new(2.0, tokens.accent),
                egui::StrokeKind::Inside,
            );
        }
        ctx.request_repaint();
    }

    /// The background effect behind the pages: the focused book's. Solid
    /// takes the theme's colour until the user sets one.
    pub(super) fn background_effect(&self, tokens: &Tokens) -> EffectUse {
        let mut background = self.effects_for(self.active).background;
        if background.id == glyphst_effects::SOLID
            && !self
                .prefs
                .effect_params
                .contains_key(glyphst_effects::SOLID)
        {
            let [r, g, b] = tokens.pasteboard_rgb();
            background.params = [r, g, b, 1.0];
        }
        background
    }

    /// The document path effects are chosen by.
    pub(crate) fn book_key(&self, index: usize) -> String {
        self.tabs[index].path.display().to_string()
    }

    /// The effects tab `index` shows: its book's choices, else the
    /// defaults (`None`, the home tab, has only the defaults).
    pub(crate) fn effects_for(&self, index: Option<usize>) -> Resolved {
        let book = index.and_then(|index| self.prefs.books.get(&self.book_key(index)));
        let defaults = Defaults {
            background: &self.prefs.background,
            transition: self.prefs.transition.as_deref(),
            overlay: self.prefs.overlay.as_deref(),
            params: &self.prefs.effect_params,
        };
        self.with_gpu(|gpu| resolve(book, defaults, |id| gpu.effects().find(id)))
            .unwrap_or_else(|| resolve(book, defaults, |_| None))
    }

    /// Whether the dock and the rail show this frame: after pointer
    /// movement, and while something needs them.
    fn controls_visible(&mut self, ctx: &egui::Context, active: usize) -> bool {
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let over = |rect: Option<Rect>| {
            pointer.is_some_and(|pos| rect.is_some_and(|rect| rect.expand(8.0).contains(pos)))
        };
        let keep = !self.prefs.autohide_controls
            || ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down())
            || ctx.memory(|memory| memory.focused().is_some())
            || egui::Popup::is_any_open(ctx)
            || self.tabs[active].find.open
            || self.palette.open
            || self.settings_window.open
            || over(self.chrome.rail)
            || over(self.chrome.dock);
        if let Some(shown) = self.controls {
            return shown;
        }
        let now = ctx.input(|i| i.time);
        let visible = self.chrome.fade.visible(now, keep);
        if let Some(left) = self.chrome.fade.remaining(now) {
            ctx.request_repaint_after(Duration::from_secs_f64(left + 0.05));
        }
        visible
    }

    /// The floating rail of drawer buttons.
    fn rail(&mut self, ctx: &egui::Context, layout: &StageLayout, opacity: f32) {
        let Some(rect) = layout.rail else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let mut toggled = None;
        let area = egui::Area::new(Id::new("stage-rail"))
            .order(Order::Middle)
            .fixed_pos(rect.min)
            .interactable(opacity > 0.05)
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                floating_frame(&tokens, 12)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = stage::RAIL_STEP - 32.0;
                        for tab in SidebarTab::ALL {
                            let open = self.prefs.show_sidebar && self.prefs.sidebar == tab;
                            if icons::button(ui, tab.icon(), 32.0, open, tab.label()).clicked() {
                                toggled = Some((tab, open));
                            }
                        }
                    });
            });
        self.chrome.rail = Some(area.response.rect);
        match toggled {
            Some((_, true)) => self.prefs.show_sidebar = false,
            Some((tab, false)) => {
                self.prefs.show_sidebar = true;
                self.prefs.sidebar = tab;
            }
            None => {}
        }
    }

    /// The left drawer (outline, pages, bookmarks, notes, search,
    /// translation) and the assistant on the right.
    fn drawers(&mut self, ctx: &egui::Context, layout: &StageLayout, active: usize, now: Instant) {
        let tokens = Tokens::get(ctx);
        if let Some(rect) = layout.drawer {
            let warnings: Vec<String> = self
                .user_settings
                .warnings
                .iter()
                .chain(&self.translator.warnings)
                .cloned()
                .collect();
            let info = TranslationInfo {
                target: self
                    .translator
                    .target_language(self.prefs.target_language.as_deref()),
                pinned: &self.translator.pinned,
                warnings: &warnings,
                ready: self.user_settings.get().is_some(),
            };
            let mut requests = Vec::new();
            let mut close = false;
            let mut resize = 0.0;
            // The Effects drawer edits a copy of this book's choices.
            let showing_effects = self.prefs.sidebar == SidebarTab::Effects;
            let effects = if showing_effects {
                self.effect_defs()
            } else {
                Vec::new()
            };
            let key = self.book_key(active);
            let mut book = self.prefs.books.get(&key).cloned().unwrap_or_default();
            let mut book_changed = false;
            let title = self.tabs[active].title.clone();
            let time = self.started.elapsed().as_secs_f32();
            egui::Area::new(Id::new("stage-drawer"))
                .order(Order::Middle)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    floating_frame(&tokens, 12).show(ui, |ui| {
                        let inner = rect.size() - vec2(26.0, 26.0);
                        ui.set_min_size(inner);
                        ui.set_max_size(inner);
                        close = drawer_header(
                            ui,
                            &tokens,
                            self.prefs.sidebar.icon(),
                            self.prefs.sidebar.label(),
                        );
                        if showing_effects {
                            // The previews are animated.
                            ui.ctx().request_repaint();
                            let find = |id: Option<&str>| {
                                id.and_then(|id| effects.iter().find(|def| def.id == id))
                            };
                            book_changed = crate::effects_page::book_page(
                                ui,
                                crate::effects_page::BookPage {
                                    title: &title,
                                    book: &mut book,
                                    default_background: find(Some(&self.prefs.background)),
                                    default_transition: find(self.prefs.transition.as_deref()),
                                    default_overlay: find(self.prefs.overlay.as_deref()),
                                    effects: &effects,
                                    params: &self.prefs.effect_params,
                                    has_wallpaper: self.prefs.wallpaper.is_some(),
                                    time,
                                },
                            );
                        } else {
                            requests = sidebar::show(
                                ui,
                                &mut self.tabs[active],
                                &mut self.prefs.sidebar,
                                &mut self.sidebar_state,
                                &info,
                                now,
                            );
                        }
                    });
                    resize = resize_handle(
                        ui,
                        &tokens,
                        "drawer-resize",
                        rect.right(),
                        rect,
                        self.prefs.drawer_width,
                    );
                });
            if close {
                self.prefs.show_sidebar = false;
            }
            if book_changed {
                book.used = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| since.as_secs());
                self.prefs.books.insert(key, book);
                crate::book_effects::trim(&mut self.prefs.books);
            }
            if resize != 0.0 {
                self.prefs.drawer_width =
                    (self.prefs.drawer_width + resize).clamp(stage::DRAWER_MIN, 900.0);
            }
            let mut actions = Vec::new();
            for request in requests {
                self.apply_sidebar(request, &mut actions, ctx);
            }
            self.pending_actions.extend(actions);
        }
        if let Some(rect) = layout.assistant {
            let tab = &self.tabs[active];
            let has_page = tab.text.get(tab.current_page()).is_some();
            let has_selection = tab.selection.is_some();
            let (models, model) = match self.user_settings.get() {
                Some(settings) => {
                    let main = settings.ai.resolved_main();
                    (
                        crate::settings_providers::model_choices(settings),
                        Some(glyphst_settings::ModelRef::new(main.provider, main.model)),
                    )
                }
                None => (Vec::new(), None),
            };
            let mut request = None;
            let mut resize = 0.0;
            egui::Area::new(Id::new("stage-assistant"))
                .order(Order::Middle)
                .fixed_pos(rect.min)
                .show(ctx, |ui| {
                    floating_frame(&tokens, 12).show(ui, |ui| {
                        let inner = rect.size() - vec2(26.0, 26.0);
                        ui.set_min_size(inner);
                        ui.set_max_size(inner);
                        request =
                            self.assistant
                                .ui(ui, has_page, has_selection, &models, model.clone());
                    });
                    // Dragging the left edge left widens it.
                    resize = -resize_handle(
                        ui,
                        &tokens,
                        "assistant-resize",
                        rect.left(),
                        rect,
                        self.prefs.assistant_width,
                    );
                });
            if resize != 0.0 {
                self.prefs.assistant_width =
                    (self.prefs.assistant_width + resize).clamp(stage::ASSISTANT_MIN, 1200.0);
            }
            match request {
                Some(PanelAction::Send(text)) => self.ask(text, ctx),
                Some(PanelAction::NewSession) => self.assistant.new_session(),
                Some(PanelAction::LoadSession(id)) => self.assistant.load_session(id),
                Some(PanelAction::SetModel(model)) => {
                    self.user_settings
                        .edit(|settings| settings.ai.main_model = Some(model));
                }
                None => {}
            }
        }
    }

    /// The reading progress along the stage's bottom: the part read, ticks
    /// at chapters, and scrubbing to a page.
    fn progress_line(&mut self, ctx: &egui::Context, layout: &StageLayout, active: usize) {
        let Some(line) = layout.progress else {
            return;
        };
        let tab = &self.tabs[active];
        let pages = tab.layout.len();
        if pages < 2 {
            return;
        }
        let tokens = Tokens::get(ctx);
        let current = tab.current_page();
        let ticks = stage::chapter_ticks(
            tab.outline.iter().map(|entry| (entry.depth, entry.page)),
            pages,
        );
        let hit = Rect::from_min_max(pos2(line.left(), line.bottom() - 12.0), line.max);
        let mut scrub = None;
        egui::Area::new(Id::new("stage-progress"))
            .order(Order::Middle)
            .fixed_pos(hit.min)
            .show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(hit.size(), egui::Sense::click_and_drag());
                let active_line = response.hovered() || response.dragged();
                let height = ui.ctx().animate_value_with_time(
                    Id::new("stage-progress-height"),
                    if active_line {
                        stage::PROGRESS_HOVER
                    } else {
                        stage::PROGRESS
                    },
                    0.15,
                );
                let bar = Rect::from_min_max(pos2(rect.left(), rect.bottom() - height), rect.max);
                let painter = ui.painter();
                painter.rect_filled(
                    bar,
                    CornerRadius::ZERO,
                    tokens.text_faint.gamma_multiply(0.3),
                );
                let read = Rect::from_min_size(
                    bar.min,
                    vec2(
                        bar.width() * stage::read_fraction(current, pages),
                        bar.height(),
                    ),
                );
                painter.rect_filled(read, CornerRadius::ZERO, tokens.accent);
                // Chapter ticks while the line is open for scrubbing.
                if height > stage::PROGRESS + 1.0 {
                    for tick in &ticks {
                        let x = bar.left() + bar.width() * tick;
                        painter.vline(x, bar.y_range(), Stroke::new(1.5, tokens.pasteboard));
                    }
                }
                if let Some(pos) = response.hover_pos() {
                    let page = stage::page_at(pos.x, bar.left(), bar.right(), pages);
                    painter.circle_filled(pos2(pos.x, bar.center().y), height * 0.9, tokens.accent);
                    let chapter = tab
                        .outline
                        .iter()
                        .rfind(|entry| entry.page.is_some_and(|start| start <= page))
                        .map(|entry| format!("{} · ", entry.title))
                        .unwrap_or_default();
                    response
                        .clone()
                        .on_hover_text_at_pointer(format!("{chapter}page {} of {pages}", page + 1));
                }
                if (response.clicked() || response.dragged())
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    scrub = Some(stage::page_at(pos.x, bar.left(), bar.right(), pages));
                }
                if response.drag_stopped() {
                    self.chrome.scrub = None;
                }
            });
        if let Some(page) = scrub
            && self.chrome.scrub != Some(page)
        {
            self.chrome.scrub = Some(page);
            self.pending_actions.push(ViewAction::GoToPage(page));
        }
    }

    /// The floating dock of reading controls.
    fn dock(&mut self, ctx: &egui::Context, layout: &StageLayout, active: usize, opacity: f32) {
        let Some(anchor) = layout.dock_anchor else {
            return;
        };
        let tokens = Tokens::get(ctx);
        let area = egui::Area::new(Id::new("stage-dock"))
            .order(Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(anchor)
            .interactable(opacity > 0.05)
            .show(ctx, |ui| {
                ui.multiply_opacity(opacity);
                floating_frame(&tokens, 22)
                    .inner_margin(egui::Margin::symmetric(8, 5))
                    .show(ui, |ui| {
                        self.dock_controls(ui, active, layout.reading.width())
                    });
            });
        self.chrome.dock = Some(area.response.rect);
    }

    /// The dock's buttons; with less `room`, the less used ones go (they
    /// stay in the palette and on their keys).
    fn dock_controls(&mut self, ui: &mut egui::Ui, active: usize, room: f32) {
        let full = room >= DOCK_FULL;
        let compact = room >= DOCK_COMPACT;
        let tokens = Tokens::get(ui.ctx());
        let tab = &self.tabs[active];
        let pages = tab.layout.len();
        let mut actions = Vec::new();
        let mut find = false;
        let mut font_step = 0.0;
        let mut translate_document = false;
        let mut translation_shown = None;
        let mut assistant = false;
        let mut run = None;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let tip = self.keymap.tip("Back", Command::Back);
            if full
                && ui
                    .add_enabled_ui(tab.can_go_back(), |ui| {
                        icons::button(ui, "arrow-left", 30.0, false, &tip)
                    })
                    .inner
                    .clicked()
            {
                actions.push(ViewAction::Back);
            }
            let tip = self.keymap.tip("Forward", Command::Forward);
            if full
                && ui
                    .add_enabled_ui(tab.can_go_forward(), |ui| {
                        icons::button(ui, "arrow-right", 30.0, false, &tip)
                    })
                    .inner
                    .clicked()
            {
                actions.push(ViewAction::Forward);
            }
            if full {
                dock_divider(ui, &tokens);
            }
            let tip = self.keymap.tip("Previous page", Command::PreviousPage);
            if icons::button(ui, "chevron-left", 30.0, false, &tip).clicked() {
                actions.push(ViewAction::PreviousPage);
            }
            let current = tab.current_page() + 1;
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.page_input)
                    .desired_width(44.0)
                    .horizontal_align(egui::Align::Center)
                    .font(theme::medium(13.0))
                    .hint_text(
                        egui::RichText::new(current.to_string())
                            .font(theme::medium(13.0))
                            .color(tokens.text),
                    ),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Ok(page) = self.page_input.trim().parse::<usize>() {
                    actions.push(ViewAction::GoToPage(page.saturating_sub(1)));
                }
                self.page_input.clear();
            }
            ui.label(egui::RichText::new(format!("/ {pages}")).color(tokens.text_muted));
            let tip = self.keymap.tip("Next page", Command::NextPage);
            if icons::button(ui, "chevron-right", 30.0, false, &tip).clicked() {
                actions.push(ViewAction::NextPage);
            }
            dock_divider(ui, &tokens);
            if !compact {
                // Only the panels' toggles besides the pages.
                let tip = self
                    .keymap
                    .tip("Ask about this document", Command::Assistant);
                if icons::button(ui, "sparkles", 30.0, self.assistant.open, &tip).clicked() {
                    assistant = true;
                }
                let tip = self.keymap.tip("Find", Command::Find);
                if icons::button(ui, "search", 30.0, tab.find.open, &tip).clicked() {
                    find = true;
                }
                return;
            }
            let tip = self.keymap.tip("Zoom out", Command::ZoomOut);
            if icons::button(ui, "zoom-out", 30.0, false, &tip).clicked() {
                actions.push(ViewAction::ZoomOut);
            }
            ui.add_sized(
                [44.0, 30.0],
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{:.0}%",
                        tab.view.scale / tab.fit_scale() * 100.0
                    ))
                    .font(theme::medium(12.5)),
                ),
            );
            let tip = self.keymap.tip("Zoom in", Command::ZoomIn);
            if icons::button(ui, "zoom-in", 30.0, false, &tip).clicked() {
                actions.push(ViewAction::ZoomIn);
            }
            let tip = self.keymap.tip("Fit width", Command::FitWidth);
            if icons::button(ui, "scan", 30.0, false, &tip).clicked() {
                actions.push(ViewAction::FitWidth);
            }
            if full && !tab.is_reflowed() {
                let tip = self
                    .keymap
                    .tip("Rotate clockwise", Command::RotateClockwise);
                if icons::button(ui, "rotate-cw", 30.0, false, &tip).clicked() {
                    run = Some(Command::RotateClockwise);
                }
                let tip = self.keymap.tip("Two-page spread", Command::ToggleSpread);
                let spread = tab.layout.arrangement.spread;
                if icons::button(ui, "book-open", 30.0, spread, &tip).clicked() {
                    run = Some(Command::ToggleSpread);
                }
            }
            if let DocumentLayout::Reflow(layout) = tab.document_layout {
                dock_divider(ui, &tokens);
                let tip = self.keymap.tip("Smaller text", Command::SmallerText);
                if icons::button(ui, "a-arrow-down", 30.0, false, &tip).clicked() {
                    font_step = -1.0;
                }
                ui.label(
                    egui::RichText::new(format!("{:.0} pt", layout.font_size))
                        .font(theme::medium(12.5)),
                );
                let tip = self.keymap.tip("Larger text", Command::LargerText);
                if icons::button(ui, "a-arrow-up", 30.0, false, &tip).clicked() {
                    font_step = 1.0;
                }
            }
            dock_divider(ui, &tokens);
            let tip = self.keymap.tip("Present", Command::Present);
            if icons::button(ui, "presentation", 30.0, tab.present.is_some(), &tip).clicked() {
                actions.push(ViewAction::TogglePresent);
            }
            dock_divider(ui, &tokens);
            let current = self
                .translator
                .target_language(self.prefs.target_language.as_deref());
            let label = current
                .as_deref()
                .map(sidebar::language_label)
                .unwrap_or("Language")
                .to_string();
            let picker = if full {
                widgets::pill_button(ui, Some("languages"), &label, false)
            } else {
                icons::button(ui, "languages", 30.0, false, &label)
            }
            .on_hover_text(format!("Language to translate into: {label}"));
            egui::Popup::menu(&picker).show(|ui| {
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for language in glyphst_translation::TARGET_LANGUAGES {
                            let chosen = current.as_deref() == Some(language.code);
                            if ui
                                .add(egui::Button::selectable(chosen, language.label))
                                .clicked()
                            {
                                self.prefs.target_language = Some(language.code.to_string());
                            }
                        }
                    });
            });
            match &tab.translation {
                Some(session) => {
                    let tip = self
                        .keymap
                        .tip("Show translated pages", Command::ShowTranslated);
                    if icons::button(ui, "globe", 30.0, session.show_translated, &tip).clicked() {
                        translation_shown = Some(!session.show_translated);
                    }
                }
                None => {
                    let tip = self
                        .keymap
                        .tip("Translate this document", Command::TranslateDocument);
                    if icons::button(ui, "globe", 30.0, false, &tip).clicked() {
                        translate_document = true;
                    }
                }
            }
            dock_divider(ui, &tokens);
            let tip = self
                .keymap
                .tip("Ask about this document", Command::Assistant);
            if icons::button(ui, "sparkles", 30.0, self.assistant.open, &tip).clicked() {
                assistant = true;
            }
            let tip = self.keymap.tip("Find", Command::Find);
            if icons::button(ui, "search", 30.0, tab.find.open, &tip).clicked() {
                find = true;
            }
        });
        self.pending_actions.extend(actions);
        if find {
            let tab = &mut self.tabs[active];
            if tab.find.open {
                tab.find.close();
            } else {
                let page = tab.current_page();
                tab.find.show(page);
            }
        }
        if font_step != 0.0 {
            self.change_font_size(font_step);
        }
        if translate_document {
            let ctx = ui.ctx().clone();
            self.start_document_translation(&ctx);
        }
        if assistant {
            self.toggle_assistant();
        }
        if let Some(command) = run {
            let ctx = ui.ctx().clone();
            self.run_or_notify(command, &ctx);
        }
        if let Some(shown) = translation_shown
            && let Some(session) = self.tabs[active].translation.as_mut()
        {
            session.show_translated = shown;
        }
    }
}

/// The strip at the top of each split pane, for its title chip.
const PANE_HEADER: f32 = 40.0;

/// Reading widths from which the dock shows all its buttons, or the
/// zoom, text, present and translation groups.
const DOCK_FULL: f32 = 820.0;
const DOCK_COMPACT: f32 = 560.0;

/// The frame of the stage's floating cards.
fn floating_frame(tokens: &Tokens, radius: u8) -> egui::Frame {
    egui::Frame::new()
        .fill(tokens.card)
        .stroke(Stroke::new(1.0, tokens.border))
        .corner_radius(CornerRadius::same(radius))
        .shadow(tokens.card_shadow())
        .inner_margin(egui::Margin::same(12))
}

/// A drawer's icon, title and close button; returns whether it was closed.
fn drawer_header(ui: &mut egui::Ui, tokens: &Tokens, icon: &str, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.add(icons::image(icon, 16.0, tokens.accent));
        ui.label(egui::RichText::new(title).font(theme::semibold(15.0)));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = icons::button(ui, "x", 26.0, false, "Close").clicked();
        });
    });
    ui.add_space(4.0);
    close
}

/// A thin upright line between groups of dock buttons.
fn dock_divider(ui: &mut egui::Ui, tokens: &Tokens) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 20.0), egui::Sense::hover());
    ui.painter().vline(
        rect.center().x,
        rect.y_range(),
        Stroke::new(1.0, tokens.divider),
    );
}

/// A resize handle along `x` of a drawer `rect`; returns how far it was
/// dragged to the right.
fn resize_handle(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    id: &str,
    x: f32,
    rect: Rect,
    width: f32,
) -> f32 {
    let handle = Rect::from_min_max(
        pos2(x - 2.0, rect.top() + 14.0),
        pos2(x + 2.0, rect.bottom() - 14.0),
    );
    let style = craft_ui::layout::ResizeHandleStyle {
        hovered: tokens.accent.gamma_multiply(0.5),
        dragged: tokens.accent,
        focused: tokens.accent,
    };
    craft_ui::layout::ResizeHandle::new(
        Id::new(id),
        craft_ui::layout::SplitAxis::Horizontal,
        handle,
        "Resize",
        style,
    )
    .value(width, stage::DRAWER_MIN..=1200.0)
    .show(ui)
    .delta
}
