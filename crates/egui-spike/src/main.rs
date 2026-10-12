//! egui decision spike: GPU vector page rendering, IME/CJK text input, a
//! shader page transition and gamepad navigation in one eframe window.
//!
//! glyphst-egui-spike <pdf> [--bench]
//!
//! Mouse: Ctrl+wheel or pinch zooms at the pointer, wheel and drag scroll.
//! Keys (outside text fields): + / - zoom, 0 fits the width, P toggles
//! presentation mode, Page Up / Page Down / Space turn pages there.
//! `--bench` runs a scripted zoom and scroll, prints JSON and exits.
//! `--screenshot PATH` saves the window once the first pages are drawn, and
//! `--screenshot-transition PATH` halfway through a presentation-mode page
//! turn; both use egui's own capture of this window and then exit.

mod document;
mod fonts;
mod gamepad;
mod gpu;
mod input_panel;
mod transition;

use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use document::{FrameTimes, Layout, Loaded, Loader, PAGE_GAP, View, percentile_ms};
use eframe::egui_wgpu;
use gamepad::{Action, GamepadInput, Gamepads};
use gpu::{PAGE_BUDGET_BYTES, PageDraw, PagesCallback, SpikeGpu, pixel_size};
use serde_json::json;
use transition::Transition;

const MAX_ZOOM: f64 = 64.0;
const MIN_ZOOM: f64 = 0.1;
const KEY_ZOOM: f64 = 1.25;
/// Pages loaded beyond the visible ones, in each direction.
const PREFETCH_PAGES: usize = 2;
const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(82, 84, 90);

