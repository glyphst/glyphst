//! Mermaid diagrams: rendered to SVG on a worker thread (merman, through
//! glyphst-mermaid-render) coloured from the theme, then rasterised with
//! resvg against Inter, the CJK checkout and the installed fonts (egui's
//! SVG loader asks for Arial, so labels vanished where it isn't
//! installed). The source shows as code until the diagram is ready, or if
//! it fails.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock},
};

use egui::{
    Color32, ColorImage, Frame, Margin, Rect, Sense, Stroke, TextureHandle, TextureOptions, Ui,
    vec2,
};
use glyphst_mermaid_render::{
    MermaidTheme,
    color::{Hsla, Rgba},
};
use resvg::{tiny_skia, usvg};

use crate::MarkdownStyle;

/// Longer sources stay code: layout time grows with the diagram.
const MAX_SOURCE: usize = 16 * 1024;
/// The smallest a diagram is drawn to fit the column.
const MIN_SCALE: f32 = 0.75;
/// Diagrams kept before the cache starts over.
const CACHE_SIZE: usize = 64;

#[derive(Clone)]
enum State {
    Rendering,
    /// The diagram and its size in points.
    Ready(TextureHandle, egui::Vec2),
    Failed,
}

#[derive(Clone, Default)]
struct Cache(Arc<Mutex<HashMap<u64, State>>>);

/// One render at a time: merman's layout is CPU-heavy, and a chat can hold
/// many diagrams.
static RENDERING: Mutex<()> = Mutex::new(());

/// Draws the diagram of `source`; false while it renders or when it can't,
/// for the caller to show the source instead.
pub(crate) fn show(ui: &mut Ui, style: &MarkdownStyle, source: &str) -> bool {
    if source.len() > MAX_SOURCE {
        return false;
    }
    let ctx = ui.ctx().clone();
    let pixels_per_point = ctx.pixels_per_point();
    let mut hasher = std::hash::DefaultHasher::new();
    (
        source,
        style.dark,
        style.text,
        style.accent,
        style.block_bg,
        pixels_per_point.to_bits(),
    )
        .hash(&mut hasher);
    let key = hasher.finish();
    let cache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Cache>(egui::Id::unique("glyphst-markdown-mermaid"))
            .clone()
    });
    let state = {
        let mut diagrams = cache
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if diagrams.len() >= CACHE_SIZE && !diagrams.contains_key(&key) {
            diagrams.retain(|_, state| matches!(state, State::Rendering));
        }
        diagrams
            .entry(key)
            .or_insert_with(|| {
                let theme = theme(style);
                let source = source.to_owned();
                let cache = cache.clone();
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let _rendering = RENDERING
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let state = match render(&source, &theme, pixels_per_point) {
                        Ok(image) => {
                            let size =
                                vec2(image.size[0] as f32, image.size[1] as f32) / pixels_per_point;
                            let texture = ctx.load_texture(
                                format!("mermaid-{key:x}"),
                                image,
                                TextureOptions::LINEAR,
                            );
                            State::Ready(texture, size)
                        }
                        Err(error) => {
                            tracing::debug!("mermaid diagram failed: {error}");
                            State::Failed
                        }
                    };
                    cache
                        .0
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(key, state);
                    ctx.request_repaint();
                });
                State::Rendering
            })
            .clone()
    };
    let State::Ready(texture, size) = state else {
        return false;
    };
    Frame::new()
        .fill(style.block_bg)
        .stroke(Stroke::new(1.0, style.rule))
        .corner_radius(style.radius)
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            let width = ui.available_width();
            // Shrunk to fit the column, never enlarged, and never so far
            // that labels can't be read: past that it scrolls.
            let shown = size * (width / size.x).clamp(MIN_SCALE, 1.0);
            let draw = |ui: &mut Ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(width.max(shown.x), shown.y), Sense::hover());
                ui.painter().image(
                    texture.id(),
                    Rect::from_center_size(rect.center(), shown),
                    Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            };
            if shown.x > width {
                egui::ScrollArea::horizontal()
                    .id_salt(ui.next_auto_id())
                    .show(ui, draw);
            } else {
                draw(ui);
            }
        });
    true
}

