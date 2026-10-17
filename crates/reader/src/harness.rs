//! Scripted runs for unattended checks: a zoom, scroll, sidebar and find
//! benchmark that prints JSON, and in-app screenshots (egui's own capture of this
//! window), optionally with a search, a selection or a sidebar tab shown.
//! Both exit when done and neither saves preferences or library history.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use serde_json::json;

use crate::{
    app::ReaderApp,
    document::percentile_ms,
    document_tab::{MAX_ZOOM, ViewAction},
};

const BENCH_ZOOM_FRAMES: usize = 240;
const BENCH_SCROLL_FRAMES: usize = 600;
/// View pixels per frame while scrolling.
const BENCH_SCROLL_STEP: f64 = 40.0;
/// Frames to wait once the pages are drawn, so loading has settled; long
/// enough for raster layers to be refreshed for the view (150 ms).
const SETTLE_FRAMES: u32 = 40;
/// Frames between the sidebar's toggles, enough for a slide to settle.
const BENCH_SIDEBAR_FRAMES: usize = 40;
/// Times the sidebar slides in or out.
const BENCH_SIDEBAR_TOGGLES: usize = 6;
/// The benchmark searches for this word; it is common in English books.
const BENCH_QUERY: &str = "the";
/// The find run gives up after this many frames.
const BENCH_FIND_FRAMES: usize = 20_000;

