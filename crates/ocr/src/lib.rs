//! Local text recognition for scanned pages, on this computer through ONNX
//! Runtime: PP-OCRv6 (tiny, small or medium; oar-ocr's staged detection and
//! recognition, the way Firecrawl's pdf-inspector runs it, with a second
//! detection pass for dense fine print) or Indra OCR (oar-ocr's port of
//! Datalab's pipeline: lines, layout blocks in reading order, tables). The
//! models are the `glyphst-models` catalog's; the runtime (a CUDA build for
//! an NVIDIA card) is downloaded on request, pinned and checked (see
//! [`manifest`]). Pages never leave the computer.
//!
//! PP-OCR finds lines, not layout; Indra adds blocks. Neither reads
//! formulas; remote OCR providers stay for that.

pub mod manifest;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context as _, Result, anyhow, bail};
use glyphst_models::{
    catalog::{self, ModelEntry, Task},
    setup::Setup,
};
use image::RgbImage;
use oar_ocr::core::config::onnx::{OrtExecutionProvider, OrtSessionConfig};
use oar_ocr::domain::tasks::TextDetectionConfig;
use oar_ocr::indra::{IndraOcr, IndraOcrBuilder};
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

/// Where a model runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Device {
    #[default]
    Cpu,
    /// CUDA with an NVIDIA card (its runtime build), CoreML on a Mac.
    Gpu,
}

/// The folder local OCR keeps its runtimes and models in, or the one named
/// by `GLYPHST_OCR_DIR`.
pub fn root() -> PathBuf {
    Task::TextRecognition.root()
}

/// Downloading (or resuming) `model` and the runtime for `device`, and
/// their state.
pub fn setup(model: &ModelEntry, device: Device) -> Setup {
    Setup::with(
        root(),
        manifest::runtime(device),
        runtime_override(),
        model.files.to_vec(),
    )
}

/// The default model on the CPU: what local OCR sets up when asked.
pub fn default_setup() -> Setup {
    setup(catalog::default_for(Task::TextRecognition), Device::Cpu)
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

/// A layout block Indra found: its class (`Text`, `Section-Header`,
/// `Page-Header`, `Page-Footer`, `List-Group`, `Table`, `Caption`,
/// `Footnote`, `Table-Of-Contents`, `Form`), its box in image pixels and
/// its lines' text, newline-joined.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrBlock {
    pub label: String,
    pub bbox: [f32; 4],
    pub text: String,
}

/// What a page holds: its lines, and its blocks in reading order when the
/// model finds layout (Indra).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recognized {
    pub lines: Vec<OcrLine>,
    pub blocks: Vec<OcrBlock>,
}

/// A local OCR model, loaded.
pub struct LocalOcr {
    /// Its catalog id.
    pub model: &'static str,
    engine: Engine,
}

enum Engine {
    PpOcr(Box<PpOcr>),
    Indra(Box<IndraOcr>),
}

impl LocalOcr {
    /// Loads `model` from its `files` on `device`, the runtime library
    /// first (once a process: another runtime takes a restart).
    pub fn load(
        model: &'static ModelEntry,
        device: Device,
        runtime: &Path,
        files: &[PathBuf],
    ) -> Result<Self> {
        load_runtime(runtime)?;
        let session = session_config(device);
        let engine = if model.id.starts_with("indra") {
            let folder = files
                .first()
                .and_then(|file| file.parent())
                .ok_or_else(|| anyhow!("Indra OCR has no files"))?;
            let ocr = IndraOcrBuilder::new(folder)
                .ort_session(session)
                .build()
                .context("could not load Indra OCR")?;
            Engine::Indra(Box::new(ocr))
        } else {
            Engine::PpOcr(Box::new(PpOcr::load(model, session, files)?))
        };
        Ok(Self {
            model: model.id,
            engine,
        })
    }

