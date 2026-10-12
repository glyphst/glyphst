//! The reading stage's chrome: the backdrop, the rail, the drawers, the
//! dock and the progress line. `stage.rs` has the geometry; this draws it.
//! Floating parts are egui Areas above the document view, so the pointer
//! over them never reaches the pages.

use egui::{Align2, CornerRadius, Id, Order, Rect, Stroke, pos2, vec2};

use super::*;
use crate::{
    gpu::BackdropCallback,
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
            present: presenting,
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
        let drawn = self.document_view(ui, layout.reading, actions, pad, seconds, now);
        if presenting {
            return drawn;
        }
        let visible = self.controls_visible(&ctx, active);
        let opacity = ctx.animate_bool_with_time(Id::new("stage-controls"), visible, 0.25);
        self.rail(&ctx, &layout, opacity);
        self.drawers(&ctx, &layout, active, now);
        self.progress_line(&ctx, &layout, active);
        self.dock(&ctx, &layout, active, opacity);
        drawn
    }

    /// The background effect behind the pages.
    pub(super) fn background_effect(&self, tokens: &Tokens) -> EffectUse {
        let id = self.prefs.background.clone();
        let def = self
            .with_gpu(|gpu| gpu.effects().find(&id).cloned())
            .flatten();
        let params = match (&def, id.as_str()) {
            (_, glyphst_effects::SOLID) => {
                let [r, g, b] = tokens.pasteboard_rgb();
                [r, g, b, 1.0]
            }
            (Some(def), _) => def.default_params(),
            (None, _) => [0.0; 4],
        };
        EffectUse { id, params }
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
                        requests = sidebar::show(
                            ui,
                            &mut self.tabs[active],
                            &mut self.prefs.sidebar,
                            &mut self.sidebar_state,
                            &info,
                            now,
                        );
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
        if let Some(shown) = translation_shown
            && let Some(session) = self.tabs[active].translation.as_mut()
        {
            session.show_translated = shown;
        }
    }
}

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