fn main() -> Result<()> {
    let started = Instant::now();
    let mut args = std::env::args().skip(1);
    let mut pdf = PathBuf::new();
    let mut bench = false;
    let mut screenshot = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bench" => bench = true,
            "--screenshot" | "--screenshot-transition" => {
                let path = PathBuf::from(args.next().context("missing screenshot path")?);
                screenshot = Some(Screenshot {
                    path,
                    transition: arg == "--screenshot-transition",
                    ready_frames: 0,
                    requested: false,
                });
            }
            _ if arg.starts_with("--") => bail!("unknown option {arg}"),
            _ => pdf = arg.into(),
        }
    }
    if pdf.as_os_str().is_empty() {
        bail!(
            "usage: glyphst-egui-spike <pdf> [--bench | --screenshot PATH | --screenshot-transition PATH]"
        );
    }
    let bounds =
        document::page_bounds(&pdf).with_context(|| format!("failed to open {}", pdf.display()))?;
    if bounds.is_empty() {
        bail!("{} has no pages", pdf.display());
    }

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Glyphst egui spike")
            .with_app_id("glyphst-egui-spike")
            .with_inner_size([1440.0, 920.0]),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "glyphst-egui-spike",
        native,
        Box::new(move |cc| {
            let mut app = SpikeApp::new(cc, pdf, bounds, bench, started)?;
            if screenshot.is_some() {
                app.input
                    .prefill("中文输入测试", "日本語のテキスト\n한국어 텍스트");
            }
            app.screenshot = screenshot;
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchPhase {
    WaitForPages,
    Zoom(usize),
    Scroll(usize),
    Done,
}

const BENCH_ZOOM_FRAMES: usize = 240;
const BENCH_SCROLL_FRAMES: usize = 600;
/// View pixels per frame while scrolling in the benchmark.
const BENCH_SCROLL_STEP: f64 = 40.0;

struct Screenshot {
    path: PathBuf,
    transition: bool,
    /// Frames drawn since the visible pages became resident.
    ready_frames: u32,
    requested: bool,
}

/// Frames to wait after the pages are resident, so the HUD has settled.
const SCREENSHOT_SETTLE_FRAMES: u32 = 10;

struct Bench {
    phase: BenchPhase,
    last_frame: Option<Instant>,
    zoom_intervals: Vec<Duration>,
    scroll_intervals: Vec<Duration>,
    max_encode_ms: f64,
    peak_gpu_bytes: u64,
}

struct SpikeApp {
    layout: Layout,
    view: View,
    /// View size in device pixels from the last frame.
    viewport: [f64; 2],
    fitted: bool,
    loader: Loader,
    thumbnails: HashMap<usize, egui::TextureHandle>,
    failed: HashMap<usize, String>,
    render_state: egui_wgpu::RenderState,
    fonts: fonts::FontStatus,
    input: input_panel::InputPanel,
    gamepads: Result<Gamepads, String>,
    present: Option<usize>,
    transition: Option<Transition>,
    frames: FrameTimes,
    continuous: bool,
    repaints: u64,
    last_update: Instant,
    load_seconds: Vec<f64>,
    upload_ms: Vec<f64>,
    started: Instant,
    first_page_ms: Option<f64>,
    bench: Option<Bench>,
    screenshot: Option<Screenshot>,
}

impl SpikeApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        pdf: PathBuf,
        bounds: Vec<glyphst_pdf::vector::PageRect>,
        bench: bool,
        started: Instant,
    ) -> Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .context("the spike needs eframe's wgpu renderer")?;
        SpikeGpu::install(&render_state);
        let fonts = fonts::install(&cc.egui_ctx);
        Ok(Self {
            layout: Layout::new(bounds),
            view: View::new(1.0),
            viewport: [1.0, 1.0],
            fitted: false,
            loader: Loader::new(pdf, cc.egui_ctx.clone()),
            thumbnails: HashMap::new(),
            failed: HashMap::new(),
            render_state,
            fonts,
            input: input_panel::InputPanel::default(),
            gamepads: Gamepads::new(),
            present: None,
            transition: None,
            frames: FrameTimes::default(),
            continuous: false,
            repaints: 0,
            last_update: Instant::now(),
            load_seconds: Vec::new(),
            upload_ms: Vec::new(),
            started,
            first_page_ms: None,
            bench: bench.then(|| Bench {
                phase: BenchPhase::WaitForPages,
                last_frame: None,
                zoom_intervals: Vec::new(),
                scroll_intervals: Vec::new(),
                max_encode_ms: 0.0,
                peak_gpu_bytes: 0,
            }),
            screenshot: None,
        })
    }

    fn with_gpu<R>(&self, f: impl FnOnce(&mut SpikeGpu) -> R) -> Option<R> {
        let mut renderer = self.render_state.renderer.write();
        renderer.callback_resources.get_mut::<SpikeGpu>().map(f)
    }

    fn is_resident(&self, page: usize) -> bool {
        self.with_gpu(|gpu| gpu.is_resident(page)).unwrap_or(false)
    }

    /// Takes loader results; uploads at most one page per frame so a heavy
    /// page costs one long frame rather than several in a row.
    fn receive_pages(&mut self, ctx: &egui::Context) {
        while let Some(loaded) = self.loader.try_recv() {
            match loaded {
                Loaded::Thumbnail { page, image } => {
                    let texture = ctx.load_texture(
                        format!("thumbnail-{page}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumbnails.insert(page, texture);
                }
                Loaded::Prepared {
                    page,
                    prepared,
                    layers,
                    seconds,
                } => {
                    let (device, queue) = (
                        self.render_state.device.clone(),
                        self.render_state.queue.clone(),
                    );
                    if let Some(ms) =
                        self.with_gpu(|gpu| gpu.upload(&device, &queue, page, &prepared, &layers))
                    {
                        self.upload_ms.push(ms);
                    }
                    self.load_seconds.push(seconds);
                    if self.first_page_ms.is_none() {
                        self.first_page_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
                    }
                    ctx.request_repaint();
                    break;
                }
                Loaded::Failed { page, error } => {
                    self.failed.insert(page, error);
                }
            }
        }
    }

    fn fit_width(&mut self, viewport: [f64; 2]) {
        self.view = View::new(viewport[0] / (self.layout.width + 2.0 * PAGE_GAP));
        let first = self
            .layout
            .visible(self.view.origin[1], self.view.origin[1] + 1.0)
            .start;
        self.view.origin = [
            -PAGE_GAP,
            self.layout.offsets[first.min(self.layout.len() - 1)][1] - PAGE_GAP,
        ];
    }

    /// The page at the vertical center of the view.
    fn current_page(&self, viewport: [f64; 2]) -> usize {
        let center = self.view.origin[1] + viewport[1] / 2.0 / self.view.scale;
        self.layout
            .offsets
            .partition_point(|offset| offset[1] <= center)
            .saturating_sub(1)
    }

    fn go_to_page(&mut self, page: usize) {
        self.view.origin[1] = self.layout.offsets[page][1] - PAGE_GAP;
    }

    fn step_page(&mut self, delta: isize, viewport: [f64; 2], now: Instant) {
        let last = self.layout.len() - 1;
        match self.present {
            Some(current) => {
                let target = current.saturating_add_signed(delta).min(last);
                if target != current && self.transition.is_none() {
                    self.transition = Some(Transition::new(current, target, now));
                }
            }
            None => {
                let target = self
                    .current_page(viewport)
                    .saturating_add_signed(delta)
                    .min(last);
                self.go_to_page(target);
            }
        }
    }

    fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        viewport: [f64; 2],
        pad: &GamepadInput,
        seconds: f64,
        now: Instant,
    ) {
        let ppp = f64::from(ui.ctx().pixels_per_point());
        let rect = response.rect;
        let fit = viewport[0] / (self.layout.width + 2.0 * PAGE_GAP);
        let (min, max) = (fit * MIN_ZOOM, fit * MAX_ZOOM);
        let center = [viewport[0] / 2.0, viewport[1] / 2.0];
        let pointer = response.hover_pos().map(|pos| {
            [
                f64::from(pos.x - rect.min.x) * ppp,
                f64::from(pos.y - rect.min.y) * ppp,
            ]
        });

        if self.present.is_none() {
            let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if let Some(pointer) = pointer {
                if zoom != 1.0 {
                    self.view.zoom_by(f64::from(zoom), pointer, min, max);
                }
                self.view
                    .scroll_by_pixels(-f64::from(scroll.x) * ppp, -f64::from(scroll.y) * ppp);
            }
            let drag = response.drag_delta();
            self.view
                .scroll_by_pixels(-f64::from(drag.x) * ppp, -f64::from(drag.y) * ppp);
            self.view.scroll_by_pixels(
                f64::from(pad.scroll[0]) * seconds,
                f64::from(pad.scroll[1]) * seconds,
            );
            if pad.zoom != 0.0 {
                self.view
                    .zoom_by((f64::from(pad.zoom) * seconds).exp(), center, min, max);
            }
        }

        let typing = input_panel::InputPanel::has_focus(ui.ctx());
        let pressed = |key| !typing && ui.input(|i| i.key_pressed(key));
        let mut actions = pad.actions.clone();
        if pressed(egui::Key::Plus) || pressed(egui::Key::Equals) {
            self.view.zoom_by(KEY_ZOOM, center, min, max);
        }
        if pressed(egui::Key::Minus) {
            self.view.zoom_by(1.0 / KEY_ZOOM, center, min, max);
        }
        if pressed(egui::Key::Num0) {
            actions.push(Action::FitWidth);
        }
        if pressed(egui::Key::P) {
            actions.push(Action::TogglePresent);
        }
        if pressed(egui::Key::PageDown) || pressed(egui::Key::Space) {
            actions.push(Action::NextPage);
        }
        if pressed(egui::Key::PageUp) {
            actions.push(Action::PreviousPage);
        }
        for action in actions {
            match action {
                Action::PreviousPage => self.step_page(-1, viewport, now),
                Action::NextPage => self.step_page(1, viewport, now),
                Action::FitWidth => {
                    let page = self.current_page(viewport);
                    self.fit_width(viewport);
                    self.go_to_page(page);
                }
                Action::TogglePresent => {
                    self.present = match self.present {
                        Some(page) => {
                            self.go_to_page(page);
                            None
                        }
                        None => Some(self.current_page(viewport)),
                    };
                    self.transition = None;
                }
                Action::Step(steps) => {
                    self.view
                        .scroll_by_pixels(0.0, f64::from(steps as f32 * gamepad::DPAD_STEP));
                }
            }
        }
    }

    /// One page fitted and centered, for presentation mode.
    fn presented(&self, page: usize, size: [u32; 2]) -> PageDraw {
        let [width, height] = self.layout.page_size(page);
        let viewport = [f64::from(size[0]), f64::from(size[1])];
        let scale = (viewport[0] / width).min(viewport[1] / height) * 0.96;
        let mut view = View::new(scale);
        view.origin = [
            self.layout.offsets[page][0] + (width - viewport[0] / scale) / 2.0,
            self.layout.offsets[page][1] + (height - viewport[1] / scale) / 2.0,
        ];
        self.page_draw(page, &view, size)
    }

    fn page_draw(&self, page: usize, view: &View, size: [u32; 2]) -> PageDraw {
        let offset = self.layout.offsets[page];
        let [width, height] = self.layout.page_size(page);
        let x0 = (offset[0] - view.origin[0]) * view.scale;
        let y0 = (offset[1] - view.origin[1]) * view.scale;
        PageDraw {
            page,
            camera: self
                .layout
                .page_camera(page, view.origin, view.scale as f32, size),
            paper: [
                x0 as f32,
                y0 as f32,
                (x0 + width * view.scale) as f32,
                (y0 + height * view.scale) as f32,
            ],
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        self.input.ui(ui, &self.fonts);
        ui.separator();
        ui.heading("Rendering");
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        ui.monospace(format!(
            "frame interval p50 {:5.1} ms  p95 {:5.1} ms\nencode {:5.2} ms  draws {}  segments {}\nresident pages {}  GPU {:.0} MiB\nzoom {:.2} px/pt  repaints {}",
            self.frames.percentile_ms(0.5),
            self.frames.percentile_ms(0.95),
            stats.encode_ms,
            stats.draw.draw_calls,
            stats.draw.segments,
            stats.resident,
            stats.gpu_bytes as f64 / 1048576.0,
            self.view.scale,
            self.repaints,
        ));
        let page = self
            .present
            .unwrap_or_else(|| self.current_page(self.viewport));
        let source = self
            .with_gpu(|gpu| gpu.source(page, self.view.scale as f32))
            .flatten()
            .unwrap_or_else(|| "loading".into());
        ui.monospace(format!(
            "page {} of {} ({source})",
            page + 1,
            self.layout.len()
        ));
        if let Some(ms) = self.first_page_ms {
            ui.monospace(format!("first page drawn after {ms:.0} ms"));
        }
        if let Some(last) = self.load_seconds.last() {
            ui.monospace(format!(
                "last page: load {:.0} ms, upload {:.0} ms",
                last * 1000.0,
                self.upload_ms.last().copied().unwrap_or_default()
            ));
        }
        if !self.failed.is_empty() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{} pages failed", self.failed.len()),
            );
        }
        ui.separator();
        ui.heading("Gamepad");
        match &self.gamepads {
            Ok(pads) => {
                let names = pads.connected();
                if names.is_empty() {
                    ui.label("No gamepad connected");
                } else {
                    for name in names {
                        ui.label(name);
                    }
                }
            }
            Err(error) => {
                ui.label(format!("Unavailable: {error}"));
            }
        }
        ui.label("Stick: scroll · triggers: zoom · LB/RB: page · A: fit · Start: present");
        ui.separator();
        ui.label("Ctrl+wheel/pinch: zoom · 0: fit width · P: present · PgUp/PgDn: page");
    }

    /// Drives `--screenshot`: waits for the pages, starts a page turn when
    /// asked to capture one, requests the capture and saves it.
    fn run_screenshot(
        &mut self,
        ctx: &egui::Context,
        visible_ready: bool,
        now: Instant,
    ) -> Result<()> {
        let current = self.current_page(self.viewport);
        let last = self.layout.len() - 1;
        let Some(shot) = &mut self.screenshot else {
            return Ok(());
        };
        let captured = ctx.input(|i| {
            i.raw.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = captured {
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect();
            let [width, height] = image.size;
            image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                .context("bad screenshot size")?
                .save(&shot.path)
                .with_context(|| format!("failed to save {}", shot.path.display()))?;
            println!("saved {}", shot.path.display());
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return Ok(());
        }
        if !visible_ready || shot.requested {
            return Ok(());
        }
        shot.ready_frames += 1;
        if shot.ready_frames < SCREENSHOT_SETTLE_FRAMES {
            return Ok(());
        }
        if shot.transition {
            match (self.present, self.transition) {
                (None, _) => self.present = Some(current),
                (Some(page), None) if page < last => {
                    self.transition = Some(Transition::new(page, page + 1, now));
                }
                (Some(_), Some(transition)) if transition.progress(now) >= 0.5 => {
                    shot.requested = true;
                }
                _ => {}
            }
        } else {
            shot.requested = true;
        }
        if shot.requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        Ok(())
    }

    fn run_bench(&mut self, ctx: &egui::Context, viewport: [f64; 2], visible_ready: bool) {
        let Some(bench) = &mut self.bench else {
            return;
        };
        let now = Instant::now();
        let interval = bench.last_frame.replace(now).map(|last| now - last);
        let stats = {
            let mut renderer = self.render_state.renderer.write();
            renderer
                .callback_resources
                .get_mut::<SpikeGpu>()
                .map(|gpu| gpu.stats.clone())
                .unwrap_or_default()
        };
        bench.max_encode_ms = bench.max_encode_ms.max(stats.encode_ms);
        bench.peak_gpu_bytes = bench.peak_gpu_bytes.max(stats.gpu_bytes);
        let fit = viewport[0] / (self.layout.width + 2.0 * PAGE_GAP);
        let center = [viewport[0] / 2.0, viewport[1] / 2.0];
        let previous = bench.phase;
        bench.phase = match bench.phase {
            BenchPhase::WaitForPages if visible_ready => BenchPhase::Zoom(0),
            BenchPhase::WaitForPages => BenchPhase::WaitForPages,
            BenchPhase::Zoom(frame) => {
                if frame > 0
                    && let Some(interval) = interval
                {
                    bench.zoom_intervals.push(interval);
                }
                let half = BENCH_ZOOM_FRAMES / 2;
                let t = if frame < half {
                    frame as f64 / half as f64
                } else {
                    (BENCH_ZOOM_FRAMES - frame) as f64 / half as f64
                };
                self.view.anchor = center;
                self.view.set_scale(fit * MAX_ZOOM.powf(t));
                self.view.target_scale = self.view.scale;
                if frame + 1 >= BENCH_ZOOM_FRAMES {
                    self.view.set_scale(fit);
                    self.view.target_scale = fit;
                    BenchPhase::Scroll(0)
                } else {
                    BenchPhase::Zoom(frame + 1)
                }
            }
            BenchPhase::Scroll(frame) => {
                if frame > 0
                    && let Some(interval) = interval
                {
                    bench.scroll_intervals.push(interval);
                }
                self.view.scroll_by_pixels(0.0, BENCH_SCROLL_STEP);
                if frame + 1 >= BENCH_SCROLL_FRAMES || self.layout.len() == 1 {
                    BenchPhase::Done
                } else {
                    BenchPhase::Scroll(frame + 1)
                }
            }
            BenchPhase::Done => BenchPhase::Done,
        };
        if bench.phase == BenchPhase::Done && previous != BenchPhase::Done {
            let summarize = |intervals: &[Duration]| {
                json!({
                    "frames": intervals.len(),
                    "p50_ms": percentile_ms(intervals, 0.5),
                    "p95_ms": percentile_ms(intervals, 0.95),
                    "max_ms": percentile_ms(intervals, 1.0),
                    "over_25_ms": intervals.iter().filter(|i| i.as_secs_f64() > 0.025).count(),
                })
            };
            let summary = json!({
                "pages": self.layout.len(),
                "first_page_ms": self.first_page_ms,
                "zoom": summarize(&bench.zoom_intervals),
                "scroll": summarize(&bench.scroll_intervals),
                "max_encode_ms": bench.max_encode_ms,
                "peak_gpu_mib": bench.peak_gpu_bytes as f64 / 1048576.0,
                "pages_loaded": self.load_seconds.len(),
                "load_ms_p50": percentile_ms(&self.load_seconds.iter().map(|s| Duration::from_secs_f64(*s)).collect::<Vec<_>>(), 0.5),
                "upload_ms_max": self.upload_ms.iter().copied().fold(0.0, f64::max),
            });
            println!("{summary}");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for SpikeApp {
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

        self.receive_pages(&ctx);
        let pad = match &mut self.gamepads {
            Ok(pads) => pads.poll(),
            Err(_) => GamepadInput::default(),
        };

        egui::Panel::right("spike_side")
            .default_size(340.0)
            .show(ui, |ui| self.side_panel(ui));

        let mut continuous = pad.is_active() || self.bench.is_some() || self.screenshot.is_some();
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(BACKGROUND))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                let ppp = ctx.pixels_per_point();
                let size = pixel_size(rect, ppp);
                let viewport = [f64::from(size[0]), f64::from(size[1])];
                self.viewport = viewport;
                if !self.fitted {
                    self.fit_width(viewport);
                    self.fitted = true;
                }
                self.handle_input(ui, &response, viewport, &pad, seconds, now);
                continuous |= self.view.animate(seconds);
                self.view.clamp(&self.layout, viewport);

                // Pages to draw (layers) and to keep loaded (wanted).
                let (layers, progress, direction, wanted) = match (self.present, self.transition) {
                    (Some(_), Some(transition)) => {
                        let progress = transition.progress(now);
                        if transition.finished(now) {
                            self.present = Some(transition.to);
                            self.transition = None;
                        }
                        continuous = true;
                        (
                            vec![
                                vec![self.presented(transition.from, size)],
                                vec![self.presented(transition.to, size)],
                            ],
                            progress,
                            transition.direction(),
                            vec![transition.to, transition.from, transition.to + 1],
                        )
                    }
                    (Some(page), None) => (
                        vec![vec![self.presented(page, size)]],
                        0.0,
                        1.0,
                        vec![page, page + 1, page.saturating_sub(1)],
                    ),
                    (None, _) => {
                        let y1 = self.view.origin[1] + viewport[1] / self.view.scale;
                        let visible = self.layout.visible(self.view.origin[1], y1);
                        let draws: Vec<PageDraw> = visible
                            .clone()
                            .map(|page| self.page_draw(page, &self.view, size))
                            .collect();
                        let mut wanted: Vec<usize> = visible.clone().collect();
                        for distance in 1..=PREFETCH_PAGES {
                            wanted.push(visible.end + distance - 1);
                            if let Some(before) = visible.start.checked_sub(distance) {
                                wanted.push(before);
                            }
                        }
                        (vec![draws], 0.0, 1.0, wanted)
                    }
                };
                let wanted: Vec<usize> = wanted
                    .into_iter()
                    .filter(|&page| page < self.layout.len())
                    .collect();
                let resident_pages: std::collections::HashSet<usize> = wanted
                    .iter()
                    .copied()
                    .filter(|&page| self.is_resident(page))
                    .collect();
                self.loader
                    .want(&wanted, |page| resident_pages.contains(&page));
                if let Some(dropped) = self.with_gpu(|gpu| gpu.evict(&wanted, PAGE_BUDGET_BYTES)) {
                    for page in dropped {
                        self.loader.forget(page);
                    }
                }

                // Pages not resident yet show their thumbnail on paper.
                let painter = ui.painter_at(rect);
                let to_points = |px: f32| px / ppp;
                let mut gpu_layers = Vec::new();
                let mut visible_ready = true;
                for draws in &layers {
                    let mut ready = Vec::new();
                    for draw in draws {
                        if resident_pages.contains(&draw.page) {
                            ready.push(*draw);
                            continue;
                        }
                        visible_ready = false;
                        let [x0, y0, x1, y1] = draw.paper.map(to_points);
                        let paper = egui::Rect::from_min_max(
                            rect.min + egui::vec2(x0, y0),
                            rect.min + egui::vec2(x1, y1),
                        );
                        painter.rect_filled(paper, 0.0, egui::Color32::WHITE);
                        if let Some(texture) = self.thumbnails.get(&draw.page) {
                            painter.image(
                                texture.id(),
                                paper,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        }
                    }
                    gpu_layers.push(ready);
                }
                painter.add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    PagesCallback {
                        rect,
                        layers: gpu_layers,
                        progress,
                        direction,
                    },
                ));
                self.run_bench(&ctx, viewport, visible_ready);
                if let Err(error) = self.run_screenshot(&ctx, visible_ready, now) {
                    eprintln!("screenshot: {error:#}");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

        self.continuous = continuous;
        if continuous {
            ctx.request_repaint();
        }
    }
}
