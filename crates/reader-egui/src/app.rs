//! The reader window: tabs (home and documents), chrome, the document view
//! with its effects, preferences and reading positions.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui_wgpu;
use glyphst_effects::EffectKind;
use glyphst_library::{DocumentPosition, LibraryStore};

use crate::{
    document::FrameTimes,
    document_tab::{DocumentTab, ViewAction, ViewInput},
    gamepad::{GamepadInput, Gamepads},
    gpu::{
        EffectUse, PAGE_BUDGET_BYTES, PageKey, ReaderGpu, TransitionUse, ViewCallback, pixel_size,
    },
    harness::Harness,
    home::{Home, HomeAction},
    pickers::{Pickers, is_document},
    prefs::{LastSession, Prefs, SessionFile},
    theme::{self, ThemeKind, ThemePreference, Tokens},
    transition::ViewTransition,
};

/// Positions are written to the library at most this often while reading.
const POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(1);
const TAB_MIN_WIDTH: f32 = 90.0;
const TAB_MAX_LABEL: usize = 32;
const TAB_OVERFLOW_WIDTH: f32 = 28.0;

/// How the app was started.
pub struct Launch {
    pub files: Vec<PathBuf>,
    pub harness: Option<Harness>,
    pub background: Option<String>,
    pub transition: Option<String>,
    /// Opens the documents at this zoom (relative to fit width).
    pub zoom: Option<f32>,
}

pub struct ReaderApp {
    pub(crate) prefs: Prefs,
    theme_kind: Option<ThemeKind>,
    library: LibraryStore,
    pub(crate) tabs: Vec<DocumentTab>,
    /// Index into `tabs`; `None` shows the home tab.
    pub(crate) active: Option<usize>,
    next_tab_id: u64,
    home: Home,
    pickers: Pickers,
    pub(crate) render_state: egui_wgpu::RenderState,
    pub(crate) transition: Option<ViewTransition>,
    started: Instant,
    gamepads: Result<Gamepads, String>,
    pub(crate) frames: FrameTimes,
    continuous: bool,
    pub(crate) repaints: u64,
    last_update: Instant,
    notices: Vec<String>,
    show_outline: bool,
    show_stats: bool,
    page_input: String,
    saved_positions: HashMap<u64, DocumentPosition>,
    last_position_save: Instant,
    /// Scripted runs neither save preferences nor touch the library.
    persist: bool,
    pub(crate) harness: Option<Harness>,
    pub(crate) first_page_ms: Option<f64>,
    pub(crate) upload_ms: Vec<f64>,
    /// Actions queued for the next frame of the document view.
    pub(crate) pending_actions: Vec<ViewAction>,
}

