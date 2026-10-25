//! Recognising a page on this computer (glyphst-ocr): the page is drawn by
//! MuPDF and read by the chosen model. Indra's layout blocks become a
//! PaddleOCR layout payload as they are; PP-OCR's lines are grouped into
//! paragraphs first. Either way they are normalised and fused with the
//! page's own text the way the services' results are.

use glyphst_ocr::{Device, LocalOcr, OcrBlock, OcrLine};
use glyphst_settings::{OcrDevice, OcrSettings};
use mupdf::{Colorspace, Document, Matrix};
use serde_json::{Value, json};

/// Pixels per point the page is drawn at (200 dpi).
const SCALE: f32 = 200.0 / 72.0;
/// Lines of one paragraph differ in height by less than this factor (a
/// title over its authors does not).
const SAME_SIZE: f32 = 1.25;

/// The device local OCR runs on in `settings`.
pub(crate) fn device(settings: &OcrSettings) -> Device {
    match settings.local.device {
        OcrDevice::Cpu => Device::Cpu,
        OcrDevice::Gpu => Device::Gpu,
    }
}

/// Whether the local model `settings` name is downloaded, with its runtime.
pub(crate) fn is_installed(settings: &OcrSettings) -> bool {
    glyphst_models::catalog::entry(&settings.local.model).is_some_and(|model| {
        glyphst_ocr::setup(model, device(settings))
            .all_paths()
            .is_some()
    })
}

/// The text of the one-page PDF `pdf`, as a PaddleOCR payload in its
/// pixels.
pub(crate) fn recognize_page(ocr: &LocalOcr, pdf: &[u8]) -> Result<Value, String> {
    let document =
        Document::from_copied_bytes(pdf, "application/pdf").map_err(|e| e.to_string())?;
    let page = document.load_page(0).map_err(|e| e.to_string())?;
    let pixmap = page
        .to_pixmap(
            &Matrix::new_scale(SCALE, SCALE),
            &Colorspace::device_rgb(),
            false,
            true,
        )
        .map_err(|error| format!("could not draw the page: {error}"))?;
    let (width, height) = (pixmap.width(), pixmap.height());
    let components = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in samples.chunks(stride).take(height as usize) {
        for pixel in row.chunks(components).take(width as usize) {
            rgb.extend_from_slice(&pixel[..3]);
        }
    }
    let image = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| "the drawn page has the wrong size".to_string())?;
    let page = ocr.recognize(image).map_err(|error| format!("{error:#}"))?;
    Ok(if page.blocks.is_empty() {
        payload(&page.lines, width as f32, height as f32)
    } else {
        block_payload(&page.blocks, width as f32, height as f32)
    })
}

/// Indra's (or PaddleOCR-VL's) blocks, in reading order, under
/// PaddleOCR's labels.
fn block_payload(blocks: &[OcrBlock], width: f32, height: f32) -> Value {
    let blocks: Vec<Value> = blocks
        .iter()
        .map(|block| {
            let label = match block.label.as_str() {
                "Section-Header" => "paragraph_title",
                "Page-Header" => "header",
                "Page-Footer" => "footer",
                "List-Group" => "list",
                "Table" => "table",
                "Caption" => "figure_title",
                "Footnote" => "footnote",
                "Table-Of-Contents" => "content",
                // PaddleOCR-VL's labels are PaddleOCR's own.
                label if label.chars().all(|c| c.is_ascii_lowercase() || c == '_') => label,
                _ => "text",
            };
            json!({
                "block_label": label,
                "block_content": block.text,
                "block_bbox": block.bbox,
            })
        })
        .collect();
    paddle_payload(blocks, width, height)
}

fn paddle_payload(blocks: Vec<Value>, width: f32, height: f32) -> Value {
    json!({
        "layoutParsingResults": [{
            "prunedResult": {
                "width": width,
                "height": height,
                "parsing_res_list": blocks,
            }
        }]
    })
}

/// A line's box: `[x0, y0, x1, y1]`.
fn bounds(line: &OcrLine) -> [f32; 4] {
    let xs = line.quad.map(|point| point[0]);
    let ys = line.quad.map(|point| point[1]);
    [
        xs.into_iter().fold(f32::MAX, f32::min),
        ys.into_iter().fold(f32::MAX, f32::min),
        xs.into_iter().fold(f32::MIN, f32::max),
        ys.into_iter().fold(f32::MIN, f32::max),
    ]
}

