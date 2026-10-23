//! Local text recognition for scanned pages: PP-OCRv6 Small run on this
//! computer through ONNX Runtime, the way Firecrawl's pdf-inspector runs it
//! (oar-ocr's staged detection and recognition, a second detection pass for
//! dense fine print). The runtime and models are downloaded on request,
//! pinned and checked (see [`manifest`]); pages never leave the computer.
//!
//! The recogniser finds lines of text, not layout: no formulas, tables or
//! reading order beyond top to bottom. Remote OCR providers stay for that.

pub mod manifest;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context as _, Result, anyhow};
use glyphst_embed::setup::Setup;
use image::RgbImage;
use oar_ocr::core::config::onnx::OrtSessionConfig;
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::oarocr::{EdgeProcessor as _, TextCroppingProcessor};
use oar_ocr::predictors::{TextDetectionPredictor, TextRecognitionPredictor};
use oar_ocr::processors::BoundingBox;

/// The environment variable naming another ONNX Runtime library (a
/// system's own, on NixOS for one).
pub const RUNTIME_ENV: &str = "ORT_DYLIB_PATH";

/// Detection's input cap, PaddleOCR's default: enough for body text.
const DETECTION_LIMIT: u32 = 960;
/// The cap for a second pass over pages dense with small text.
const DETECTION_LIMIT_FINE: u32 = 2560;
const DETECTION_MAXIMUM_SIDE: u32 = 4000;
/// A page is dense with small text with this many regions…
const FINE_MINIMUM_REGIONS: usize = 80;
/// …whose median height at detection scale is under this many pixels.
const FINE_MAXIMUM_HEIGHT: f32 = 15.0;
/// Lines recognised with less confidence are dropped.
const MINIMUM_CONFIDENCE: f32 = 0.5;

