//! The docked shell around a document (frame 02a): the icon rail, the
//! sidebar, the breadcrumb or split panes' headers over the pages, and the
//! assistant, split by hairlines. `shell.rs` has the geometry; this draws
//! it. Below `shell::NARROW` the sidebar and the assistant float over the
//! pages as Areas, so the pointer over them never reaches the pages.

use egui::{Align2, Id, Order, Rect, pos2, vec2};
use glyphst_ui::{motion, overlay, rail as ui_rail, text as ui_text};

use super::*;
use crate::{
    book_effects::{Defaults, Resolved, resolve},
    gpu::{BackdropCallback, Sweep},
    overview,
    panes::{self, Divider},
    shell::{self, ShellInput, ShellLayout},
};

/// The chrome's state between frames.
#[derive(Default)]
pub(super) struct StageChrome {
    /// The page last asked for while scrubbing the progress meter.
    pub scrub: Option<usize>,
    /// Where the panes were drawn last frame, for dropping a tab dragged
    /// from the tab bar.
    pub panes: Vec<(u64, Rect)>,
    /// The tab being dragged from the tab bar.
    pub tab_drag: Option<u64>,
}

/// What a split pane's header asked for.
enum PaneHeaderAction {
    Focus(u64),
    Close(u64),
    /// The zoom was clicked: back to 100% (fit width).
    FitWidth(u64),
}

