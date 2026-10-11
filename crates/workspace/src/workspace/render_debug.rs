use glyphst_document_ui::document_view::{
    DocumentRenderDebugSnapshot, PageRenderDebugSnapshot, PageRenderDebugStatus,
    RenderCacheDebugSnapshot, TextIndexDebugState, render_cache_debug_snapshot,
};
use gpui::{
    AnyElement, App, ClipboardItem, Div, FrameEvent, FrameTiming, FrameTimingCollector, Hsla,
    InteractiveElement as _, IntoElement as _, ParentElement as _, PresentTiming,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, WindowId, deferred, div,
    hsla, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
};
use serde_json::{Value, json};
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
const RENDER_DEBUG_PRIORITY: usize = 25;

struct RenderDebugCaptureNotification;

pub(super) struct RenderDebugCapture {
    collector: FrameTimingCollector,
    window_id: WindowId,
    started_at: Instant,
    captured_at: String,
    document: WeakEntity<DocumentView>,
    document_entity_id: u64,
    document_start: DocumentRenderDebugSnapshot,
    render_cache_start: RenderCacheDebugSnapshot,
}

impl Workspace {
    /// The rendering HUD: GPUI Kit's FPS overlay plus Glyphst's cache, page
    /// and capture diagnostics in a panel of their own.
    pub(super) fn render_rendering_debug(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let monitor = self.rendering_debug.clone()?;
        let workspace = cx.weak_entity();
        Some(
            deferred(
                div()
                    .absolute()
                    .inset_0()
                    .child(FpsOverlay::new(&monitor).frame_budget(FRAME_BUDGET))
                    .child(
                        div()
                            .id("render-debug-panel")
                            .absolute()
                            .bottom(px(8.))
                            .right(px(8.))
                            .w(px(420.))
                            .max_h(relative(0.6))
                            .overflow_y_scroll()
                            .p_2()
                            .rounded(px(6.))
                            .bg(hsla(0., 0., 0.08, 0.88))
                            .text_xs()
                            .text_color(FOREGROUND)
                            .occlude()
                            .occlude()
                            .child(render_footer(
                                workspace,
                                self.active_render_debug_document(cx),
                                self.rendering_debug_capture.is_some(),
                                cx,
                            )),
                    ),
            )
            // Above the application's floating overlays, below Kit
            // notifications, so the capture controls receive their clicks.
            .with_priority(RENDER_DEBUG_PRIORITY)
            .into_any_element(),
        )
    }

    pub(super) fn toggle_rendering_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rendering_debug.is_some() {
            self.rendering_debug = None;
            self.rendering_debug_capture = None;
        } else {
            self.rendering_debug = Some(cx.new(|cx| {
                FpsMonitor::new(window, cx)
                    .capacity(240)
                    .frame_budget(FRAME_BUDGET)
            }));
        }
        cx.notify();
    }

    pub(super) fn active_render_debug_document(&self, cx: &App) -> Option<Entity<DocumentView>> {
        (self.surface_mode == WorkspaceSurfaceMode::Panes)
            .then(|| self.active_pane(cx).active_document_view(cx))
            .flatten()
    }

    pub(super) fn start_render_debug_capture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rendering_debug_capture.is_some() {
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
        self.rendering_debug_capture = Some(RenderDebugCapture {
            collector: FrameTimingCollector::new(),
            window_id: window.window_handle().window_id(),
            started_at: Instant::now(),
            captured_at: chrono::Utc::now().to_rfc3339(),
            document_entity_id: document.entity_id().as_u64(),
            document: document.downgrade(),
            document_start,
            render_cache_start: render_cache_debug_snapshot(cx),
        });
        cx.notify();
    }

    pub(super) fn finish_render_debug_capture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut capture) = self.rendering_debug_capture.take() else {
            return;
        };
        let timings = captured_frame_timings(
            &capture.collector.collect_unseen(),
            capture.window_id,
            capture.started_at,
        );
        let frame_count = timings.len();
        let report = capture_report(&capture, timings, self, window, cx);
        match serde_json::to_string_pretty(&report) {
            Ok(serialized) => {
                cx.write_to_clipboard(ClipboardItem::new_string(serialized.clone()));
                #[cfg(not(test))]
                let saved = persist_render_debug_report(&serialized, &report);
                #[cfg(test)]
                let saved: io::Result<Option<PathBuf>> = Ok(None);
                let notification = if frame_count == 0 {
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
                window.push_notification(
                    Notification::error(format!("Failed to serialize rendering capture: {error}"))
                        .title("Rendering capture failed")
                        .id::<RenderDebugCaptureNotification>(),
                    cx,
                );
            }
        }
        cx.notify();
    }
}