    /// `model` on `device` once its files are in place: loaded on first
    /// use and shared, so each translation does not load it again. `None`
    /// until its [`setup`] is ready.
    pub fn installed(model: &str, device: Device) -> Result<Option<Arc<Self>>> {
        type Loaded = HashMap<(&'static str, Device), Arc<LocalOcr>>;
        static LOADED: OnceLock<Mutex<Loaded>> = OnceLock::new();
        let entry = catalog::entry(model)
            .filter(|entry| entry.task == Task::TextRecognition)
            .ok_or_else(|| anyhow!("{model} is not a local OCR model"))?;
        let mut loaded = LOADED
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(ocr) = loaded.get(&(entry.id, device)) {
            return Ok(Some(ocr.clone()));
        }
        let Some((runtime, files)) = setup(entry, device).all_paths() else {
            return Ok(None);
        };
        let ocr = Arc::new(Self::load(entry, device, &runtime, &files)?);
        loaded.insert((entry.id, device), ocr.clone());
        Ok(Some(ocr))
    }

    /// The lines (and blocks) of text in `image`, top to bottom.
    pub fn recognize(&self, image: RgbImage) -> Result<Recognized> {
        match &self.engine {
            Engine::PpOcr(ocr) => Ok(Recognized {
                lines: ocr.recognize(image)?,
                blocks: Vec::new(),
            }),
            Engine::Indra(ocr) => indra_page(ocr, image),
        }
    }
}

/// PP-OCRv6 at one size.
struct PpOcr {
    detector: TextDetectionPredictor,
    recognizer: TextRecognitionPredictor,
    /// The second pass's detector, made on the first page needing it.
    fine: OnceLock<Option<TextDetectionPredictor>>,
    detection: PathBuf,
    box_threshold: f32,
    session: OrtSessionConfig,
}

impl PpOcr {
    /// The detection and recognition models and the dictionary, in the
    /// catalog's order.
    fn load(model: &ModelEntry, session: OrtSessionConfig, files: &[PathBuf]) -> Result<Self> {
        let [detection, recognition, dictionary] = files else {
            bail!("{} needs three model files", model.name);
        };
        // PaddleOCR's box threshold for each size.
        let box_threshold = if model.id.contains("tiny") { 0.4 } else { 0.45 };
        let detector = detector(detection, DETECTION_LIMIT, box_threshold, &session)?;
        let recognizer = TextRecognitionPredictor::builder()
            .dict_path(dictionary)
            .with_ort_config(session.clone())
            .build(recognition)
            .context("could not load the recognition model")?;
        Ok(Self {
            detector,
            recognizer,
            fine: OnceLock::new(),
            detection: detection.clone(),
            box_threshold,
            session,
        })
    }

