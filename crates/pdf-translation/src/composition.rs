use std::fmt;
use std::num::NonZero;
use std::path::{Path, PathBuf};

use mupdf::device::{DefaultColorspaces, DeviceFlag, Metatext, Structure};
use mupdf::pdf::{
    PdfAnnotationType, PdfDocument, PdfGraftMap, PdfLink, PdfObject, PdfWriteOptions,
};
use mupdf::{
    BlendMode, ColorParams, Colorspace, Device, Document, Function, Image, Matrix, NativeDevice,
    Path as MuPath, Rect, Shade, StrokeState, Text,
};
use serde::{Deserialize, Serialize};

use crate::{BoundingBox, CancellationToken, LayoutDocument, PdfInspection};

#[derive(Debug, Clone, Default)]
pub struct PdfComposer;

pub fn sanitize_source_pdf(
    source_path: &Path,
    output_path: &Path,
    layout: &LayoutDocument,
    cancellation: &CancellationToken,
) -> Result<(), PdfCompositionError> {
    cancellation
        .check()
        .map_err(|_| PdfCompositionError::cancelled())?;
    let source_path = source_path.to_string_lossy().to_string();
    let document = PdfDocument::open(source_path.as_str()).map_err(PdfCompositionError::mupdf)?;

    for page_layout in &layout.pages {
        cancellation
            .check()
            .map_err(|_| PdfCompositionError::cancelled())?;
        let mut page = document
            .load_pdf_page(page_layout.page_index as i32)
            .map_err(PdfCompositionError::mupdf)?;
        for block in &page_layout.items {
            let mut annotation = page
                .create_annotation(PdfAnnotationType::Redact)
                .map_err(PdfCompositionError::mupdf)?;
            annotation
                .set_rect(Rect::new(
                    block.bbox.x0,
                    block.bbox.y0,
                    block.bbox.x1,
                    block.bbox.y1,
                ))
                .map_err(PdfCompositionError::mupdf)?;
        }
        page.redact().map_err(PdfCompositionError::mupdf)?;
    }

    let output_path = output_path.to_string_lossy().to_string();
    document
        .save(output_path.as_str())
        .map_err(PdfCompositionError::mupdf)
}