impl ReaderApp {
    /// The active document's shell: rail, sidebar, breadcrumb, pages and
    /// assistant. Returns whether the view animates and whether every page
    /// is drawn.
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
        let presenting = self.tabs[active].present.is_some();
        let sidebar_shown = motion::smooth_bool(
            &ctx,
            Id::unique("stage-sidebar-shown"),
            self.prefs.show_sidebar,
            motion::PANEL,
        );
        let assistant_shown = motion::smooth_bool(
            &ctx,
            Id::unique("stage-assistant-shown"),
            self.assistant.open,
            motion::PANEL,
        );
        let layout = ShellLayout::compute(ShellInput {
            area: ui.max_rect(),
            sidebar: Some(self.prefs.drawer_width),
            sidebar_shown,
            assistant: Some(self.prefs.assistant_width),
            assistant_shown,
            present: presenting || self.bare(),
        });
        // The focused pane's tab is always shown.
        let focused_id = self.tabs[active].id;
        if !self.panes.contains(focused_id) {
            self.panes.show(focused_id);
        }
        // Split panes take the breadcrumb's row for their own headers.
        let column = match layout.breadcrumb {
            Some(breadcrumb) => layout.reading.union(breadcrumb),
            None => layout.reading,
        };
        let (panes, dividers) = if presenting {
            (vec![(focused_id, layout.reading)], Vec::new())
        } else {
            let split = self.panes.count() > 1 && !self.bare();
            self.panes
                .rects(if split { column } else { layout.reading })
        };
        let split = panes.len() > 1 && !self.bare();
        let ppp = ctx.pixels_per_point();
        let backdrop = if split { column } else { layout.reading };
        let pointer = ctx
            .pointer_hover_pos()
            .filter(|pos| backdrop.contains(*pos))
            .map(|pos| {
                [
                    (pos.x - backdrop.min.x) * ppp,
                    (pos.y - backdrop.min.y) * ppp,
                ]
            });
        // While a switch runs between books with different backgrounds,
        // the backdrop transitions with the pages.
        let sweep = self.transition.as_ref().and_then(|transition| {
            Some(Sweep {
                other: transition.from_background.clone()?,
                effect: transition.effect.clone(),
                progress: transition.progress(now),
                direction: transition.direction,
                origin: transition.origin.map(|origin| {
                    let at = (origin - backdrop.min) * ppp;
                    [at.x, at.y]
                }),
                seed: transition.seed,
                reversed: transition.reversed(),
            })
        });
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            backdrop,
            BackdropCallback {
                rect: backdrop,
                background: self.background_effect(&tokens),
                time: self.started.elapsed().as_secs_f32(),
                pointer,
                opacity: tokens.opacity,
                sweep,
            },
        ));
        let mut actions = Some(actions);
        let (mut animating, mut drawn) = (false, true);
        let mut keep = Vec::new();
        self.generations.begin_frame(self.prefs.reduce_motion);
        let mut header_action = None;
        for &(id, pane) in &panes {
            let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
                continue;
            };
            let focused = index == active;
            // Split panes start with their own header.
            let rect = if split {
                let header = Rect::from_min_max(
                    pane.min,
                    pos2(
                        pane.right(),
                        (pane.top() + shell::BREADCRUMB_HEIGHT).min(pane.bottom()),
                    ),
                );
                if let Some(action) = self.pane_header(ui, header, index, focused) {
                    header_action = Some(action);
                }
                pane.with_min_y(header.bottom())
            } else {
                pane
            };
            // Pages take the whole pane; the page ticks float over them.
            let mut view = PaneView {
                rect,
                draw: rect,
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
                // The pad's triggers size the cells.
                if let Some(pad) = view.pad.filter(|pad| pad.zoom != 0.0) {
                    let range = overview::GRID_ZOOM_RANGE;
                    let factor = (f64::from(pad.zoom) * seconds).exp() as f32;
                    tab.overview_zoom =
                        (tab.overview_zoom * factor).clamp(*range.start(), *range.end());
                    animating = true;
                }
                if tab.overview.is_some() {
                    if let Some(page) = overview::show(ui, rect, tab) {
                        tab.overview = None;
                        let exit = tab.overview_exit(page);
                        tab.apply(exit, false);
                    }
                    // Search works on the grid too: its matches move the
                    // cursor.
                    let enter_leaves = self.keymap.preset() == KeymapPreset::Vim;
                    let tab = &mut self.tabs[index];
                    if focused
                        && tab.overview.is_some()
                        && tab.find.open
                        && let Some(action) =
                            find::find_bar(&mut tab.find, rect, ui.ctx(), now, enter_leaves)
                    {
                        match action {
                            FindBarAction::Step(forward) => self.find_step(forward),
                            FindBarAction::Close => tab.find.close(),
                        }
                    }
                    continue;
                }
            }
            let (pane_animating, pane_drawn) = self.document_view(ui, view, &mut keep);
            animating |= pane_animating;
            drawn &= pane_drawn;
        }
        self.generations.end_frame();
        animating |= self.generations.animating();
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
        if presenting || self.bare() {
            return (animating, drawn);
        }
        if !split
            && let Some(breadcrumb) = layout.breadcrumb
            && self.breadcrumb(ui, breadcrumb, active)
        {
            self.tabs[active].apply(ViewAction::FitMargins, false);
        }
        if split {
            self.pane_dividers(ui, &dividers);
        }
        match header_action {
            Some(PaneHeaderAction::Close(id)) => {
                if self.panes.close_pane(id)
                    && let Some(focused) = self.panes.focused()
                {
                    self.focus_tab(focused);
                }
            }
            Some(PaneHeaderAction::Focus(id)) => self.focus_tab(id),
            Some(PaneHeaderAction::FitWidth(id)) => {
                self.focus_tab(id);
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
                    tab.apply(ViewAction::FitMargins, false);
                }
            }
            None => {}
        }
        self.tab_drop(&ctx);
        let active = self.active.unwrap_or(active);
        if let Some(rail) = layout.rail {
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rail)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            self.rail(&mut child, rail, active);
        }
        if let Some(rect) = layout.sidebar {
            self.docked(
                ui,
                "stage-sidebar",
                rect,
                layout.inner,
                layout.overlay,
                Edge::Right,
                |app, ui| {
                    app.sidebar_pane(ui, rect, active, now);
                },
            );
        }
        if let Some(rect) = layout.assistant {
            self.docked(
                ui,
                "stage-assistant",
                rect,
                layout.inner,
                layout.overlay,
                Edge::Left,
                |app, ui| {
                    app.assistant_pane(ui, rect, Some(active));
                },
            );
        }
        (animating, drawn)
    }

    /// Draws `add` in a pane at `rect`, shown where it is within `inner`
    /// (all of it, unless it is sliding): in place when docked, or floating
    /// over the pages in a narrow window. `edge` is the side facing the
    /// pages, where the hairline and the resize handle go.
    #[allow(clippy::too_many_arguments)]
    fn docked(
        &mut self,
        ui: &mut egui::Ui,
        id: &str,
        rect: Rect,
        inner: Rect,
        overlay: bool,
        edge: Edge,
        add: impl FnOnce(&mut Self, &mut egui::Ui),
    ) {
        let tokens = Tokens::get(ui.ctx());
        let shown = rect.intersect(inner);
        let paint = |ui: &mut egui::Ui, app: &mut Self| {
            let painter = ui.painter().with_clip_rect(shown);
            painter.rect_filled(rect, 0, tokens.surface.to_opaque());
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            child.set_clip_rect(shown);
            add(app, &mut child);
            let x = match edge {
                Edge::Left => rect.left() + 0.5,
                Edge::Right => rect.right() - 0.5,
            };
            painter.vline(x, rect.y_range(), tokens.hairline());
        };
        if overlay {
            egui::Area::new(Id::unique(id))
                .order(Order::Middle)
                .fixed_pos(shown.min)
                .show(ui.ctx(), |ui| {
                    ui.set_min_size(shown.size());
                    paint(ui, self);
                });
        } else {
            paint(ui, self);
        }
        // It is resized once it has slid in.
        if shown != rect {
            return;
        }
        let (width, min) = match edge {
            Edge::Right => (self.prefs.drawer_width, shell::SIDEBAR_MIN),
            Edge::Left => (self.prefs.assistant_width, shell::ASSISTANT_MIN),
        };
        let x = match edge {
            Edge::Left => rect.left(),
            Edge::Right => rect.right(),
        };
        let delta = resize_handle(ui, &tokens, id, x, rect, width, min);
        if delta != 0.0 {
            match edge {
                Edge::Right => {
                    self.prefs.drawer_width =
                        (self.prefs.drawer_width + delta).clamp(shell::SIDEBAR_MIN, 900.0);
                }
                // Dragging the assistant's left edge left widens it.
                Edge::Left => {
                    self.prefs.assistant_width =
                        (self.prefs.assistant_width - delta).clamp(shell::ASSISTANT_MIN, 1200.0);
                }
            }
        }
    }

    /// The breadcrumb over the pages: the file and the outline path to the
    /// current page, and at the right the render time and zoom (or the
    /// find counts while finding). Returns whether the zoom was clicked,
    /// which resets it.
    fn breadcrumb(&self, ui: &mut egui::Ui, rect: Rect, active: usize) -> bool {
        let tokens = Tokens::get(ui.ctx());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, tokens.surface.to_opaque());
        painter.hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
        let tab = &self.tabs[active];
        let meta = if tab.overview.is_some() && !tab.find.matches.is_empty() {
            format!(
                "overview · {} matches on {} pages · {:.0}%",
                tab.find.matches.len(),
                tab.find.matched_pages(),
                tab.overview_zoom * 100.0
            )
        } else if tab.overview.is_some() {
            format!(
                "overview · {} pages · {:.0}%",
                tab.layout.len(),
                tab.overview_zoom * 100.0
            )
        } else if tab.find.open && !tab.find.query.is_empty() {
            let pages = tab.find.matched_pages();
            format!("{} matches · {pages} pages", tab.find.matches.len())
        } else {
            format!("gpu · {:.1} ms · ", self.frames.percentile_ms(0.5))
        };
        let font = theme::mono(11.0);
        let y = rect.center().y;
        // The page zoom ends the meta, on its own to be clicked.
        let zoomed = tab.overview.is_none() && !(tab.find.open && !tab.find.query.is_empty());
        let mut reset = false;
        let mut meta_right = rect.right() - 12.0;
        if zoomed {
            let zoom = zoom_label(ui, pos2(meta_right, y), Align2::RIGHT_CENTER, tab, &tokens);
            reset = zoom.clicked();
            meta_right = zoom.rect.left();
        }
        let meta_rect = ui_text::line(
            ui,
            pos2(meta_right, y),
            Align2::RIGHT_CENTER,
            meta,
            font.clone(),
            tokens.faint,
            rect.width() * 0.4,
        );
        let file = tab
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tab.title.clone());
        let page = tab.current_page();
        let sections = if tab.overview.is_some() {
            vec!["overview"]
        } else {
            shell::section_path(
                tab.outline
                    .iter()
                    .map(|entry| (entry.depth, entry.page, entry.title.as_str())),
                page,
            )
        };
        let mut x = rect.left() + 12.0;
        let right = meta_rect.left() - 16.0;
        let last = sections.len();
        for (index, part) in std::iter::once(file.as_str())
            .chain(sections.iter().copied())
            .enumerate()
        {
            if x >= right {
                break;
            }
            if index > 0 {
                let sep = ui_text::line(
                    ui,
                    pos2(x, y),
                    Align2::LEFT_CENTER,
                    "›",
                    font.clone(),
                    tokens.faint,
                    right - x,
                );
                x = sep.right() + 8.0;
            }
            let color = if index == last {
                tokens.text
            } else {
                tokens.muted
            };
            let part_rect = ui_text::line(
                ui,
                pos2(x, y),
                Align2::LEFT_CENTER,
                part.to_lowercase(),
                font.clone(),
                color,
                right - x,
            );
            x = part_rect.right() + 8.0;
        }
        reset
    }

    /// A split pane's header: `▌ file — p.4 / 15 — 100%` and a close
    /// button. Clicking it focuses the pane.
    fn pane_header(
        &self,
        ui: &mut egui::Ui,
        rect: Rect,
        index: usize,
        focused: bool,
    ) -> Option<PaneHeaderAction> {
        let tokens = Tokens::get(ui.ctx());
        let tab = &self.tabs[index];
        let id = tab.id;
        let response = ui.interact(rect, Id::unique(("pane-header", id)), egui::Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, tokens.surface.to_opaque());
        painter.hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
        let y = rect.center().y;
        let mut x = rect.left() + 12.0;
        if focused {
            painter.rect_filled(
                Rect::from_center_size(pos2(x + 2.0, y), vec2(4.0, 13.0)),
                0,
                tokens.accent,
            );
            x += 14.0;
        }
        let file = tab
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| tab.title.clone());
        let title = ui_text::line(
            ui,
            pos2(x, y),
            Align2::LEFT_CENTER,
            file,
            theme::mono(12.0),
            if focused { tokens.text } else { tokens.muted },
            rect.width() * 0.5,
        );
        let meta = format!("— p.{} / {} — ", tab.current_page() + 1, tab.layout.len());
        let meta = ui_text::line(
            ui,
            pos2(title.right() + 8.0, y),
            Align2::LEFT_CENTER,
            meta,
            theme::mono(11.0),
            tokens.faint,
            (rect.right() - 40.0 - title.right()).max(0.0),
        );
        if meta.right() + 32.0 < rect.right() - 40.0
            && zoom_label(ui, pos2(meta.right(), y), Align2::LEFT_CENTER, tab, &tokens).clicked()
        {
            return Some(PaneHeaderAction::FitWidth(id));
        }
        let close = Rect::from_center_size(pos2(rect.right() - 16.0, y), vec2(20.0, 20.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(close));
        if icons::button(&mut child, "x", 20.0, false, "Close the pane").clicked() {
            return Some(PaneHeaderAction::Close(id));
        }
        response.clicked().then_some(PaneHeaderAction::Focus(id))
    }

    /// The hairlines between split panes, with their resize handles.
    fn pane_dividers(&mut self, ui: &mut egui::Ui, dividers: &[Divider]) {
        let tokens = Tokens::get(ui.ctx());
        for divider in dividers {
            let horizontal = divider.axis == glyphst_ui::craft::layout::SplitAxis::Horizontal;
            if horizontal {
                ui.painter().vline(
                    divider.rect.center().x,
                    divider.rect.y_range(),
                    tokens.hairline(),
                );
            } else {
                ui.painter().hline(
                    divider.rect.x_range(),
                    divider.rect.center().y,
                    tokens.hairline(),
                );
            }
            let style = glyphst_ui::craft::layout::ResizeHandleStyle {
                hovered: tokens.rule_strong,
                dragged: tokens.accent,
                focused: tokens.accent,
            };
            let handle = if horizontal {
                Rect::from_center_size(divider.rect.center(), vec2(4.0, divider.rect.height()))
            } else {
                Rect::from_center_size(divider.rect.center(), vec2(divider.rect.width(), 4.0))
            };
            let delta = glyphst_ui::craft::layout::ResizeHandle::new(
                Id::unique(("pane-divider", divider.path_bits, divider.depth)),
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
                    glyphst_ui::craft::layout::SplitSize::Ratio(first / divider.available.max(1.0)),
                );
            }
        }
    }

    /// While a tab is dragged from the tab bar over the pages: where it
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
                    let zone = glyphst_ui::craft::docking::drop_zone(rect, 0.0, pos)?;
                    Some((
                        pane,
                        zone,
                        glyphst_ui::craft::docking::zone_rect(rect, zone)?,
                    ))
                })
        });
        if released {
            self.chrome.tab_drag = None;
            if let Some((pane, zone, _)) = target {
                self.drop_tab(tab, pane, zone);
            }
            return;
        }
        if let Some((pane, zone, preview)) = target
            && pane != tab
        {
            let tokens = Tokens::get(ctx);
            let painter = ctx.layer_painter(egui::LayerId::new(
                Order::Foreground,
                Id::unique("tab-drop-preview"),
            ));
            // A dotted accent outline on an accent wash, and where it goes.
            painter.rect_filled(preview, 0, Tokens::fade(tokens.accent_soft, 0.6));
            let dots = |a: egui::Pos2, b: egui::Pos2| {
                painter.add(egui::Shape::dotted_line(&[a, b], tokens.accent, 4.0, 1.0));
            };
            let r = preview.shrink(2.0);
            dots(r.left_top(), r.right_top());
            dots(r.right_top(), r.right_bottom());
            dots(r.right_bottom(), r.left_bottom());
            dots(r.left_bottom(), r.left_top());
            let label = match zone {
                glyphst_ui::craft::docking::Zone::Left => "split left",
                glyphst_ui::craft::docking::Zone::Right => "split right",
                glyphst_ui::craft::docking::Zone::Top => "split up",
                glyphst_ui::craft::docking::Zone::Bottom => "split down",
                _ => "open here",
            };
            painter.text(
                preview.center(),
                Align2::CENTER_CENTER,
                label,
                theme::mono(12.0),
                tokens.accent_text,
            );
        }
        ctx.request_repaint();
    }

    /// The background effect behind the pages: the focused book's. Solid
    /// takes the theme's colour until the user sets one.
    pub(super) fn background_effect(&self, _tokens: &Tokens) -> EffectUse {
        self.background_for(self.active)
    }

    /// The background behind tab `index`'s pages; Home (`None`) shows the
    /// theme's base colour.
    pub(crate) fn background_for(&self, index: Option<usize>) -> EffectUse {
        let [r, g, b] = self.theme_bg;
        let Some(index) = index else {
            return EffectUse {
                id: glyphst_effects::SOLID.into(),
                params: [r, g, b, 1.0],
            };
        };
        let mut background = self.effects_for(Some(index)).background;
        if background.id == glyphst_effects::SOLID
            && !self
                .prefs
                .effect_params
                .contains_key(glyphst_effects::SOLID)
        {
            background.params = [r, g, b, 1.0];
        }
        background
    }

    /// The theme drawn (light until the first frame applies one).
    pub(crate) fn theme_kind(&self) -> ThemeKind {
        self.theme_applied
            .as_ref()
            .map_or(ThemeKind::Light, |applied| applied.kind)
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
            switch: self.prefs.switch_transition.as_deref(),
            overlay: self.prefs.tint(self.theme_kind()),
            params: &self.prefs.effect_params,
            theme: self.theme_colours,
        };
        self.with_gpu(|gpu| resolve(book, defaults, |id| gpu.effects().find(id)))
            .unwrap_or_else(|| resolve(book, defaults, |_| None))
    }

    /// The icon rail: one button per sidebar tab, the open one active;
    /// clicking the open one closes the sidebar.
    fn rail(&mut self, ui: &mut egui::Ui, rect: Rect, active: usize) {
        let tokens = Tokens::get(ui.ctx());
        ui.painter()
            .rect_filled(rect, 0, tokens.surface.to_opaque());
        ui.painter()
            .vline(rect.right() - 0.5, rect.y_range(), tokens.hairline());
        ui.spacing_mut().item_spacing.y = 0.0;
        let open = self.prefs.show_sidebar.then_some(self.prefs.sidebar);
        let mut toggled = None;
        for which in SidebarTab::ALL {
            let selected = open == Some(which);
            let badge = sidebar::badge(&self.tabs[active], which).map(sidebar::badge_label);
            let tip = which.label().to_lowercase();
            if ui_rail::button(ui, which.icon(), selected, &tip, badge.as_deref()).clicked() {
                toggled = Some((which, selected));
            }
        }
        self.toggle_drawer(toggled);
    }

    /// Opens the sidebar on a rail button's tab, or closes it when that tab
    /// is open.
    fn toggle_drawer(&mut self, toggled: Option<(SidebarTab, bool)>) {
        match toggled {
            Some((_, true)) => self.prefs.show_sidebar = false,
            Some((tab, false)) => {
                self.prefs.show_sidebar = true;
                self.prefs.sidebar = tab;
            }
            None => {}
        }
    }

    /// The sidebar: outline, pages, bookmarks, notes, search, translation
    /// or effects.
    fn sidebar_pane(&mut self, ui: &mut egui::Ui, rect: Rect, active: usize, now: Instant) {
        let ctx = ui.ctx().clone();
        let warnings: Vec<String> = self
            .user_settings
            .warnings
            .iter()
            .chain(&self.translator.warnings)
            .cloned()
            .collect();
        let other_bookmarks = if self.prefs.sidebar == SidebarTab::Bookmarks {
            self.other_bookmarks(active)
        } else {
            Vec::new()
        };
        let settings = self.user_settings.get();
        let info = SidebarInfo {
            target: self
                .translator
                .target_language(self.prefs.target_language.as_deref()),
            source: settings.and_then(|settings| settings.translation.source_language.clone()),
            provider: settings
                .map(|settings| settings.translation.active_provider)
                .unwrap_or_default(),
            original_on_hover: self.prefs.original_on_hover,
            translate_as_read: self.prefs.translate_as_read,
            pinned: &self.translator.pinned,
            warnings: &warnings,
            ready: settings.is_some(),
            other_bookmarks: &other_bookmarks,
            bookmark_key: self.keymap.label(Command::Bookmark).unwrap_or_default(),
            opened: self.opened_label(active),
            page_columns: self.prefs.page_columns,
        };
        // The Effects tab edits a copy of this book's choices.
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
        self.tabs[active]
            .outline_view
            .set_follow(self.prefs.outline_follow);
        let mut requests = sidebar::header(
            ui,
            &self.tabs[active],
            self.prefs.sidebar,
            self.prefs.page_columns,
        );
        let content = Rect::from_min_max(
            pos2(rect.left() + 12.0, ui.cursor().top()),
            pos2(rect.right() - 12.0, rect.bottom() - 8.0),
        );
        let mut body = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        if showing_effects {
            // The previews are animated.
            ctx.request_repaint();
            let find = |id: Option<&str>| id.and_then(|id| effects.iter().find(|def| def.id == id));
            let changes = crate::effects_page::book_page(
                &mut body,
                crate::effects_page::BookPage {
                    title: &title,
                    book: &mut book,
                    default_background: find(Some(&self.prefs.background)),
                    default_transition: find(self.prefs.transition.as_deref()),
                    default_switch: match self.prefs.switch_transition.as_deref() {
                        Some(glyphst_effects::RANDOM) => Some("random".to_owned()),
                        other => find(other).map(|def| def.name.to_lowercase()),
                    },
                    default_overlay: find(self.prefs.tint(self.theme_kind())),
                    effects: &effects,
                    params: &mut self.prefs.effect_params,
                    has_wallpaper: self.prefs.wallpaper.is_some(),
                    smooth_scrolling: &mut self.prefs.smooth_scrolling,
                    reduce_motion: &mut self.prefs.reduce_motion,
                    time,
                },
            );
            book_changed = changes.book;
        } else if self.prefs.sidebar == SidebarTab::Item {
            self.sidebar_item(&mut body, content, active, &ctx);
        } else {
            requests.extend(sidebar::show(
                &mut body,
                &mut self.tabs[active],
                &mut self.prefs.sidebar,
                &mut self.sidebar_state,
                &info,
                now,
            ));
        }
        if book_changed {
            book.used = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            self.prefs.books.insert(key, book);
            crate::book_effects::trim(&mut self.prefs.books);
        }
        let mut actions = Vec::new();
        for request in requests {
            self.apply_sidebar(request, &mut actions, &ctx);
        }
        self.pending_actions.extend(actions);
    }

    /// The sidebar's item tab (frame 03b): the document's library card, or
    /// an offer to add it.
    fn sidebar_item(&mut self, ui: &mut egui::Ui, rect: Rect, active: usize, ctx: &egui::Context) {
        let path = self.tabs[active].path.clone();
        let Some(id) = self
            .library
            .library_item_for_path(&path)
            .map(|item| item.id)
        else {
            let tokens = Tokens::get(ctx);
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("not in the library")
                    .font(crate::theme::mono(12.0))
                    .color(tokens.text_2),
            );
            ui.add_space(6.0);
            if ui
                .add(glyphst_ui::button::Button::bracket("add to library"))
                .clicked()
            {
                self.library_tab_action(
                    crate::library_tab::LibraryTabAction::AddOpened(vec![path]),
                    ctx,
                );
            }
            return;
        };
        let docs = crate::home::documents(&self.library, &self.prefs.page_counts);
        let vault = self
            .prefs
            .library
            .vault_root(self.library.files_root(), self.library.files_root_chosen());
        let card = Rect::from_min_max(
            pos2(rect.left() - 12.0, rect.top()),
            pos2(rect.right() + 12.0, rect.bottom() + 8.0),
        );
        let action = self.sidebar_card.item_card(
            ui,
            &mut crate::library_tab::TabContext {
                library: &self.library,
                covers: &mut self.covers,
                docs: &docs,
                vault: vault.as_deref(),
                import: None,
                search: None,
                meaning: None,
            },
            card,
            id,
            false,
            12.0,
        );
        if let Some(action) = action {
            self.library_tab_action(action, ctx);
        }
    }

    /// The assistant docked at the right: beside a document, or beside the
    /// library's pages, where it asks about the library.
    pub(crate) fn assistant_pane(&mut self, ui: &mut egui::Ui, rect: Rect, active: Option<usize>) {
        let ctx = ui.ctx().clone();
        let (document, page, selection_words) = match active {
            Some(active) => {
                let tab = &self.tabs[active];
                // Not waiting for the page's text: the page mention reads
                // the file, and the suggestions would flicker on every jump.
                let page = (tab.layout.len() > 0).then(|| tab.current_page());
                let selection_words = tab
                    .selection
                    .is_some()
                    .then(|| tab.selected_text())
                    .flatten()
                    .map(|text| text.split_whitespace().count())
                    .filter(|words| *words > 0);
                (Some(tab.title.clone()), page, selection_words)
            }
            None => (Some("the library".to_string()), None, None),
        };
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
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_min_size(rect.size());
        let (tabs, active_tab, library) = (&self.tabs, self.active, &self.library);
        let agents = self.agent_specs();
        let request = self.assistant.ui(
            &mut child,
            crate::ai::Panel {
                document,
                page,
                selection_words,
                models: &models,
                model: model.clone(),
                changes: &mut self.agent_changes,
                sources: &mut || super::agent::mention_sources(tabs, active_tab, library),
                agents: &agents,
            },
        );
        match request {
            Some(PanelAction::Send(draft)) => self.ask(draft, &ctx),
            Some(PanelAction::NewSession) => self.assistant.new_session(),
            Some(PanelAction::LoadSession(id)) => {
                self.assistant.load_session(id, &self.library);
            }
            Some(PanelAction::DeleteSession(id)) => {
                if let Err(error) = self.library.delete_ai_session(id) {
                    self.notices
                        .push(format!("Could not delete the chat: {error:#}"));
                }
                self.assistant.refresh_sessions(&self.library);
            }
            Some(PanelAction::SetModel(model)) => {
                // Choosing a model leaves a coding agent's chat.
                if self.assistant.agent.is_some() {
                    self.assistant.new_session();
                    self.assistant.agent = None;
                }
                self.user_settings
                    .edit(|settings| settings.ai.main_model = Some(model));
            }
            Some(PanelAction::Change(action)) => self.apply_change_action(action, &ctx),
            Some(PanelAction::Restore(index, draft)) => {
                self.restore_assistant(index, draft, &ctx);
            }
            Some(PanelAction::SetAgent(agent)) => {
                self.assistant.new_session();
                self.assistant.agent = agent;
            }
            Some(PanelAction::ChoosePicture) => self.pickers.choose_picture(&ctx),
            Some(PanelAction::SelectRegion) => {
                self.region_for_assistant = true;
                self.region_tool = true;
                self.notices
                    .push("Drag a rectangle on the page for the assistant. Esc cancels.");
            }
            Some(PanelAction::Command(command)) => {
                if let Err(reason) = self.run(command, &ctx) {
                    self.notices.push(reason.to_string());
                }
            }
            Some(PanelAction::OpenAiSettings) => {
                self.settings_window
                    .show_section(crate::settings_window::Section::Ai);
            }
            None => {}
        }
    }

    /// The keys of a sequence typed so far and its count (vim's showcmd),
    /// at the bottom, `lift` above the window's bottom.
    pub(super) fn pending_keys_chip(&self, ctx: &egui::Context, lift: f32) {
        let Some(label) = self.pending_keys.indicator() else {
            return;
        };
        let hints = self
            .keymap
            .continuations(&self.pending_keys)
            .into_iter()
            .take(4)
            .map(|(keys, command)| {
                let name = command.label().to_lowercase();
                let short: String = name
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{keys} {short}")
            })
            .collect::<Vec<_>>()
            .join(" · ");
        // At the bottom left of the pages, or of the window.
        let at = self
            .chrome
            .panes
            .first()
            .map(|(_, rect)| rect.left_bottom() + vec2(16.0, -16.0))
            .unwrap_or_else(|| ctx.content_rect().left_bottom() + vec2(16.0, -lift));
        overlay::pending_keys(ctx, &label, &hints, at);
    }
}

