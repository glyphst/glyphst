//! LaTeX formulas drawn with RaTeX (KaTeX's layout and fonts), rasterised
//! once per source, size and colour and kept as textures.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions, Vec2};
use ratex_types::{Color, MathStyle};

/// Formulas longer than this are shown as source: layout time grows with
/// the input, and nobody reads a page-long formula inline.
const MAX_SOURCE: usize = 4_000;
/// Transparent pixels around the image, so glyph overhangs aren't cut.
const PAD_PX: f32 = 2.0;
/// Formulas kept before the least recently drawn are dropped.
const CACHE_SIZE: usize = 512;

pub(crate) struct Formula {
    pub texture: TextureHandle,
    /// Size in points, padding included.
    pub size: Vec2,
    /// From the image's top to the formula's baseline, in points.
    pub ascent: f32,
}

type Rendered = Result<Arc<Formula>, String>;

#[derive(Clone, Default)]
struct Cache(Arc<Mutex<HashMap<u64, (Rendered, u64)>>>);

/// `tex` at `size` points in `color`, as display math when `display`.
pub(crate) fn formula(
    ctx: &egui::Context,
    tex: &str,
    display: bool,
    size: f32,
    color: Color32,
) -> Rendered {
    let pixels_per_point = ctx.pixels_per_point();
    let mut hasher = std::hash::DefaultHasher::new();
    (
        tex,
        display,
        size.to_bits(),
        pixels_per_point.to_bits(),
        color,
    )
        .hash(&mut hasher);
    let key = hasher.finish();
    let frame = ctx.cumulative_frame_nr();
    let cache = ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<Cache>(egui::Id::unique("glyphst-markdown-math"))
            .clone()
    });
    let mut formulas = cache
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((rendered, used)) = formulas.get_mut(&key) {
        *used = frame;
        return rendered.clone();
    }
    let rendered =
        render(tex, display, size * pixels_per_point, color).map(|(image, ascent_px)| {
            let size = image.size;
            let texture = ctx.load_texture(format!("math-{key:x}"), image, TextureOptions::LINEAR);
            Arc::new(Formula {
                texture,
                size: Vec2::new(size[0] as f32, size[1] as f32) / pixels_per_point,
                ascent: ascent_px / pixels_per_point,
            })
        });
    if formulas.len() >= CACHE_SIZE {
        let mut used: Vec<u64> = formulas.values().map(|(_, used)| *used).collect();
        used.sort_unstable();
        let cutoff = used[used.len() / 2];
        formulas.retain(|_, (_, used)| *used > cutoff);
    }
    formulas.insert(key, (rendered.clone(), frame));
    rendered
}

/// The formula's pixels and the baseline's distance from their top.
fn render(
    tex: &str,
    display: bool,
    size_px: f32,
    color: Color32,
) -> Result<(ColorImage, f32), String> {
    if tex.len() > MAX_SOURCE {
        return Err("formula too long".into());
    }
    let nodes = ratex_parser::parse(tex).map_err(|error| error.message)?;
    let [r, g, b, a] = color
        .to_srgba_unmultiplied()
        .map(|channel| f32::from(channel) / 255.0);
    let options = ratex_layout::LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        color: Color { r, g, b, a },
        ..ratex_layout::LayoutOptions::default()
    };
    let root = ratex_layout::layout(&nodes, &options);
    ratex_layout::validate_layout(&root)?;
    let list = ratex_layout::to_display_list(&root);
    let png = ratex_render::render_to_png(
        &list,
        &ratex_render::RenderOptions {
            font_size: size_px,
            padding: PAD_PX,
            background_color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            ..ratex_render::RenderOptions::default()
        },
    )?;
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?
        .to_rgba8();
    let image = ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    );
    Ok((image, list.height as f32 * size_px + PAD_PX))
}
