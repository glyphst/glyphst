use glyphst_document_ui::document_view::{
    DocumentRenderDebugSnapshot, GpuCacheDebugSnapshot, GpuDirectRenderSnapshot,
    GpuPresentationQuality, GpuVectorCoverageStrategy, PageRenderBackendDebug,
    PageRenderDebugSnapshot, PageRenderDebugStatus, RenderCacheDebugSnapshot,
    SynchronizedScrollDelta, SynchronizedScrollMode, TextIndexDebugState,
    render_cache_debug_snapshot,
};
use gpui::{
    AnyElement, App, ClipboardItem, Div, FrameEvent, FrameTiming, FrameTimingCollector, Hsla,
    IntoElement as _, ParentElement as _, PresentTiming, Styled as _, WeakEntity, Window, WindowId,
    div, hsla, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use serde_json::{Map, Value, json};
use std::{
    fs, io,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::*;

const FOREGROUND: Hsla = hsla(0., 0., 0.98, 1.);
const MUTED: Hsla = hsla(0., 0., 0.62, 1.);
const GOOD: Hsla = hsla(0.39, 0.70, 0.62, 1.);
const WARN: Hsla = hsla(0.12, 0.85, 0.62, 1.);
const BAD: Hsla = hsla(0.00, 0.78, 0.67, 1.);
const BORDER: Hsla = hsla(0., 0., 1., 0.14);
const FRAME_BUDGET: Duration = Duration::from_micros(8_333);
const MAX_RAW_FRAME_SAMPLES: usize = 4_096;
const TRACE_WARMUP_READY_FRAMES: usize = 5;
// Dense retained CAD pages can spend more than 15 seconds in a debug-profile
// cold compile before producing their first surface. Keep that observable as
// cold preparation rather than aborting the interaction trace before it can
// measure any zoom frames.
const TRACE_WARMUP_TIMEOUT: Duration = Duration::from_secs(90);
const TRACE_SCROLL_FRAMES_PER_DIRECTION: usize = 120;
const TRACE_SCROLL_MIN_PIXELS_PER_FRAME: f32 = 8.0;
const TRACE_SCROLL_MAX_PIXELS_PER_FRAME: f32 = 32.0;
const TRACE_ZOOM_FRAMES_PER_DIRECTION: usize = 60;
const TRACE_SETTLING_FRAMES: usize = 30;
const TRACE_SETTLING_TIMEOUT: Duration = Duration::from_secs(15);
const TRACE_ZOOM_FACTOR_PER_FRAME: f32 = 1.011_619_4;
const TRACE_SCROLL_ACTIVATION_LIMIT: Duration = Duration::from_millis(150);
const TRACE_VECTOR_LOD_MAX_UNDERPREPARED_FRAMES: usize = 30;
const TRACE_REPORT_VERSION: u32 = 15;

struct RenderDebugCaptureNotification;

pub(super) struct RenderDebugCapture {
    collector: FrameTimingCollector,
    automated_timings: Vec<CapturedFrameTiming>,
    owns_frame_trace: bool,
    window_id: WindowId,
    started_at: Instant,
    measurement_started_at: Option<Instant>,
    captured_at: String,
    document: WeakEntity<DocumentView>,
    document_entity_id: u64,
    document_start: DocumentRenderDebugSnapshot,
    render_cache_start: RenderCacheDebugSnapshot,
    trace: Option<RenderDebugTraceReport>,
    automated_output: Option<PathBuf>,
}

impl Drop for RenderDebugCapture {
    fn drop(&mut self) {
        if self.owns_frame_trace {
            gpui::set_trace_enabled(false);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderDebugTraceKind {
    Scroll,
    Zoom,
    ZoomScroll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutomatedRenderTraceStart {
    Waiting,
    Started,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomatedRenderTraceCompletion {
    pub output: PathBuf,
    pub failure: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutomatedRenderTraceProgress {
    pub phase: String,
    pub phase_frame: usize,
    pub ready_frames: usize,
    pub completed_interaction_frames: usize,
    pub completed_settling_frames: usize,
}

impl RenderDebugTraceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Scroll => "scroll",
            Self::Zoom => "zoom",
            Self::ZoomScroll => "zoom-scroll",
        }
    }

    fn frames_per_direction(self) -> usize {
        match self {
            Self::Scroll => TRACE_SCROLL_FRAMES_PER_DIRECTION,
            Self::Zoom | Self::ZoomScroll => TRACE_ZOOM_FRAMES_PER_DIRECTION,
        }
    }

    fn planned_interaction_frames(self) -> usize {
        self.frames_per_direction() * 2
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenderDebugTracePhase {
    Warmup,
    Forward,
    Reverse,
    Settle,
}

impl RenderDebugTracePhase {
    fn name(self) -> &'static str {
        match self {
            Self::Warmup => "warmup",
            Self::Forward => "forward",
            Self::Reverse => "reverse",
            Self::Settle => "settle",
        }
    }
}

#[derive(Clone, Debug)]
struct RenderDebugTraceMarker {
    phase: &'static str,
    frame_index: usize,
    offset: Duration,
}

#[derive(Clone, Debug)]
struct RenderDebugTraceFrame {
    phase: &'static str,
    frame_index: usize,
    offset: Duration,
    zoom: f32,
    target_zoom: f32,
    scroll_x_doc: f64,
    scroll_y_doc: f64,
    target_scroll_x_doc: f64,
    target_scroll_y_doc: f64,
    camera_serial: u64,
    presented_camera_serial: u64,
    callback_scheduled: u64,
    callback_executed: u64,
    callback_encoded: u64,
    callback_superseded: u64,
    callback_failures: u64,
    callback_cpu_time: Duration,
    callback_frame_pool_wait_time: Duration,
    callback_command_encoding_time: Duration,
    callback_frame_pool_grew: bool,
    callback_frame_pool_capacity_bytes: u64,
    callback_target_size: [u32; 2],
    callback_passes_executed: u64,
    callback_coverage_strategy: Option<GpuVectorCoverageStrategy>,
    callback_presentation_quality: Option<GpuPresentationQuality>,
    callback_transient_peak_bytes: u64,
    callback_transient_resident_bytes: u64,
    callback_render_bundle_runs: u64,
    callback_stencil_draws: u64,
    callback_retained_stroke_candidate_references: u64,
    callback_retained_stroke_cull_dispatches: u64,
    callback_retained_stroke_indirect_draws: u64,
    callback_retained_stroke_indirect_runs: u64,
    callback_selected_stroke_lod: u32,
    callback_active_stroke_lod_levels: u64,
    callback_selected_stroke_segments: u64,
    callback_target_visible_stroke_segments: u64,
    callback_visible_stroke_tiles: u64,
    callback_max_visible_stroke_tile_segments: u64,
    pages: Vec<RenderDebugTracePageFrame>,
}

#[derive(Clone, Debug)]
struct RenderDebugTracePageFrame {
    direct_render: Option<GpuDirectRenderSnapshot>,
    page_number: usize,
    backend: &'static str,
    source: Option<String>,
    page_bounds: Option<[f32; 4]>,
    texture_bounds: Option<[f32; 4]>,
    direct_bounds: Option<[f32; 4]>,
    texture_physical_size: Option<[u32; 2]>,
    requested_physical_size: Option<[u32; 2]>,
    vector_lod_requested_scale: Option<String>,
    vector_lod_available_scale: Option<String>,
    vector_lod_ready: Option<bool>,
}

#[derive(Clone, Debug, Default)]
struct RenderDebugTraceObservations {
    unexpected_backend_frames: usize,
    pending_backend_frames: usize,
    failed_backend_frames: usize,
    fallback_frames: usize,
    gpu_failure_frames: usize,
    max_pending_render_count: usize,
    max_surface_render_pending: usize,
    max_refinement_pages: usize,
    zoom_min: Option<f32>,
    zoom_max: Option<f32>,
    target_zoom_min: Option<f32>,
    target_zoom_max: Option<f32>,
    zoom_scroll_started_at: Option<Duration>,
    zoom_scroll_start_y_doc: Option<f64>,
    zoom_scroll_activation_latency: Option<Duration>,
    last_camera: Option<[f64; 3]>,
    consecutive_camera_stalls: usize,
    max_consecutive_camera_stalls: usize,
    out_of_page_geometry_frames: usize,
    scaled_texture_frames: usize,
    vector_lod_underprepared_frames: usize,
    settled_vector_lod_underprepared_frames: usize,
    max_consecutive_vector_lod_underprepared_frames: usize,
    consecutive_vector_lod_underprepared_frames: usize,
    max_vector_lod_scale_ratio: f64,
}

#[derive(Clone, Debug)]
struct RenderDebugTraceReport {
    kind: RenderDebugTraceKind,
    automated: bool,
    planned_interaction_frames: usize,
    completed_interaction_frames: usize,
    planned_settling_frames: usize,
    completed_settling_frames: usize,
    warmup_duration: Duration,
    scroll_pixels_per_frame: f32,
    markers: Vec<RenderDebugTraceMarker>,
    cancelled: bool,
    failure: Option<String>,
    original_position: DocumentPosition,
    final_position: Option<DocumentPosition>,
    observations: RenderDebugTraceObservations,
    frames: Vec<RenderDebugTraceFrame>,
}

pub(super) struct RenderDebugTrace {
    generation: u64,
    kind: RenderDebugTraceKind,
    automated: bool,
    pub(super) phase: RenderDebugTracePhase,
    phase_frame: usize,
    ready_frames: usize,
    started_at: Instant,
    warmup_duration: Duration,
    document: WeakEntity<DocumentView>,
    document_entity_id: u64,
    original_position: DocumentPosition,
    scroll_pixels_per_frame: f32,
    completed_interaction_frames: usize,
    completed_settling_frames: usize,
    markers: Vec<RenderDebugTraceMarker>,
    observations: RenderDebugTraceObservations,
    frames: Vec<RenderDebugTraceFrame>,
}

impl Workspace {
    pub(super) fn toggle_rendering_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rendering_debug.is_some() {
            if let Some(trace) = self.rendering_debug_trace.take() {
                restore_render_debug_trace_position(&trace, cx);
            }
            self.rendering_debug_trace_generation =
                self.rendering_debug_trace_generation.wrapping_add(1);
            self.rendering_debug = None;
            self.rendering_debug_capture = None;
        } else {
            let workspace = cx.weak_entity();
            self.rendering_debug = Some(cx.new(|cx| {
                FpsMonitor::new(window, cx)
                    .capacity(240)
                    .frame_budget(FRAME_BUDGET)
                    .width(px(620.))
                    .footer(move |window, cx| render_footer(workspace.clone(), window, cx))
            }));
        }
        cx.notify();
    }

    pub(super) fn active_render_debug_document(&self, cx: &App) -> Option<Entity<DocumentView>> {
        (self.surface_mode == WorkspaceSurfaceMode::Panes)
            .then(|| self.active_pane.read(cx).active_document_view())
            .flatten()
    }

    pub(super) fn start_render_debug_capture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rendering_debug_capture.is_some() || self.rendering_debug_trace.is_some() {
            return;
        }
        let Some(document) = self.active_render_debug_document(cx) else {
            window.push_notification(
                Notification::warning("Open a document before capturing rendering performance")
                    .title("Rendering capture unavailable")
                    .id::<RenderDebugCaptureNotification>(),
                cx,
            );
            return;
        };
        let document_start = document.read(cx).render_debug_snapshot(cx);
        let started_at = Instant::now();
        self.rendering_debug_capture = Some(RenderDebugCapture {
            collector: FrameTimingCollector::new(),
            automated_timings: Vec::new(),
            owns_frame_trace: false,
            window_id: window.window_handle().window_id(),
            started_at,
            measurement_started_at: Some(started_at),
            captured_at: chrono::Utc::now().to_rfc3339(),
            document_entity_id: document.entity_id().as_u64(),
            document: document.downgrade(),
            document_start,
            render_cache_start: render_cache_debug_snapshot(cx),
            trace: None,
            automated_output: None,
        });
        cx.notify();
    }

    pub(super) fn start_render_debug_trace(
        &mut self,
        kind: RenderDebugTraceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(reason) = self.start_render_debug_trace_inner(kind, None, window, cx) {
            render_debug_capture_warning(window, reason, cx);
        }
    }

    pub fn try_start_automated_render_debug_trace(
        &mut self,
        kind: RenderDebugTraceKind,
        page_number: usize,
        initial_zoom: f32,
        output: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AutomatedRenderTraceStart {
        if self.rendering_debug_capture.is_some() || self.rendering_debug_trace.is_some() {
            return AutomatedRenderTraceStart::Failed(
                "another rendering capture is already active".to_string(),
            );
        }
        let Some(document) = self.active_render_debug_document(cx) else {
            return AutomatedRenderTraceStart::Waiting;
        };
        let page_count = document.read(cx).render_debug_page_count();
        if page_count == 0 {
            return AutomatedRenderTraceStart::Waiting;
        }
        let readiness = document.read(cx).render_debug_snapshot(cx);
        if readiness.pages.is_empty()
            || !readiness.interaction.viewport_width.is_finite()
            || !readiness.interaction.viewport_height.is_finite()
            || readiness.interaction.viewport_width <= 0.0
            || readiness.interaction.viewport_height <= 0.0
        {
            return AutomatedRenderTraceStart::Waiting;
        }
        if !(1..=page_count).contains(&page_number) {
            return AutomatedRenderTraceStart::Failed(format!(
                "render page {page_number} is outside the document's 1..={page_count} range"
            ));
        }
        let positioned = document.update(cx, |document, cx| {
            document.prepare_render_debug_trace_position(page_number, initial_zoom, cx)
        });
        if !positioned {
            return AutomatedRenderTraceStart::Failed(
                "failed to establish the requested render trace position".to_string(),
            );
        }
        match self.start_render_debug_trace_inner(kind, Some(output), window, cx) {
            Ok(()) => AutomatedRenderTraceStart::Started,
            Err(error) => AutomatedRenderTraceStart::Failed(error),
        }
    }

    pub fn take_automated_render_debug_trace_completion(
        &mut self,
    ) -> Option<AutomatedRenderTraceCompletion> {
        self.rendering_debug_trace_completion.take()
    }

    pub fn automated_render_debug_trace_progress(&self) -> Option<AutomatedRenderTraceProgress> {
        self.rendering_debug_trace
            .as_ref()
            .map(|trace| AutomatedRenderTraceProgress {
                phase: trace.phase.name().to_string(),
                phase_frame: trace.phase_frame,
                ready_frames: trace.ready_frames,
                completed_interaction_frames: trace.completed_interaction_frames,
                completed_settling_frames: trace.completed_settling_frames,
            })
    }

    fn start_render_debug_trace_inner(
        &mut self,
        kind: RenderDebugTraceKind,
        automated_output: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.rendering_debug_capture.is_some() || self.rendering_debug_trace.is_some() {
            return Err("another rendering capture is already active".to_string());
        }
        let Some(document) = self.active_render_debug_document(cx) else {
            return Err(
                "open a fixed-layout document before starting a scripted trace".to_string(),
            );
        };
        let document_start = document.read(cx).render_debug_snapshot(cx);
        let original_position = document.read(cx).current_document_position();
        if let Some(reason) =
            render_debug_trace_precondition(kind, &document_start, &original_position)
        {
            return Err(reason);
        }

        let scroll_pixels_per_frame = if matches!(
            kind,
            RenderDebugTraceKind::Scroll | RenderDebugTraceKind::ZoomScroll
        ) {
            let interaction = &document_start.interaction;
            let scale = interaction.effective_scale.max(0.001);
            let room_before = interaction.target_scroll_y_doc.max(0.0) * f64::from(scale);
            let room_after = (interaction.max_scroll_y_doc - interaction.target_scroll_y_doc)
                .max(0.0)
                * f64::from(scale);
            let room = room_before.max(room_after);
            let step = ((room - 0.5).max(0.0) / kind.frames_per_direction() as f64)
                .floor()
                .clamp(
                    f64::from(TRACE_SCROLL_MIN_PIXELS_PER_FRAME),
                    f64::from(TRACE_SCROLL_MAX_PIXELS_PER_FRAME),
                ) as f32;
            step.copysign((room_after - room_before) as f32)
        } else {
            0.0
        };
        let started_at = Instant::now();
        let automated = automated_output.is_some();
        if automated {
            document.update(cx, |document, _| {
                document.set_render_debug_fixed_frame_driver(true)
            });
        }
        let owns_frame_trace = automated && gpui::set_trace_enabled(true);
        self.rendering_debug_trace_generation =
            self.rendering_debug_trace_generation.wrapping_add(1);
        self.rendering_debug_trace_completion = None;
        let generation = self.rendering_debug_trace_generation;
        self.rendering_debug_capture = Some(RenderDebugCapture {
            collector: FrameTimingCollector::new(),
            automated_timings: Vec::new(),
            owns_frame_trace,
            window_id: window.window_handle().window_id(),
            started_at,
            measurement_started_at: None,
            captured_at: chrono::Utc::now().to_rfc3339(),
            document_entity_id: document.entity_id().as_u64(),
            document: document.downgrade(),
            document_start,
            render_cache_start: render_cache_debug_snapshot(cx),
            trace: None,
            automated_output,
        });
        self.rendering_debug_trace = Some(RenderDebugTrace {
            generation,
            kind,
            automated,
            phase: RenderDebugTracePhase::Warmup,
            phase_frame: 0,
            ready_frames: 0,
            started_at,
            warmup_duration: Duration::ZERO,
            document: document.downgrade(),
            document_entity_id: document.entity_id().as_u64(),
            original_position,
            scroll_pixels_per_frame,
            completed_interaction_frames: 0,
            completed_settling_frames: 0,
            markers: vec![RenderDebugTraceMarker {
                phase: RenderDebugTracePhase::Warmup.name(),
                frame_index: 0,
                offset: Duration::ZERO,
            }],
            observations: RenderDebugTraceObservations::default(),
            frames: Vec::with_capacity(kind.planned_interaction_frames() + TRACE_SETTLING_FRAMES),
        });
        if !automated {
            schedule_render_debug_trace_frame(cx.weak_entity(), generation, window);
        }
        cx.notify();
        Ok(())
    }

    /// Advances a CLI-driven trace independently of compositor frame callbacks.
    ///
    /// Manual traces intentionally remain coupled to `on_next_frame`, while the
    /// Reader benchmark supplies a fixed-deadline draw loop so an obscured or
    /// unfocused Wayland window is not measured at the compositor's idle rate.
    pub fn step_automated_render_debug_trace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = self
            .rendering_debug_trace
            .as_ref()
            .filter(|trace| trace.automated)
            .map(|trace| trace.generation)
        else {
            return;
        };
        self.step_render_debug_trace(generation, window, cx);
    }

    /// Retains the synchronous draw and presentation owned by one automated-driver tick.
    pub fn collect_automated_render_debug_frame(&mut self, draw_started_at: Instant) {
        // Slow reference renders may drain after the fixed comparison sample is complete.
        if self.rendering_debug_trace.as_ref().is_some_and(|trace| {
            trace.phase == RenderDebugTracePhase::Settle
                && trace.completed_settling_frames >= TRACE_SETTLING_FRAMES
        }) {
            return;
        }
        let Some(capture) = self
            .rendering_debug_capture
            .as_mut()
            .filter(|capture| capture.automated_output.is_some())
        else {
            return;
        };
        let events = capture.collector.collect_unseen();
        let Some(measurement_started_at) = capture.measurement_started_at else {
            return;
        };
        if let Some(timing) = latest_owned_frame(
            &events,
            capture.window_id,
            draw_started_at.max(measurement_started_at),
        ) {
            capture.automated_timings.push(timing);
        }
    }

    fn step_render_debug_trace(
        &mut self,
        generation: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut trace) = self.rendering_debug_trace.take() else {
            return;
        };
        if trace.generation != generation || self.rendering_debug_trace_generation != generation {
            self.rendering_debug_trace = Some(trace);
            return;
        }
        let active_document_id = self
            .active_render_debug_document(cx)
            .map(|document| document.entity_id().as_u64());
        if active_document_id != Some(trace.document_entity_id) {
            self.complete_render_debug_trace(
                trace,
                true,
                Some("the active document changed during the trace".to_string()),
                window,
                cx,
            );
            return;
        }
        let Some(document) = trace.document.upgrade() else {
            self.complete_render_debug_trace(
                trace,
                true,
                Some("the traced document was closed".to_string()),
                window,
                cx,
            );
            return;
        };
        let snapshot = document.read(cx).render_debug_snapshot(cx);
        let elapsed = Instant::now().saturating_duration_since(trace.started_at);

        match trace.phase {
            RenderDebugTracePhase::Warmup => {
                if render_debug_trace_ready(&snapshot) {
                    trace.ready_frames += 1;
                } else {
                    trace.ready_frames = 0;
                }
                if trace.ready_frames >= TRACE_WARMUP_READY_FRAMES {
                    trace.warmup_duration = elapsed;
                    trace.phase = RenderDebugTracePhase::Forward;
                    trace.phase_frame = 0;
                    trace.markers.push(RenderDebugTraceMarker {
                        phase: RenderDebugTracePhase::Forward.name(),
                        frame_index: 0,
                        offset: elapsed,
                    });
                } else if elapsed >= TRACE_WARMUP_TIMEOUT {
                    self.complete_render_debug_trace(
                        trace,
                        false,
                        Some(format!(
                            "warmup did not reach five consecutive ready frames within {} seconds",
                            TRACE_WARMUP_TIMEOUT.as_secs()
                        )),
                        window,
                        cx,
                    );
                    return;
                }
            }
            RenderDebugTracePhase::Forward | RenderDebugTracePhase::Reverse => {
                if trace.completed_interaction_frames == 0
                    && let Some(capture) = self.rendering_debug_capture.as_mut()
                {
                    capture.measurement_started_at = Some(Instant::now());
                    capture.document_start = snapshot.clone();
                    capture.render_cache_start = render_cache_debug_snapshot(cx);
                }
                capture_render_debug_trace_frame(&mut trace, &snapshot, elapsed);
                observe_render_debug_trace(&mut trace.observations, &snapshot, false);
                let forward = trace.phase == RenderDebugTracePhase::Forward;
                if trace.kind == RenderDebugTraceKind::ZoomScroll && !forward {
                    if trace.observations.zoom_scroll_started_at.is_none() {
                        trace.observations.zoom_scroll_started_at = Some(elapsed);
                        trace.observations.zoom_scroll_start_y_doc =
                            Some(snapshot.interaction.scroll_y_doc);
                    } else if trace.observations.zoom_scroll_activation_latency.is_none()
                        && trace
                            .observations
                            .zoom_scroll_start_y_doc
                            .is_some_and(|start| {
                                (snapshot.interaction.scroll_y_doc - start).abs() > f64::EPSILON
                            })
                    {
                        trace.observations.zoom_scroll_activation_latency = trace
                            .observations
                            .zoom_scroll_started_at
                            .map(|started| elapsed.saturating_sub(started));
                    }
                }
                let input_applied = match trace.kind {
                    RenderDebugTraceKind::Scroll => {
                        let y_pixels = if forward {
                            trace.scroll_pixels_per_frame
                        } else {
                            -trace.scroll_pixels_per_frame
                        };
                        document.update(cx, |document, cx| {
                            document.apply_synchronized_scroll_delta(
                                SynchronizedScrollDelta {
                                    x_pixels: 0.0,
                                    y_pixels,
                                    mode: SynchronizedScrollMode::Target,
                                },
                                cx,
                            );
                        });
                        true
                    }
                    RenderDebugTraceKind::Zoom => {
                        let factor = if forward {
                            TRACE_ZOOM_FACTOR_PER_FRAME
                        } else {
                            TRACE_ZOOM_FACTOR_PER_FRAME.recip()
                        };
                        document.update(cx, |document, cx| {
                            document.apply_render_debug_zoom_factor(factor, cx)
                        })
                    }
                    RenderDebugTraceKind::ZoomScroll if forward => {
                        document.update(cx, |document, cx| {
                            document.apply_render_debug_zoom_factor(TRACE_ZOOM_FACTOR_PER_FRAME, cx)
                        })
                    }
                    RenderDebugTraceKind::ZoomScroll => {
                        document.update(cx, |document, cx| {
                            document.apply_synchronized_scroll_delta(
                                SynchronizedScrollDelta {
                                    x_pixels: 0.0,
                                    y_pixels: trace.scroll_pixels_per_frame,
                                    mode: SynchronizedScrollMode::Target,
                                },
                                cx,
                            );
                        });
                        true
                    }
                };
                if !input_applied {
                    self.complete_render_debug_trace(
                        trace,
                        false,
                        Some("the scripted input reached a document limit".to_string()),
                        window,
                        cx,
                    );
                    return;
                }
                trace.phase_frame += 1;
                trace.completed_interaction_frames += 1;
                if trace.phase_frame == trace.kind.frames_per_direction() {
                    trace.phase_frame = 0;
                    trace.phase = if forward {
                        RenderDebugTracePhase::Reverse
                    } else {
                        let position = trace.original_position.clone();
                        document.update(cx, |document, cx| {
                            document.restore_render_debug_position(position, cx);
                        });
                        RenderDebugTracePhase::Settle
                    };
                    trace.markers.push(RenderDebugTraceMarker {
                        phase: trace.phase.name(),
                        frame_index: trace.completed_interaction_frames,
                        offset: elapsed,
                    });
                }
            }
            RenderDebugTracePhase::Settle => {
                if trace.completed_settling_frames < TRACE_SETTLING_FRAMES {
                    capture_render_debug_trace_frame(&mut trace, &snapshot, elapsed);
                    observe_render_debug_trace(&mut trace.observations, &snapshot, true);
                    trace.completed_settling_frames += 1;
                }
                trace.phase_frame += 1;
                let settle_started_at = trace
                    .markers
                    .last()
                    .filter(|marker| marker.phase == RenderDebugTracePhase::Settle.name())
                    .map_or(elapsed, |marker| marker.offset);
                let settle_elapsed = elapsed.saturating_sub(settle_started_at);
                let ready = render_debug_trace_ready(&snapshot);
                if render_debug_trace_should_finish_settling(
                    trace.completed_settling_frames,
                    ready,
                    settle_elapsed,
                ) {
                    let failure = if ready {
                        render_debug_trace_settle_failure(&trace, &snapshot)
                    } else {
                        Some(format!(
                            "rendering did not settle within {} seconds after the interaction",
                            TRACE_SETTLING_TIMEOUT.as_secs()
                        ))
                    };
                    trace.markers.push(RenderDebugTraceMarker {
                        phase: "complete",
                        frame_index: trace.completed_interaction_frames
                            + trace.completed_settling_frames,
                        offset: elapsed,
                    });
                    self.complete_render_debug_trace(trace, false, failure, window, cx);
                    return;
                }
            }
        }

        if trace.automated {
            document.update(cx, |document, _| document.arm_render_debug_fixed_frame());
        } else {
            schedule_render_debug_trace_frame(cx.weak_entity(), generation, window);
        }
        self.rendering_debug_trace = Some(trace);
        cx.notify();
    }

    fn complete_render_debug_trace(
        &mut self,
        trace: RenderDebugTrace,
        cancelled: bool,
        failure: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rendering_debug_trace_generation =
            self.rendering_debug_trace_generation.wrapping_add(1);
        if trace.automated
            && let Some(document) = trace.document.upgrade()
        {
            document.update(cx, |document, _| {
                document.set_render_debug_fixed_frame_driver(false)
            });
        }
        let final_position = trace
            .document
            .upgrade()
            .map(|document| document.read(cx).current_document_position());
        let warmup_duration = if trace.warmup_duration.is_zero() {
            Instant::now().saturating_duration_since(trace.started_at)
        } else {
            trace.warmup_duration
        };
        if let Some(capture) = self.rendering_debug_capture.as_mut() {
            if capture.measurement_started_at.is_none() {
                capture.measurement_started_at = Some(Instant::now());
            }
            capture.trace = Some(RenderDebugTraceReport {
                kind: trace.kind,
                automated: trace.automated,
                planned_interaction_frames: trace.kind.planned_interaction_frames(),
                completed_interaction_frames: trace.completed_interaction_frames,
                planned_settling_frames: TRACE_SETTLING_FRAMES,
                completed_settling_frames: trace.completed_settling_frames,
                warmup_duration,
                scroll_pixels_per_frame: trace.scroll_pixels_per_frame.abs(),
                markers: trace.markers.clone(),
                cancelled,
                failure,
                original_position: trace.original_position.clone(),
                final_position,
                observations: trace.observations.clone(),
                frames: trace.frames.clone(),
            });
        }
        self.finish_render_debug_capture_inner(window, cx);
        restore_render_debug_trace_position(&trace, cx);
        cx.notify();
    }

    pub(super) fn finish_render_debug_capture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(trace) = self.rendering_debug_trace.take() {
            self.complete_render_debug_trace(
                trace,
                true,
                Some("cancelled from the rendering debug panel".to_string()),
                window,
                cx,
            );
            return;
        }
        self.finish_render_debug_capture_inner(window, cx);
    }

    pub fn cancel_automated_render_debug_trace(
        &mut self,
        reason: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(trace) = self.rendering_debug_trace.take() {
            self.complete_render_debug_trace(trace, true, Some(reason), window, cx);
        }
    }

    fn finish_render_debug_capture_inner(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut capture) = self.rendering_debug_capture.take() else {
            return;
        };
        let automated_output = capture.automated_output.take();
        let measurement_started_at = capture.measurement_started_at;
        let timings = if automated_output.is_some() {
            std::mem::take(&mut capture.automated_timings)
        } else {
            measurement_started_at.map_or_else(Vec::new, |started_at| {
                captured_frame_timings(
                    &capture.collector.collect_unseen(),
                    capture.window_id,
                    started_at,
                )
            })
        };
        let frame_count = timings.len();
        let report = capture_report(&capture, timings, self, window, cx);
        match serde_json::to_string_pretty(&report) {
            Ok(serialized) => {
                if let Some(output) = automated_output {
                    let trace_failure = report["trace"]["failure"].as_str().map(str::to_string);
                    let saved = persist_render_debug_report_explicit(&serialized, &output);
                    let failure = match saved {
                        Ok(()) if frame_count == 0 => {
                            Some("the rendering trace produced no drawn frame samples".to_string())
                        }
                        Ok(()) => trace_failure,
                        Err(error) => Some(format!(
                            "failed to save automated rendering report {}: {error}",
                            output.display()
                        )),
                    };
                    self.rendering_debug_trace_completion =
                        Some(AutomatedRenderTraceCompletion { output, failure });
                    cx.notify();
                    return;
                }
                cx.write_to_clipboard(ClipboardItem::new_string(serialized.clone()));
                #[cfg(not(test))]
                let saved = persist_render_debug_report(&serialized, &report);
                #[cfg(test)]
                let saved: io::Result<Option<PathBuf>> = Ok(None);
                let trace_failure = report["trace"]["failure"].as_str();
                let notification = if let Some(failure) = trace_failure {
                    Notification::warning(format!(
                        "The rendering trace finished with a failure: {failure}"
                    ))
                } else if frame_count == 0 {
                    Notification::warning(
                        "The rendering report was copied, but it contains no drawn frames",
                    )
                } else {
                    match &saved {
                        Ok(Some(path)) => Notification::success(format!(
                            "Captured {frame_count} frames; copied the report and saved {}",
                            path.display()
                        )),
                        Ok(None) => Notification::success(format!(
                            "Copied a rendering report with {frame_count} frame samples"
                        )),
                        Err(error) => Notification::warning(format!(
                            "Copied {frame_count} frame samples, but could not save the report: {error}"
                        )),
                    }
                };
                window.push_notification(
                    notification
                        .title("Rendering capture finished")
                        .id::<RenderDebugCaptureNotification>(),
                    cx,
                );
            }
            Err(error) => {
                if let Some(output) = automated_output {
                    self.rendering_debug_trace_completion = Some(AutomatedRenderTraceCompletion {
                        output,
                        failure: Some(format!(
                            "failed to serialize automated rendering report: {error}"
                        )),
                    });
                } else {
                    window.push_notification(
                        Notification::error(format!(
                            "Failed to serialize rendering capture: {error}"
                        ))
                        .title("Rendering capture failed")
                        .id::<RenderDebugCaptureNotification>(),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }
}

fn render_debug_trace_should_finish_settling(
    sampled_frames: usize,
    ready: bool,
    elapsed: Duration,
) -> bool {
    sampled_frames >= TRACE_SETTLING_FRAMES && (ready || elapsed >= TRACE_SETTLING_TIMEOUT)
}

fn required_preparation_count(active: usize, queued: usize, speculative: usize) -> usize {
    active.saturating_add(queued).saturating_sub(speculative)
}

fn render_debug_capture_warning(window: &mut Window, message: impl Into<String>, cx: &mut App) {
    window.push_notification(
        Notification::warning(message.into())
            .title("Rendering trace unavailable")
            .id::<RenderDebugCaptureNotification>(),
        cx,
    );
}

fn render_debug_trace_precondition(
    kind: RenderDebugTraceKind,
    snapshot: &DocumentRenderDebugSnapshot,
    position: &DocumentPosition,
) -> Option<String> {
    if position.is_reflow() {
        return Some("scripted traces require a fixed-layout document".to_string());
    }
    let interaction = &snapshot.interaction;
    if interaction.grid_overview_open {
        return Some("close the page grid before starting a scripted trace".to_string());
    }
    if snapshot.pages.is_empty() {
        return Some("no visible page is available for a scripted trace".to_string());
    }
    if !matches!(
        interaction.configured_engine.as_str(),
        "mupdf" | "glyphst" | "glyphst-first"
    ) {
        return Some(format!(
            "unsupported rendering engine '{}'",
            interaction.configured_engine
        ));
    }
    if !interaction.viewport_width.is_finite()
        || !interaction.viewport_height.is_finite()
        || interaction.viewport_width <= 0.0
        || interaction.viewport_height <= 0.0
    {
        return Some("the document viewport has no measurable extent".to_string());
    }
    if matches!(
        kind,
        RenderDebugTraceKind::Scroll | RenderDebugTraceKind::ZoomScroll
    ) {
        let scale = interaction.effective_scale.max(0.001);
        let before = interaction.target_scroll_y_doc.max(0.0) * f64::from(scale);
        let after = (interaction.max_scroll_y_doc - interaction.target_scroll_y_doc).max(0.0)
            * f64::from(scale);
        let required =
            kind.frames_per_direction() as f64 * f64::from(TRACE_SCROLL_MIN_PIXELS_PER_FRAME);
        if before.max(after) + 0.5 < required {
            return Some(format!(
                "the current position needs {required:.0} physical pixels of scroll room in one direction"
            ));
        }
    }
    None
}

fn render_debug_trace_ready(snapshot: &DocumentRenderDebugSnapshot) -> bool {
    let interaction = &snapshot.interaction;
    let required_preparations = required_preparation_count(
        snapshot.gpu_cache.active_preparations,
        snapshot.gpu_cache.queued_preparations,
        snapshot.gpu_cache.speculative_preparations,
    );
    if interaction.grid_overview_open
        || interaction.motion_active
        || snapshot.pages.is_empty()
        || snapshot.pending_render_count != 0
        || snapshot.gpu_cache.surface_seed_tasks != 0
        || snapshot.gpu_cache.surface_render_pending != 0
        || snapshot.gpu_cache.refinement_pages != 0
        || required_preparations != 0
    {
        return false;
    }

    if interaction.configured_engine == "mupdf" {
        return snapshot.pages.iter().all(|page| {
            page.backend == PageRenderBackendDebug::MuPdf
                && page.status == PageRenderDebugStatus::Exact
        });
    }

    let all_exact = snapshot
        .pages
        .iter()
        .all(|page| page.status == PageRenderDebugStatus::Exact);
    let all_stable = snapshot.pages.iter().all(|page| {
        page.backend == PageRenderBackendDebug::GlyphstGpu
            && page.gpu_failure.is_none()
            && page.gpu_fallbacks.is_empty()
            && matches!(
                page.status,
                PageRenderDebugStatus::Exact
                    | PageRenderDebugStatus::RetainedGpu {
                        exact_pending: false
                    }
            )
    });
    all_stable && (all_exact || snapshot.gpu_cache.continuity_surface_count >= snapshot.pages.len())
}

fn capture_render_debug_trace_frame(
    trace: &mut RenderDebugTrace,
    snapshot: &DocumentRenderDebugSnapshot,
    offset: Duration,
) {
    let gpu = &snapshot.gpu_cache;
    trace.frames.push(RenderDebugTraceFrame {
        phase: trace.phase.name(),
        frame_index: trace.completed_interaction_frames + trace.completed_settling_frames,
        offset,
        zoom: snapshot.interaction.zoom,
        target_zoom: snapshot.interaction.target_zoom,
        scroll_x_doc: snapshot.interaction.scroll_x_doc,
        scroll_y_doc: snapshot.interaction.scroll_y_doc,
        target_scroll_x_doc: snapshot.interaction.target_scroll_x_doc,
        target_scroll_y_doc: snapshot.interaction.target_scroll_y_doc,
        camera_serial: gpu.motion_render_camera_serial,
        presented_camera_serial: gpu.motion_render_presented_camera_serial,
        callback_scheduled: gpu.motion_render_callback_scheduled,
        callback_executed: gpu.motion_render_callback_executed,
        callback_encoded: gpu.motion_render_callback_encoded,
        callback_superseded: gpu.motion_render_callback_superseded,
        callback_failures: gpu.motion_render_callback_failures,
        callback_cpu_time: gpu.motion_render_callback_cpu_time,
        callback_frame_pool_wait_time: gpu.motion_render_callback_frame_pool_wait_time,
        callback_command_encoding_time: gpu.motion_render_callback_command_encoding_time,
        callback_frame_pool_grew: gpu.motion_render_callback_frame_pool_grew,
        callback_frame_pool_capacity_bytes: gpu.motion_render_callback_frame_pool_capacity_bytes,
        callback_target_size: gpu.motion_render_callback_target_size,
        callback_passes_executed: gpu.motion_render_callback_passes_executed,
        callback_coverage_strategy: gpu.motion_render_callback_coverage_strategy,
        callback_presentation_quality: gpu.motion_render_callback_presentation_quality,
        callback_transient_peak_bytes: gpu.motion_render_callback_transient_peak_bytes,
        callback_transient_resident_bytes: gpu.motion_render_callback_transient_resident_bytes,
        callback_render_bundle_runs: gpu.motion_render_callback_render_bundle_runs,
        callback_stencil_draws: gpu.motion_render_callback_stencil_draws,
        callback_retained_stroke_candidate_references: gpu
            .motion_render_callback_retained_stroke_candidate_references,
        callback_retained_stroke_cull_dispatches: gpu
            .motion_render_callback_retained_stroke_cull_dispatches,
        callback_retained_stroke_indirect_draws: gpu
            .motion_render_callback_retained_stroke_indirect_draws,
        callback_retained_stroke_indirect_runs: gpu
            .motion_render_callback_retained_stroke_indirect_runs,
        callback_selected_stroke_lod: gpu.motion_render_callback_selected_stroke_lod,
        callback_active_stroke_lod_levels: gpu.motion_render_callback_active_stroke_lod_levels,
        callback_selected_stroke_segments: gpu.motion_render_callback_selected_stroke_segments,
        callback_target_visible_stroke_segments: gpu
            .motion_render_callback_target_visible_stroke_segments,
        callback_visible_stroke_tiles: gpu.motion_render_callback_visible_stroke_tiles,
        callback_max_visible_stroke_tile_segments: gpu
            .motion_render_callback_max_visible_stroke_tile_segments,
        pages: snapshot
            .pages
            .iter()
            .map(|page| RenderDebugTracePageFrame {
                direct_render: page.direct_render,
                page_number: page.page_number,
                backend: page_backend_name(page.backend),
                source: page
                    .gpu_presentation
                    .as_ref()
                    .map(|presentation| presentation.source.clone()),
                page_bounds: page
                    .gpu_presentation
                    .as_ref()
                    .map(|presentation| presentation.page_bounds),
                texture_bounds: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.texture_bounds),
                direct_bounds: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.direct_bounds),
                texture_physical_size: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.texture_physical_size),
                requested_physical_size: page
                    .requested_raster
                    .map(|raster| [raster.physical_width, raster.physical_height]),
                vector_lod_requested_scale: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.vector_lod_requested_scale)
                    .map(|scale| format!("{scale:.3}"))
                    .or_else(|| {
                        page.gpu
                            .as_ref()
                            .map(|gpu| gpu.vector_lod_requested_scale.clone())
                    }),
                vector_lod_available_scale: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.vector_lod_available_scale)
                    .map(|scale| format!("{scale:.3}"))
                    .or_else(|| {
                        page.gpu
                            .as_ref()
                            .map(|gpu| gpu.vector_lod_available_scale.clone())
                    }),
                vector_lod_ready: page
                    .gpu_presentation
                    .as_ref()
                    .and_then(|presentation| presentation.vector_lod_ready)
                    .or_else(|| page.gpu.as_ref().map(|gpu| !gpu.vector_lod_deferred)),
            })
            .collect(),
    });
}

