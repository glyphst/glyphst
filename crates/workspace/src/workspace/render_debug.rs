use glyphst_document_ui::document_view::{
    DocumentRenderDebugSnapshot, PageRenderBackendDebug, PageRenderDebugSnapshot,
    PageRenderDebugStatus, TextIndexDebugState, render_cache_debug_snapshot,
};
use gpui::{
    AnyElement, App, Div, Hsla, IntoElement as _, ParentElement as _, Styled as _, WeakEntity,
    Window, div, hsla, prelude::FluentBuilder as _, px,
};

use super::*;

const FOREGROUND: Hsla = hsla(0., 0., 0.98, 1.);
const MUTED: Hsla = hsla(0., 0., 0.62, 1.);
const GOOD: Hsla = hsla(0.39, 0.70, 0.62, 1.);
const WARN: Hsla = hsla(0.12, 0.85, 0.62, 1.);
const BAD: Hsla = hsla(0.00, 0.78, 0.67, 1.);
const BORDER: Hsla = hsla(0., 0., 1., 0.14);

impl Workspace {
    pub(super) fn toggle_rendering_debug(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rendering_debug.take().is_none() {
            let workspace = cx.weak_entity();
            self.rendering_debug = Some(cx.new(|cx| {
                FpsMonitor::new(window, cx)
                    .continuous(false)
                    .footer(move |window, cx| render_footer(workspace.clone(), window, cx))
            }));
        }
        cx.notify();
    }
}

fn render_footer(
    workspace: WeakEntity<Workspace>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let cache = render_cache_debug_snapshot(cx);
    let active_document = workspace.upgrade().and_then(|workspace| {
        let workspace = workspace.read(cx);
        (workspace.surface_mode == WorkspaceSurfaceMode::Panes)
            .then(|| workspace.active_pane.read(cx).active_document_view())
            .flatten()
    });
    let document = active_document.map(|document| document.read(cx).render_debug_snapshot(cx));

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
        .into_any_element()
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
            "GPU RESIDENT",
            format_bytes(document.gpu_cache.resident_bytes),
            FOREGROUND,
        ))
        .child(reading(
            "GPU SURFACES",
            format!(
                "{} / {}  ×{}",
                format_bytes(document.gpu_cache.surface_bytes),
                format_bytes(document.gpu_cache.surface_byte_limit),
                document.gpu_cache.surface_count
            ),
            FOREGROUND,
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
        PageRenderDebugStatus::Fallback {
            exact_pending: true,
        } => ("fallback → pending", WARN),
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
        PageRenderBackendDebug::MuPdfWhileGpuPending => ("MuPDF · GPU pending", WARN),
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
                    "{} op · {} vec batch · {} LOD · {} clip",
                    preparation.retained_operations,
                    preparation.vector_batches,
                    preparation.vector_lod_levels,
                    preparation.unique_clip_paths
                ),
                FOREGROUND,
            ));
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
                    "{} · {} vertex · {} ref · {} instance",
                    format_bytes(preparation.vector_bytes),
                    preparation.vector_vertices,
                    preparation.vector_triangle_references,
                    preparation.vector_instances
                ),
                FOREGROUND,
            ));
        }
        if preparation.hairline_bytes > 0 {
            section = section.child(reading(
                "HAIRLINES",
                format!(
                    "{} · {} batch · {} LOD · {} primitive · {} draw",
                    format_bytes(preparation.hairline_bytes),
                    preparation.hairline_batches,
                    preparation.hairline_lod_levels,
                    preparation.hairline_primitives,
                    preparation.hairline_draws
                ),
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
                    "{} pass · {} cb · {} vec · {} hair · {} bundle · {} grad · {} img",
                    gpu.passes_executed,
                    gpu.command_buffers,
                    gpu.vector_batches,
                    gpu.hairline_batches,
                    gpu.render_bundle_runs,
                    gpu.gradient_draws,
                    gpu.image_draws
                ),
                FOREGROUND,
            ))
            .child(reading(
                "COVERAGE",
                format!(
                    "{} · LOD {} · {} compute · {} stencil",
                    gpu.coverage_strategy,
                    gpu.selected_vector_lod,
                    gpu.compute_dispatches,
                    gpu.stencil_draws
                ),
                FOREGROUND,
            ))
            .child(reading(
                "COMPOSITOR",
                format!(
                    "{} offscreen · {} mask · {} composite",
                    gpu.offscreen_passes, gpu.mask_passes, gpu.composite_passes
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
                    "{} reused · {} rendered · {} dirty · refine {} · pool {} · {} clip · gen {}",
                    gpu.reused_tiles,
                    gpu.rendered_tiles,
                    gpu.dirty_tiles,
                    if gpu.refinement_pending {
                        "pending"
                    } else {
                        "exact"
                    },
                    if gpu.frame_pool_reused { "yes" } else { "no" },
                    gpu.clip_state_changes,
                    gpu.generation
                ),
                if gpu.frame_pool_reused || gpu.reused_tiles > 0 {
                    GOOD
                } else {
                    FOREGROUND
                },
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
}