#[cfg_attr(test, allow(dead_code))]
fn persist_render_debug_report(serialized: &str, report: &Value) -> io::Result<Option<PathBuf>> {
    let directory = glyphst_core::paths::state_dir().join("render-captures");
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
    let base = format!("{timestamp}-{source}");
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
    active_document: Option<Entity<DocumentView>>,
    capture_active: bool,
    cx: &mut App,
) -> AnyElement {
    let cache = render_cache_debug_snapshot(cx);
    let document = active_document.map(|document| document.read(cx).render_debug_snapshot(cx));
    let document_available = document.is_some();

    div()
        .mt_1()
        .pt_1()
        .border_t_1()
        .border_color(BORDER)
        .flex()
        .flex_col()
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
        // Last, so the bottom-anchored panel keeps the control in place while
        // the readings above it change.
        .child(render_capture_controls(
            workspace,
            capture_active,
            document_available,
        ))
        .into_any_element()
}

fn render_capture_controls(
    workspace: WeakEntity<Workspace>,
    capture_active: bool,
    document_available: bool,
) -> Div {
    div()
        .flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .pb_1()
        .child(
            div()
                .text_color(if capture_active { WARN } else { MUTED })
                .child(if capture_active {
                    "capturing"
                } else {
                    "FRAME CAPTURE"
                }),
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
                .label(if capture_active {
                    "Stop & Copy"
                } else {
                    "Start Capture"
                })
                .debug_selector(|| "render-debug-capture".to_string())
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let Some(workspace) = workspace.upgrade() else {
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
    let capture_duration = Instant::now().saturating_duration_since(capture.started_at);
    let samples = captured_frame_samples(&timings);
    let document_end = capture
        .document
        .upgrade()
        .map(|document| document.read(cx).render_debug_snapshot(cx));
    let active_document_entity_id = workspace
        .active_render_debug_document(cx)
        .map(|document| document.entity_id().as_u64());
    let gpu_specs = window.gpu_specs();

    json!({
        "schema_version": 20,
        "kind": "glyphst-reader-render-capture",
        "captured_at": capture.captured_at,
        "capture_duration_ms": milliseconds(capture_duration),
        "frame_budget_ms": milliseconds(FRAME_BUDGET),
        "application": {
            "package": env!("CARGO_PKG_NAME"),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
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
            "end": render_cache_json(render_cache_debug_snapshot(cx)),
        },
        "document": {
            "entity_id": capture.document_entity_id,
            "active_at_end": active_document_entity_id == Some(capture.document_entity_id),
            "released_before_end": document_end.is_none(),
            "start": document_snapshot_json(&capture.document_start),
            "end": document_end.as_ref().map(document_snapshot_json),
        },
    })
}

fn document_snapshot_json(document: &DocumentRenderDebugSnapshot) -> Value {
    let interaction = &document.interaction;
    json!({
        "source_path": interaction.source_path.to_string_lossy(),
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
        "pages": document.pages.iter().map(page_snapshot_json).collect::<Vec<_>>(),
    })
}

fn page_snapshot_json(page: &PageRenderDebugSnapshot) -> Value {
    json!({
        "page_number": page.page_number,
        "status": page_status_json(page.status),
        "requested_raster": page.requested_raster.map(raster_json),
        "displayed_raster": page.displayed_raster.map(raster_json),
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
        "text": {
            "state": text_index_state_name(page.text_index_state),
            "glyphs": page.glyph_count,
            "lines": page.line_count,
            "links": page.link_count,
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
        PageRenderDebugStatus::Fallback { exact_pending } => {
            json!({ "kind": "fallback", "exact_pending": exact_pending })
        }
        PageRenderDebugStatus::Pending => json!({ "kind": "pending" }),
        PageRenderDebugStatus::Failed => json!({ "kind": "failed" }),
        PageRenderDebugStatus::Unavailable => json!({ "kind": "unavailable" }),
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
        ));
    for page in document.pages {
        this = this.child(render_page(page));
    }
    this
}

fn render_page(page: PageRenderDebugSnapshot) -> Div {
    let (status, status_color) = match page.status {
        PageRenderDebugStatus::Exact => ("exact", GOOD),
        PageRenderDebugStatus::Fallback {
            exact_pending: true,
        } => ("placeholder → exact", WARN),
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
    fn rendering_reports_are_created_without_overwriting_an_existing_capture() {
        let directory = std::env::temp_dir().join(format!(
            "glyphst-render-capture-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let report = json!({
            "captured_at": "2026-08-28T12:34:56Z",
            "document": { "start": {
                "source_path": "/tmp/a document.pdf"
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
                .contains("a-document")
        );

        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
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
    fn non_finite_interaction_values_are_json_null() {
        assert!(finite_f32_json(f32::NAN).is_null());
        assert!(finite_f32_json(f32::INFINITY).is_null());
        assert_eq!(finite_f32_json(1.25), 1.25);
    }
}