fn debug_bounds_contain(outer: [f32; 4], inner: [f32; 4]) -> bool {
    outer.into_iter().chain(inner).all(f32::is_finite)
        && inner[0] >= outer[0] - 1.0
        && inner[1] >= outer[1] - 1.0
        && inner[2] <= outer[2] + 1.0
        && inner[3] <= outer[3] + 1.0
        && inner[2] >= inner[0]
        && inner[3] >= inner[1]
}

fn observe_render_debug_trace(
    observations: &mut RenderDebugTraceObservations,
    snapshot: &DocumentRenderDebugSnapshot,
    settling: bool,
) {
    let native = snapshot.interaction.configured_engine != "mupdf";
    let unexpected_backend = snapshot.pages.iter().any(|page| {
        if native {
            page.backend != PageRenderBackendDebug::GlyphstGpu
        } else {
            page.backend != PageRenderBackendDebug::MuPdf
        }
    });
    observations.unexpected_backend_frames += usize::from(unexpected_backend);
    observations.pending_backend_frames += usize::from(snapshot.pages.iter().any(|page| {
        matches!(
            page.backend,
            PageRenderBackendDebug::GlyphstPending | PageRenderBackendDebug::MuPdfWhileGpuPending
        ) || matches!(page.status, PageRenderDebugStatus::Pending)
    }));
    observations.failed_backend_frames += usize::from(snapshot.pages.iter().any(|page| {
        page.backend == PageRenderBackendDebug::GlyphstFailed
            || page.status == PageRenderDebugStatus::Failed
    }));
    observations.fallback_frames += usize::from(
        snapshot.pages.iter().any(|page| {
            page.backend == PageRenderBackendDebug::MuPdfFallback
                || matches!(page.status, PageRenderDebugStatus::Fallback { .. })
                || !page.gpu_fallbacks.is_empty()
        }) || !snapshot.gpu_navigation_fallbacks.is_empty(),
    );
    observations.gpu_failure_frames +=
        usize::from(snapshot.pages.iter().any(|page| page.gpu_failure.is_some()));
    observations.max_pending_render_count = observations
        .max_pending_render_count
        .max(snapshot.pending_render_count);
    observations.max_surface_render_pending = observations
        .max_surface_render_pending
        .max(snapshot.gpu_cache.surface_render_pending);
    observations.max_refinement_pages = observations
        .max_refinement_pages
        .max(snapshot.gpu_cache.refinement_pages);
    observe_finite_range(
        &mut observations.zoom_min,
        &mut observations.zoom_max,
        snapshot.interaction.zoom,
    );
    observe_finite_range(
        &mut observations.target_zoom_min,
        &mut observations.target_zoom_max,
        snapshot.interaction.target_zoom,
    );
    let camera = [
        f64::from(snapshot.interaction.zoom),
        snapshot.interaction.scroll_x_doc,
        snapshot.interaction.scroll_y_doc,
    ];
    let camera_pending = (snapshot.interaction.zoom - snapshot.interaction.target_zoom).abs()
        > f32::EPSILON
        || (snapshot.interaction.scroll_x_doc - snapshot.interaction.target_scroll_x_doc).abs()
            > f64::EPSILON
        || (snapshot.interaction.scroll_y_doc - snapshot.interaction.target_scroll_y_doc).abs()
            > f64::EPSILON;
    let stalled = camera_pending && observations.last_camera == Some(camera);
    observations.consecutive_camera_stalls = if stalled {
        observations.consecutive_camera_stalls.saturating_add(1)
    } else {
        0
    };
    observations.max_consecutive_camera_stalls = observations
        .max_consecutive_camera_stalls
        .max(observations.consecutive_camera_stalls);
    observations.last_camera = Some(camera);

    let out_of_page = snapshot.pages.iter().any(|page| {
        page.gpu_presentation.as_ref().is_some_and(|presentation| {
            presentation
                .texture_bounds
                .is_some_and(|bounds| !debug_bounds_contain(presentation.page_bounds, bounds))
                || presentation
                    .direct_bounds
                    .is_some_and(|bounds| !debug_bounds_contain(presentation.page_bounds, bounds))
        })
    });
    observations.out_of_page_geometry_frames += usize::from(out_of_page);
    let scaled_texture = snapshot.pages.iter().any(|page| {
        let Some(presentation) = page.gpu_presentation.as_ref() else {
            return false;
        };
        let Some(texture_size) = presentation.texture_physical_size else {
            return false;
        };
        page.requested_raster.is_some_and(|requested| {
            texture_size != [requested.physical_width, requested.physical_height]
        })
    });
    observations.scaled_texture_frames += usize::from(scaled_texture);

    let mut vector_lod_underprepared = false;
    for page in &snapshot.pages {
        let Some(presentation) = page.gpu_presentation.as_ref() else {
            continue;
        };
        if presentation.vector_lod_ready == Some(false) {
            vector_lod_underprepared = true;
        }
        if let (Some(requested), Some(available)) = (
            presentation.vector_lod_requested_scale,
            presentation.vector_lod_available_scale,
        ) && requested.is_finite()
            && available.is_finite()
            && available > 0.0
        {
            observations.max_vector_lod_scale_ratio = observations
                .max_vector_lod_scale_ratio
                .max(requested / f64::from(available));
        }
    }
    observations.vector_lod_underprepared_frames += usize::from(vector_lod_underprepared);
    observations.settled_vector_lod_underprepared_frames +=
        usize::from(settling && vector_lod_underprepared);
    observations.consecutive_vector_lod_underprepared_frames = if vector_lod_underprepared {
        observations
            .consecutive_vector_lod_underprepared_frames
            .saturating_add(1)
    } else {
        0
    };
    observations.max_consecutive_vector_lod_underprepared_frames = observations
        .max_consecutive_vector_lod_underprepared_frames
        .max(observations.consecutive_vector_lod_underprepared_frames);
}