impl ReaderApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the reader needs eframe's wgpu renderer"))?;
        let user_effects = dirs::config_dir().map(|dir| dir.join("glyphst").join("effects"));
        ReaderGpu::install(&render_state, user_effects.as_deref());
        crate::fonts::install(&cc.egui_ctx);
        let persist = launch.harness.is_none();
        let mut prefs = Prefs::load(cc.storage);
        if let Some(background) = launch.background {
            prefs.background = background;
        }
        if let Some(transition) = launch.transition {
            prefs.transition = (transition != "none").then_some(transition);
        }
        let mut app = Self {
            prefs,
            theme_kind: None,
            library: LibraryStore::load(),
            tabs: Vec::new(),
            active: None,
            next_tab_id: 1,
            home: Home::default(),
            pickers: Pickers::default(),
            render_state,
            transition: None,
            started: Instant::now(),
            gamepads: Gamepads::new(),
            frames: FrameTimes::default(),
            continuous: false,
            repaints: 0,
            last_update: Instant::now(),
            notices: Vec::new(),
            show_outline: true,
            show_stats: launch.harness.is_some(),
            page_input: String::new(),
            saved_positions: HashMap::new(),
            last_position_save: Instant::now(),
            persist,
            harness: launch.harness,
            first_page_ms: None,
            upload_ms: Vec::new(),
            pending_actions: Vec::new(),
        };
        let ctx = cc.egui_ctx.clone();
        if launch.files.is_empty() && app.prefs.reopen_last_session && persist {
            let session = app.prefs.last_session.clone();
            for file in &session.files {
                app.open_quietly(Path::new(&file.path), &ctx, Some(file.page));
            }
            app.active = session.active.filter(|&active| active < app.tabs.len());
        } else {
            for file in &launch.files {
                app.open_quietly(file, &ctx, None);
                if let (Some(zoom), Some(tab)) = (launch.zoom, app.tabs.last_mut()) {
                    tab.set_initial_zoom(zoom);
                }
            }
        }
        app.transition = None;
        Ok(app)
    }

    pub(crate) fn with_gpu<R>(&self, f: impl FnOnce(&mut ReaderGpu) -> R) -> Option<R> {
        let mut renderer = self.render_state.renderer.write();
        renderer.callback_resources.get_mut::<ReaderGpu>().map(f)
    }

    fn is_resident(&self, key: PageKey) -> bool {
        self.with_gpu(|gpu| gpu.is_resident(key)).unwrap_or(false)
    }

    /// Opens `path` in a new tab (or shows the tab that has it) and makes it
    /// active. `page` overrides the remembered position.
    pub fn open(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.activate(Some(index));
            return;
        }
        let mut restore = self.library.history_position(path);
        if let Some(page) = page {
            let mut position =
                restore.unwrap_or_else(|| DocumentPosition::fixed(0.0, 0.0, 1.0, page));
            position.page = page;
            restore = Some(position);
        }
        match DocumentTab::open(self.next_tab_id, path, ctx, restore) {
            Ok(tab) => {
                self.next_tab_id += 1;
                self.tabs.push(tab);
                if self.persist {
                    let _ = self.library.mark_library_item_opened(path);
                }
                self.activate(Some(self.tabs.len() - 1));
            }
            Err(error) => self
                .notices
                .push(format!("Could not open {}: {error:#}", path.display())),
        }
    }

    /// Opens without a transition (startup, session restore).
    fn open_quietly(&mut self, path: &Path, ctx: &egui::Context, page: Option<usize>) {
        self.open(path, ctx, page);
        self.transition = None;
    }

    /// Makes another tab active, with the transition effect between two
    /// document views.
    pub(crate) fn activate(&mut self, next: Option<usize>) {
        if next == self.active {
            return;
        }
        let previous = self.active;
        self.active = next;
        let (Some(previous), Some(next)) = (previous, next) else {
            return;
        };
        let (Some(from), Some(effect)) = (self.tabs.get(previous), self.prefs.transition.clone())
        else {
            return;
        };
        let direction = if next >= previous { 1.0 } else { -1.0 };
        self.begin_transition(effect, from.id, direction);
    }

    fn begin_transition(&mut self, effect: String, from_tab: u64, direction: f32) {
        let duration = self
            .with_gpu(|gpu| gpu.effects().find(&effect).map(|def| def.duration_ms))
            .flatten();
        if let Some(duration) = duration {
            self.transition = Some(ViewTransition::new(
                effect,
                from_tab,
                direction,
                duration,
                Instant::now(),
            ));
        }
    }

    fn close(&mut self, index: usize) {
        self.save_position(index, true);
        let tab = self.tabs.remove(index);
        self.saved_positions.remove(&tab.id);
        self.with_gpu(|gpu| gpu.drop_tab(tab.id));
        self.transition = None;
        self.active = match self.active {
            Some(active) if active == index => {
                if self.tabs.is_empty() {
                    None
                } else {
                    Some(index.min(self.tabs.len() - 1))
                }
            }
            Some(active) if active > index => Some(active - 1),
            other => other,
        };
    }

    /// Records a tab's position in the library when it moved.
    fn save_position(&mut self, index: usize, flush: bool) {
        if !self.persist {
            return;
        }
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let position = tab.position();
        if self.saved_positions.get(&tab.id) != Some(&position) {
            self.library.record_history(&tab.path, position.clone());
            self.saved_positions.insert(tab.id, position);
            if flush {
                let _ = self.library.save();
            }
        }
    }

    fn session(&self) -> LastSession {
        LastSession {
            files: self
                .tabs
                .iter()
                .map(|tab| SessionFile {
                    path: tab.path.display().to_string(),
                    page: tab.current_page(),
                })
                .collect(),
            active: self.active,
        }
    }

    /// Uploads at most one loaded page per frame (the active tab's first),
    /// so a heavy page costs one long frame rather than several in a row.
    /// Swaps in raster layers re-rasterized for the current view.
    fn upload_ready_layers(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        for index in 0..self.tabs.len() {
            let tab_id = self.tabs[index].id;
            while let Some((page, layer, image)) = self.tabs[index].ready_layers.pop_front() {
                let key = PageKey { tab: tab_id, page };
                self.with_gpu(|gpu| gpu.replace_layer(&device, &queue, key, layer, &image));
            }
        }
    }

    fn upload_ready_page(&mut self) {
        let (device, queue) = (
            self.render_state.device.clone(),
            self.render_state.queue.clone(),
        );
        self.with_gpu(|gpu| gpu.step_overview(&device, &queue));
        let order: Vec<usize> = self.active.into_iter().chain(0..self.tabs.len()).collect();
        for index in order {
            let Some(ready) = self.tabs[index].ready.pop_front() else {
                continue;
            };
            let key = PageKey {
                tab: self.tabs[index].id,
                page: ready.page,
            };
            let (device, queue) = (
                self.render_state.device.clone(),
                self.render_state.queue.clone(),
            );
            if let Some(ms) = self
                .with_gpu(|gpu| gpu.upload(&device, &queue, key, &ready.prepared, &ready.layers))
            {
                self.upload_ms.push(ms);
            }
            if self.first_page_ms.is_none() {
                self.first_page_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
            }
            return;
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let fallback = self.theme_kind.unwrap_or_default();
        let kind = self.prefs.theme.resolve(ctx.system_theme(), fallback);
        if self.theme_kind != Some(kind) {
            theme::apply(ctx, kind);
            self.theme_kind = Some(kind);
        }
    }

    fn global_actions(&mut self, ctx: &egui::Context) -> Vec<ViewAction> {
        let typing = ctx.memory(|memory| memory.focused().is_some());
        let mut actions = Vec::new();
        let (ctrl, shift) = ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
        let pressed = |key| ctx.input(|i| i.key_pressed(key));
        if ctrl && pressed(egui::Key::O) {
            self.pickers.open_documents(ctx);
        }
        if ctrl
            && pressed(egui::Key::W)
            && let Some(active) = self.active
        {
            self.close(active);
        }
        if ctrl && pressed(egui::Key::Tab) {
            let count = self.tabs.len() + 1;
            let current = self.active.map_or(0, |active| active + 1);
            let next = if shift {
                (current + count - 1) % count
            } else {
                (current + 1) % count
            };
            self.activate(next.checked_sub(1));
        }
        if typing || ctrl {
            return actions;
        }
        let map = [
            (egui::Key::PageDown, ViewAction::NextPage),
            (egui::Key::Space, ViewAction::NextPage),
            (egui::Key::PageUp, ViewAction::PreviousPage),
            (egui::Key::Plus, ViewAction::ZoomIn),
            (egui::Key::Equals, ViewAction::ZoomIn),
            (egui::Key::Minus, ViewAction::ZoomOut),
            (egui::Key::Num0, ViewAction::FitWidth),
            (egui::Key::P, ViewAction::TogglePresent),
            (egui::Key::Home, ViewAction::GoToPage(0)),
            (egui::Key::End, ViewAction::GoToPage(usize::MAX)),
        ];
        for (key, action) in map {
            if pressed(key) {
                actions.push(action);
            }
        }
        if pressed(egui::Key::F9) {
            self.show_outline = !self.show_outline;
        }
        if pressed(egui::Key::F12) {
            self.show_stats = !self.show_stats;
        }
        actions
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        let tokens = Tokens::get(ui.ctx());
        let labels: Vec<String> = std::iter::once("Home".to_string())
            .chain(
                self.tabs
                    .iter()
                    .map(|tab| truncate(&tab.title, TAB_MAX_LABEL)),
            )
            .collect();
        let font = egui::TextStyle::Button.resolve(ui.style());
        let natural: Vec<f32> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let text = ui
                    .painter()
                    .layout_no_wrap(label.clone(), font.clone(), tokens.text)
                    .size()
                    .x;
                text + if index == 0 { 24.0 } else { 44.0 }
            })
            .collect();
        let selected = self.active.map_or(0, |active| active + 1);
        let strip = ui.available_rect_before_wrap();
        let menu_width = 36.0;
        let fit = craft_ui::tabs::fit(
            &natural,
            selected,
            strip.width() - menu_width,
            TAB_MIN_WIDTH,
            TAB_OVERFLOW_WIDTH,
        );
        let mut x = strip.min.x;
        let mut select = None;
        let mut close = None;
        for &(index, width) in &fit.shown {
            let rect = egui::Rect::from_min_size(
                egui::pos2(x, strip.min.y),
                egui::vec2(width, strip.height()),
            );
            x += width;
            let id = ui.id().with(("tab", index));
            let is_selected = index == selected;
            let label = labels[index].clone();
            let response = craft_ui::tabs::Tab::new(id, &label, is_selected)
                .focus_stroke(egui::Stroke::new(1.0, tokens.accent))
                .show_at(ui, rect, |ui, response| {
                    let fill = if is_selected {
                        tokens.chrome
                    } else if response.hovered() {
                        tokens.hover
                    } else {
                        tokens.titlebar
                    };
                    ui.painter()
                        .rect_filled(rect.shrink2(egui::vec2(1.0, 0.0)), 4.0, fill);
                    let text_rect = rect.shrink2(egui::vec2(10.0, 0.0));
                    ui.painter().with_clip_rect(text_rect).text(
                        text_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &label,
                        font.clone(),
                        if is_selected {
                            tokens.text
                        } else {
                            tokens.text_muted
                        },
                    );
                });
            if response.clicked() {
                select = Some(index);
            }
            if index > 0 {
                if response.middle_clicked() {
                    close = Some(index - 1);
                }
                let close_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 16.0, rect.center().y),
                    egui::vec2(18.0, 18.0),
                );
                let close_button = ui.interact(close_rect, id.with("close"), egui::Sense::click());
                if close_button.hovered() {
                    ui.painter().rect_filled(close_rect, 4.0, tokens.pressed);
                }
                ui.painter().text(
                    close_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "×",
                    font.clone(),
                    tokens.text_muted,
                );
                if close_button.clicked() {
                    close = Some(index - 1);
                }
            }
        }
        if !fit.overflow.is_empty() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(x, strip.min.y),
                egui::vec2(fit.overflow_width, strip.height()),
            );
            ui.put(rect, |ui: &mut egui::Ui| {
                ui.menu_button("»", |ui| {
                    for &index in &fit.overflow {
                        if ui.button(&labels[index]).clicked() {
                            select = Some(index);
                            ui.close();
                        }
                    }
                })
                .response
            });
        }
        let menu_rect = egui::Rect::from_min_max(
            egui::pos2(strip.right() - menu_width, strip.min.y),
            strip.max,
        );
        ui.put(menu_rect, |ui: &mut egui::Ui| {
            ui.menu_button("☰", |ui| self.menu(ui)).response
        });
        if let Some(index) = close {
            self.close(index);
        } else if let Some(index) = select {
            self.activate(index.checked_sub(1));
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                !self.pickers.is_showing(),
                egui::Button::new("Open documents…  Ctrl+O"),
            )
            .clicked()
        {
            self.pickers.open_documents(ui.ctx());
            ui.close();
        }
        ui.separator();
        ui.label("Theme");
        for preference in ThemePreference::ALL {
            ui.radio_value(&mut self.prefs.theme, preference, preference.label());
        }
        ui.separator();
        let defs: Vec<_> = self
            .with_gpu(|gpu| gpu.effects().defs().cloned().collect())
            .unwrap_or_default();
        ui.label("Transition");
        ui.radio_value(&mut self.prefs.transition, None, "None");
        for def in defs.iter().filter(|def| def.kind == EffectKind::Transition) {
            ui.radio_value(&mut self.prefs.transition, Some(def.id.clone()), &def.name);
        }
        ui.separator();
        ui.label("Background");
        for def in defs.iter().filter(|def| def.kind == EffectKind::Background) {
            let label = if def.animated {
                format!("{} (animated)", def.name)
            } else {
                def.name.clone()
            };
            ui.radio_value(&mut self.prefs.background, def.id.clone(), label);
        }
        ui.separator();
        ui.checkbox(&mut self.prefs.reopen_last_session, "Reopen last session");
        ui.checkbox(&mut self.show_outline, "Outline  F9");
        ui.checkbox(&mut self.show_stats, "Statistics  F12");
        let gamepads = match &self.gamepads {
            Ok(pads) if pads.connected().is_empty() => "none connected".to_string(),
            Ok(pads) => pads.connected().join(", "),
            Err(error) => format!("unavailable ({error})"),
        };
        ui.label(format!("Gamepad: {gamepads}"));
        let errors = self
            .with_gpu(|gpu| {
                gpu.effects()
                    .errors()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !errors.is_empty() {
            ui.separator();
            ui.collapsing(format!("Effect errors ({})", errors.len()), |ui| {
                for error in errors {
                    ui.label(error);
                }
            });
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ViewAction>) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let pages = tab.layout.len();
        ui.horizontal(|ui| {
            if !tab.outline.is_empty() {
                ui.toggle_value(&mut self.show_outline, "☰ Outline");
            }
            if ui.button("◀").clicked() {
                actions.push(ViewAction::PreviousPage);
            }
            let current = tab.current_page() + 1;
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.page_input)
                    .desired_width(48.0)
                    .hint_text(current.to_string()),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Ok(page) = self.page_input.trim().parse::<usize>() {
                    actions.push(ViewAction::GoToPage(page.saturating_sub(1)));
                }
                self.page_input.clear();
            }
            ui.label(format!("/ {pages}"));
            if ui.button("▶").clicked() {
                actions.push(ViewAction::NextPage);
            }
            ui.separator();
            if ui.button("−").clicked() {
                actions.push(ViewAction::ZoomOut);
            }
            ui.label(format!("{:.0}%", tab.view.scale / tab.fit_scale() * 100.0));
            if ui.button("+").clicked() {
                actions.push(ViewAction::ZoomIn);
            }
            if ui.button("Fit width").clicked() {
                actions.push(ViewAction::FitWidth);
            }
            ui.separator();
            let presenting = tab.present.is_some();
            if ui.selectable_label(presenting, "Present  P").clicked() {
                actions.push(ViewAction::TogglePresent);
            }
        });
    }

    fn outline(&mut self, ui: &mut egui::Ui, actions: &mut Vec<ViewAction>) {
        let Some(active) = self.active else {
            return;
        };
        let tab = &self.tabs[active];
        let current = tab.current_page();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for entry in &tab.outline {
                ui.horizontal(|ui| {
                    ui.add_space(entry.depth as f32 * 12.0);
                    let selected = entry.page == Some(current);
                    let response = ui.add_enabled(
                        entry.page.is_some(),
                        egui::Button::selectable(selected, entry.title.as_str()).frame(false),
                    );
                    if response.clicked()
                        && let Some(page) = entry.page
                    {
                        actions.push(ViewAction::GoToPage(page));
                    }
                });
            }
        });
    }

    /// The document view: input, layout, loading, the GPU callback and
    /// loading placeholders. Returns whether it is animating and whether
    /// every visible page is drawn.
    fn document_view(
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
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let size = pixel_size(rect, ppp);
        let pointer = response.hover_pos().map(|pos| {
            [
                f64::from((pos.x - rect.min.x) * ppp),
                f64::from((pos.y - rect.min.y) * ppp),
            ]
        });
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        let drag = response.drag_delta();
        let ppp64 = f64::from(ppp);
        let input = ViewInput {
            zoom: if pointer.is_some() {
                f64::from(zoom)
            } else {
                1.0
            },
            scroll: [
                -f64::from(drag.x) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.x) * ppp64
                    } else {
                        0.0
                    },
                -f64::from(drag.y) * ppp64
                    - if pointer.is_some() {
                        f64::from(scroll.y) * ppp64
                    } else {
                        0.0
                    },
            ],
            pointer,
            actions,
            gamepad: Some(pad),
            seconds,
            now: Some(now),
        };
        let turning = self.transition.is_some();
        let tab_id = self.tabs[active].id;
        let mut frame = self.tabs[active].frame(size, input, turning);
        if let Some(direction) = frame.turn
            && let Some(effect) = self.prefs.transition.clone()
        {
            self.begin_transition(effect, tab_id, direction);
        }

        // Loading: ask for wanted pages, drop pages over the budget.
        let keep: Vec<PageKey> = frame
            .wanted
            .iter()
            .map(|&page| PageKey { tab: tab_id, page })
            .collect();
        let resident: std::collections::HashSet<usize> = frame
            .wanted
            .iter()
            .copied()
            .filter(|&page| self.is_resident(PageKey { tab: tab_id, page }))
            .collect();
        self.tabs[active].want(&frame.wanted, |page| resident.contains(&page));
        if let Some(dropped) = self.with_gpu(|gpu| gpu.evict(&keep, PAGE_BUDGET_BYTES)) {
            for key in dropped {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == key.tab) {
                    tab.forget(key.page);
                }
            }
        }
        self.tabs[active].placeholders(&mut frame, |page| resident.contains(&page));

        let tokens = Tokens::get(&ctx);
        let background = {
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
        };
        let transition = self.transition.as_mut().map(|transition| {
            let params = self
                .render_state
                .renderer
                .write()
                .callback_resources
                .get::<ReaderGpu>()
                .and_then(|gpu| {
                    gpu.effects()
                        .find(&transition.effect)
                        .map(|def| def.default_params())
                })
                .unwrap_or_default();
            TransitionUse {
                effect: EffectUse {
                    id: transition.effect.clone(),
                    params,
                },
                progress: transition.progress(now),
                direction: transition.direction,
                capture_from: transition.take_capture(),
            }
        });
        let page_color = tokens.page.to_normalized_gamma_f32();
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewCallback {
                rect,
                tab: tab_id,
                draws: frame.draws.clone(),
                page_color,
                background,
                transition,
                time: self.started.elapsed().as_secs_f32(),
                pointer: pointer.map(|[x, y]| [x as f32, y as f32]),
            },
        ));
        let painter = ui.painter_at(rect);
        for (paper, thumbnail) in &frame.placeholders {
            let paper = egui::Rect::from_min_max(
                rect.min + egui::vec2(paper[0], paper[1]) / ppp,
                rect.min + egui::vec2(paper[2], paper[3]) / ppp,
            );
            if let Some(texture) = thumbnail {
                painter.image(
                    *texture,
                    paper,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
        if self.show_stats {
            self.stats_overlay(&painter, rect, active);
        }
        (frame.animating, frame.placeholders.is_empty())
    }

    pub(crate) fn fit_scale(&self) -> Option<f64> {
        self.active.map(|active| self.tabs[active].fit_scale())
    }

    fn stats_overlay(&self, painter: &egui::Painter, rect: egui::Rect, active: usize) {
        let tab = &self.tabs[active];
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        let key = PageKey {
            tab: tab.id,
            page: tab.current_page(),
        };
        let source = self
            .with_gpu(|gpu| gpu.source(key, tab.view.scale as f32))
            .flatten()
            .unwrap_or_else(|| "loading".into());
        let text = format!(
            "frame p50 {:.1} ms  p95 {:.1} ms\nencode {:.2} ms  draws {}  segments {}\nresident {}  GPU {:.0} MiB  {source}\nrepaints {}",
            self.frames.percentile_ms(0.5),
            self.frames.percentile_ms(0.95),
            stats.encode_ms,
            stats.draw.draw_calls,
            stats.draw.segments,
            stats.resident,
            stats.gpu_bytes as f64 / 1048576.0,
            self.repaints,
        );
        let font = egui::FontId::monospace(11.0);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
        let at = rect.right_bottom() - galley.size() - egui::vec2(12.0, 12.0);
        painter.rect_filled(
            egui::Rect::from_min_size(at, galley.size()).expand(6.0),
            6.0,
            egui::Color32::from_black_alpha(170),
        );
        painter.galley(at, galley, egui::Color32::WHITE);
    }
}

impl eframe::App for ReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        let seconds = (now - self.last_update).as_secs_f64().min(0.1);
        self.last_update = now;
        self.repaints += 1;
        if !self.continuous {
            self.frames.idle();
        }
        self.frames.tick();
        self.apply_theme(&ctx);

        for path in self.pickers.take() {
            self.open(&path, &ctx, None);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .filter(|path| is_document(path))
                .collect()
        });
        for path in dropped {
            self.open(&path, &ctx, None);
        }
        for tab in &mut self.tabs {
            tab.receive(&ctx);
        }
        self.upload_ready_page();
        self.upload_ready_layers();
        let pad = match &mut self.gamepads {
            Ok(pads) => pads.poll(),
            Err(_) => GamepadInput::default(),
        };
        let mut actions = self.global_actions(&ctx);
        actions.extend(pad.actions.iter().copied());
        actions.append(&mut self.pending_actions);

        let tokens = Tokens::get(&ctx);
        egui::Panel::top("tabs")
            .exact_size(34.0)
            .frame(
                egui::Frame::new()
                    .fill(tokens.titlebar)
                    .inner_margin(egui::Margin::symmetric(6, 3)),
            )
            .show(ui, |ui| self.tab_strip(ui));
        if self.active.is_some() {
            egui::Panel::top("toolbar")
                .frame(
                    egui::Frame::new()
                        .fill(tokens.chrome)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| self.toolbar(ui, &mut actions));
            let has_outline = self
                .active
                .is_some_and(|active| !self.tabs[active].outline.is_empty());
            if self.show_outline && has_outline {
                egui::Panel::left("outline")
                    .default_size(260.0)
                    .frame(egui::Frame::new().fill(tokens.chrome).inner_margin(8))
                    .show(ui, |ui| self.outline(ui, &mut actions));
            }
        }
        if !self.notices.is_empty() {
            egui::Panel::bottom("notices").show(ui, |ui| {
                let mut dismiss = false;
                ui.horizontal(|ui| {
                    ui.label(self.notices.last().cloned().unwrap_or_default());
                    dismiss = ui.button("Dismiss").clicked();
                });
                if dismiss {
                    self.notices.clear();
                }
            });
        }

        let mut animating = false;
        let mut pages_drawn = true;
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(tokens.pasteboard))
            .show(ui, |ui| {
                if self.active.is_some() {
                    (animating, pages_drawn) = self.document_view(ui, actions, &pad, seconds, now);
                } else if let Some(action) =
                    self.home.ui(ui, &self.library, self.pickers.is_showing())
                {
                    match action {
                        HomeAction::Open(path) => self.open(&path, &ctx, None),
                        HomeAction::OpenDialog => self.pickers.open_documents(&ctx),
                    }
                }
            });

        self.run_harness(&ctx, pages_drawn, now);
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| transition.finished(now))
        {
            self.transition = None;
        }
        if let Some(active) = self.active
            && now - self.last_position_save >= POSITION_SAVE_INTERVAL
        {
            self.save_position(active, true);
            self.last_position_save = now;
        }

        // A pending overview advances one tile per frame, so keep drawing.
        let overviews = self
            .with_gpu(|gpu| gpu.has_pending_overview())
            .unwrap_or(false);
        let continuous = animating
            || self.transition.is_some()
            || pad.is_active()
            || self.harness.is_some()
            || overviews;
        self.continuous = continuous;
        if continuous {
            ctx.request_repaint();
        } else if self.active.is_some() && self.background_is_animated() && ctx.input(|i| i.focused)
        {
            let fps = self.prefs.background_fps.max(1);
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / f64::from(fps)));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.persist {
            return;
        }
        for index in 0..self.tabs.len() {
            self.save_position(index, false);
        }
        let _ = self.library.save();
        self.prefs.last_session = if self.prefs.reopen_last_session {
            self.session()
        } else {
            LastSession::default()
        };
        self.prefs.save(storage);
    }
}

impl ReaderApp {
    fn background_is_animated(&self) -> bool {
        let id = self.prefs.background.clone();
        self.with_gpu(|gpu| gpu.effects().find(&id).is_some_and(|def| def.animated))
            .unwrap_or(false)
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut short: String = text.chars().take(limit - 1).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_tab_titles_are_shortened() {
        assert_eq!(truncate("short", 8), "short");
        assert_eq!(truncate("a rather long title", 8), "a rathe…");
        assert_eq!(truncate("中文标题很长很长很长", 4), "中文标…");
    }
}