/// The side of a docked pane facing the pages.
#[derive(Clone, Copy)]
enum Edge {
    Left,
    Right,
}

/// A resize handle along `x` of a pane `rect`; returns how far it was
/// dragged to the right.
fn resize_handle(
    ui: &mut egui::Ui,
    tokens: &Tokens,
    id: &str,
    x: f32,
    rect: Rect,
    width: f32,
    min: f32,
) -> f32 {
    let handle = Rect::from_min_max(pos2(x - 2.0, rect.top()), pos2(x + 2.0, rect.bottom()));
    let style = glyphst_ui::craft::layout::ResizeHandleStyle {
        hovered: tokens.rule_strong,
        dragged: tokens.accent,
        focused: tokens.accent,
    };
    glyphst_ui::craft::layout::ResizeHandle::new(
        Id::unique((id, "resize")),
        glyphst_ui::craft::layout::SplitAxis::Horizontal,
        handle,
        "Resize",
        style,
    )
    .value(width, min..=1200.0)
    .show(ui)
    .delta
}

/// A tab's zoom as a percentage of fit width, anchored at `pos`; clicking
/// it goes back to 100%.
fn zoom_label(
    ui: &mut egui::Ui,
    pos: egui::Pos2,
    align: Align2,
    tab: &DocumentTab,
    tokens: &Tokens,
) -> egui::Response {
    let label = format!("{:.0}%", tab.view.scale / tab.fit_scale() * 100.0);
    let font = theme::mono(11.0);
    let size = ui_text::width(ui, &label, &font);
    let rect = align.anchor_size(pos, vec2(size, 20.0));
    let response = ui
        .interact(
            rect,
            Id::unique(("zoom-label", tab.id)),
            egui::Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .tip("back to 100% (fit width with margins)");
    let color = if response.hovered() {
        tokens.text
    } else {
        tokens.faint
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, label, font, color);
    response
}