fn observe_finite_range(minimum: &mut Option<f32>, maximum: &mut Option<f32>, value: f32) {
    if !value.is_finite() {
        return;
    }
    *minimum = Some(minimum.map_or(value, |current| current.min(value)));
    *maximum = Some(maximum.map_or(value, |current| current.max(value)));
}

fn render_debug_trace_settle_failure(
    trace: &RenderDebugTrace,
    snapshot: &DocumentRenderDebugSnapshot,
) -> Option<String> {
    let required_preparations = required_preparation_count(
        snapshot.gpu_cache.active_preparations,
        snapshot.gpu_cache.queued_preparations,
        snapshot.gpu_cache.speculative_preparations,
    );
    if trace.completed_interaction_frames != trace.kind.planned_interaction_frames() {
        return Some("the scripted interaction did not complete every planned frame".to_string());
    }
    if snapshot.interaction.motion_active
        || snapshot.pending_render_count != 0
        || snapshot.gpu_cache.surface_seed_tasks != 0
        || snapshot.gpu_cache.surface_render_pending != 0
        || snapshot.gpu_cache.refinement_pages != 0
        || required_preparations != 0
    {
        return Some(format!(
            "rendering did not settle within {TRACE_SETTLING_FRAMES} frames"
        ));
    }
    if trace.observations.failed_backend_frames != 0 || trace.observations.gpu_failure_frames != 0 {
        return Some("a renderer failure was observed during the trace".to_string());
    }
    let native = snapshot.interaction.configured_engine != "mupdf";
    if native && trace.observations.fallback_frames != 0 {
        return Some("a fallback was observed during the trace".to_string());
    }
    if trace.observations.unexpected_backend_frames != 0 {
        return Some("an unexpected page backend was observed during the trace".to_string());
    }
    if trace.observations.max_consecutive_camera_stalls > 2 {
        return Some(format!(
            "the camera stopped for {} consecutive drawn frames while an input target was pending",
            trace.observations.max_consecutive_camera_stalls
        ));
    }
    if trace.observations.out_of_page_geometry_frames != 0 {
        return Some(format!(
            "native GPU paint geometry escaped the current page on {} frames",
            trace.observations.out_of_page_geometry_frames
        ));
    }
    if trace.observations.scaled_texture_frames != 0 {
        return Some(format!(
            "native zoom presented a differently scaled texture on {} frames",
            trace.observations.scaled_texture_frames
        ));
    }
    if trace.observations.settled_vector_lod_underprepared_frames != 0 {
        return Some(format!(
            "native vector LOD remained underprepared on {} settling frames",
            trace.observations.settled_vector_lod_underprepared_frames
        ));
    }
    if trace
        .observations
        .max_consecutive_vector_lod_underprepared_frames
        > TRACE_VECTOR_LOD_MAX_UNDERPREPARED_FRAMES
    {
        return Some(format!(
            "native vector LOD lagged the camera for {} consecutive frames",
            trace
                .observations
                .max_consecutive_vector_lod_underprepared_frames
        ));
    }
    if native && snapshot.gpu_cache.motion_render_callback_scheduled > 0 {
        let finalized = snapshot
            .gpu_cache
            .motion_render_callback_encoded
            .saturating_add(snapshot.gpu_cache.motion_render_callback_superseded)
            .saturating_add(snapshot.gpu_cache.motion_render_callback_failures);
        if finalized != snapshot.gpu_cache.motion_render_callback_scheduled {
            return Some(format!(
                "{} current-camera callbacks were scheduled but only {finalized} reached a final outcome",
                snapshot.gpu_cache.motion_render_callback_scheduled
            ));
        }
        if snapshot.gpu_cache.motion_render_callback_failures != 0 {
            return Some(format!(
                "{} current-camera GPU callbacks failed",
                snapshot.gpu_cache.motion_render_callback_failures
            ));
        }
        if snapshot.gpu_cache.motion_render_callback_encoded == 0 {
            return Some("no scheduled current-camera GPU draw was encoded".to_string());
        }
        if snapshot
            .gpu_cache
            .motion_render_callback_scheduled_camera_serial
            != snapshot
                .gpu_cache
                .motion_render_callback_encoded_camera_serial
        {
            return Some(format!(
                "the latest scheduled camera serial {} was not encoded (latest encoded {})",
                snapshot
                    .gpu_cache
                    .motion_render_callback_scheduled_camera_serial,
                snapshot
                    .gpu_cache
                    .motion_render_callback_encoded_camera_serial
            ));
        }
    }
    if trace.kind == RenderDebugTraceKind::ZoomScroll {
        let Some(latency) = trace.observations.zoom_scroll_activation_latency else {
            return Some("target scrolling never advanced the camera after zoom".to_string());
        };
        if latency > TRACE_SCROLL_ACTIVATION_LIMIT {
            return Some(format!(
                "target scrolling took {:.3} ms to advance the camera after zoom",
                milliseconds(latency)
            ));
        }
    }
    if snapshot.pages.iter().any(|page| {
        page.status != PageRenderDebugStatus::Exact
            || if native {
                page.backend != PageRenderBackendDebug::GlyphstGpu
            } else {
                page.backend != PageRenderBackendDebug::MuPdf
            }
    }) {
        return Some("the visible pages were not exact after settling".to_string());
    }
    None
}

fn restore_render_debug_trace_position(trace: &RenderDebugTrace, cx: &mut Context<Workspace>) {
    if let Some(document) = trace.document.upgrade() {
        let position = trace.original_position.clone();
        document.update(cx, |document, cx| {
            document.restore_render_debug_position(position, cx);
        });
    }
}

fn schedule_render_debug_trace_frame(
    workspace: WeakEntity<Workspace>,
    generation: u64,
    window: &mut Window,
) {
    window.on_next_frame(move |window, cx| {
        if let Some(workspace) = workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.step_render_debug_trace(generation, window, cx);
            });
        }
    });
}

#[cfg(not(test))]
fn persist_render_debug_report(serialized: &str, report: &Value) -> io::Result<Option<PathBuf>> {
    let directory = glyphst_core::paths::state_dir().join("render-traces");
    persist_render_debug_report_to(serialized, report, &directory).map(Some)
}

fn persist_render_debug_report_to(
    serialized: &str,
    report: &Value,
    directory: &Path,
) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let timestamp = sanitize_report_file_component(
        report["captured_at"].as_str().unwrap_or("capture"),
        "capture",
    );
    let source = report["document"]["start"]["source_path"]
        .as_str()
        .and_then(|path| Path::new(path).file_stem())
        .and_then(|stem| stem.to_str())
        .unwrap_or("document");
    let source = sanitize_report_file_component(source, "document");
    let engine = sanitize_report_file_component(
        report["document"]["start"]["configured_engine"]
            .as_str()
            .unwrap_or("engine"),
        "engine",
    );
    let trace = sanitize_report_file_component(
        report["trace"]["kind"].as_str().unwrap_or("manual"),
        "manual",
    );
    let base = format!("{timestamp}-{source}-{engine}-{trace}");
    for suffix in 0..1_000_u16 {
        let suffix = if suffix == 0 {
            String::new()
        } else {
            format!("-{suffix}")
        };
        let path = directory.join(format!("{base}{suffix}.json"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(serialized.as_bytes())?;
                file.write_all(b"\n")?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique rendering report file name",
    ))
}

fn persist_render_debug_report_explicit(serialized: &str, path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "render report output path is empty",
        ));
    }
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("render-trace.json");
    for suffix in 0..1_000_u16 {
        let temporary = parent.join(format!(
            ".{file_name}.partial-{}-{suffix}",
            std::process::id()
        ));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let write_result: io::Result<()> = (|| {
            file.write_all(serialized.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::hard_link(&temporary, path)?;
            Ok(())
        })();
        drop(file);
        let cleanup_result = fs::remove_file(&temporary);
        write_result?;
        cleanup_result?;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a temporary rendering report path",
    ))
}