    fn recognize(&self, image: RgbImage) -> Result<Vec<OcrLine>> {
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
                detector(
                    &self.detection,
                    DETECTION_LIMIT_FINE,
                    self.box_threshold,
                    &self.session,
                )
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

/// A page through Indra: its lines and blocks, mapped back from the upright
/// page it read to the image given.
fn indra_page(ocr: &IndraOcr, image: RgbImage) -> Result<Recognized> {
    let mut pages = ocr.predict(vec![image])?;
    if pages.is_empty() {
        return Ok(Recognized::default());
    }
    let page = pages.swap_remove(0);
    let (width, height) = (page.width as f32, page.height as f32);
    // The page was turned clockwise by `rotation` to read it upright.
    let back = |[x, y]: [f32; 2]| match page.rotation {
        90 => [y, width - x],
        180 => [width - x, height - y],
        270 => [height - y, x],
        _ => [x, y],
    };
    let unturn = |[x0, y0, x1, y1]: [f32; 4]| {
        let ([ax, ay], [bx, by]) = (back([x0, y0]), back([x1, y1]));
        [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]
    };
    let lines = page
        .lines
        .iter()
        .filter(|line| !line.text.trim().is_empty() && line.confidence >= MINIMUM_CONFIDENCE)
        .map(|line| {
            let [x0, y0, x1, y1] = unturn(line.bbox);
            OcrLine {
                text: line.text.clone(),
                quad: [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
                confidence: line.confidence.clamp(0.0, 1.0),
            }
        })
        .collect();
    let blocks = page
        .blocks
        .iter()
        .filter(|block| !block.text.trim().is_empty())
        .map(|block| OcrBlock {
            label: block.label.clone(),
            bbox: unturn(block.bbox),
            text: block.text.clone(),
        })
        .collect();
    Ok(Recognized { lines, blocks })
}

/// The runtime library loaded in this process: ONNX Runtime is loaded once,
/// so another one (switching between CPU and GPU) takes a restart.
fn load_runtime(path: &Path) -> Result<()> {
    static LOADED: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut loaded = LOADED.lock().unwrap_or_else(|poison| poison.into_inner());
    match loaded.as_deref() {
        Some(current) if current == path => return Ok(()),
        Some(_) => bail!("switching between the CPU and the GPU takes effect after a restart"),
        None => {}
    }
    drop(
        ort::init_from(path)
            .with_context(|| format!("could not load ONNX Runtime from {}", path.display()))?,
    );
    *loaded = Some(path.to_path_buf());
    Ok(())
}

fn session_config(device: Device) -> OrtSessionConfig {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let config = OrtSessionConfig::new()
        .with_intra_threads(cores.clamp(1, 4))
        .with_inter_threads(1)
        .with_parallel_execution(false);
    match device {
        Device::Cpu => config,
        // A GPU that cannot be used is an error, not a quiet CPU run.
        Device::Gpu => config.with_execution_providers(vec![gpu_provider()]),
    }
}

/// The GPU's execution provider on this system.
fn gpu_provider() -> OrtExecutionProvider {
    if cfg!(target_os = "macos") {
        OrtExecutionProvider::CoreML {
            ane_only: None,
            subgraphs: None,
        }
    } else {
        OrtExecutionProvider::CUDA {
            device_id: Some(0),
            gpu_mem_limit: None,
            arena_extend_strategy: None,
            cudnn_conv_algo_search: None,
            cudnn_conv_use_max_workspace: None,
        }
    }
}

fn detector(
    path: &Path,
    limit: u32,
    box_threshold: f32,
    session: &OrtSessionConfig,
) -> Result<TextDetectionPredictor> {
    // Every field the general preset would set, pinned: unclip 2.0 keeps
    // glyph edges inside the boxes.
    let config = TextDetectionConfig {
        limit_side_len: Some(limit),
        limit_type: Some(oar_ocr::processors::LimitType::Max),
        max_side_len: Some(DETECTION_MAXIMUM_SIDE),
        unclip_ratio: 2.0,
        box_threshold,
        ..Default::default()
    };
    TextDetectionPredictor::builder()
        .with_config(config)
        .with_ort_config(session.clone())
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

    /// Recognises the image `GLYPHST_OCR_TEST_IMAGE` with the model
    /// `GLYPHST_OCR_TEST_MODEL` (PP-OCRv6 small unless set), installed
    /// under `GLYPHST_OCR_DIR` or the data folder, with the runtime library
    /// `ORT_DYLIB_PATH` if set:
    /// `cargo test -p glyphst-ocr -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs the runtime and models (Settings · Models)"]
    fn the_installed_engine_reads_a_page() {
        let image = std::env::var_os("GLYPHST_OCR_TEST_IMAGE").expect("GLYPHST_OCR_TEST_IMAGE");
        let image = image::open(image).unwrap().to_rgb8();
        let model =
            std::env::var("GLYPHST_OCR_TEST_MODEL").unwrap_or_else(|_| "pp-ocrv6-small".to_owned());
        let ocr = LocalOcr::installed(&model, Device::Cpu)
            .unwrap()
            .expect("the model is installed");
        let started = std::time::Instant::now();
        let page = ocr.recognize(image).unwrap();
        println!(
            "{}: {} lines, {} blocks in {:?}",
            model,
            page.lines.len(),
            page.blocks.len(),
            started.elapsed()
        );
        for line in &page.lines {
            println!("{:.2} {:?} {}", line.confidence, line.quad[0], line.text);
        }
        for block in &page.blocks {
            println!("[{}] {:?} {}", block.label, block.bbox, block.text);
        }
        assert!(!page.lines.is_empty());
    }
}