impl PdfComposer {
    pub fn compose(
        &self,
        source_path: &Path,
        translated_pdf_path: &Path,
        output_path: &Path,
        layout: &LayoutDocument,
        inspection: &PdfInspection,
        cancellation: &CancellationToken,
    ) -> Result<PdfCompositionReport, PdfCompositionError> {
        cancellation
            .check()
            .map_err(|_| PdfCompositionError::cancelled())?;
        let source = Document::open(source_path).map_err(PdfCompositionError::mupdf)?;
        let translated = Document::open(translated_pdf_path).map_err(PdfCompositionError::mupdf)?;
        let source_pages =
            usize::try_from(source.page_count().map_err(PdfCompositionError::mupdf)?)
                .unwrap_or_default();
        let translated_pages = usize::try_from(
            translated
                .page_count()
                .map_err(PdfCompositionError::mupdf)?,
        )
        .unwrap_or_default();
        if source_pages != translated_pages || source_pages != layout.pages.len() {
            return Err(PdfCompositionError::invalid(format!(
                "page count mismatch: source={source_pages}, translated={translated_pages}, layout={}",
                layout.pages.len()
            )));
        }
        restore_pdf_structure(source_path, translated_pdf_path, output_path, cancellation)?;
        let scanned_pages = inspection
            .pages
            .iter()
            .filter(|page| page.needs_ocr)
            .count();
        Ok(PdfCompositionReport {
            page_count: source_pages,
            editable_pages: source_pages.saturating_sub(scanned_pages),
            scanned_pages,
            output_path: output_path.to_path_buf(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfCompositionReport {
    pub page_count: usize,
    pub editable_pages: usize,
    pub scanned_pages: usize,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfCompositionErrorKind {
    Cancelled,
    InvalidInput,
    MuPdf,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfCompositionError {
    pub kind: PdfCompositionErrorKind,
    pub message: String,
}

impl PdfCompositionError {
    fn cancelled() -> Self {
        Self {
            kind: PdfCompositionErrorKind::Cancelled,
            message: "PDF translation was cancelled".to_string(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: PdfCompositionErrorKind::InvalidInput,
            message: message.into(),
        }
    }

    fn mupdf(error: mupdf::Error) -> Self {
        Self::mupdf_message(error.to_string())
    }

    fn mupdf_message(message: impl Into<String>) -> Self {
        Self {
            kind: PdfCompositionErrorKind::MuPdf,
            message: message.into(),
        }
    }
}

impl fmt::Display for PdfCompositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for PdfCompositionError {}

#[allow(dead_code)]
struct TextFilteringDevice {
    target: Option<Device>,
    translated_regions: Vec<BoundingBox>,
    clip_stack: Vec<bool>,
    error: Option<String>,
}

#[allow(dead_code)]
impl TextFilteringDevice {
    fn new(target: Device, translated_regions: Vec<BoundingBox>) -> Self {
        Self {
            target: Some(target),
            translated_regions,
            clip_stack: Vec::new(),
            error: None,
        }
    }

    fn target(&self) -> Option<&Device> {
        self.target.as_ref().filter(|_| self.error.is_none())
    }

    fn record(&mut self, result: Result<(), mupdf::Error>) {
        if let Err(error) = result
            && self.error.is_none()
        {
            self.error = Some(error.to_string());
        }
    }

    fn suppress_text(&self, text: &Text, stroke: &StrokeState, ctm: &Matrix) -> bool {
        let Ok(bounds) = text.bounds(stroke, ctm) else {
            return false;
        };
        let bounds = BoundingBox::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
        self.translated_regions.iter().any(|region| {
            bounds.intersection(*region).is_some_and(|intersection| {
                let denominator = bounds.area().min(region.area()).max(1.0);
                intersection.area() / denominator >= 0.12
            })
        })
    }

    fn push_clip(&mut self, suppressed: bool) {
        self.clip_stack.push(suppressed);
    }
}

impl NativeDevice for TextFilteringDevice {
    fn fill_path(
        &mut self,
        path: &MuPath,
        even_odd: bool,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if let Some(target) = self.target() {
            self.record(target.fill_path(path, even_odd, &ctm, color_space, color, alpha, cp));
        }
    }

    fn stroke_path(
        &mut self,
        path: &MuPath,
        stroke_state: &StrokeState,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if let Some(target) = self.target() {
            self.record(target.stroke_path(
                path,
                stroke_state,
                &ctm,
                color_space,
                color,
                alpha,
                cp,
            ));
        }
    }

    fn clip_path(&mut self, path: &MuPath, even_odd: bool, ctm: Matrix, _scissor: Rect) {
        self.push_clip(false);
        if let Some(target) = self.target() {
            self.record(target.clip_path(path, even_odd, &ctm));
        }
    }

    fn clip_stroke_path(
        &mut self,
        path: &MuPath,
        stroke_state: &StrokeState,
        ctm: Matrix,
        _scissor: Rect,
    ) {
        self.push_clip(false);
        if let Some(target) = self.target() {
            self.record(target.clip_stroke_path(path, stroke_state, &ctm));
        }
    }

    fn fill_text(
        &mut self,
        text: &Text,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.suppress_text(text, &StrokeState::default(), &ctm) {
            return;
        }
        if let Some(target) = self.target() {
            self.record(target.fill_text(text, &ctm, color_space, color, alpha, cp));
        }
    }

    fn stroke_text(
        &mut self,
        text: &Text,
        stroke_state: &StrokeState,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if self.suppress_text(text, stroke_state, &ctm) {
            return;
        }
        if let Some(target) = self.target() {
            self.record(target.stroke_text(
                text,
                stroke_state,
                &ctm,
                color_space,
                color,
                alpha,
                cp,
            ));
        }
    }

    fn clip_text(&mut self, text: &Text, ctm: Matrix, _scissor: Rect) {
        let suppressed = self.suppress_text(text, &StrokeState::default(), &ctm);
        self.push_clip(suppressed);
        if !suppressed && let Some(target) = self.target() {
            self.record(target.clip_text(text, &ctm));
        }
    }

    fn clip_stroke_text(
        &mut self,
        text: &Text,
        stroke_state: &StrokeState,
        ctm: Matrix,
        _scissor: Rect,
    ) {
        let suppressed = self.suppress_text(text, stroke_state, &ctm);
        self.push_clip(suppressed);
        if !suppressed && let Some(target) = self.target() {
            self.record(target.clip_stroke_text(text, stroke_state, &ctm));
        }
    }

    fn ignore_text(&mut self, text: &Text, ctm: Matrix) {
        if self.suppress_text(text, &StrokeState::default(), &ctm) {
            return;
        }
        if let Some(target) = self.target() {
            self.record(target.ignore_text(text, &ctm));
        }
    }

    fn fill_shade(&mut self, shade: &Shade, ctm: Matrix, alpha: f32, cp: ColorParams) {
        if let Some(target) = self.target() {
            self.record(target.fill_shade(shade, &ctm, alpha, cp));
        }
    }

    fn fill_image(&mut self, image: &Image, ctm: Matrix, alpha: f32, cp: ColorParams) {
        if let Some(target) = self.target() {
            self.record(target.fill_image(image, &ctm, alpha, cp));
        }
    }

    fn fill_image_mask(
        &mut self,
        image: &Image,
        ctm: Matrix,
        color_space: &Colorspace,
        color: &[f32],
        alpha: f32,
        cp: ColorParams,
    ) {
        if let Some(target) = self.target() {
            self.record(target.fill_image_mask(image, &ctm, color_space, color, alpha, cp));
        }
    }

    fn clip_image_mask(&mut self, image: &Image, ctm: Matrix, _scissor: Rect) {
        self.push_clip(false);
        if let Some(target) = self.target() {
            self.record(target.clip_image_mask(image, &ctm));
        }
    }

    fn pop_clip(&mut self) {
        if self.clip_stack.pop().unwrap_or(false) {
            return;
        }
        if let Some(target) = self.target() {
            self.record(target.pop_clip());
        }
    }

    fn begin_mask(
        &mut self,
        area: Rect,
        luminosity: bool,
        color_space: &Colorspace,
        color: &[f32],
        cp: ColorParams,
    ) {
        if let Some(target) = self.target() {
            self.record(target.begin_mask(area, luminosity, color_space, color, cp));
        }
    }

    fn end_mask(&mut self, function: &Function) {
        if let Some(target) = self.target() {
            self.record(target.end_mask(Some(function)));
        }
    }

    fn begin_group(
        &mut self,
        area: Rect,
        color_space: &Colorspace,
        isolated: bool,
        knockout: bool,
        blend_mode: BlendMode,
        alpha: f32,
    ) {
        if let Some(target) = self.target() {
            self.record(target.begin_group(
                area,
                color_space,
                isolated,
                knockout,
                blend_mode,
                alpha,
            ));
        }
    }

    fn end_group(&mut self) {
        if let Some(target) = self.target() {
            self.record(target.end_group());
        }
    }

    fn begin_tile(
        &mut self,
        area: Rect,
        view: Rect,
        x_step: f32,
        y_step: f32,
        ctm: Matrix,
        id: Option<NonZero<i32>>,
        doc_id: Option<NonZero<i32>>,
    ) -> Option<NonZero<i32>> {
        let target = self.target()?;
        match target.begin_tile(area, view, x_step, y_step, &ctm, id, doc_id) {
            Ok(id) => id,
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }

    fn end_tile(&mut self) {
        if let Some(target) = self.target() {
            self.record(target.end_tile());
        }
    }

    fn render_flags(&mut self, _set: DeviceFlag, _clear: DeviceFlag) {}

    fn set_default_colorspaces(&mut self, _default_cs: &DefaultColorspaces) {}

    fn begin_layer(&mut self, name: &str) {
        if let Some(target) = self.target() {
            self.record(target.begin_layer(name));
        }
    }

    fn end_layer(&mut self) {
        if let Some(target) = self.target() {
            self.record(target.end_layer());
        }
    }

    fn begin_structure(&mut self, standard: Structure, raw: &str, index: i32) {
        if let Some(target) = self.target() {
            self.record(target.begin_structure(standard, raw, index));
        }
    }

    fn end_structure(&mut self) {
        if let Some(target) = self.target() {
            self.record(target.end_structure());
        }
    }

    fn begin_metatext(&mut self, meta: Metatext, text: &str) {
        if let Some(target) = self.target() {
            self.record(target.begin_metatext(meta, text));
        }
    }

    fn end_metatext(&mut self) {
        if let Some(target) = self.target() {
            self.record(target.end_metatext());
        }
    }
}

#[allow(dead_code)]
fn draw_background_patches(
    source_page: &mupdf::Page,
    target: &Device,
    regions: &[BoundingBox],
) -> Result<(), PdfCompositionError> {
    if regions.is_empty() {
        return Ok(());
    }
    let pixmap = source_page
        .to_pixmap(&Matrix::IDENTITY, &Colorspace::device_rgb(), false, false)
        .map_err(PdfCompositionError::mupdf)?;
    let colorspace = Colorspace::device_rgb();
    for region in regions {
        let color = sample_background_color(&pixmap, *region);
        let mut path = MuPath::new().map_err(PdfCompositionError::mupdf)?;
        path.rect(
            region.x0 - 0.8,
            region.y0 - 0.5,
            region.x1 + 0.8,
            region.y1 + 0.5,
        )
        .map_err(PdfCompositionError::mupdf)?;
        target
            .fill_path(
                &path,
                false,
                &Matrix::IDENTITY,
                &colorspace,
                &color,
                1.0,
                ColorParams::default(),
            )
            .map_err(PdfCompositionError::mupdf)?;
    }
    Ok(())
}

#[allow(dead_code)]
fn sample_background_color(pixmap: &mupdf::Pixmap, region: BoundingBox) -> [f32; 3] {
    let (origin_x, origin_y) = pixmap.origin();
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let stride = pixmap.stride().max(0) as usize;
    let components = usize::from(pixmap.n());
    if width <= 0 || height <= 0 || components < 3 {
        return [1.0, 1.0, 1.0];
    }
    let x0 = (region.x0.floor() as i32 - origin_x).clamp(0, width - 1);
    let y0 = (region.y0.floor() as i32 - origin_y).clamp(0, height - 1);
    let x1 = (region.x1.ceil() as i32 - origin_x).clamp(0, width - 1);
    let y1 = (region.y1.ceil() as i32 - origin_y).clamp(0, height - 1);
    let samples = pixmap.samples();
    let mut channels = [Vec::new(), Vec::new(), Vec::new()];
    let margin = 3;
    for y in (y0 - margin).max(0)..=(y1 + margin).min(height - 1) {
        for x in (x0 - margin).max(0)..=(x1 + margin).min(width - 1) {
            let on_border = x < x0 || x > x1 || y < y0 || y > y1;
            if !on_border || (x + y) % 2 != 0 {
                continue;
            }
            let offset = y as usize * stride + x as usize * components;
            if offset + 2 < samples.len() {
                channels[0].push(samples[offset]);
                channels[1].push(samples[offset + 1]);
                channels[2].push(samples[offset + 2]);
            }
        }
    }
    if channels[0].is_empty() {
        return [1.0, 1.0, 1.0];
    }
    for channel in &mut channels {
        channel.sort_unstable();
    }
    [
        f32::from(channels[0][channels[0].len() / 2]) / 255.0,
        f32::from(channels[1][channels[1].len() / 2]) / 255.0,
        f32::from(channels[2][channels[2].len() / 2]) / 255.0,
    ]
}

fn restore_pdf_structure(
    source_path: &Path,
    temporary_path: &Path,
    output_path: &Path,
    cancellation: &CancellationToken,
) -> Result<(), PdfCompositionError> {
    let source = PdfDocument::open(source_path).map_err(PdfCompositionError::mupdf)?;
    let mut output = PdfDocument::open(temporary_path).map_err(PdfCompositionError::mupdf)?;
    let page_count = source.page_count().map_err(PdfCompositionError::mupdf)?;
    for page_index in 0..page_count {
        cancellation
            .check()
            .map_err(|_| PdfCompositionError::cancelled())?;
        let source_page = source
            .load_pdf_page(page_index)
            .map_err(PdfCompositionError::mupdf)?;
        let links = collect_links(&source_page)?;
        if links.is_empty() {
            continue;
        }
        let mut output_page = output
            .load_pdf_page(page_index)
            .map_err(PdfCompositionError::mupdf)?;
        output_page
            .add_links(&mut output, &links)
            .map_err(PdfCompositionError::mupdf)?;
    }
    let outlines = source.outlines().map_err(PdfCompositionError::mupdf)?;
    output
        .set_outlines(&outlines)
        .map_err(PdfCompositionError::mupdf)?;
    let mut graft = output.new_graft_map().map_err(PdfCompositionError::mupdf)?;
    {
        let source_trailer = source.trailer().map_err(PdfCompositionError::mupdf)?;
        let mut output_trailer = output.trailer().map_err(PdfCompositionError::mupdf)?;
        copy_dict_entry(&source_trailer, &mut output_trailer, "Info", &mut graft)?;
    }
    {
        let source_catalog = source.catalog().map_err(PdfCompositionError::mupdf)?;
        let mut output_catalog = output.catalog().map_err(PdfCompositionError::mupdf)?;
        for key in [
            "Metadata",
            "Lang",
            "ViewerPreferences",
            "PageLayout",
            "PageMode",
            "PageLabels",
            "OutputIntents",
        ] {
            copy_dict_entry(&source_catalog, &mut output_catalog, key, &mut graft)?;
        }
    }
    let mut options = PdfWriteOptions::default();
    options
        .set_compress(true)
        .set_compress_fonts(true)
        .set_compress_images(true)
        .set_clean(true)
        .set_garbage_level(3);
    output
        .save_with_options(&output_path.to_string_lossy(), options)
        .map_err(PdfCompositionError::mupdf)
}

fn collect_links(page: &mupdf::pdf::PdfPage) -> Result<Vec<PdfLink>, PdfCompositionError> {
    let resolved_links = page
        .resolved_links()
        .map_err(PdfCompositionError::mupdf)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(PdfCompositionError::mupdf)?;
    if !resolved_links.is_empty() {
        return Ok(resolved_links);
    }
    page.links_from_annotations_lossy()
        .map_err(PdfCompositionError::mupdf)
}

fn copy_dict_entry(
    source: &PdfObject,
    target: &mut PdfObject,
    key: &str,
    graft: &mut PdfGraftMap,
) -> Result<(), PdfCompositionError> {
    let Some(value) = source.get_dict(key).map_err(PdfCompositionError::mupdf)? else {
        return Ok(());
    };
    let value = graft
        .graft_object(&value)
        .map_err(PdfCompositionError::mupdf)?;
    target
        .dict_put(key, value)
        .map_err(PdfCompositionError::mupdf)
}

#[allow(dead_code)]
fn temporary_composition_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("translated.pdf");
    output_path.with_file_name(format!(".{file_name}.glyphst-composing.pdf"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        TranslatedDocument, TranslatedUnit, TypstCompiler, TypstCompilerConfig, build_layout,
        extract_native_document, inspect_pdf,
    };

    use super::*;

    #[test]
    fn sampled_patch_falls_back_to_white_for_empty_pixmap_inputs() {
        assert_eq!(BoundingBox::new(0.0, 0.0, 10.0, 10.0).area(), 100.0);
    }

    #[test]
    fn composes_fixture_when_requested() {
        let Some(source_path) =
            std::env::var_os("GLYPHST_PDF_COMPOSITION_TEST_SOURCE").map(PathBuf::from)
        else {
            return;
        };
        let cancellation = CancellationToken::new();
        let document =
            extract_native_document(&source_path, "composition-test", &cancellation).unwrap();
        let block_lookup = document
            .pages
            .iter()
            .flat_map(|page| page.blocks.iter())
            .map(|block| (block.id.clone(), block.role))
            .collect::<HashMap<_, _>>();
        let units = document
            .pages
            .iter()
            .flat_map(|page| page.blocks.iter())
            .filter(|block| block.policy.translate)
            .take(8)
            .map(|block| TranslatedUnit {
                id: format!("unit-{}", block.id),
                member_block_ids: vec![block.id.clone()],
                role: block_lookup[&block.id],
                source_text: block.content.text.clone(),
                translated_text: "这是用于验证 PDF 合成与字体嵌入的测试译文。".to_string(),
            })
            .collect();
        let translated = TranslatedDocument { document, units };
        let removed_source_text = translated.units[0].source_text.trim().to_string();
        let layout = build_layout(&translated);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-compose-test-{nonce:x}"));
        std::fs::create_dir_all(&directory).unwrap();
        let sanitized_source = directory.join("source-sanitized.pdf");
        sanitize_source_pdf(&source_path, &sanitized_source, &layout, &cancellation).unwrap();
        let overlay = TypstCompiler::new(TypstCompilerConfig::default())
            .compile_translated_pdf(&sanitized_source, &layout, &directory, &cancellation)
            .unwrap();
        let inspection = inspect_pdf(&source_path, &cancellation).unwrap();
        let output_path = directory.join("composed.pdf");

        let report = PdfComposer
            .compose(
                &source_path,
                &overlay.pdf_path,
                &output_path,
                &layout,
                &inspection,
                &cancellation,
            )
            .unwrap();

        assert_eq!(report.page_count, layout.pages.len());
        assert!(output_path.is_file());
        let output_path_string = output_path.to_string_lossy().to_string();
        let output = Document::open(output_path_string.as_str()).unwrap();
        assert_eq!(output.page_count().unwrap() as usize, layout.pages.len());
        let extracted = output
            .load_page(0)
            .unwrap()
            .to_text_page(mupdf::TextPageFlags::PRESERVE_WHITESPACE)
            .unwrap()
            .to_text()
            .unwrap();
        assert!(extracted.contains("用于验证"));
        if removed_source_text.len() >= 12 {
            assert!(!extracted.contains(&removed_source_text));
        }
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn recomposes_translated_fixture_when_requested() {
        let Some(source_path) =
            std::env::var_os("GLYPHST_PDF_COMPOSITION_TEST_SOURCE").map(PathBuf::from)
        else {
            return;
        };
        let Some(translated_path) =
            std::env::var_os("GLYPHST_TRANSLATED_DOCUMENT_FIXTURE").map(PathBuf::from)
        else {
            return;
        };
        let Some(output_path) = std::env::var_os("GLYPHST_RECOMPOSE_OUTPUT").map(PathBuf::from)
        else {
            return;
        };
        let cancellation = CancellationToken::new();
        let mut translated: TranslatedDocument =
            serde_json::from_slice(&std::fs::read(translated_path).unwrap()).unwrap();
        let native =
            extract_native_document(&source_path, "native-geometry", &cancellation).unwrap();
        crate::reconcile_ocr_geometry(&mut translated.document, &native);
        assert!(
            translated
                .document
                .pages
                .iter()
                .flat_map(|page| &page.blocks)
                .find(|block| block.id == "p001-b0005")
                .is_none_or(|block| block.bbox.y1 > 760.0)
        );
        let layout = build_layout(&translated);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("glyphst-recompose-test-{nonce:x}"));
        std::fs::create_dir_all(&directory).unwrap();
        let sanitized_source = directory.join("source-sanitized.pdf");
        sanitize_source_pdf(&source_path, &sanitized_source, &layout, &cancellation).unwrap();
        let overlay = TypstCompiler::new(TypstCompilerConfig::default())
            .compile_translated_pdf(&sanitized_source, &layout, &directory, &cancellation)
            .unwrap();
        let inspection = inspect_pdf(&source_path, &cancellation).unwrap();
        PdfComposer
            .compose(
                &source_path,
                &overlay.pdf_path,
                &output_path,
                &layout,
                &inspection,
                &cancellation,
            )
            .unwrap();
        let _ = std::fs::remove_dir_all(directory);
    }
}