fn sanitize_report_file_component(value: &str, fallback: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn render_footer(
    workspace: WeakEntity<Workspace>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let cache = render_cache_debug_snapshot(cx);
    let workspace_entity = workspace.upgrade();
    let (active_document, capture_active, trace_status) = workspace_entity
        .as_ref()
        .map(|workspace| {
            let workspace = workspace.read(cx);
            (
                workspace.active_render_debug_document(cx),
                workspace.rendering_debug_capture.is_some(),
                workspace
                    .rendering_debug_trace
                    .as_ref()
                    .map(|trace| format!("{} · {}", trace.kind.name(), trace.phase.name())),
            )
        })
        .unwrap_or((None, false, None));
    let document = active_document.map(|document| document.read(cx).render_debug_snapshot(cx));

    div()
        .mt_1()
        .pt_1()
        .border_t_1()
        .border_color(BORDER)
        .flex()
        .flex_col()
        .child(render_capture_controls(
            workspace,
            capture_active,
            document.is_some(),
            trace_status,
        ))
        .child(section_label("RENDER CACHE"))
        .child(reading(
            "RGBA",
            format!(
                "{} / {}  ×{}",
                format_bytes(cache.raster_bytes),
                format_bytes(cache.raster_byte_limit),
                cache.raster_count
            ),
            FOREGROUND,
        ))
        .child(reading(
            "PROTECTED",
            cache.protected_raster_count.to_string(),
            FOREGROUND,
        ))
        .child(reading(
            "DLISTS",
            format!(
                "{} / {}",
                cache.display_list_count, cache.display_list_limit
            ),
            FOREGROUND,
        ))
        .when_some(document, render_document)
        .into_any_element()
}

fn render_capture_controls(
    workspace: WeakEntity<Workspace>,
    capture_active: bool,
    document_available: bool,
    trace_status: Option<String>,
) -> Div {
    let trace_active = trace_status.is_some();
    let label = if trace_active {
        "Cancel & Copy"
    } else if capture_active {
        "Stop & Copy"
    } else {
        "Start Capture"
    };
    let manual_workspace = workspace.clone();
    let scroll_workspace = workspace.clone();
    let zoom_workspace = workspace;
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_2()
        .pb_1()
        .child(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_color(if capture_active { WARN } else { MUTED })
                        .child(trace_status.unwrap_or_else(|| {
                            if capture_active {
                                "manual · capturing".to_string()
                            } else {
                                "INTERACTION TRACE".to_string()
                            }
                        })),
                )
                .child(
                    Button::new("render-debug-capture")
                        .xsmall()
                        .compact()
                        .map(|button| {
                            if capture_active {
                                button.warning()
                            } else {
                                button.secondary()
                            }
                        })
                        .disabled(!capture_active && !document_available)
                        .label(label)
                        .debug_selector(|| "render-debug-capture".to_string())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let Some(workspace) = manual_workspace.upgrade() else {
                                return;
                            };
                            workspace.update(cx, |workspace, cx| {
                                if capture_active {
                                    workspace.finish_render_debug_capture(window, cx);
                                } else {
                                    workspace.start_render_debug_capture(window, cx);
                                }
                            });
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .w_full()
                .items_center()
                .justify_end()
                .gap_2()
                .child(
                    Button::new("render-debug-scroll-trace")
                        .xsmall()
                        .compact()
                        .secondary()
                        .disabled(capture_active || !document_available)
                        .label("Run Scroll Trace")
                        .debug_selector(|| "render-debug-scroll-trace".to_string())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let Some(workspace) = scroll_workspace.upgrade() else {
                                return;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.start_render_debug_trace(
                                    RenderDebugTraceKind::Scroll,
                                    window,
                                    cx,
                                );
                            });
                        }),
                )
                .child(
                    Button::new("render-debug-zoom-trace")
                        .xsmall()
                        .compact()
                        .secondary()
                        .disabled(capture_active || !document_available)
                        .label("Run Zoom Trace")
                        .debug_selector(|| "render-debug-zoom-trace".to_string())
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let Some(workspace) = zoom_workspace.upgrade() else {
                                return;
                            };
                            workspace.update(cx, |workspace, cx| {
                                workspace.start_render_debug_trace(
                                    RenderDebugTraceKind::Zoom,
                                    window,
                                    cx,
                                );
                            });
                        }),
                ),
        )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CapturedFrameSample {
    offset: Duration,
    interval: Option<Duration>,
    draw: Duration,
    present: Option<Duration>,
    work: Duration,
    dirty_to_draw: Option<Duration>,
    invalidations: u64,
}

#[derive(Clone, Copy, Debug)]
struct CapturedFrameTiming {
    draw: FrameTiming,
    present: Option<PresentTiming>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DurationStatistics {
    count: usize,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CountStatistics {
    count: usize,
    total: u64,
    mean: f64,
    p50: u64,
    p95: u64,
    max: u64,
}

fn captured_frame_timings(
    events: &[FrameEvent],
    window_id: WindowId,
    not_before: Instant,
) -> Vec<CapturedFrameTiming> {
    let mut frames = Vec::new();
    for event in events {
        match event {
            FrameEvent::Draw(draw)
                if draw.window_id == window_id && draw.draw_start >= not_before =>
            {
                frames.push(CapturedFrameTiming {
                    draw: *draw,
                    present: None,
                });
            }
            FrameEvent::Present(present) if present.window_id == window_id => {
                if let Some(frame) = frames.iter_mut().rev().find(|frame| {
                    frame.present.is_none() && frame.draw.draw_end <= present.present_start
                }) {
                    frame.present = Some(*present);
                }
            }
            FrameEvent::Draw(_) | FrameEvent::Present(_) => {}
        }
    }
    frames
}

fn latest_owned_frame(
    events: &[FrameEvent],
    window_id: WindowId,
    not_before: Instant,
) -> Option<CapturedFrameTiming> {
    captured_frame_timings(events, window_id, not_before).pop()
}

fn captured_frame_samples(timings: &[CapturedFrameTiming]) -> Vec<CapturedFrameSample> {
    let Some(first) = timings.first().map(|timing| timing.draw.draw_start) else {
        return Vec::new();
    };
    let mut previous = None;
    timings
        .iter()
        .map(|timing| {
            let interval = previous.map(|previous| timing.draw.draw_start.duration_since(previous));
            previous = Some(timing.draw.draw_start);
            let draw = timing.draw.draw_duration();
            let present = timing.present.map(|present| present.present_duration());
            CapturedFrameSample {
                offset: timing.draw.draw_start.duration_since(first),
                interval,
                draw,
                present,
                work: draw + present.unwrap_or_default(),
                dirty_to_draw: timing.draw.dirty_to_draw_duration(),
                invalidations: timing.draw.invalidations,
            }
        })
        .collect()
}

fn nearest_rank_index(len: usize, percentile: usize) -> usize {
    len.saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1)
        .min(len.saturating_sub(1))
}

fn duration_statistics(values: impl IntoIterator<Item = Duration>) -> Option<DurationStatistics> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    let mean_ms =
        values.iter().map(|value| milliseconds(*value)).sum::<f64>() / values.len() as f64;
    values.sort_unstable();
    Some(DurationStatistics {
        count: values.len(),
        mean_ms,
        p50_ms: milliseconds(values[nearest_rank_index(values.len(), 50)]),
        p95_ms: milliseconds(values[nearest_rank_index(values.len(), 95)]),
        max_ms: milliseconds(*values.last().expect("non-empty duration statistics")),
    })
}

fn count_statistics(values: impl IntoIterator<Item = u64>) -> Option<CountStatistics> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    let total = values.iter().copied().fold(0_u64, u64::saturating_add);
    let mean = total as f64 / values.len() as f64;
    values.sort_unstable();
    Some(CountStatistics {
        count: values.len(),
        total,
        mean,
        p50: values[nearest_rank_index(values.len(), 50)],
        p95: values[nearest_rank_index(values.len(), 95)],
        max: *values.last().expect("non-empty count statistics"),
    })
}

fn duration_statistics_json(statistics: Option<DurationStatistics>) -> Value {
    statistics.map_or(Value::Null, |statistics| {
        json!({
            "count": statistics.count,
            "mean_ms": statistics.mean_ms,
            "p50_ms": statistics.p50_ms,
            "p95_ms": statistics.p95_ms,
            "max_ms": statistics.max_ms,
        })
    })
}

fn count_statistics_json(statistics: Option<CountStatistics>) -> Value {
    statistics.map_or(Value::Null, |statistics| {
        json!({
            "count": statistics.count,
            "total": statistics.total,
            "mean": statistics.mean,
            "p50": statistics.p50,
            "p95": statistics.p95,
            "max": statistics.max,
        })
    })
}

fn capture_report(
    capture: &RenderDebugCapture,
    timings: Vec<CapturedFrameTiming>,
    workspace: &Workspace,
    window: &Window,
    cx: &App,
) -> Value {
    let finished_at = Instant::now();
    let capture_duration = finished_at.saturating_duration_since(capture.started_at);
    let measurement_duration = capture
        .measurement_started_at
        .map(|started_at| finished_at.saturating_duration_since(started_at));
    let samples = captured_frame_samples(&timings);
    let document_end = capture
        .document
        .upgrade()
        .map(|document| document.read(cx).render_debug_snapshot(cx));
    let active_document_entity_id = workspace
        .active_render_debug_document(cx)
        .map(|document| document.entity_id().as_u64());
    let render_cache_end = render_cache_debug_snapshot(cx);
    let gpu_specs = window.gpu_specs();
    let trace = capture.trace.as_ref().map(render_debug_trace_report_json);
    let binary_sha256 = std::env::var("GLYPHST_INTERACTION_BUILD_SHA256")
        .ok()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()));

    json!({
        "schema_version": 19,
        "kind": "glyphst-reader-interaction",
        "captured_at": capture.captured_at,
        "capture_duration_ms": milliseconds(capture_duration),
        "measurement_duration_ms": measurement_duration.map(milliseconds),
        "frame_budget_ms": milliseconds(FRAME_BUDGET),
        "trace": trace,
        "application": {
            "package": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "binary_sha256": binary_sha256,
        },
        "window": {
            "id": capture.window_id.as_u64(),
            "gpu": gpu_specs.map(|gpu| json!({
                "software_emulated": gpu.is_software_emulated,
                "device": gpu.device_name,
                "driver": gpu.driver_name,
                "driver_info": gpu.driver_info,
            })),
        },
        "frames": frame_report(&samples),
        "render_cache": {
            "start": render_cache_json(capture.render_cache_start),
            "end": render_cache_json(render_cache_end),
        },
        "document": {
            "entity_id": capture.document_entity_id,
            "active_at_end": active_document_entity_id == Some(capture.document_entity_id),
            "released_before_end": document_end.is_none(),
            "start": document_snapshot_json(&capture.document_start),
            "end": document_end.as_ref().map(document_snapshot_json),
            "gpu_counter_delta": document_end.as_ref().map(|end| {
                gpu_counter_delta_json(&capture.document_start.gpu_cache, &end.gpu_cache)
            }),
        },
    })
}