/// Lines into paragraphs: a line joins the paragraph whose last line is
/// just above it, overlaps it across, and is about as tall; columns stay
/// apart. A paragraph of one line much taller than the page's text is a
/// title.
fn payload(lines: &[OcrLine], width: f32, height: f32) -> Value {
    let mut boxes: Vec<([f32; 4], &str)> = lines
        .iter()
        .map(|line| (bounds(line), line.text.trim()))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    boxes.sort_by(|a, b| a.0[1].total_cmp(&b.0[1]).then(a.0[0].total_cmp(&b.0[0])));
    let mut heights: Vec<f32> = boxes.iter().map(|(b, _)| b[3] - b[1]).collect();
    heights.sort_by(f32::total_cmp);
    let body = heights.get(heights.len() / 2).copied().unwrap_or(10.0);
    // Paragraphs: their box, their last line's box, and their lines.
    let mut paragraphs: Vec<([f32; 4], [f32; 4], Vec<&str>)> = Vec::new();
    for (line, text) in boxes {
        let height = line[3] - line[1];
        let joins = paragraphs.iter_mut().rev().find(|(_, last, _)| {
            let last_height = last[3] - last[1];
            let gap = line[1] - last[3];
            let overlap = line[2].min(last[2]) - line[0].max(last[0]);
            let narrower = (line[2] - line[0]).min(last[2] - last[0]);
            gap > -0.5 * height
                && gap < 0.9 * height.max(last_height)
                && overlap > 0.3 * narrower
                && (height / last_height.max(1.0)) < SAME_SIZE
                && (last_height / height.max(1.0)) < SAME_SIZE
        });
        match joins {
            Some((area, last, texts)) => {
                *area = [
                    area[0].min(line[0]),
                    area[1].min(line[1]),
                    area[2].max(line[2]),
                    area[3].max(line[3]),
                ];
                *last = line;
                texts.push(text);
            }
            None => paragraphs.push((line, line, vec![text])),
        }
    }
    paragraphs.sort_by(|a, b| a.0[1].total_cmp(&b.0[1]).then(a.0[0].total_cmp(&b.0[0])));
    let blocks: Vec<Value> = paragraphs
        .iter()
        .map(|(area, last, texts)| {
            let title = texts.len() == 1 && (last[3] - last[1]) > body * 1.4;
            json!({
                "block_label": if title { "paragraph_title" } else { "text" },
                "block_content": texts.join("\n"),
                "block_bbox": area,
            })
        })
        .collect();
    paddle_payload(blocks, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> OcrLine {
        OcrLine {
            text: text.to_string(),
            quad: [[x0, y0], [x1, y0], [x1, y1], [x0, y1]],
            confidence: 0.99,
        }
    }

    /// The first page of `GLYPHST_OCR_TEST_PDF` recognised and normalised:
    /// `GLYPHST_OCR_DIR=… cargo test -p glyphst-pdf-translation-service --
    /// --ignored --nocapture`.
    #[test]
    #[ignore = "needs local OCR installed"]
    fn a_real_page_is_recognised_into_blocks() {
        let path = std::env::var_os("GLYPHST_OCR_TEST_PDF").expect("GLYPHST_OCR_TEST_PDF");
        let path = std::path::PathBuf::from(path);
        let document = Document::open(path.to_str().unwrap()).unwrap();
        let bytes = crate::job::extract_single_pdf_page(&path, 0).unwrap();
        let model =
            std::env::var("GLYPHST_OCR_TEST_MODEL").unwrap_or_else(|_| "pp-ocrv6-small".to_owned());
        let ocr = LocalOcr::installed(&model, Device::Cpu)
            .unwrap()
            .expect("local OCR is installed");
        let payload = recognize_page(&ocr, &bytes).unwrap();
        let bounds = document.load_page(0).unwrap().bounds().unwrap();
        let normalized = glyphst_pdf_translation::normalize_paddle_payload(
            &payload,
            "test.pdf",
            "test",
            ocr.model,
            &[glyphst_pdf_translation::PageGeometry::new(
                bounds.x1 - bounds.x0,
                bounds.y1 - bounds.y0,
            )],
        )
        .unwrap();
        let blocks = &normalized.pages[0].blocks;
        for block in blocks {
            println!(
                "{:?} {:?} {}",
                block.role,
                block.bbox,
                block.content.text.replace('\n', " ⏎ ")
            );
        }
        assert!(blocks.iter().any(|block| block.policy.translate));
    }

    #[test]
    fn lines_group_into_paragraphs_and_columns_stay_apart() {
        let lines = [
            line("A Title Here", 100.0, 40.0, 700.0, 90.0),
            line("left one", 100.0, 120.0, 380.0, 140.0),
            line("right one", 420.0, 121.0, 700.0, 141.0),
            line("left two", 100.0, 146.0, 380.0, 166.0),
            line("right two", 420.0, 147.0, 700.0, 167.0),
            line("left after a gap", 100.0, 230.0, 380.0, 250.0),
        ];
        let payload = payload(&lines, 800.0, 1000.0);
        let blocks = payload["layoutParsingResults"][0]["prunedResult"]["parsing_res_list"]
            .as_array()
            .unwrap();
        let texts: Vec<&str> = blocks
            .iter()
            .map(|block| block["block_content"].as_str().unwrap())
            .collect();
        assert_eq!(
            texts,
            [
                "A Title Here",
                "left one\nleft two",
                "right one\nright two",
                "left after a gap"
            ]
        );
        assert_eq!(blocks[0]["block_label"], "paragraph_title");
        assert_eq!(blocks[1]["block_label"], "text");
        assert_eq!(blocks[1]["block_bbox"], json!([100.0, 120.0, 380.0, 166.0]));
    }
}