/// The folder local OCR keeps its runtime and models in, or the one named
/// by `GLYPHST_OCR_DIR`.
pub fn root() -> PathBuf {
    std::env::var_os("GLYPHST_OCR_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| glyphst_core::paths::data_dir().join("ocr"))
}

/// Downloading (or resuming) the runtime and models, and their state.
pub fn setup() -> Setup {
    Setup::with(
        root(),
        manifest::runtime(),
        runtime_override(),
        manifest::MODELS.to_vec(),
    )
}

/// The runtime library named by [`RUNTIME_ENV`], if any.
fn runtime_override() -> Option<PathBuf> {
    std::env::var_os(RUNTIME_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// A recognised line: its text, its corners in image pixels (clockwise
/// from the top left) and how sure the model is (0 to 1).
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub quad: [[f32; 2]; 4],
    pub confidence: f32,
}

/// PP-OCRv6 Small, loaded.
pub struct LocalOcr {
    detector: TextDetectionPredictor,
    recognizer: TextRecognitionPredictor,
    /// The second pass's detector, made on the first page needing it.
    fine: OnceLock<Option<TextDetectionPredictor>>,
    detection: PathBuf,
}

impl LocalOcr {
    /// Loads the runtime library and the models (detection, recognition,
    /// dictionary, as [`manifest::MODELS`]).
    pub fn load(runtime: &Path, models: &[PathBuf]) -> Result<Self> {
        let [detection, recognition, dictionary] = models else {
            return Err(anyhow!("local OCR needs three model files"));
        };
        load_runtime(runtime)?;
        let detector = detector(detection, DETECTION_LIMIT)?;
        let recognizer = TextRecognitionPredictor::builder()
            .dict_path(dictionary)
            .with_ort_config(session_config())
            .build(recognition)
            .context("could not load the recognition model")?;
        Ok(Self {
            detector,
            recognizer,
            fine: OnceLock::new(),
            detection: detection.clone(),
        })
    }

    /// The recogniser once its files are in place: loaded on first use and
    /// shared, so each translation does not load it again. `None` until
    /// [`setup`] is ready.
    pub fn installed() -> Result<Option<Arc<Self>>> {
        static LOADED: Mutex<Option<Arc<LocalOcr>>> = Mutex::new(None);
        let mut loaded = LOADED.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(ocr) = loaded.as_ref() {
            return Ok(Some(ocr.clone()));
        }
        let Some((runtime, models)) = setup().all_paths() else {
            return Ok(None);
        };
        let ocr = Arc::new(Self::load(&runtime, &models)?);
        *loaded = Some(ocr.clone());
        Ok(Some(ocr))
    }

    /// The lines of text in `image`, top to bottom.
    pub fn recognize(&self, image: RgbImage) -> Result<Vec<OcrLine>> {
        let image = Arc::new(image);
        let boxes = self.detect(&image)?;
        let boxes = oar_ocr::processors::sort_quad_boxes(&boxes);
        let crops = TextCroppingProcessor::new(true).process((image.clone(), boxes.clone()))?;
        let (width, height) = (image.width() as f32, image.height() as f32);
        let mut lines = Vec::new();
        for (bounding, crop) in boxes.iter().zip(crops) {
            let Some(crop) = crop else { continue };
            // A crop at a time: batching pads every crop to the widest.
            let crop = Arc::try_unwrap(crop).unwrap_or_else(|shared| (*shared).clone());
            let recognized = self.recognizer.predict(vec![crop])?;
            let (Some(text), Some(confidence)) = (
                recognized.texts.into_iter().next(),
                recognized.scores.into_iter().next(),
            ) else {
                continue;
            };
            if text.trim().is_empty() || !confidence.is_finite() || confidence < MINIMUM_CONFIDENCE
            {
                continue;
            }
            if let Some(quad) = quad(bounding, width, height) {
                lines.push(OcrLine {
                    text,
                    quad,
                    confidence: confidence.clamp(0.0, 1.0),
                });
            }
        }
        Ok(lines)
    }

    /// The text regions of a page: a standard pass, then a finer one when
    /// the page is dense with small text the standard pass shrank.
    fn detect(&self, image: &Arc<RgbImage>) -> Result<Vec<BoundingBox>> {
        let longest = image.width().max(image.height()) as f32;
        let boxes = detect_with(&self.detector, image)?;
        let downscale = (DETECTION_LIMIT as f32 / longest).min(1.0);
        let mut heights: Vec<f32> = boxes.iter().map(height).collect();
        heights.sort_by(f32::total_cmp);
        let median = heights.get(heights.len() / 2).copied().unwrap_or(f32::MAX) * downscale;
        if downscale < 1.0 && boxes.len() >= FINE_MINIMUM_REGIONS && median < FINE_MAXIMUM_HEIGHT {
            let fine = self.fine.get_or_init(|| {
                detector(&self.detection, DETECTION_LIMIT_FINE)
                    .inspect_err(|error| log::warn!("fine OCR detection unavailable: {error:#}"))
                    .ok()
            });
            if let Some(fine) = fine {
                match detect_with(fine, image) {
                    Ok(boxes) => return Ok(boxes),
                    Err(error) => log::warn!("fine OCR detection failed: {error:#}"),
                }
            }
        }
        Ok(boxes)
    }
}

fn load_runtime(path: &Path) -> Result<()> {
    drop(
        ort::init_from(path)
            .with_context(|| format!("could not load ONNX Runtime from {}", path.display()))?,
    );
    Ok(())
}

fn session_config() -> OrtSessionConfig {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    OrtSessionConfig::new()
        .with_intra_threads(cores.clamp(1, 4))
        .with_inter_threads(1)
        .with_parallel_execution(false)
}

fn detector(path: &Path, limit: u32) -> Result<TextDetectionPredictor> {
    // Every field the general preset would set, pinned: unclip 2.0 keeps
    // glyph edges inside the boxes.
    let config = TextDetectionConfig {
        limit_side_len: Some(limit),
        limit_type: Some(oar_ocr::processors::LimitType::Max),
        max_side_len: Some(DETECTION_MAXIMUM_SIDE),
        unclip_ratio: 2.0,
        ..Default::default()
    };
    TextDetectionPredictor::builder()
        .with_config(config)
        .with_ort_config(session_config())
        .build(path)
        .context("could not load the detection model")
}

fn detect_with(
    detector: &TextDetectionPredictor,
    image: &Arc<RgbImage>,
) -> Result<Vec<BoundingBox>> {
    let mut result = detector.predict(vec![(**image).clone()])?;
    if result.detections.is_empty() {
        return Ok(Vec::new());
    }
    Ok(result
        .detections
        .swap_remove(0)
        .into_iter()
        .map(|detection| detection.bbox)
        .collect())
}

fn height(polygon: &BoundingBox) -> f32 {
    let ys = polygon.points.iter().map(|point| point.y);
    let (low, high) = ys.fold((f32::MAX, f32::MIN), |(low, high), y| {
        (low.min(y), high.max(y))
    });
    (high - low).max(0.0)
}

/// A detection polygon as four corners inside the image, clockwise from the
/// top left (its bounding box when it is not a quadrilateral).
fn quad(polygon: &BoundingBox, width: f32, height: f32) -> Option<[[f32; 2]; 4]> {
    let points: Vec<[f32; 2]> = polygon
        .points
        .iter()
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .map(|point| [point.x.clamp(0.0, width), point.y.clamp(0.0, height)])
        .collect();
    if points.len() < 3 {
        return None;
    }
    let (x0, x1) = points
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
    let (y0, y1) = points
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    match points.as_slice() {
        [a, b, c, d] => Some([*a, *b, *c, *d]),
        _ => Some([[x0, y0], [x1, y0], [x1, y1], [x0, y1]]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recognises the image `GLYPHST_OCR_TEST_IMAGE` with the installed
    /// engine, or with the runtime library `ORT_DYLIB_PATH` and the models
    /// in `GLYPHST_OCR_TEST_MODELS`:
    /// `cargo test -p glyphst-ocr -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the runtime and models (Settings · OCR · install)"]
    fn the_installed_engine_reads_a_page() {
        let image = std::env::var_os("GLYPHST_OCR_TEST_IMAGE").expect("GLYPHST_OCR_TEST_IMAGE");
        let image = image::open(image).unwrap().to_rgb8();
        let ocr = match (
            runtime_override(),
            std::env::var_os("GLYPHST_OCR_TEST_MODELS"),
        ) {
            (Some(runtime), Some(models)) => {
                let models = PathBuf::from(models);
                let models: Vec<PathBuf> = manifest::MODELS
                    .iter()
                    .map(|model| models.join(model.name))
                    .collect();
                Arc::new(LocalOcr::load(&runtime, &models).unwrap())
            }
            _ => LocalOcr::installed()
                .unwrap()
                .expect("local OCR is installed"),
        };
        let started = std::time::Instant::now();
        let lines = ocr.recognize(image).unwrap();
        println!("{} lines in {:?}", lines.len(), started.elapsed());
        for line in &lines {
            println!("{:.2} {:?} {}", line.confidence, line.quad[0], line.text);
        }
        assert!(!lines.is_empty());
    }
}