/// The diagram of `source` at `pixels_per_point`.
fn render(source: &str, theme: &MermaidTheme, pixels_per_point: f32) -> Result<ColorImage, String> {
    let svg = glyphst_mermaid_render::render_to_svg(source, theme)
        .map_err(|error| format!("{error:#}"))?;
    let options = usvg::Options {
        fontdb: fonts(),
        ..usvg::Options::default()
    };
    let tree =
        usvg::Tree::from_data(svg.as_bytes(), &options).map_err(|error| error.to_string())?;
    let size = tree
        .size()
        .to_int_size()
        .scale_by(pixels_per_point)
        .ok_or("empty diagram")?;
    let mut pixmap =
        tiny_skia::Pixmap::new(size.width(), size.height()).ok_or("diagram too large")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(pixels_per_point, pixels_per_point),
        &mut pixmap.as_mut(),
    );
    Ok(ColorImage::from_rgba_premultiplied(
        [size.width() as usize, size.height() as usize],
        pixmap.data(),
    ))
}

/// Inter (also as `sans-serif`), the CJK faces of the glyphst-fonts
/// checkout and the installed fonts, loaded once.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = usvg::fontdb::Database::new();
            fonts.load_font_data(glyphst_ui::fonts::INTER_REGULAR.to_vec());
            fonts.load_font_data(glyphst_ui::fonts::INTER_SEMIBOLD.to_vec());
            if let Some(dir) = glyphst_core::paths::fonts_dir() {
                for file in [
                    glyphst_ui::fonts::CJK_REGULAR_FILE,
                    glyphst_ui::fonts::CJK_BOLD_FILE,
                ] {
                    // Missing files leave the installed CJK fonts to fall back on.
                    let _ = fonts.load_font_file(dir.join(file));
                }
            }
            fonts.load_system_fonts();
            fonts.set_sans_serif_family("Inter");
            Arc::new(fonts)
        })
        .clone()
}

fn theme(style: &MarkdownStyle) -> MermaidTheme {
    let color = |color: Color32| {
        let [r, g, b, a] = color
            .to_srgba_unmultiplied()
            .map(|channel| f32::from(channel) / 255.0);
        Hsla::from(Rgba { r, g, b, a })
    };
    let accent = color(style.accent);
    let branch = |index: usize| Hsla {
        h: (accent.h + index as f32 / 8.0).fract(),
        ..accent
    };
    let git_branch_colors: [Hsla; 8] = std::array::from_fn(branch);
    MermaidTheme {
        dark_mode: style.dark,
        font_family: "Inter, \"Noto Sans CJK SC\", sans-serif".into(),
        background: color(style.block_bg),
        primary_color: color(style.code_bg),
        primary_text_color: color(style.strong_text),
        primary_border_color: color(style.rule_strong),
        secondary_color: color(style.code_bg),
        tertiary_color: color(style.block_bg),
        line_color: color(style.muted),
        text_color: color(style.text),
        edge_label_background: color(style.block_bg),
        cluster_background: color(style.block_bg),
        cluster_border: color(style.rule),
        note_background: Hsla { a: 0.18, ..accent },
        note_border: accent,
        actor_background: color(style.code_bg),
        actor_border: color(style.rule_strong),
        activation_background: color(style.code_bg),
        activation_border: color(style.rule_strong),
        git_branch_colors,
        git_branch_label_colors: git_branch_colors
            .map(glyphst_mermaid_render::text_color_for_background),
        er_attr_bg_odd: color(style.code_bg),
        er_attr_bg_even: color(style.block_bg),
        error_color: color(style.error),
        warning_color: accent,
        accent_colors: Vec::new(),
    }
}