fn frame_report(samples: &[CapturedFrameSample]) -> Value {
    let intervals = samples.iter().filter_map(|sample| sample.interval);
    let draws = samples.iter().map(|sample| sample.draw);
    let presents = samples.iter().filter_map(|sample| sample.present);
    let work = samples.iter().map(|sample| sample.work);
    let dirty_to_draw = samples.iter().filter_map(|sample| sample.dirty_to_draw);
    let frame_span = samples
        .last()
        .map(|sample| sample.offset)
        .unwrap_or_default();
    let effective_fps = (samples.len() >= 2 && !frame_span.is_zero())
        .then(|| (samples.len() - 1) as f64 / frame_span.as_secs_f64());
    let draw_over_budget = samples
        .iter()
        .filter(|sample| sample.draw > FRAME_BUDGET)
        .count();
    let work_over_budget = samples
        .iter()
        .filter(|sample| sample.work > FRAME_BUDGET)
        .count();
    let interval_count = samples
        .iter()
        .filter(|sample| sample.interval.is_some())
        .count();
    let interval_over_budget = samples
        .iter()
        .filter(|sample| {
            sample
                .interval
                .is_some_and(|interval| interval > FRAME_BUDGET)
        })
        .count();
    let retained_start = samples.len().saturating_sub(MAX_RAW_FRAME_SAMPLES);
    let raw_samples = samples[retained_start..]
        .iter()
        .map(|sample| {
            json!({
                "offset_ms": milliseconds(sample.offset),
                "interval_ms": sample.interval.map(milliseconds),
                "draw_ms": milliseconds(sample.draw),
                "present_ms": sample.present.map(milliseconds),
                "work_ms": milliseconds(sample.work),
                "dirty_to_draw_ms": sample.dirty_to_draw.map(milliseconds),
                "invalidations": sample.invalidations,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "total_frames": samples.len(),
        "raw_samples_retained": raw_samples.len(),
        "raw_samples_truncated": retained_start > 0,
        "frame_span_ms": milliseconds(frame_span),
        "effective_fps": effective_fps,
        "draw": duration_statistics_json(duration_statistics(draws)),
        "present": duration_statistics_json(duration_statistics(presents)),
        "work": duration_statistics_json(duration_statistics(work)),
        "interval": duration_statistics_json(duration_statistics(intervals)),
        "dirty_to_draw": duration_statistics_json(duration_statistics(dirty_to_draw)),
        "invalidations": count_statistics_json(count_statistics(
            samples.iter().map(|sample| sample.invalidations),
        )),
        "budget": {
            "draw_overruns": draw_over_budget,
            "draw_overrun_percent": percentage(draw_over_budget, samples.len()),
            "work_overruns": work_over_budget,
            "work_overrun_percent": percentage(work_over_budget, samples.len()),
            "interval_overruns": interval_over_budget,
            "interval_overrun_percent": percentage(interval_over_budget, interval_count),
        },
        "samples": raw_samples,
    })
}

fn render_cache_json(cache: RenderCacheDebugSnapshot) -> Value {
    json!({
        "raster_bytes": cache.raster_bytes,
        "raster_count": cache.raster_count,
        "raster_byte_limit": cache.raster_byte_limit,
        "protected_raster_count": cache.protected_raster_count,
        "display_list_count": cache.display_list_count,
        "display_list_limit": cache.display_list_limit,
    })
}

fn document_snapshot_json(document: &DocumentRenderDebugSnapshot) -> Value {
    let interaction = &document.interaction;
    json!({
        "source_path": interaction.source_path.to_string_lossy(),
        "configured_engine": interaction.configured_engine,
        "page_label": document.page_label,
        "view": {
            "document_layout": interaction.document_layout,
            "page_layout": interaction.page_layout,
            "rotation": interaction.rotation,
            "grid_overview_open": interaction.grid_overview_open,
            "viewport_width": finite_f32_json(interaction.viewport_width),
            "viewport_height": finite_f32_json(interaction.viewport_height),
            "zoom": finite_f32_json(interaction.zoom),
            "target_zoom": finite_f32_json(interaction.target_zoom),
            "effective_scale": finite_f32_json(interaction.effective_scale),
            "scroll_x_doc": finite_f64_json(interaction.scroll_x_doc),
            "scroll_y_doc": finite_f64_json(interaction.scroll_y_doc),
            "target_scroll_x_doc": finite_f64_json(interaction.target_scroll_x_doc),
            "target_scroll_y_doc": finite_f64_json(interaction.target_scroll_y_doc),
            "max_scroll_y_doc": finite_f64_json(interaction.max_scroll_y_doc),
            "motion_active": interaction.motion_active,
        },
        "pending_render_count": document.pending_render_count,
        "pending_render_limit": document.pending_render_limit,
        "gpu_cache": gpu_cache_json(&document.gpu_cache),
        "pages": document.pages.iter().map(page_snapshot_json).collect::<Vec<_>>(),
        "navigation_fallbacks": document.gpu_navigation_fallbacks.iter().map(|fallback| json!({
            "feature": fallback.feature,
            "code": fallback.code,
            "message": fallback.message,
            "source": fallback.source,
        })).collect::<Vec<_>>(),
    })
}

fn render_debug_trace_report_json(trace: &RenderDebugTraceReport) -> Value {
    json!({
        "version": TRACE_REPORT_VERSION,
        "kind": trace.kind.name(),
        "driver": if trace.automated { "fixed-deadline-draw" } else { "platform-frame" },
        "sample_scope": if trace.automated { "driver-owned-draws" } else { "all-window-draws" },
        "planned_interaction_frames": trace.planned_interaction_frames,
        "completed_interaction_frames": trace.completed_interaction_frames,
        "planned_settling_frames": trace.planned_settling_frames,
        "completed_settling_frames": trace.completed_settling_frames,
        "warmup_ready_frames": TRACE_WARMUP_READY_FRAMES,
        "warmup_timeout_ms": milliseconds(TRACE_WARMUP_TIMEOUT),
        "warmup_ms": milliseconds(trace.warmup_duration),
        "scroll_pixels_per_frame": finite_f32_json(trace.scroll_pixels_per_frame),
        "phase_markers": trace.markers.iter().map(|marker| json!({
            "phase": marker.phase,
            "frame_index": marker.frame_index,
            "offset_ms": milliseconds(marker.offset),
        })).collect::<Vec<_>>(),
        "cancelled": trace.cancelled,
        "failure": trace.failure,
        "original_position": document_position_json(&trace.original_position),
        "final_position": trace.final_position.as_ref().map(document_position_json),
        "observations": {
            "unexpected_backend_frames": trace.observations.unexpected_backend_frames,
            "pending_backend_frames": trace.observations.pending_backend_frames,
            "failed_backend_frames": trace.observations.failed_backend_frames,
            "fallback_frames": trace.observations.fallback_frames,
            "gpu_failure_frames": trace.observations.gpu_failure_frames,
            "max_pending_render_count": trace.observations.max_pending_render_count,
            "max_surface_render_pending": trace.observations.max_surface_render_pending,
            "max_refinement_pages": trace.observations.max_refinement_pages,
            "zoom_min": trace.observations.zoom_min.map(finite_f32_json),
            "zoom_max": trace.observations.zoom_max.map(finite_f32_json),
            "target_zoom_min": trace.observations.target_zoom_min.map(finite_f32_json),
            "target_zoom_max": trace.observations.target_zoom_max.map(finite_f32_json),
            "zoom_scroll_activation_latency_ms": trace.observations.zoom_scroll_activation_latency.map(milliseconds),
            "max_consecutive_camera_stalls": trace.observations.max_consecutive_camera_stalls,
            "out_of_page_geometry_frames": trace.observations.out_of_page_geometry_frames,
            "scaled_texture_frames": trace.observations.scaled_texture_frames,
            "vector_lod_underprepared_frames": trace.observations.vector_lod_underprepared_frames,
            "settled_vector_lod_underprepared_frames": trace.observations.settled_vector_lod_underprepared_frames,
            "max_consecutive_vector_lod_underprepared_frames": trace.observations.max_consecutive_vector_lod_underprepared_frames,
            "max_vector_lod_scale_ratio": finite_f64_json(trace.observations.max_vector_lod_scale_ratio),
        },
        "frames": trace.frames.iter().map(|frame| json!({
            "phase": frame.phase,
            "frame_index": frame.frame_index,
            "offset_ms": milliseconds(frame.offset),
            "camera": {
                "zoom": finite_f32_json(frame.zoom),
                "target_zoom": finite_f32_json(frame.target_zoom),
                "scroll_x_doc": finite_f64_json(frame.scroll_x_doc),
                "scroll_y_doc": finite_f64_json(frame.scroll_y_doc),
                "target_scroll_x_doc": finite_f64_json(frame.target_scroll_x_doc),
                "target_scroll_y_doc": finite_f64_json(frame.target_scroll_y_doc),
                "serial": frame.camera_serial,
                "presented_serial": frame.presented_camera_serial,
            },
            "direct_callback": render_debug_trace_direct_callback_json(frame),
            "pages": frame.pages.iter().map(|page| json!({
                "page_number": page.page_number,
                "direct_render": page.direct_render.as_ref().map(render_debug_direct_snapshot_json),
                "backend": page.backend,
                "source": page.source,
                "page_bounds": page.page_bounds,
                "texture_bounds": page.texture_bounds,
                "direct_bounds": page.direct_bounds,
                "texture_physical_size": page.texture_physical_size,
                "requested_physical_size": page.requested_physical_size,
                "vector_lod_requested_scale": page.vector_lod_requested_scale,
                "vector_lod_available_scale": page.vector_lod_available_scale,
                "vector_lod_ready": page.vector_lod_ready,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn render_debug_direct_snapshot_json(snapshot: &GpuDirectRenderSnapshot) -> Value {
    let stats = snapshot.stats;
    let region = snapshot.visible_region;
    json!({
        "camera_serial": snapshot.camera_serial,
        "generation": stats.generation,
        "visible_region": [region.x, region.y, region.width, region.height],
        "root_transform": snapshot.root_transform,
        "target_size": snapshot.target_size,
        "target_origin": snapshot.target_origin,
        "presentation_quality": presentation_quality_name(stats.presentation_quality),
        "selected_stroke_lod": stats.selected_stroke_lod,
        "active_stroke_lod_levels": stats.active_stroke_lod_levels,
        "selected_stroke_segments": stats.selected_stroke_segments,
        "target_visible_stroke_segments": stats.target_visible_stroke_segments,
        "visible_stroke_tiles": stats.visible_stroke_tiles,
        "max_visible_stroke_tile_segments": stats.max_visible_stroke_tile_segments,
        "selected_vector_lod": stats.selected_vector_lod,
        "vector_lod_deferred": stats.vector_lod_deferred,
        "patch_lod_deferred": stats.patch_lod_deferred,
        "motion_stroke_sample_mask": stats.motion_stroke_sample_mask,
        "dirty_tiles": stats.dirty_tiles,
        "reprojected_frame": stats.reprojected_frame,
    })
}

fn render_debug_trace_direct_callback_json(frame: &RenderDebugTraceFrame) -> Value {
    json!({
        "scheduled": frame.callback_scheduled,
        "executed": frame.callback_executed,
        "encoded": frame.callback_encoded,
        "superseded": frame.callback_superseded,
        "failures": frame.callback_failures,
        "cpu_ms": milliseconds(frame.callback_cpu_time),
        "frame_pool_wait_ms": milliseconds(frame.callback_frame_pool_wait_time),
        "command_encoding_ms": milliseconds(frame.callback_command_encoding_time),
        "frame_pool_grew": frame.callback_frame_pool_grew,
        "frame_pool_capacity_bytes": frame.callback_frame_pool_capacity_bytes,
        "target_size": (!frame.callback_target_size.contains(&0)).then_some(frame.callback_target_size),
        "passes": frame.callback_passes_executed,
        "coverage_strategy": frame.callback_coverage_strategy.map(vector_coverage_strategy_name),
        "presentation_quality": frame.callback_presentation_quality.map(presentation_quality_name),
        "transient_peak_bytes": frame.callback_transient_peak_bytes,
        "transient_resident_bytes": frame.callback_transient_resident_bytes,
        "render_bundle_runs": frame.callback_render_bundle_runs,
        "stencil_draws": frame.callback_stencil_draws,
        "retained_stroke_candidate_references": frame.callback_retained_stroke_candidate_references,
        "retained_stroke_cull_dispatches": frame.callback_retained_stroke_cull_dispatches,
        "retained_stroke_indirect_draws": frame.callback_retained_stroke_indirect_draws,
        "retained_stroke_indirect_runs": frame.callback_retained_stroke_indirect_runs,
        "selected_stroke_lod": frame.callback_selected_stroke_lod,
        "active_stroke_lod_levels": frame.callback_active_stroke_lod_levels,
        "selected_stroke_segments": frame.callback_selected_stroke_segments,
        "target_visible_stroke_segments": frame.callback_target_visible_stroke_segments,
        "visible_stroke_tiles": frame.callback_visible_stroke_tiles,
        "max_visible_stroke_tile_segments": frame.callback_max_visible_stroke_tile_segments,
    })
}

fn document_position_json(position: &DocumentPosition) -> Value {
    json!({
        "scroll_x_doc": finite_f32_json(position.scroll_x_doc),
        "scroll_y_doc": finite_f32_json(position.scroll_y_doc),
        "zoom": finite_f32_json(position.zoom),
        "page": position.page,
        "reflow": position.semantic.is_some(),
    })
}

fn gpu_cache_json(cache: &GpuCacheDebugSnapshot) -> Value {
    let frontend = json!({
        "outline_fonts": cache.frontend_outline_fonts,
        "outline_font_bytes": cache.frontend_outline_font_bytes,
        "outline_font_hits": cache.frontend_outline_font_hits,
        "outline_font_misses": cache.frontend_outline_font_misses,
        "outline_font_evictions": cache.frontend_outline_font_evictions,
        "objects": cache.frontend_objects,
        "object_hits": cache.frontend_object_hits,
        "object_misses": cache.frontend_object_misses,
        "object_evictions": cache.frontend_object_evictions,
        "preflight_reports": cache.frontend_preflight_reports,
        "preflight_scans": cache.frontend_preflight_scans,
    });
    let scenes = json!({
        "compiled_pages": cache.compiled_pages,
        "compiled_page_limit": cache.compiled_page_limit,
        "compiled_bytes": cache.compiled_bytes,
        "compiled_byte_limit": cache.compiled_byte_limit,
        "content_bound_pages": cache.content_bound_pages,
        "content_bound_page_limit": cache.content_bound_page_limit,
        "prepared_pages": cache.prepared_pages,
        "prepared_page_limit": cache.prepared_page_limit,
        "resident_bytes": cache.resident_bytes,
        "resident_byte_limit": cache.resident_byte_limit,
    });
    let zoom_worker_latency = json!({
        "queue_p95_ms": milliseconds(cache.motion_render_queue_latency_p95),
        "queue_max_ms": milliseconds(cache.motion_render_queue_latency_max),
        "execution_p95_ms": milliseconds(cache.motion_render_execution_latency_p95),
        "execution_max_ms": milliseconds(cache.motion_render_execution_latency_max),
        "total_p95_ms": milliseconds(cache.motion_render_total_latency_p95),
        "total_max_ms": milliseconds(cache.motion_render_total_latency_max),
    });
    let mut zoom_motion = json!({
        "reprojections": cache.surface_zoom_motion_reprojections,
        "refreshes_suppressed": cache.surface_zoom_motion_refreshes_suppressed,
        "cancellations": cache.surface_zoom_motion_cancellations,
        "emergency_renders": cache.surface_zoom_emergency_renders,
        "live_redraws": cache.surface_zoom_live_redraws,
        "cached_sharp_presentations": cache.surface_zoom_cached_sharp_presentations,
        "live_failures": cache.surface_zoom_live_failures,
        "gpu_holds": cache.surface_zoom_gpu_holds,
        "animation_steps": cache.surface_zoom_animation_steps,
        "admission_holds": cache.surface_zoom_admission_holds,
        "worker_holds": cache.surface_zoom_worker_holds,
        "sample_holds": cache.surface_zoom_sample_holds,
        "presentation_holds": cache.surface_zoom_presentation_holds,
        "scaled_presentations": cache.surface_zoom_scaled_presentations,
        "target_reuses": cache.surface_zoom_target_reuses,
        "target_allocations": cache.surface_zoom_target_allocations,
        "jobs_started": cache.motion_render_jobs_started,
        "jobs_completed": cache.motion_render_jobs_completed,
        "jobs_superseded": cache.motion_render_jobs_superseded,
        "jobs_pending": cache.motion_render_jobs_pending,
        "jobs_peak": cache.motion_render_jobs_peak,
        "detached_target_reuses": cache.motion_render_detached_target_reuses,
        "ui_thread_submissions": cache.motion_render_inline_frames,
        "inline_frames": cache.motion_render_inline_frames,
        "inline_failures": cache.motion_render_inline_failures,
        "pan_inline_frames": cache.camera_pan_inline_frames,
        "pan_admission_holds": cache.camera_pan_admission_holds,
        "camera_serial": cache.motion_render_camera_serial,
        "presented_camera_serial": cache.motion_render_presented_camera_serial,
        "camera_serial_mismatches": cache.motion_render_camera_serial_mismatches,
        "camera_batch_commits": cache.motion_render_camera_batch_commits,
        "reservations_started": cache.camera_target_reservations_started,
        "reservations_consumed": cache.camera_target_reservations_consumed,
        "reservations_released": cache.camera_target_reservations_released,
        "reservations_pending": cache.camera_target_reservations_pending,
        "worker_latency": zoom_worker_latency,
        "active": cache.zoom_motion_active,
    });
    zoom_motion
        .as_object_mut()
        .expect("zoom-motion report is an object")
        .extend([
            (
                "callback_scheduled".into(),
                cache.motion_render_callback_scheduled.into(),
            ),
            (
                "callback_executed".into(),
                cache.motion_render_callback_executed.into(),
            ),
            (
                "callback_encoded".into(),
                cache.motion_render_callback_encoded.into(),
            ),
            (
                "callback_superseded".into(),
                cache.motion_render_callback_superseded.into(),
            ),
            (
                "callback_failures".into(),
                cache.motion_render_callback_failures.into(),
            ),
            (
                "callback_scheduled_camera_serial".into(),
                cache.motion_render_callback_scheduled_camera_serial.into(),
            ),
            (
                "callback_executed_camera_serial".into(),
                cache.motion_render_callback_executed_camera_serial.into(),
            ),
            (
                "callback_encoded_camera_serial".into(),
                cache.motion_render_callback_encoded_camera_serial.into(),
            ),
            (
                "callback_cpu_ms".into(),
                milliseconds(cache.motion_render_callback_cpu_time).into(),
            ),
            (
                "callback_frame_pool_wait_ms".into(),
                milliseconds(cache.motion_render_callback_frame_pool_wait_time).into(),
            ),
            (
                "callback_command_encoding_ms".into(),
                milliseconds(cache.motion_render_callback_command_encoding_time).into(),
            ),
            (
                "callback_frame_pool_grew".into(),
                cache.motion_render_callback_frame_pool_grew.into(),
            ),
            (
                "callback_frame_pool_capacity_bytes".into(),
                cache
                    .motion_render_callback_frame_pool_capacity_bytes
                    .into(),
            ),
            (
                "callback_target_size".into(),
                json!(
                    (!cache.motion_render_callback_target_size.contains(&0))
                        .then_some(cache.motion_render_callback_target_size)
                ),
            ),
            (
                "callback_passes".into(),
                cache.motion_render_callback_passes_executed.into(),
            ),
            (
                "callback_coverage_strategy".into(),
                json!(
                    cache
                        .motion_render_callback_coverage_strategy
                        .map(vector_coverage_strategy_name)
                ),
            ),
            (
                "callback_presentation_quality".into(),
                json!(
                    cache
                        .motion_render_callback_presentation_quality
                        .map(presentation_quality_name)
                ),
            ),
            (
                "callback_transient_peak_bytes".into(),
                cache.motion_render_callback_transient_peak_bytes.into(),
            ),
            (
                "callback_transient_resident_bytes".into(),
                cache.motion_render_callback_transient_resident_bytes.into(),
            ),
            (
                "callback_render_bundle_runs".into(),
                cache.motion_render_callback_render_bundle_runs.into(),
            ),
            (
                "callback_stencil_draws".into(),
                cache.motion_render_callback_stencil_draws.into(),
            ),
            (
                "callback_retained_stroke_candidate_references".into(),
                cache
                    .motion_render_callback_retained_stroke_candidate_references
                    .into(),
            ),
            (
                "callback_retained_stroke_cull_dispatches".into(),
                cache
                    .motion_render_callback_retained_stroke_cull_dispatches
                    .into(),
            ),
            (
                "callback_retained_stroke_indirect_draws".into(),
                cache
                    .motion_render_callback_retained_stroke_indirect_draws
                    .into(),
            ),
            (
                "callback_retained_stroke_indirect_runs".into(),
                cache
                    .motion_render_callback_retained_stroke_indirect_runs
                    .into(),
            ),
            (
                "callback_selected_stroke_lod".into(),
                cache.motion_render_callback_selected_stroke_lod.into(),
            ),
            (
                "callback_active_stroke_lod_levels".into(),
                cache.motion_render_callback_active_stroke_lod_levels.into(),
            ),
            (
                "callback_selected_stroke_segments".into(),
                cache.motion_render_callback_selected_stroke_segments.into(),
            ),
            (
                "callback_target_visible_stroke_segments".into(),
                cache
                    .motion_render_callback_target_visible_stroke_segments
                    .into(),
            ),
            (
                "callback_visible_stroke_tiles".into(),
                cache.motion_render_callback_visible_stroke_tiles.into(),
            ),
            (
                "callback_max_visible_stroke_tile_segments".into(),
                cache
                    .motion_render_callback_max_visible_stroke_tile_segments
                    .into(),
            ),
        ]);
    let surfaces = json!({
        "count": cache.surface_count,
        "bytes": cache.surface_bytes,
        "byte_limit": cache.surface_byte_limit,
        "continuity_count": cache.continuity_surface_count,
        "continuity_bytes": cache.continuity_surface_bytes,
        "seed_tasks": cache.surface_seed_tasks,
        "in_flight_bytes": cache.surface_in_flight_bytes,
        "render_pending": cache.surface_render_pending,
        "pending_peak": cache.surface_pending_peak,
        "renders_started": cache.surface_renders_started,
        "renders_completed": cache.surface_renders_completed,
        "renders_coalesced": cache.surface_renders_coalesced,
        "renders_superseded": cache.surface_renders_superseded,
        "renders_deferred": cache.surface_renders_deferred,
        "renders_discarded": cache.surface_renders_discarded,
        "cache_pressure": cache.surface_render_cache_pressure,
        "scroll_reuses": cache.surface_scroll_reuses,
        "scroll_prefetches": cache.surface_scroll_prefetches,
        "scroll_misses": cache.surface_scroll_misses,
        "sharp_pan_cache_presentations": cache.surface_sharp_pan_cache_presentations,
        "zoom_reuses": cache.surface_zoom_reuses,
        "zoom_quality_refreshes": cache.surface_zoom_quality_refreshes,
        "zoom_coverage_refreshes": cache.surface_zoom_coverage_refreshes,
        "zoom_settled_upgrades": cache.surface_zoom_settled_upgrades,
        "zoom_motion": zoom_motion,
        "warm_requests": cache.surface_warm_requests,
        "warm_hits": cache.surface_warm_hits,
        "warm_pages": cache.surface_warm_pages,
        "continuity_requests": cache.surface_continuity_requests,
        "continuity_completed": cache.surface_continuity_completed,
        "continuity_hits": cache.surface_continuity_hits,
        "partial_reuses": cache.surface_partial_reuses,
        "presentation_gaps": cache.surface_presentation_gaps,
        "incomplete_suppressed": cache.surface_incomplete_suppressed,
        "refinement_submissions": cache.surface_refinement_submissions,
        "atomic_swaps": cache.surface_atomic_swaps,
        "queue_latency_p95_ms": milliseconds(cache.surface_queue_latency_p95),
        "ready_latency_p95_ms": milliseconds(cache.surface_ready_latency_p95),
        "present_latency_p95_ms": milliseconds(cache.surface_present_latency_p95),
        "refinement_pages": cache.refinement_pages,
        "retained_refinement_pages": cache.retained_refinement_pages,
    });
    let preparation = json!({
        "active": cache.active_preparations,
        "queued": cache.queued_preparations,
        "speculative": cache.speculative_preparations,
        "completed": cache.completed_preparations,
        "cancelled": cache.cancelled_preparations,
    });
    json!({
        "frontend": frontend,
        "scenes": scenes,
        "surfaces": surfaces,
        "preparation": preparation,
        "motion_active": cache.motion_active,
    })
}

fn page_snapshot_json(page: &PageRenderDebugSnapshot) -> Value {
    let gpu_presentation = page.gpu_presentation.as_ref().map(|presentation| {
        json!({
            "source": presentation.source,
            "page_bounds": presentation.page_bounds,
            "texture_bounds": presentation.texture_bounds,
            "direct_bounds": presentation.direct_bounds,
            "texture_physical_size": presentation.texture_physical_size,
            "camera_serial": presentation.camera_serial,
            "vector_lod_requested_scale": presentation.vector_lod_requested_scale.map(finite_f64_json),
            "vector_lod_available_scale": presentation.vector_lod_available_scale.map(finite_f32_json),
            "vector_lod_ready": presentation.vector_lod_ready,
        })
    });
    let gpu = page.gpu.as_ref().map(|gpu| {
        let retained_strokes = json!({
            "candidate_references": gpu.retained_stroke_candidate_references,
            "cull_dispatches": gpu.retained_stroke_cull_dispatches,
            "indirect_draws": gpu.retained_stroke_indirect_draws,
            "indirect_runs": gpu.retained_stroke_indirect_runs,
            "global_replay": gpu.retained_stroke_global_replay,
            "motion_sample_mask": gpu.motion_stroke_sample_mask,
        });
        let culling = json!({
            "ordered_operations": gpu.culled_operations,
            "bundled_operations": gpu.culled_bundle_operations,
            "retained_strokes": retained_strokes,
        });
        json!({
            "cpu_ms": milliseconds(gpu.cpu_time),
            "frame_pool_wait_ms": milliseconds(gpu.frame_pool_wait_time),
            "command_encoding_ms": milliseconds(gpu.command_encoding_time),
            "gpu_ms": gpu.gpu_time.map(milliseconds),
            "host_to_device_bytes": gpu.host_to_device_bytes,
            "passes": gpu.passes_executed,
            "command_buffers": gpu.command_buffers,
            "vector_batches": gpu.vector_batches,
            "procedural_stroke_batches": gpu.procedural_stroke_batches,
            "render_bundle_runs": gpu.render_bundle_runs,
            "culling": culling,
            "reprojected": gpu.reprojected_frame,
            "reused_tiles": gpu.reused_tiles,
            "rendered_tiles": gpu.rendered_tiles,
            "dirty_tiles": gpu.dirty_tiles,
            "refinement_pending": gpu.refinement_pending,
            "presentation_quality": gpu.presentation_quality,
            "scheduled_refinement_work": gpu.scheduled_refinement_work,
            "refinement_work_budget": gpu.refinement_work_budget,
            "selected_vector_lod": gpu.selected_vector_lod,
            "vector_lod_requested_scale": gpu.vector_lod_requested_scale,
            "vector_lod_available_scale": gpu.vector_lod_available_scale,
            "vector_lod_deferred": gpu.vector_lod_deferred,
            "frame_pool_reused": gpu.frame_pool_reused,
            "frame_pool_grew": gpu.frame_pool_grew,
            "frame_pool_capacity_bytes": gpu.frame_pool_capacity_bytes,
            "generation": gpu.generation,
            "coverage_strategy": gpu.coverage_strategy,
            "retained_stroke_strategy": gpu.retained_stroke_strategy,
            "motion_render_mode": gpu.motion_render_mode,
        })
    });
    let gpu_preparation = page.gpu_preparation.as_ref().map(|preparation| {
        json!({
            "frontend_preflight_ms": milliseconds(preparation.frontend_preflight),
            "compile_ms": milliseconds(preparation.compile),
            "link_conversion_ms": milliseconds(preparation.link_conversion),
            "gpu_prepare_ms": milliseconds(preparation.gpu_prepare),
            "total_ms": milliseconds(preparation.total),
            "compiled_cache_hit": preparation.compiled_cache_hit,
            "elapsed_ms": milliseconds(preparation.elapsed),
            "host_to_device_bytes": preparation.host_to_device_bytes,
            "resident_bytes": preparation.resident_bytes,
            "retained_operations": preparation.retained_operations,
            "vector_batches": preparation.vector_batches,
            "vector_lod_levels": preparation.vector_lod_levels,
            "vector_lod_compacted": preparation.vector_lod_compacted,
            "vector_vertices": preparation.vector_vertices,
            "vector_triangle_references": preparation.vector_triangle_references,
            "vector_reference_bytes": preparation.vector_reference_bytes,
            "vector_instances": preparation.vector_instances,
            "vector_canonical_indices": preparation.vector_canonical_indices,
            "vector_reference_segments": preparation.vector_reference_segments,
            "vector_bytes": preparation.vector_bytes,
            "motion_stroke_lod_levels": preparation.motion_stroke_lod_levels,
            "motion_stroke_coarsest_segments": preparation.motion_stroke_coarsest_segments,
            "motion_stroke_lod_segments": preparation.motion_stroke_lod_segments,
            "motion_stroke_source_segments": preparation.motion_stroke_source_segments,
            "motion_stroke_quadratic_segments": preparation.motion_stroke_quadratic_segments,
            "exact_stroke_segments": preparation.exact_stroke_segments,
            "camera_program_partitions": preparation.camera_program_partitions,
            "camera_program_operations": preparation.camera_program_operations,
            "analytic_paths": preparation.analytic_paths,
            "analytic_segments": preparation.analytic_segments,
            "analytic_draws": preparation.analytic_draws,
            "analytic_groups": preparation.analytic_groups,
            "analytic_bvh_nodes": preparation.analytic_bvh_nodes,
            "analytic_bytes": preparation.analytic_bytes,
            "analytic_fallback": preparation.analytic_fallback,
        })
    });
    json!({
        "page_number": page.page_number,
        "status": page_status_json(page.status),
        "backend": page_backend_name(page.backend),
        "requested_raster": page.requested_raster.map(raster_json),
        "displayed_raster": page.displayed_raster.map(raster_json),
        "gpu_presentation": gpu_presentation,
        "cached_raster_bytes": page.cached_raster_bytes,
        "cached_raster_variants": page.cached_raster_variants,
        "source_revision": page.source_revision,
        "latest_timing": page.latest_timing.map(|timing| json!({
            "total_ms": milliseconds(timing.total),
            "display_list_ms": timing.display_list.map(milliseconds),
            "display_list_cached": timing.display_list_cached,
            "raster_ms": timing.raster.map(milliseconds),
            "mupdf_draw_ms": timing.mupdf_draw.map(milliseconds),
            "raster_bands": timing.raster_bands,
            "succeeded": timing.succeeded,
        })),
        "gpu": gpu,
        "gpu_preparation": gpu_preparation,
        "gpu_fallbacks": page.gpu_fallbacks.iter().map(|fallback| json!({
            "feature": fallback.feature,
            "code": fallback.code,
            "message": fallback.message,
            "source": fallback.source,
        })).collect::<Vec<_>>(),
        "gpu_warnings": page.gpu_warnings.iter().map(|warning| json!({
            "feature": warning.feature,
            "code": warning.code,
            "message": warning.message,
        })).collect::<Vec<_>>(),
        "gpu_failure": page.gpu_failure,
        "gpu_refinement_failure": page.gpu_refinement_failure,
        "text": {
            "state": text_index_state_name(page.text_index_state),
            "glyphs": page.glyph_count,
            "lines": page.line_count,
            "links": page.link_count,
        },
    })
}

fn gpu_counter_delta_json(start: &GpuCacheDebugSnapshot, end: &GpuCacheDebugSnapshot) -> Value {
    let mut counters = Map::new();
    let mut resets = Vec::new();
    macro_rules! counter {
        ($field:ident) => {
            insert_counter_delta(
                &mut counters,
                &mut resets,
                stringify!($field),
                start.$field,
                end.$field,
            );
        };
    }
    counter!(frontend_outline_font_hits);
    counter!(frontend_outline_font_misses);
    counter!(frontend_outline_font_evictions);
    counter!(frontend_object_hits);
    counter!(frontend_object_misses);
    counter!(frontend_object_evictions);
    counter!(frontend_preflight_scans);
    counter!(surface_renders_started);
    counter!(surface_renders_completed);
    counter!(surface_renders_coalesced);
    counter!(surface_renders_superseded);
    counter!(surface_renders_deferred);
    counter!(surface_renders_discarded);
    counter!(surface_render_cache_pressure);
    counter!(surface_scroll_reuses);
    counter!(surface_scroll_prefetches);
    counter!(surface_scroll_misses);
    counter!(surface_sharp_pan_cache_presentations);
    counter!(surface_zoom_reuses);
    counter!(surface_zoom_quality_refreshes);
    counter!(surface_zoom_coverage_refreshes);
    counter!(surface_zoom_settled_upgrades);
    counter!(surface_zoom_motion_reprojections);
    counter!(surface_zoom_motion_refreshes_suppressed);
    counter!(surface_zoom_motion_cancellations);
    counter!(surface_zoom_emergency_renders);
    counter!(surface_zoom_live_redraws);
    counter!(surface_zoom_cached_sharp_presentations);
    counter!(surface_zoom_live_failures);
    counter!(surface_zoom_gpu_holds);
    counter!(surface_zoom_animation_steps);
    counter!(surface_zoom_admission_holds);
    counter!(surface_zoom_worker_holds);
    counter!(surface_zoom_sample_holds);
    counter!(surface_zoom_presentation_holds);
    counter!(surface_zoom_scaled_presentations);
    counter!(surface_zoom_target_reuses);
    counter!(surface_zoom_target_allocations);
    counter!(motion_render_jobs_started);
    counter!(motion_render_jobs_completed);
    counter!(motion_render_jobs_superseded);
    counter!(motion_render_detached_target_reuses);
    counter!(motion_render_inline_frames);
    counter!(motion_render_inline_failures);
    counter!(motion_render_callback_scheduled);
    counter!(motion_render_callback_executed);
    counter!(motion_render_callback_encoded);
    counter!(motion_render_callback_superseded);
    counter!(motion_render_callback_failures);
    counter!(camera_pan_inline_frames);
    counter!(camera_pan_admission_holds);
    counter!(motion_render_camera_serial_mismatches);
    counter!(motion_render_camera_batch_commits);
    counter!(camera_target_reservations_started);
    counter!(camera_target_reservations_consumed);
    counter!(camera_target_reservations_released);
    counter!(surface_warm_requests);
    counter!(surface_warm_hits);
    counter!(surface_continuity_requests);
    counter!(surface_continuity_completed);
    counter!(surface_continuity_hits);
    counter!(surface_partial_reuses);
    counter!(surface_presentation_gaps);
    counter!(surface_incomplete_suppressed);
    counter!(surface_refinement_submissions);
    counter!(surface_atomic_swaps);
    counter!(completed_preparations);
    counter!(cancelled_preparations);
    json!({
        "counters": counters,
        "reset_fields": resets,
    })
}

fn insert_counter_delta(
    counters: &mut Map<String, Value>,
    resets: &mut Vec<&'static str>,
    name: &'static str,
    start: u64,
    end: u64,
) {
    if let Some(delta) = end.checked_sub(start) {
        counters.insert(name.to_string(), json!(delta));
    } else {
        counters.insert(name.to_string(), Value::Null);
        resets.push(name);
    }
}

fn raster_json(raster: glyphst_document_ui::document_view::RasterDebugSnapshot) -> Value {
    json!({
        "physical_width": raster.physical_width,
        "physical_height": raster.physical_height,
        "bytes": raster.bytes,
        "scale": finite_f32_json(raster.scale),
    })
}

fn page_status_json(status: PageRenderDebugStatus) -> Value {
    match status {
        PageRenderDebugStatus::Exact => json!({ "kind": "exact" }),
        PageRenderDebugStatus::RetainedGpu { exact_pending } => {
            json!({ "kind": "retained-gpu", "exact_pending": exact_pending })
        }
        PageRenderDebugStatus::Fallback { exact_pending } => {
            json!({ "kind": "fallback", "exact_pending": exact_pending })
        }
        PageRenderDebugStatus::Pending => json!({ "kind": "pending" }),
        PageRenderDebugStatus::Failed => json!({ "kind": "failed" }),
        PageRenderDebugStatus::Unavailable => json!({ "kind": "unavailable" }),
    }
}

fn page_backend_name(backend: PageRenderBackendDebug) -> &'static str {
    match backend {
        PageRenderBackendDebug::MuPdf => "mupdf",
        PageRenderBackendDebug::GlyphstGpu => "glyphst-gpu",
        PageRenderBackendDebug::GlyphstPending => "glyphst-pending",
        PageRenderBackendDebug::GlyphstFailed => "glyphst-failed",
        PageRenderBackendDebug::MuPdfWhileGpuPending => "mupdf-placeholder",
        PageRenderBackendDebug::MuPdfFallback => "mupdf-fallback",
    }
}

fn vector_coverage_strategy_name(strategy: GpuVectorCoverageStrategy) -> &'static str {
    match strategy {
        GpuVectorCoverageStrategy::TiledAnalytic => "TiledAnalytic",
        GpuVectorCoverageStrategy::RetainedTriangles => "RetainedTriangles",
    }
}

fn presentation_quality_name(quality: GpuPresentationQuality) -> &'static str {
    match quality {
        GpuPresentationQuality::Exact => "Exact",
        GpuPresentationQuality::ErrorBounded => "ErrorBounded",
        GpuPresentationQuality::CoherentPreview => "CoherentPreview",
        GpuPresentationQuality::Incomplete => "Incomplete",
    }
}

fn text_index_state_name(state: TextIndexDebugState) -> &'static str {
    match state {
        TextIndexDebugState::Pending => "pending",
        TextIndexDebugState::Indexing => "indexing",
        TextIndexDebugState::Ready => "ready",
        TextIndexDebugState::Failed => "failed",
    }
}

fn finite_f32_json(value: f32) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

fn finite_f64_json(value: f64) -> Value {
    if value.is_finite() {
        json!(value)
    } else {
        Value::Null
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64 * 100.0)
}

fn render_document(this: Div, document: DocumentRenderDebugSnapshot) -> Div {
    if document.pages.is_empty() {
        return this;
    }

    let mut this = this
        .child(section_label("ACTIVE VIEW"))
        .child(reading("VIEW", document.page_label, FOREGROUND))
        .child(reading(
            "QUEUE",
            format!(
                "{} / {}",
                document.pending_render_count, document.pending_render_limit
            ),
            if document.pending_render_count == document.pending_render_limit {
                WARN
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "FRONTEND CACHE",
            format!(
                "{} fonts / {} · {}/{} hit/miss · {} evict · {} objects · {}/{} hit/miss · {} evict · {} reports / {} scans",
                document.gpu_cache.frontend_outline_fonts,
                format_bytes(document.gpu_cache.frontend_outline_font_bytes),
                document.gpu_cache.frontend_outline_font_hits,
                document.gpu_cache.frontend_outline_font_misses,
                document.gpu_cache.frontend_outline_font_evictions,
                document.gpu_cache.frontend_objects,
                document.gpu_cache.frontend_object_hits,
                document.gpu_cache.frontend_object_misses,
                document.gpu_cache.frontend_object_evictions,
                document.gpu_cache.frontend_preflight_reports,
                document.gpu_cache.frontend_preflight_scans,
            ),
            FOREGROUND,
        ))
        .child(reading(
            "GPU SCENES",
            format!(
                "{} / {} compiled · {} / {} bounds · {} / {} prepared",
                document.gpu_cache.compiled_pages,
                document.gpu_cache.compiled_page_limit,
                document.gpu_cache.content_bound_pages,
                document.gpu_cache.content_bound_page_limit,
                document.gpu_cache.prepared_pages,
                document.gpu_cache.prepared_page_limit
            ),
            FOREGROUND,
        ))
        .child(reading(
            "COMPILED CPU",
            format!(
                "{} / {} estimated",
                format_bytes(document.gpu_cache.compiled_bytes),
                format_bytes(document.gpu_cache.compiled_byte_limit)
            ),
            FOREGROUND,
        ))
        .child(reading(
            "GPU RESIDENT",
            format!(
                "{} / {}",
                format_bytes(document.gpu_cache.resident_bytes),
                format_bytes(document.gpu_cache.resident_byte_limit)
            ),
            FOREGROUND,
        ))
        .child(reading(
            "GPU SURFACES",
            format!(
                "{} / {}  ×{} · {} in flight",
                format_bytes(document.gpu_cache.surface_bytes),
                format_bytes(document.gpu_cache.surface_byte_limit),
                document.gpu_cache.surface_count,
                format_bytes(document.gpu_cache.surface_in_flight_bytes),
            ),
            FOREGROUND,
        ))
        .child(reading(
            "GPU CONTINUITY",
            format!(
                "{} surfaces / {} · {} requests · {} ready · {} hits · {} partial · {} gaps",
                document.gpu_cache.continuity_surface_count,
                format_bytes(document.gpu_cache.continuity_surface_bytes),
                document.gpu_cache.surface_continuity_requests,
                document.gpu_cache.surface_continuity_completed,
                document.gpu_cache.surface_continuity_hits,
                document.gpu_cache.surface_partial_reuses,
                document.gpu_cache.surface_presentation_gaps,
            ),
            if document.gpu_cache.surface_presentation_gaps > 0 {
                BAD
            } else if document.gpu_cache.surface_continuity_hits > 0 {
                GOOD
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU PIPELINE",
            format!(
                "{} active · {}/{} pending/peak · {} submitted · {} done · {}/{} visible/retained refine · {} hidden · {} swaps · motion {}",
                document.gpu_cache.surface_seed_tasks,
                document.gpu_cache.surface_render_pending,
                document.gpu_cache.surface_pending_peak,
                document.gpu_cache.surface_renders_started,
                document.gpu_cache.surface_renders_completed,
                document.gpu_cache.refinement_pages,
                document.gpu_cache.retained_refinement_pages,
                document.gpu_cache.surface_incomplete_suppressed,
                document.gpu_cache.surface_atomic_swaps,
                if document.gpu_cache.motion_active {
                    "active"
                } else {
                    "settled"
                }
            ),
            if document.gpu_cache.surface_seed_tasks > 0
                || document.gpu_cache.surface_render_pending > 0
                || document.gpu_cache.refinement_pages > 0
                || document.gpu_cache.motion_active
            {
                WARN
            } else {
                GOOD
            },
        ))
        .child(reading(
            "GPU OUTCOMES",
            format!(
                "{} deferred · {} superseded · {} discarded · {} coalesced · {} cache pressure",
                document.gpu_cache.surface_renders_deferred,
                document.gpu_cache.surface_renders_superseded,
                document.gpu_cache.surface_renders_discarded,
                document.gpu_cache.surface_renders_coalesced,
                document.gpu_cache.surface_render_cache_pressure,
            ),
            if document.gpu_cache.surface_render_cache_pressure > 0 {
                WARN
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU SCROLL CACHE",
            format!(
                "{} exact reuse · {} sharp presentations · {} proactive refresh · {} uncovered refresh",
                document.gpu_cache.surface_scroll_reuses,
                document.gpu_cache.surface_sharp_pan_cache_presentations,
                document.gpu_cache.surface_scroll_prefetches,
                document.gpu_cache.surface_scroll_misses,
            ),
            if document.gpu_cache.surface_scroll_misses > 0 {
                WARN
            } else {
                GOOD
            },
        ))
        .child(reading(
            "GPU ZOOM CACHE",
            format!(
                "{} retained reuse · {} quality refresh · {} coverage refresh · {} exact upgrade",
                document.gpu_cache.surface_zoom_reuses,
                document.gpu_cache.surface_zoom_quality_refreshes,
                document.gpu_cache.surface_zoom_coverage_refreshes,
                document.gpu_cache.surface_zoom_settled_upgrades,
            ),
            if document.gpu_cache.surface_zoom_reuses > 0 {
                GOOD
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU CAMERA MOTION",
            format!(
                "{} live redraw / {} zoom steps · {} inline ({} fixed-scale) / {} cached sharp · {} detached reuse · {}/{} pending/peak · {} admission hold ({} pan-cache) · {} scaled · {} failed",
                document.gpu_cache.surface_zoom_live_redraws,
                document.gpu_cache.surface_zoom_animation_steps,
                document.gpu_cache.motion_render_inline_frames,
                document.gpu_cache.camera_pan_inline_frames,
                document.gpu_cache.surface_zoom_cached_sharp_presentations,
                document.gpu_cache.motion_render_detached_target_reuses,
                document.gpu_cache.motion_render_jobs_pending,
                document.gpu_cache.motion_render_jobs_peak,
                document.gpu_cache.surface_zoom_admission_holds,
                document.gpu_cache.camera_pan_admission_holds,
                document.gpu_cache.surface_zoom_scaled_presentations,
                document.gpu_cache.surface_zoom_live_failures,
            ),
            if document.gpu_cache.surface_zoom_live_failures > 0
                || document.gpu_cache.surface_zoom_scaled_presentations > 0
            {
                WARN
            } else if document.gpu_cache.surface_zoom_live_redraws > 0 {
                GOOD
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU CAMERA CONTROL",
            format!(
                "{} worker / {} sample / {} presentation holds · camera {}/{} ({} mismatches) · {} reservations ({}/{} consumed/released, {} pending) · {} stale canceled · {} motion allocation · {} legacy emergency",
                document.gpu_cache.surface_zoom_worker_holds,
                document.gpu_cache.surface_zoom_sample_holds,
                document.gpu_cache.surface_zoom_presentation_holds,
                document.gpu_cache.motion_render_presented_camera_serial,
                document.gpu_cache.motion_render_camera_serial,
                document.gpu_cache.motion_render_camera_serial_mismatches,
                document.gpu_cache.camera_target_reservations_started,
                document.gpu_cache.camera_target_reservations_consumed,
                document.gpu_cache.camera_target_reservations_released,
                document.gpu_cache.camera_target_reservations_pending,
                document.gpu_cache.surface_zoom_motion_cancellations,
                document.gpu_cache.surface_zoom_target_allocations,
                document.gpu_cache.surface_zoom_emergency_renders,
            ),
            if document.gpu_cache.surface_zoom_target_allocations > 0
                || document.gpu_cache.surface_zoom_emergency_renders > 0
            {
                WARN
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU PAGE WARMING",
            format!(
                "{} requests · {} hits · {} ready",
                document.gpu_cache.surface_warm_requests,
                document.gpu_cache.surface_warm_hits,
                document.gpu_cache.surface_warm_pages,
            ),
            if document.gpu_cache.surface_warm_pages > 0
                || document.gpu_cache.surface_warm_hits > 0
            {
                GOOD
            } else {
                FOREGROUND
            },
        ))
        .child(reading(
            "GPU LATENCY P95",
            format!(
                "{} queue · {} ready · {} first paint",
                format_duration(document.gpu_cache.surface_queue_latency_p95),
                format_duration(document.gpu_cache.surface_ready_latency_p95),
                format_duration(document.gpu_cache.surface_present_latency_p95),
            ),
            FOREGROUND,
        ))
        .child(reading(
            "GPU PREP QUEUE",
            format!(
                "{} active · {} queued · {} speculative · {} done · {} cancelled",
                document.gpu_cache.active_preparations,
                document.gpu_cache.queued_preparations,
                document.gpu_cache.speculative_preparations,
                document.gpu_cache.completed_preparations,
                document.gpu_cache.cancelled_preparations,
            ),
            if document.gpu_cache.active_preparations > 0
                || document.gpu_cache.queued_preparations > 0
            {
                WARN
            } else {
                GOOD
            },
        ));

    for fallback in document.gpu_navigation_fallbacks {
        this = this.child(reading(
            "NAV FALLBACK",
            format!(
                "[{}:{}] {} · {}",
                fallback.feature, fallback.code, fallback.source, fallback.message
            ),
            WARN,
        ));
    }

    for page in document.pages {
        this = this.child(render_page(page));
    }
    this
}

fn render_page(page: PageRenderDebugSnapshot) -> Div {
    let (status, status_color) = match page.status {
        PageRenderDebugStatus::Exact => ("exact", GOOD),
        PageRenderDebugStatus::RetainedGpu {
            exact_pending: true,
        } => ("retained GPU → exact", WARN),
        PageRenderDebugStatus::RetainedGpu {
            exact_pending: false,
        } => ("retained GPU", GOOD),
        PageRenderDebugStatus::Fallback {
            exact_pending: true,
        } => ("placeholder → native", WARN),
        PageRenderDebugStatus::Fallback {
            exact_pending: false,
        } => ("fallback", WARN),
        PageRenderDebugStatus::Pending => ("pending", WARN),
        PageRenderDebugStatus::Failed => ("failed", BAD),
        PageRenderDebugStatus::Unavailable => ("unavailable", MUTED),
    };
    let mut section = div()
        .mt_1()
        .pt_1()
        .border_t_1()
        .border_color(BORDER)
        .flex()
        .flex_col()
        .child(section_label(format!("PAGE {}", page.page_number)))
        .child(reading("STATE", status.to_string(), status_color));

    let (engine, engine_color) = match page.backend {
        PageRenderBackendDebug::MuPdf => ("MuPDF", FOREGROUND),
        PageRenderBackendDebug::GlyphstGpu => ("Glyphst GPU", GOOD),
        PageRenderBackendDebug::GlyphstPending => ("Glyphst GPU · pending", WARN),
        PageRenderBackendDebug::GlyphstFailed => ("Glyphst GPU · unavailable", BAD),
        PageRenderBackendDebug::MuPdfWhileGpuPending => ("Glyphst GPU · MuPDF placeholder", WARN),
        PageRenderBackendDebug::MuPdfFallback => ("MuPDF · GPU fallback", WARN),
    };
    section = section.child(reading("ENGINE", engine.to_string(), engine_color));

    if let Some(displayed) = page.displayed_raster {
        section = section
            .child(reading(
                "DISPLAY",
                format!(
                    "{}×{} @{:.2}x",
                    displayed.physical_width, displayed.physical_height, displayed.scale
                ),
                FOREGROUND,
            ))
            .child(reading(
                "DISPLAY RGBA",
                format_bytes(displayed.bytes),
                FOREGROUND,
            ));
    }
    if page.displayed_raster != page.requested_raster
        && let Some(requested) = page.requested_raster
    {
        section = section.child(reading(
            "TARGET",
            format!(
                "{}×{} @{:.2}x",
                requested.physical_width, requested.physical_height, requested.scale
            ),
            MUTED,
        ));
    }

    section = section
        .child(reading(
            "PAGE RGBA",
            format!(
                "{}  ×{}",
                format_bytes(page.cached_raster_bytes),
                page.cached_raster_variants
            ),
            FOREGROUND,
        ))
        .child(reading("REV", page.source_revision.to_string(), FOREGROUND));

    if let Some(timing) = page.latest_timing {
        section = section
            .child(reading(
                "TOTAL",
                format_duration(timing.total),
                if timing.succeeded { FOREGROUND } else { BAD },
            ))
            .child(reading(
                "LIST",
                if timing.display_list_cached {
                    "cached".to_string()
                } else {
                    timing
                        .display_list
                        .map(format_duration)
                        .unwrap_or_else(|| "failed".to_string())
                },
                if timing.display_list_cached {
                    GOOD
                } else {
                    FOREGROUND
                },
            ));
        if let Some(raster) = timing.raster {
            section = section.child(reading("RASTER", format_duration(raster), FOREGROUND));
        }
        if let Some(mupdf_draw) = timing.mupdf_draw {
            let value = format_draw_duration(mupdf_draw, timing.raster_bands);
            section = section.child(reading("DRAW", value, FOREGROUND));
        }
    }

    if let Some(preparation) = page.gpu_preparation {
        section = section
            .child(reading(
                "COLD TOTAL",
                format_duration(preparation.total),
                FOREGROUND,
            ))
            .child(reading(
                "COLD STAGES",
                format!(
                    "preflight {} · compile {}{} · links {} · GPU {}",
                    format_duration(preparation.frontend_preflight),
                    format_duration(preparation.compile),
                    if preparation.compiled_cache_hit {
                        " (cached)"
                    } else {
                        ""
                    },
                    format_duration(preparation.link_conversion),
                    format_duration(preparation.gpu_prepare),
                ),
                FOREGROUND,
            ))
            .child(reading(
                "PREPARE",
                format_duration(preparation.elapsed),
                FOREGROUND,
            ))
            .child(reading(
                "PREP H2D",
                format_bytes(preparation.host_to_device_bytes),
                FOREGROUND,
            ))
            .child(reading(
                "RESIDENT",
                format_bytes(preparation.resident_bytes),
                FOREGROUND,
            ))
            .child(reading(
                "RETAINED",
                format!(
                    "{} op · {} vec batch · {} LOD{} · {} clip",
                    preparation.retained_operations,
                    preparation.vector_batches,
                    preparation.vector_lod_levels,
                    if preparation.vector_lod_compacted {
                        " strict-only"
                    } else {
                        ""
                    },
                    preparation.unique_clip_paths
                ),
                FOREGROUND,
            ));
        if preparation.flattened_opaque_knockout_groups > 0 {
            section = section.child(reading(
                "KNOCKOUT FAST",
                format!(
                    "{} opaque text group",
                    preparation.flattened_opaque_knockout_groups
                ),
                GOOD,
            ));
        }
        if preparation.render_bundle_runs > 0 {
            section = section.child(reading(
                "BUNDLES",
                format!(
                    "{} run · {} ordered op",
                    preparation.render_bundle_runs, preparation.render_bundle_operations
                ),
                GOOD,
            ));
        }
        if preparation.patch_shading_lod_levels > 0
            || preparation.patch_shading_deferred_lod_levels > 0
        {
            section = section.child(reading(
                "PATCH LODS",
                format!(
                    "{} cold · {} deferred · {} cold mesh",
                    preparation.patch_shading_lod_levels,
                    preparation.patch_shading_deferred_lod_levels,
                    format_bytes(preparation.patch_shading_mesh_bytes),
                ),
                if preparation.patch_shading_deferred_lod_levels > 0 {
                    WARN
                } else {
                    FOREGROUND
                },
            ));
        }
        if preparation.deferred_patch_lod_levels > 0 {
            section = section.child(reading(
                "PATCH REFINE",
                format!(
                    "{} · {} LOD · {}",
                    format_duration(preparation.deferred_patch_lod_elapsed),
                    preparation.deferred_patch_lod_levels,
                    format_bytes(preparation.deferred_patch_lod_bytes),
                ),
                GOOD,
            ));
        }
        if preparation.retained_form_resources > 0 {
            section = section.child(reading(
                "FORMS",
                format!(
                    "{} resource · {} instance · {} stored node · {} expanded node",
                    preparation.retained_form_resources,
                    preparation.retained_form_instances,
                    preparation.retained_subscene_nodes,
                    preparation.expanded_scene_nodes
                ),
                FOREGROUND,
            ));
        }
        if preparation.retained_type3_glyph_resources > 0 {
            section = section.child(reading(
                "TYPE 3",
                format!(
                    "{} glyph resource · {} instance",
                    preparation.retained_type3_glyph_resources,
                    preparation.retained_type3_glyph_instances
                ),
                FOREGROUND,
            ));
        }
        if preparation.retained_tiling_pattern_resources > 0 {
            section = section.child(reading(
                "PATTERNS",
                format!(
                    "{} resource · {} target · {} expanded cell",
                    preparation.retained_tiling_pattern_resources,
                    preparation.retained_tiling_pattern_instances,
                    preparation.expanded_tiling_pattern_cells
                ),
                FOREGROUND,
            ));
        }
        if preparation.retained_transparency_groups > 0
            || preparation.retained_soft_mask_resources > 0
            || preparation.retained_soft_mask_instances > 0
        {
            section = section.child(reading(
                "GROUPS",
                format!(
                    "{} group · {} mask resource · {} mask instance",
                    preparation.retained_transparency_groups,
                    preparation.retained_soft_mask_resources,
                    preparation.retained_soft_mask_instances
                ),
                FOREGROUND,
            ));
        }
        if preparation.analytic_bytes > 0 {
            section = section.child(reading(
                "ANALYTIC",
                format!(
                    "{} · {} path · {} seg · {} draw · {} BVH",
                    format_bytes(preparation.analytic_bytes),
                    preparation.analytic_paths,
                    preparation.analytic_segments,
                    preparation.analytic_draws,
                    preparation.analytic_bvh_nodes
                ),
                FOREGROUND,
            ));
        } else if preparation.vector_bytes > 0 {
            section = section.child(reading(
                "TRIANGLES",
                format!(
                    "{} total · {} refs · {} vertex · {} logical ref · {} instance · {} compact / {} indexed / {} total LOD · {} canonical index · {} segment",
                    format_bytes(preparation.vector_bytes),
                    format_bytes(preparation.vector_reference_bytes),
                    preparation.vector_vertices,
                    preparation.vector_triangle_references,
                    preparation.vector_instances,
                    preparation.vector_compact_reference_lods,
                    preparation.vector_indexed_reference_lods,
                    preparation.vector_lod_levels,
                    preparation.vector_canonical_indices,
                    preparation.vector_reference_segments,
                ),
                FOREGROUND,
            ));
        }
        if preparation.hairline_geometry_records > 0 {
            section = section.child(reading(
                "STROKE GEOM",
                format!(
                    "{} geometry · {} state · {} chunk",
                    preparation.hairline_geometry_records,
                    preparation.stroke_state_records,
                    preparation.hairline_chunks
                ),
                FOREGROUND,
            ));
        }
        if preparation.hairline_bytes > 0 {
            section = section.child(reading(
                "HAIRLINES",
                format!(
                    "{} · {} LOD · {} primitive · {} draw",
                    format_bytes(preparation.hairline_bytes),
                    preparation.hairline_lod_levels,
                    preparation.hairline_primitives,
                    preparation.hairline_draws
                ),
                FOREGROUND,
            ));
        }
        if preparation.procedural_stroke_bytes > 0 {
            section = section.child(reading(
                "STROKES",
                format!(
                    "{} · {} LOD · {} primitive · {} draw",
                    format_bytes(preparation.procedural_stroke_bytes),
                    preparation.procedural_stroke_lod_levels,
                    preparation.procedural_stroke_primitives,
                    preparation.procedural_stroke_draws
                ),
                FOREGROUND,
            ));
        }
        if preparation.procedural_stroke_batches > 0 {
            section = section.child(reading(
                "STROKE OPS",
                format!("{} ordered batch", preparation.procedural_stroke_batches),
                FOREGROUND,
            ));
        }
        if preparation.image_textures > 0
            || preparation.alpha_textures > 0
            || preparation.gradient_tables > 0
        {
            section = section.child(reading(
                "RESOURCES",
                format!(
                    "{} image · {} alpha · {} gradient",
                    preparation.image_textures,
                    preparation.alpha_textures,
                    preparation.gradient_tables
                ),
                FOREGROUND,
            ));
            if preparation.image_textures > 0 || preparation.alpha_textures > 0 {
                section = section
                    .child(reading(
                        "IMAGE MIPS",
                        format!(
                            "{} color · {} alpha",
                            preparation.image_mip_levels, preparation.alpha_mip_levels
                        ),
                        FOREGROUND,
                    ))
                    .child(reading(
                        "IMAGE VRAM",
                        format!(
                            "{} total · {} mips",
                            format_bytes(preparation.image_texture_bytes),
                            format_bytes(preparation.mip_texture_bytes)
                        ),
                        FOREGROUND,
                    ));
            }
        }
        if let Some(fallback) = preparation.analytic_fallback {
            section = section.child(reading("VECTOR PLAN", fallback, WARN));
        }
    }

    if let Some(gpu) = page.gpu {
        section = section
            .child(reading(
                "GPU CPU",
                format_duration(gpu.cpu_time),
                FOREGROUND,
            ))
            .child(reading(
                "GPU TIME",
                gpu.gpu_time
                    .map(format_duration)
                    .unwrap_or_else(|| "timestamps unavailable".to_string()),
                if gpu.gpu_time.is_some() {
                    FOREGROUND
                } else {
                    MUTED
                },
            ))
            .child(reading(
                "GPU HOST",
                format!(
                    "pool wait {} · encode {}",
                    format_duration(gpu.frame_pool_wait_time),
                    format_duration(gpu.command_encoding_time),
                ),
                if gpu.frame_pool_wait_time > Duration::from_millis(1) {
                    WARN
                } else {
                    FOREGROUND
                },
            ))
            .child(reading(
                "H2D",
                format_bytes(gpu.host_to_device_bytes),
                if gpu.host_to_device_bytes == 0 {
                    GOOD
                } else {
                    FOREGROUND
                },
            ))
            .child(reading(
                "GPU WORK",
                format!(
                    "{} pass · {} cb · {} vec · {} stroke · {} bundle · {} grad · {} img",
                    gpu.passes_executed,
                    gpu.command_buffers,
                    gpu.vector_batches,
                    gpu.procedural_stroke_batches,
                    gpu.render_bundle_runs,
                    gpu.gradient_draws,
                    gpu.image_draws
                ),
                FOREGROUND,
            ))
            .child(reading(
                "COVERAGE",
                format!(
                    "{} · {} strokes · LOD {} · {} compute · {} stencil",
                    gpu.coverage_strategy,
                    gpu.retained_stroke_strategy,
                    gpu.selected_vector_lod,
                    gpu.compute_dispatches,
                    gpu.stencil_draws
                ),
                FOREGROUND,
            ))
            .child(reading(
                "CULL",
                format!(
                    "{} ordered op · {} bundled op · {} stroke ref · {}/{} indirect draw/run",
                    gpu.culled_operations,
                    gpu.culled_bundle_operations,
                    gpu.retained_stroke_candidate_references,
                    gpu.retained_stroke_indirect_draws,
                    gpu.retained_stroke_indirect_runs,
                ),
                if gpu.culled_operations > 0 || gpu.retained_stroke_cull_dispatches > 0 {
                    GOOD
                } else {
                    FOREGROUND
                },
            ))
            .child(reading(
                "VECTOR LOD",
                format!(
                    "requested {} · available {} · {}",
                    gpu.vector_lod_requested_scale,
                    gpu.vector_lod_available_scale,
                    if gpu.vector_lod_deferred {
                        "background pending"
                    } else {
                        "exact"
                    },
                ),
                if gpu.vector_lod_deferred { WARN } else { GOOD },
            ))
            .when(gpu.patch_lod_required_scale != "0.000", |section| {
                section.child(reading(
                    "PATCH LOD",
                    format!(
                        "required {} · available {} · {}",
                        gpu.patch_lod_required_scale,
                        gpu.patch_lod_available_scale,
                        if gpu.patch_lod_deferred {
                            "background pending"
                        } else {
                            "exact"
                        },
                    ),
                    if gpu.patch_lod_deferred { WARN } else { GOOD },
                ))
            })
            .child(reading(
                "COMPOSITOR",
                format!(
                    "{} offscreen · {} mask · {} composite · {} backdrop copies",
                    gpu.offscreen_passes,
                    gpu.mask_passes,
                    gpu.composite_passes,
                    gpu.backdrop_copies
                ),
                FOREGROUND,
            ))
            .child(reading(
                "TRANSIENT",
                format!(
                    "{} peak · {} resident · {} reuse",
                    format_bytes(gpu.transient_peak_bytes),
                    format_bytes(gpu.transient_resident_bytes),
                    gpu.transient_pool_reuses
                ),
                if gpu.transient_pool_reuses > 0 {
                    GOOD
                } else {
                    FOREGROUND
                },
            ))
            .child(reading(
                "REUSE",
                format!(
                    "{} reused · {} rendered · {} dirty · {} · source {} · refine {} ({}/{}) · pool {} · {} clip · gen {}",
                    gpu.reused_tiles,
                    gpu.rendered_tiles,
                    gpu.dirty_tiles,
                    gpu.presentation_quality,
                    if gpu.reprojected_frame {
                        "reprojected"
                    } else {
                        "live"
                    },
                    if gpu.refinement_pending {
                        "pending"
                    } else {
                        "exact"
                    },
                    gpu.scheduled_refinement_work,
                    gpu.refinement_work_budget,
                    if gpu.frame_pool_reused { "yes" } else { "no" },
                    gpu.clip_state_changes,
                    gpu.generation
                ),
                if gpu.frame_pool_reused || gpu.reused_tiles > 0 {
                    GOOD
                } else {
                    FOREGROUND
                },
            ))
            .child(reading(
                "FRAME POOL",
                format!(
                    "{} · {}",
                    format_bytes(gpu.frame_pool_capacity_bytes),
                    if gpu.frame_pool_grew {
                        "grew this frame"
                    } else {
                        "stable"
                    }
                ),
                if gpu.frame_pool_grew { WARN } else { GOOD },
            ));
    }

    for warning in page.gpu_warnings {
        section = section.child(reading(
            "WARNING",
            format!(
                "[{}:{}] frontend · {}",
                warning.feature, warning.code, warning.message
            ),
            WARN,
        ));
    }
    for fallback in page.gpu_fallbacks {
        section = section.child(reading(
            "FALLBACK",
            format!(
                "[{}:{}] {} · {}",
                fallback.feature, fallback.code, fallback.source, fallback.message
            ),
            WARN,
        ));
    }
    if let Some(failure) = page.gpu_failure {
        section = section.child(reading("GPU ERROR", failure, BAD));
    }
    if let Some(failure) = page.gpu_refinement_failure {
        section = section.child(reading("VECTOR REFINE", failure, WARN));
    }

    let text = match page.text_index_state {
        TextIndexDebugState::Ready => format!(
            "{}g {}l {}k",
            page.glyph_count, page.line_count, page.link_count
        ),
        TextIndexDebugState::Pending => "pending".to_string(),
        TextIndexDebugState::Indexing => "indexing".to_string(),
        TextIndexDebugState::Failed => "failed".to_string(),
    };
    let text_color = match page.text_index_state {
        TextIndexDebugState::Ready => FOREGROUND,
        TextIndexDebugState::Failed => BAD,
        TextIndexDebugState::Pending | TextIndexDebugState::Indexing => WARN,
    };
    section.child(reading("TEXT", text, text_color))
}

fn section_label(label: impl Into<String>) -> Div {
    div().pb(px(1.)).text_color(MUTED).child(label.into())
}

fn reading(label: &'static str, value: String, color: Hsla) -> Div {
    div()
        .flex()
        .w_full()
        .items_start()
        .gap_2()
        .py(px(1.))
        .child(div().flex_shrink_0().text_color(MUTED).child(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_color(color)
                .text_right()
                .child(soft_wrap_value(&value)),
        )
}

fn soft_wrap_value(value: &str) -> String {
    let mut wrapped = String::with_capacity(value.len());
    for character in value.chars() {
        wrapped.push(character);
        if matches!(character, ':' | '-' | '/' | '_' | '.' | '·') {
            wrapped.push('\u{200b}');
        }
    }
    wrapped
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.;
    const MIB: f64 = KIB * 1024.;
    const GIB: f64 = MIB * 1024.;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} B")
    }
}

fn format_duration(duration: Duration) -> String {
    if duration < Duration::from_millis(1) {
        format!("{} µs", duration.as_micros())
    } else if duration < Duration::from_secs(1) {
        format!("{:.1} ms", duration.as_secs_f64() * 1_000.)
    } else {
        format!("{:.2} s", duration.as_secs_f64())
    }
}

fn format_draw_duration(duration: Duration, bands: Option<usize>) -> String {
    match bands {
        Some(bands) if bands > 1 => format!("{} ×{bands}", format_duration(duration)),
        _ => format_duration(duration),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automated_trace_selects_its_owned_draw_and_present() {
        let before = Instant::now();
        let other_window = WindowId::from(41_u64);
        let traced_window = WindowId::from(42_u64);
        let stale = FrameTiming {
            window_id: traced_window,
            dirty_at: None,
            invalidations: 1,
            draw_start: before - Duration::from_millis(1),
            draw_end: before,
        };
        let owned = FrameTiming {
            window_id: traced_window,
            dirty_at: Some(before),
            invalidations: 2,
            draw_start: before + Duration::from_millis(1),
            draw_end: before + Duration::from_millis(2),
        };
        let events = [
            FrameEvent::Draw(stale),
            FrameEvent::Draw(FrameTiming {
                window_id: other_window,
                ..owned
            }),
            FrameEvent::Draw(owned),
            FrameEvent::Present(PresentTiming {
                window_id: traced_window,
                present_start: before + Duration::from_millis(2),
                present_end: before + Duration::from_millis(5),
                animation_interval: None,
            }),
        ];

        let frame = latest_owned_frame(&events, traced_window, before).unwrap();
        assert_eq!(frame.draw.draw_start, owned.draw_start);
        assert_eq!(
            frame.present.unwrap().present_duration(),
            Duration::from_millis(3)
        );
    }

    #[test]
    fn automated_trace_keeps_fixed_samples_while_slow_reference_work_drains() {
        assert!(!render_debug_trace_should_finish_settling(
            TRACE_SETTLING_FRAMES - 1,
            true,
            TRACE_SETTLING_TIMEOUT,
        ));
        assert!(!render_debug_trace_should_finish_settling(
            TRACE_SETTLING_FRAMES,
            false,
            TRACE_SETTLING_TIMEOUT - Duration::from_millis(1),
        ));
        assert!(render_debug_trace_should_finish_settling(
            TRACE_SETTLING_FRAMES,
            true,
            Duration::ZERO,
        ));
        assert!(render_debug_trace_should_finish_settling(
            TRACE_SETTLING_FRAMES,
            false,
            TRACE_SETTLING_TIMEOUT,
        ));
    }

    #[test]
    fn speculative_preparations_do_not_block_trace_settling() {
        assert_eq!(required_preparation_count(0, 5, 5), 0);
        assert_eq!(required_preparation_count(1, 5, 5), 1);
    }

    #[test]
    fn presentation_geometry_guard_rejects_stale_zoom_bounds() {
        let page = [100.0, 50.0, 1_100.0, 850.0];
        assert!(debug_bounds_contain(page, [100.0, 50.0, 1_100.0, 850.0]));
        assert!(debug_bounds_contain(page, [200.0, 100.0, 900.0, 700.0]));
        assert!(!debug_bounds_contain(page, [0.0, 0.0, 8_000.0, 12_000.0]));
    }

    #[test]
    fn scripted_trace_report_records_reproducible_workload_metadata() {
        let report = render_debug_trace_report_json(&RenderDebugTraceReport {
            kind: RenderDebugTraceKind::Zoom,
            automated: true,
            planned_interaction_frames: 120,
            completed_interaction_frames: 120,
            planned_settling_frames: 30,
            completed_settling_frames: 30,
            warmup_duration: Duration::from_millis(42),
            scroll_pixels_per_frame: 0.0,
            markers: vec![RenderDebugTraceMarker {
                phase: "forward",
                frame_index: 0,
                offset: Duration::from_millis(42),
            }],
            cancelled: false,
            failure: None,
            original_position: DocumentPosition::fixed(1.0, 2.0, 1.25, 3),
            final_position: Some(DocumentPosition::fixed(1.0, 2.0, 1.25, 3)),
            observations: RenderDebugTraceObservations::default(),
            frames: Vec::new(),
        });

        assert_eq!(report["version"], TRACE_REPORT_VERSION);
        assert_eq!(report["kind"], "zoom");
        assert_eq!(report["driver"], "fixed-deadline-draw");
        assert_eq!(report["sample_scope"], "driver-owned-draws");
        assert_eq!(report["planned_interaction_frames"], 120);
        assert_eq!(report["warmup_ms"], 42.0);
        assert_eq!(report["scroll_pixels_per_frame"], 0.0);
        assert_eq!(report["phase_markers"][0]["phase"], "forward");
        assert_eq!(report["original_position"]["page"], 3);
        assert_eq!(report["observations"]["fallback_frames"], 0);
    }

    #[test]
    fn rendering_reports_are_created_without_overwriting_an_existing_capture() {
        let directory = std::env::temp_dir().join(format!(
            "glyphst-render-trace-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let report = json!({
            "captured_at": "2026-08-28T12:34:56Z",
            "trace": { "kind": "zoom" },
            "document": { "start": {
                "source_path": "/tmp/a document.pdf",
                "configured_engine": "glyphst"
            }}
        });
        let first = persist_render_debug_report_to("{}", &report, &directory).unwrap();
        let second = persist_render_debug_report_to("{}", &report, &directory).unwrap();

        assert_ne!(first, second);
        assert_eq!(fs::read_to_string(&first).unwrap(), "{}\n");
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("zoom")
        );

        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn automated_rendering_report_uses_the_exact_output_once() {
        let directory = std::env::temp_dir().join(format!(
            "glyphst-automated-render-trace-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let output = directory.join("nested").join("trace.json");
        persist_render_debug_report_explicit("{\"schema\":3}", &output).unwrap();
        assert_eq!(fs::read_to_string(&output).unwrap(), "{\"schema\":3}\n");

        let error = persist_render_debug_report_explicit("different", &output).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&output).unwrap(), "{\"schema\":3}\n");

        fs::remove_file(output).unwrap();
        fs::remove_dir(directory.join("nested")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn debug_units_stay_compact() {
        assert_eq!(format_bytes(768), "768 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_duration(Duration::from_micros(850)), "850 µs");
        assert_eq!(format_duration(Duration::from_micros(12_250)), "12.2 ms");
        assert_eq!(
            format_draw_duration(Duration::from_micros(12_250), Some(8)),
            "12.2 ms ×8"
        );
        assert_eq!(
            format_draw_duration(Duration::from_micros(12_250), Some(1)),
            "12.2 ms"
        );
    }

    #[test]
    fn debug_values_gain_invisible_break_opportunities() {
        assert_eq!(
            soft_wrap_value("text-extraction:text-font-metrics-unavailable"),
            "text-\u{200b}extraction:\u{200b}text-\u{200b}font-\u{200b}metrics-\u{200b}unavailable"
        );
    }

    #[test]
    fn duration_statistics_use_nearest_rank_percentiles() {
        let statistics = duration_statistics((1..=100).map(Duration::from_millis)).unwrap();
        assert_eq!(statistics.count, 100);
        assert_eq!(statistics.mean_ms, 50.5);
        assert_eq!(statistics.p50_ms, 50.0);
        assert_eq!(statistics.p95_ms, 95.0);
        assert_eq!(statistics.max_ms, 100.0);
    }

    #[test]
    fn frame_report_separates_draw_and_pacing_overruns() {
        let samples = [
            CapturedFrameSample {
                offset: Duration::ZERO,
                interval: None,
                draw: Duration::from_millis(2),
                present: Some(Duration::from_millis(1)),
                work: Duration::from_millis(3),
                dirty_to_draw: Some(Duration::from_millis(3)),
                invalidations: 1,
            },
            CapturedFrameSample {
                offset: Duration::from_millis(8),
                interval: Some(Duration::from_millis(8)),
                draw: Duration::from_millis(9),
                present: Some(Duration::from_millis(2)),
                work: Duration::from_millis(11),
                dirty_to_draw: Some(Duration::from_millis(10)),
                invalidations: 3,
            },
            CapturedFrameSample {
                offset: Duration::from_millis(20),
                interval: Some(Duration::from_millis(12)),
                draw: Duration::from_millis(2),
                present: None,
                work: Duration::from_millis(2),
                dirty_to_draw: None,
                invalidations: 2,
            },
        ];
        let report = frame_report(&samples);
        assert_eq!(report["total_frames"], 3);
        assert_eq!(report["effective_fps"], 100.0);
        assert_eq!(report["draw"]["p95_ms"], 9.0);
        assert_eq!(report["present"]["p95_ms"], 2.0);
        assert_eq!(report["work"]["p95_ms"], 11.0);
        assert_eq!(report["interval"]["p95_ms"], 12.0);
        assert_eq!(report["dirty_to_draw"]["count"], 2);
        assert_eq!(report["invalidations"]["p95"], 3);
        assert_eq!(report["budget"]["draw_overruns"], 1);
        assert_eq!(report["budget"]["work_overruns"], 1);
        assert_eq!(report["budget"]["interval_overruns"], 1);
    }

    #[test]
    fn frame_report_bounds_raw_samples_without_losing_aggregate_count() {
        let samples = (0..MAX_RAW_FRAME_SAMPLES + 4)
            .map(|index| CapturedFrameSample {
                offset: Duration::from_millis(index as u64),
                interval: (index > 0).then_some(Duration::from_millis(1)),
                draw: Duration::from_micros(500),
                present: Some(Duration::from_micros(250)),
                work: Duration::from_micros(750),
                dirty_to_draw: Some(Duration::from_micros(750)),
                invalidations: 1,
            })
            .collect::<Vec<_>>();
        let report = frame_report(&samples);
        assert_eq!(report["total_frames"], MAX_RAW_FRAME_SAMPLES + 4);
        assert_eq!(report["raw_samples_retained"], MAX_RAW_FRAME_SAMPLES);
        assert_eq!(report["raw_samples_truncated"], true);
        assert_eq!(report["samples"][0]["offset_ms"], 4.0);
    }

    #[test]
    fn gpu_cache_report_includes_continuity_surfaces_and_gap_counters() {
        let cache = GpuCacheDebugSnapshot {
            continuity_surface_count: 2,
            continuity_surface_bytes: 12_582_912,
            surface_continuity_requests: 7,
            surface_continuity_completed: 5,
            surface_continuity_hits: 11,
            surface_partial_reuses: 3,
            surface_presentation_gaps: 1,
            surface_incomplete_suppressed: 13,
            surface_refinement_submissions: 13,
            surface_atomic_swaps: 2,
            surface_zoom_motion_reprojections: 120,
            surface_zoom_motion_refreshes_suppressed: 17,
            surface_zoom_motion_cancellations: 2,
            surface_zoom_emergency_renders: 0,
            surface_zoom_live_redraws: 119,
            surface_zoom_cached_sharp_presentations: 118,
            surface_zoom_live_failures: 0,
            surface_zoom_gpu_holds: 18,
            surface_zoom_animation_steps: 120,
            surface_zoom_admission_holds: 1,
            surface_zoom_worker_holds: 1,
            surface_zoom_sample_holds: 0,
            surface_zoom_presentation_holds: 119,
            surface_zoom_scaled_presentations: 0,
            surface_zoom_target_reuses: 119,
            surface_zoom_target_allocations: 0,
            motion_render_jobs_started: 120,
            motion_render_jobs_completed: 119,
            motion_render_jobs_superseded: 1,
            motion_render_jobs_pending: 0,
            motion_render_jobs_peak: 1,
            motion_render_detached_target_reuses: 119,
            motion_render_inline_frames: 118,
            motion_render_inline_failures: 0,
            motion_render_callback_target_size: [1427, 1756],
            motion_render_callback_passes_executed: 6,
            motion_render_callback_coverage_strategy: Some(
                GpuVectorCoverageStrategy::RetainedTriangles,
            ),
            motion_render_callback_presentation_quality: Some(GpuPresentationQuality::ErrorBounded),
            motion_render_callback_transient_peak_bytes: 154_140_672,
            motion_render_callback_transient_resident_bytes: 154_140_672,
            motion_render_callback_selected_stroke_lod: 2,
            motion_render_callback_active_stroke_lod_levels: 4,
            motion_render_callback_selected_stroke_segments: 48_000,
            motion_render_callback_target_visible_stroke_segments: 50_000,
            motion_render_callback_visible_stroke_tiles: 21,
            motion_render_callback_max_visible_stroke_tile_segments: 712,
            camera_pan_inline_frames: 42,
            camera_pan_admission_holds: 3,
            motion_render_camera_batch_commits: 117,
            camera_target_reservations_started: 120,
            camera_target_reservations_consumed: 119,
            camera_target_reservations_released: 1,
            camera_target_reservations_pending: 0,
            motion_render_queue_latency_p95: Duration::from_micros(1_250),
            motion_render_queue_latency_max: Duration::from_micros(2_500),
            motion_render_execution_latency_p95: Duration::from_micros(2_000),
            motion_render_execution_latency_max: Duration::from_micros(3_000),
            motion_render_total_latency_p95: Duration::from_micros(3_250),
            motion_render_total_latency_max: Duration::from_micros(5_500),
            zoom_motion_active: true,
            refinement_pages: 1,
            retained_refinement_pages: 4,
            ..Default::default()
        };

        let report = gpu_cache_json(&cache);
        assert_eq!(report["surfaces"]["continuity_count"], 2);
        assert_eq!(report["surfaces"]["continuity_bytes"], 12_582_912);
        assert_eq!(report["surfaces"]["continuity_requests"], 7);
        assert_eq!(report["surfaces"]["continuity_completed"], 5);
        assert_eq!(report["surfaces"]["continuity_hits"], 11);
        assert_eq!(report["surfaces"]["partial_reuses"], 3);
        assert_eq!(report["surfaces"]["presentation_gaps"], 1);
        assert_eq!(report["surfaces"]["incomplete_suppressed"], 13);
        assert_eq!(report["surfaces"]["refinement_submissions"], 13);
        assert_eq!(report["surfaces"]["atomic_swaps"], 2);
        assert_eq!(report["surfaces"]["zoom_motion"]["reprojections"], 120);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["refreshes_suppressed"],
            17
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["cancellations"], 2);
        assert_eq!(report["surfaces"]["zoom_motion"]["emergency_renders"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["live_redraws"], 119);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["cached_sharp_presentations"],
            118
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["live_failures"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["gpu_holds"], 18);
        assert_eq!(report["surfaces"]["zoom_motion"]["animation_steps"], 120);
        assert_eq!(report["surfaces"]["zoom_motion"]["admission_holds"], 1);
        assert_eq!(report["surfaces"]["zoom_motion"]["worker_holds"], 1);
        assert_eq!(report["surfaces"]["zoom_motion"]["sample_holds"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["presentation_holds"], 119);
        assert_eq!(report["surfaces"]["zoom_motion"]["scaled_presentations"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["target_reuses"], 119);
        assert_eq!(report["surfaces"]["zoom_motion"]["target_allocations"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["jobs_started"], 120);
        assert_eq!(report["surfaces"]["zoom_motion"]["jobs_completed"], 119);
        assert_eq!(report["surfaces"]["zoom_motion"]["jobs_superseded"], 1);
        assert_eq!(report["surfaces"]["zoom_motion"]["jobs_pending"], 0);
        assert_eq!(report["surfaces"]["zoom_motion"]["jobs_peak"], 1);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["detached_target_reuses"],
            119
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["reservations_started"],
            120
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["reservations_consumed"],
            119
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["reservations_released"],
            1
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["reservations_pending"], 0);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["worker_latency"]["queue_p95_ms"],
            1.25
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["worker_latency"]["queue_max_ms"],
            2.5
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["worker_latency"]["execution_p95_ms"],
            2.0
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["worker_latency"]["total_max_ms"],
            5.5
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["pan_inline_frames"], 42);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["ui_thread_submissions"],
            118
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_target_size"],
            json!([1427, 1756])
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["callback_passes"], 6);
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_coverage_strategy"],
            "RetainedTriangles"
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_presentation_quality"],
            "ErrorBounded"
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_transient_peak_bytes"],
            154_140_672
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_transient_resident_bytes"],
            154_140_672
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_selected_stroke_lod"],
            2
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_selected_stroke_segments"],
            48_000
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["callback_visible_stroke_tiles"],
            21
        );
        assert_eq!(
            report["surfaces"]["zoom_motion"]["camera_batch_commits"],
            117
        );
        assert_eq!(report["surfaces"]["zoom_motion"]["pan_admission_holds"], 3);
        assert_eq!(report["surfaces"]["zoom_motion"]["active"], true);
        assert_eq!(report["surfaces"]["refinement_pages"], 1);
        assert_eq!(report["surfaces"]["retained_refinement_pages"], 4);
    }

    #[test]
    fn gpu_counter_delta_marks_resets_instead_of_inventing_progress() {
        let start = GpuCacheDebugSnapshot {
            surface_renders_started: 12,
            surface_zoom_reuses: 9,
            surface_continuity_hits: 4,
            ..Default::default()
        };
        let end = GpuCacheDebugSnapshot {
            surface_renders_started: 15,
            surface_zoom_reuses: 2,
            surface_continuity_hits: 10,
            ..Default::default()
        };
        let delta = gpu_counter_delta_json(&start, &end);
        assert_eq!(delta["counters"]["surface_renders_started"], 3);
        assert_eq!(delta["counters"]["surface_continuity_hits"], 6);
        assert!(delta["counters"]["surface_zoom_reuses"].is_null());
        assert!(
            delta["reset_fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "surface_zoom_reuses")
        );
    }

    #[test]
    fn non_finite_interaction_values_are_json_null() {
        assert!(finite_f32_json(f32::NAN).is_null());
        assert!(finite_f32_json(f32::INFINITY).is_null());
        assert_eq!(finite_f32_json(1.25), 1.25);
    }
}
