//! Scripted runs for unattended checks: a zoom-and-scroll benchmark that
//! prints JSON, and in-app screenshots (egui's own capture of this window).
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
}

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
    Done,
}

pub struct Bench {
    phase: Phase,
    last_frame: Option<Instant>,
    zoom: Vec<Duration>,
    scroll: Vec<Duration>,
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
            max_encode_ms: 0.0,
            peak_gpu_bytes: 0,
        }
    }
}

impl ReaderApp {
    pub(crate) fn run_harness(&mut self, ctx: &egui::Context, pages_drawn: bool, now: Instant) {
        let Some(mut harness) = self.harness.take() else {
            return;
        };
        let done = match &mut harness {
            Harness::Bench(bench) => self.bench_step(bench, pages_drawn, now),
            Harness::Screenshot(shot) => self.screenshot_step(ctx, shot, pages_drawn, now),
        };
        if done {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.harness = Some(harness);
    }

    fn bench_step(&mut self, bench: &mut Bench, pages_drawn: bool, now: Instant) -> bool {
        let interval = bench.last_frame.replace(now).map(|last| now - last);
        let stats = self.with_gpu(|gpu| gpu.stats.clone()).unwrap_or_default();
        bench.max_encode_ms = bench.max_encode_ms.max(stats.encode_ms);
        bench.peak_gpu_bytes = bench.peak_gpu_bytes.max(stats.gpu_bytes);
        let overviews = self
            .with_gpu(|gpu| gpu.has_pending_overview())
            .unwrap_or(false);
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
                    Phase::Done
                } else {
                    Phase::Scroll(frame + 1)
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
            "zoom": summarize(&bench.zoom),
            "scroll": summarize(&bench.scroll),
            "max_encode_ms": bench.max_encode_ms,
            "peak_gpu_mib": bench.peak_gpu_bytes as f64 / 1048576.0,
            "pages_loaded": tab.load_seconds.len(),
            "load_ms_p50": percentile_ms(&load, 0.5),
            "upload_ms_max": self.upload_ms.iter().copied().fold(0.0, f64::max),
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
        if !pages_drawn || layers_pending || shot.requested {
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
        };
        if shot.requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        false
    }
}
