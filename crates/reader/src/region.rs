//! Region selection: a rectangle dragged on a page (with the region tool,
//! S) becomes an image to copy, save or show the assistant, and its text a
//! column selection to copy or translate. The image is rendered by MuPDF
//! on a worker thread.

use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};

use anyhow::{Context as _, Result};
use glyphst_core::{DocumentLayout, PageIndex, PageRegion, RegionImageReference};
use glyphst_pdf::{
    DocumentSnapshot,
    vector::{PageRect, RegionImage, render_region},
};
use image::ImageEncoder as _;

/// The longest side of a region image, in pixels.
pub const MAX_PIXELS: u32 = 2048;
/// A drag shorter than this (in page points) on either side is a click.
const MIN_SIDE: f32 = 4.0;

/// A rectangle on one page of a tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub tab: u64,
    pub page: usize,
    pub from: [f32; 2],
    pub to: [f32; 2],
}

impl Region {
    /// Its page rectangle, corners in order.
    pub fn rect(&self) -> PageRect {
        [
            self.from[0].min(self.to[0]),
            self.from[1].min(self.to[1]),
            self.from[0].max(self.to[0]),
            self.from[1].max(self.to[1]),
        ]
    }

    /// The rectangle limited to the page `bounds`, unless that leaves too
    /// little to be a region.
    pub fn clamped(&self, bounds: PageRect) -> Option<PageRect> {
        let [x0, y0, x1, y1] = self.rect();
        let rect = [
            x0.max(bounds[0]),
            y0.max(bounds[1]),
            x1.min(bounds[2]),
            y1.min(bounds[3]),
        ];
        (rect[2] - rect[0] >= MIN_SIDE && rect[3] - rect[1] >= MIN_SIDE).then_some(rect)
    }

    /// How the assistant refers to it.
    pub fn reference(&self) -> RegionImageReference {
        let [x0, y0, x1, y1] = self.rect();
        RegionImageReference::new([PageRegion {
            page: PageIndex(self.page),
            x0,
            y0,
            x1,
            y1,
        }])
    }
}

/// What a region image is rendered for.
#[derive(Clone, Debug, PartialEq)]
pub enum Use {
    Copy,
    Save(PathBuf),
    Ask(Region),
}

/// A region image being rendered.
pub struct Job {
    pub for_: Use,
    result: Receiver<Result<RegionImage>>,
}

impl Job {
    /// Renders `rect` of page `page` of `path` on a worker thread.
    pub fn start(path: &Path, page: usize, rect: PageRect, for_: Use, ctx: &egui::Context) -> Self {
        let (tx, result) = mpsc::channel();
        let (path, ctx) = (path.to_path_buf(), ctx.clone());
        thread::spawn(move || {
            let image = (|| {
                let document = DocumentSnapshot::read(&path)?
                    .open_with_layout(DocumentLayout::Fixed)
                    .with_context(|| format!("failed to open {}", path.display()))?;
                let page = document.inner().load_page(i32::try_from(page)?)?;
                render_region(&page, rect, MAX_PIXELS)
            })();
            let _ = tx.send(image);
            ctx.request_repaint();
        });
        Self { for_, result }
    }

    /// The image, once rendered.
    pub fn poll(&self) -> Option<Result<RegionImage>> {
        self.result.try_recv().ok()
    }
}

/// The image as egui shows and copies it.
pub fn color_image(image: &RegionImage) -> egui::ColorImage {
    egui::ColorImage::from_rgb([image.size[0] as usize, image.size[1] as usize], &image.rgb)
}

/// The image as a PNG file's bytes.
pub fn png(image: &RegionImage) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            &image.rgb,
            image.size[0],
            image.size[1],
            image::ExtendedColorType::Rgb8,
        )
        .context("failed to encode the region image")?;
    Ok(bytes)
}

/// What the region popover chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    CopyImage,
    SaveImage,
    CopyText,
    Translate,
    Ask,
    Dismiss,
}

/// The region's toolbar (frame 02c): `copy · save png · copy text · ask ·
/// translate` on a popover above the region's top right, at `at`;
/// `has_text` when glyphs lie in the region. The design's letter keys are
/// left out: plain letters are the app's own shortcuts.
pub fn popover(ctx: &egui::Context, at: egui::Pos2, has_text: bool) -> Option<Action> {
    let mut chosen = None;
    let items = [
        ("copy", "copy", true, Action::CopyImage),
        ("download", "save png", true, Action::SaveImage),
        ("type", "copy text", has_text, Action::CopyText),
        ("sparkles", "ask", true, Action::Ask),
        ("languages", "translate", has_text, Action::Translate),
    ];
    egui::Area::new(egui::Id::unique("region-popover"))
        .order(egui::Order::Foreground)
        .fixed_pos(at + egui::vec2(0.0, -8.0))
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .show(ctx, |ui| {
            glyphst_ui::pane::popover(ctx)
                .inner_margin(egui::Margin::same(3))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for (index, (icon, label, enabled, action)) in items.into_iter().enumerate()
                        {
                            if index > 0 {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(1.0, 18.0),
                                    egui::Sense::hover(),
                                );
                                let tokens = crate::theme::Tokens::get(ui.ctx());
                                ui.painter().vline(
                                    rect.center().x,
                                    rect.y_range(),
                                    tokens.hairline(),
                                );
                            }
                            let response = ui.add_enabled(
                                enabled,
                                glyphst_ui::button::Button::plain(label).icon(icon).small(),
                            );
                            if response.clicked() {
                                chosen = Some(action);
                            }
                        }
                        if crate::icons::button(ui, "x", 22.0, false, "dismiss (esc)").clicked() {
                            chosen = Some(Action::Dismiss);
                        }
                    });
                });
        });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(from: [f32; 2], to: [f32; 2]) -> Region {
        Region {
            tab: 1,
            page: 2,
            from,
            to,
        }
    }

    #[test]
    fn regions_are_ordered_and_kept_on_their_page() {
        let dragged_up_left = region([80.0, 90.0], [-20.0, 10.0]);
        assert_eq!(dragged_up_left.rect(), [-20.0, 10.0, 80.0, 90.0]);
        assert_eq!(
            dragged_up_left.clamped([0.0, 0.0, 600.0, 800.0]),
            Some([0.0, 10.0, 80.0, 90.0])
        );
        assert_eq!(
            region([10.0, 10.0], [12.0, 50.0]).clamped([0.0, 0.0, 600.0, 800.0]),
            None,
            "too narrow: a click"
        );
        assert_eq!(
            region([700.0, 10.0], [800.0, 50.0]).clamped([0.0, 0.0, 600.0, 800.0]),
            None,
            "off the page"
        );
        let reference = dragged_up_left.reference();
        assert_eq!(reference.regions[0].page, PageIndex(2));
        assert_eq!(reference.regions[0].x0, -20.0);
    }

    #[test]
    fn region_images_encode_as_png() {
        let image = RegionImage {
            size: [2, 1],
            rgb: vec![255, 0, 0, 0, 0, 255],
        };
        let bytes = png(&image).unwrap();
        assert_eq!(&bytes[1..4], b"PNG");
        assert_eq!(color_image(&image).size, [2, 1]);
    }
}