pub enum Harness {
    Bench(Bench),
    Screenshot(Screenshot),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShotKind {
    /// The window as it is once the pages are drawn.
    View,
    /// Halfway through a presentation-mode page turn.
    PageTurn,
    /// Halfway through switching from the first tab to the second.
    TabSwitch,
    /// The loading animation replayed over the drawn pages, while its
    /// parts are traced.
    Generating,
    /// The loading animation's reveal, halfway down the page.
    Revealing,
    /// Presenting's tools (frame 02h): ink on the page, the laser's
    /// toolbox with the pen's colours, and the ring open.
    PresentTools,
}

/// Progress of a replayed loading animation at which `Generating` is
/// taken, and at which the pages of `Revealing` become ready.
const GENERATING_AT: f32 = 0.6;
/// The reveal's progress (in the animation's ending) at which
/// `Revealing` is taken: the scan line is about two thirds down.
const REVEALING_AT: f32 = 0.84;

pub struct Screenshot {
    path: PathBuf,
    kind: ShotKind,
    ready_frames: u32,
    started: bool,
    requested: bool,
}

impl Screenshot {
    pub fn new(path: PathBuf, kind: ShotKind) -> Self {
        Self {
            path,
            kind,
            ready_frames: 0,
            started: false,
            requested: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Settle(u32),
    Zoom(usize),
    Scroll(usize),
    /// The sidebar sliding in and out, with motion on.
    Sidebar(usize),
    Find(usize),
    Done,
}

pub struct Bench {
    phase: Phase,
    last_frame: Option<Instant>,
    zoom: Vec<Duration>,
    scroll: Vec<Duration>,
    sidebar: Vec<Duration>,
    /// View targets made while the sidebar slid.
    sidebar_targets: u64,
    find: Vec<Duration>,
    find_started: Option<Instant>,
    find_ms: Option<f64>,
    max_encode_ms: f64,
    peak_gpu_bytes: u64,
}

impl Default for Bench {
    fn default() -> Self {
        Self {
            phase: Phase::Settle(0),
            last_frame: None,
            zoom: Vec::new(),
            scroll: Vec::new(),
            sidebar: Vec::new(),
            sidebar_targets: 0,
            find: Vec::new(),
            find_started: None,
            find_ms: None,
            max_encode_ms: 0.0,
            peak_gpu_bytes: 0,
        }
    }
}

impl Harness {
    /// Whether motion runs: only while the benchmark slides the sidebar;
    /// screenshots are taken of settled frames.
    pub fn moves(&self) -> bool {
        matches!(self, Self::Bench(bench) if matches!(bench.phase, Phase::Sidebar(_)))
    }
}

impl ReaderApp {
    /// Starts the search and selection a screenshot asked for; returns
    /// whether they are still waiting for text.
    fn prepare_text_shot(&mut self, ctx: &egui::Context, now: Instant) -> bool {
        let Some(active) = self.active else {
            // Without a document, commands for the app's own tabs (a new
            // tab, the reading activity) run straight away.
            if self.tabs.is_empty() {
                for command in std::mem::take(&mut self.launch_commands) {
                    if let Err(reason) = self.run(command, ctx) {
                        eprintln!("--command {}: {reason}", command.id());
                    }
                }
            }
            return false;
        };
        for command in std::mem::take(&mut self.launch_commands) {
            if let Err(reason) = self.run(command, ctx) {
                eprintln!("--command {}: {reason}", command.id());
            }
        }
        if let Some((page, [x0, y0, x1, y1])) = self.launch_region.take() {
            self.region = Some(crate::region::Region {
                tab: self.tabs[active].id,
                page: page - 1,
                from: [x0, y0],
                to: [x1, y1],
            });
            self.finish_region(active);
        }
        let tab = &mut self.tabs[active];
        if let Some(query) = self.launch_find.take() {
            let page = tab.current_page();
            tab.find.show(page);
            tab.find.query = query;
            tab.find.edited(now - Duration::from_secs(1));
        }
        if let Some((first, last)) = self.launch_select {
            // The documents opened at the first page.
            let last_page = tab.layout.len() - 1;
            let (first, last) = ((first - 1).min(last_page), (last - 1).min(last_page));
            if !tab.select_pages(first, last) {
                // Waits for the pages' text (the next page's comes with it
                // once in view).
                return true;
            }
            self.launch_select = None;
        }
        // A search shows its final count, with the current match in view.
        let searching = tab.find.is_active() && !tab.find.is_complete();
        searching || self.prepare_translation(ctx)
    }

    /// Starts the translations a scripted run asked for once the settings
    /// are read; returns whether one is still running.
    pub(crate) fn prepare_translation(&mut self, ctx: &egui::Context) -> bool {
        let Some(active) = self.active else {
            return false;
        };
        // The assistant in a canned state once the page's text is read.
        if self.launch_demo.is_some() {
            let tab = &self.tabs[active];
            if tab.text.get(tab.current_page()).is_none() {
                return true;
            }
            let state = self.launch_demo.take().unwrap_or_default();
            let sources =
                crate::app::agent::mention_sources(&self.tabs, self.active, &self.library);
            self.assistant.open = true;
            self.assistant.demo(&state, ctx, sources);
        }
        if self.translator.settings().is_none() {
            return self.launch_translate_document
                || self.launch_translate_selection
                || self.launch_ask.is_some();
        }
        if self.launch_translate_document {
            self.launch_translate_document = false;
            self.start_document_translation(ctx);
        }
        if self.launch_translate_selection && self.launch_select.is_none() {
            if self.tabs[active].selection.is_none() {
                return true;
            }
            self.launch_translate_selection = false;
            let at = ctx.content_rect().center() - egui::vec2(180.0, 120.0);
            self.translate_selection(at, ctx);
        }
        // A question asked once the page's text is read (and the agents
        // are found, for a question to one).
        if self.launch_ask.is_some() {
            let tab = &self.tabs[active];
            if tab.text.get(tab.current_page()).is_none()
                || (self.assistant.agent.is_some() && self.agent_search_pending())
            {
                return true;
            }
            let question = self.launch_ask.take().unwrap_or_default();
            self.assistant.open = true;
            self.ask(crate::ai_editor::Draft::plain(question), ctx);
        }
        let document = self.tabs[active]
            .translation
            .as_ref()
            .is_some_and(|session| session.is_running());
        let selection = self
            .translator
            .card
            .as_ref()
            .is_some_and(|card| card.state == crate::translation::CardState::Loading);
        document
            || selection
            || self.launch_translate_selection
            || (self.assistant.is_running() && !self.assistant.is_demo())
    }

    pub(crate) fn run_harness(&mut self, ctx: &egui::Context, pages_drawn: bool, now: Instant) {
        let Some(mut harness) = self.harness.take() else {
            return;
        };
        let done = match &mut harness {
            Harness::Bench(bench) => self.bench_step(ctx, bench, pages_drawn, now),
            Harness::Screenshot(shot) => self.screenshot_step(ctx, shot, pages_drawn, now),
        };
        if done {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.harness = Some(harness);
    }

    fn bench_step(
        &mut self,
        ctx: &egui::Context,
        bench: &mut Bench,
        pages_drawn: bool,
        now: Instant,
    ) -> bool {
        // A translation runs alongside, its pages swapping in as they come.
        self.prepare_translation(ctx);
        let interval = bench.last_frame.replace(now).map(|last| now - last);
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        let targets_made = self.with_gpu(|gpu| gpu.targets_made).unwrap_or(0);
        bench.max_encode_ms = bench.max_encode_ms.max(stats.encode_ms);
        bench.peak_gpu_bytes = bench.peak_gpu_bytes.max(stats.gpu_bytes);
        let overviews = self.with_gpu(|gpu| gpu.has_pending_work()).unwrap_or(false);
        let (Some(active), Some(fit)) = (self.active, self.fit_scale()) else {
            // Nothing to measure without a document.
            return bench.phase == Phase::Done;
        };
        let tab = &mut self.tabs[active];
        let center = [tab.viewport[0] / 2.0, tab.viewport[1] / 2.0];
        let settled = pages_drawn && !overviews;
        bench.phase = match bench.phase {
            Phase::Settle(frames) if !settled => Phase::Settle(frames),
            Phase::Settle(frames) if frames < SETTLE_FRAMES => Phase::Settle(frames + 1),
            Phase::Settle(_) => Phase::Zoom(0),
            Phase::Zoom(frame) => {
                if frame > 0 {
                    bench.zoom.extend(interval);
                }
                let half = BENCH_ZOOM_FRAMES / 2;
                let t = if frame < half {
                    frame as f64 / half as f64
                } else {
                    (BENCH_ZOOM_FRAMES - frame) as f64 / half as f64
                };
                tab.view.anchor = center;
                tab.view.set_scale(fit * MAX_ZOOM.powf(t));
                tab.view.target_scale = tab.view.scale;
                if frame + 1 >= BENCH_ZOOM_FRAMES {
                    tab.view.set_scale(fit);
                    tab.view.target_scale = fit;
                    Phase::Scroll(0)
                } else {
                    Phase::Zoom(frame + 1)
                }
            }
            Phase::Scroll(frame) => {
                if frame > 0 {
                    bench.scroll.extend(interval);
                }
                tab.view.scroll_by_pixels(0.0, BENCH_SCROLL_STEP);
                if frame + 1 >= BENCH_SCROLL_FRAMES || tab.layout.len() == 1 {
                    Phase::Sidebar(0)
                } else {
                    Phase::Scroll(frame + 1)
                }
            }
            Phase::Sidebar(frame) => {
                if frame == 0 {
                    bench.sidebar_targets = targets_made;
                } else {
                    bench.sidebar.extend(interval);
                }
                if frame % BENCH_SIDEBAR_FRAMES == 0 {
                    self.prefs.show_sidebar = !self.prefs.show_sidebar;
                }
                if frame + 1 >= BENCH_SIDEBAR_FRAMES * BENCH_SIDEBAR_TOGGLES {
                    bench.sidebar_targets = targets_made - bench.sidebar_targets;
                    Phase::Find(0)
                } else {
                    Phase::Sidebar(frame + 1)
                }
            }
            // Searches the whole document while frames keep coming.
            Phase::Find(frame) => {
                if frame == 0 {
                    let page = tab.current_page();
                    tab.find.show(page);
                    tab.find.query = BENCH_QUERY.into();
                    bench.find_started = Some(now);
                } else {
                    bench.find.extend(interval);
                }
                if tab.find.is_complete() || frame + 1 >= BENCH_FIND_FRAMES {
                    bench.find_ms = tab
                        .find
                        .is_complete()
                        .then(|| bench.find_started.map(|t| (now - t).as_secs_f64() * 1000.0))
                        .flatten();
                    Phase::Done
                } else {
                    Phase::Find(frame + 1)
                }
            }
            Phase::Done => return true,
        };
        if bench.phase != Phase::Done {
            return false;
        }
        let summarize = |intervals: &[Duration]| {
            json!({
                "frames": intervals.len(),
                "p50_ms": percentile_ms(intervals, 0.5),
                "p95_ms": percentile_ms(intervals, 0.95),
                "max_ms": percentile_ms(intervals, 1.0),
                "over_25_ms": intervals.iter().filter(|i| i.as_secs_f64() > 0.025).count(),
            })
        };
        let tab = &self.tabs[active];
        let load: Vec<Duration> = tab
            .load_seconds
            .iter()
            .map(|s| Duration::from_secs_f64(*s))
            .collect();
        let summary = json!({
            "document": tab.path.display().to_string(),
            "pages": tab.layout.len(),
            "first_page_ms": self.first_page_ms,
            "first_part_ms": self.first_part_ms,
            "zoom": summarize(&bench.zoom),
            "scroll": summarize(&bench.scroll),
            "sidebar": {
                "slides": BENCH_SIDEBAR_TOGGLES,
                "targets_made": bench.sidebar_targets,
                "frames": summarize(&bench.sidebar),
            },
            "find": {
                "query": BENCH_QUERY,
                "complete_ms": bench.find_ms,
                "matches": tab.find.matches.len(),
                "frames": summarize(&bench.find),
            },
            "max_encode_ms": bench.max_encode_ms,
            "peak_gpu_mib": bench.peak_gpu_bytes as f64 / 1048576.0,
            "pages_loaded": tab.load_seconds.len(),
            "load_ms_p50": percentile_ms(&load, 0.5),
            "upload_ms_max": self.upload_ms.iter().copied().fold(0.0, f64::max),
            "first_visible_ms_max": self.upload_visible_ms.iter().copied().fold(0.0, f64::max),
            "complete_ms_max": self.upload_complete_ms.iter().copied().fold(0.0, f64::max),
            "translation": tab.translation.as_ref().map(|session| json!({
                "status": session.status_line(),
                "pages": session.previews.len(),
            })),
        });
        println!("{summary}");
        true
    }

    fn screenshot_step(
        &mut self,
        ctx: &egui::Context,
        shot: &mut Screenshot,
        pages_drawn: bool,
        now: Instant,
    ) -> bool {
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
            let saved = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                .map(|image| image.save(&shot.path));
            match saved {
                Some(Ok(())) => println!("saved {}", shot.path.display()),
                Some(Err(error)) => eprintln!("screenshot: {error}"),
                None => eprintln!("screenshot: bad image size"),
            }
            return true;
        }
        let layers_pending = self
            .active
            .is_some_and(|active| self.tabs[active].layers_pending());
        let text_pending = self.prepare_text_shot(ctx, now);
        if !pages_drawn || layers_pending || text_pending || shot.requested {
            shot.ready_frames = shot.ready_frames.min(SETTLE_FRAMES / 2);
            return false;
        }
        shot.ready_frames += 1;
        if shot.ready_frames < SETTLE_FRAMES {
            return false;
        }
        let halfway = |app: &Self| {
            app.transition
                .as_ref()
                .is_some_and(|t| t.progress(now) >= 0.5)
        };
        shot.requested = match shot.kind {
            ShotKind::View => true,
            ShotKind::PageTurn => {
                if !shot.started {
                    self.pending_actions
                        .extend([ViewAction::TogglePresent, ViewAction::NextPage]);
                    shot.started = true;
                }
                halfway(self)
            }
            ShotKind::TabSwitch => {
                // Settle on the first tab, so its image is the outgoing
                // view, then switch to the second.
                if !shot.started && self.tabs.len() >= 2 {
                    if self.active != Some(0) {
                        self.activate(Some(0));
                        self.transition = None;
                        shot.ready_frames = 0;
                        return false;
                    }
                    self.activate(Some(1));
                    shot.started = true;
                }
                shot.started && halfway(self)
            }
            ShotKind::PresentTools => {
                // Drawn the frame after the tools are set out.
                if !shot.started {
                    self.seed_present_tools(ctx);
                    shot.started = true;
                    return false;
                }
                true
            }
            ShotKind::Generating | ShotKind::Revealing => {
                let revealing = shot.kind == ShotKind::Revealing;
                if !shot.started {
                    self.generations
                        .replay(revealing.then_some(GENERATING_AT), now);
                    shot.started = true;
                }
                self.generations
                    .progress(now)
                    .is_some_and(|(progress, ending)| {
                        if revealing {
                            ending >= REVEALING_AT
                        } else {
                            progress >= GENERATING_AT
                        }
                    })
            }
        };
        if shot.requested {
            self.generations.pause_replay();
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        false
    }
}
