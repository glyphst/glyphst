use std::fs;
use std::path::{Path, PathBuf};

use glyphst_pdf_translation::{
    CancellationToken, LayoutOptions, PdfComposer, TranslationBackend, TranslationBackendError,
    TranslationUnit, TranslationUnitOutput, TypstCompiler, TypstCompilerConfig,
    build_layout_with_options, extract_native_document, fuse_native_with_ocr, inspect_pdf,
    normalize_paddle_payload, sanitize_source_pdf, translate_document,
};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Default)]
struct DeterministicCjkBackend;

impl TranslationBackend for DeterministicCjkBackend {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
        Ok(units
            .iter()
            .map(|unit| TranslationUnitOutput {
                id: unit.id.clone(),
                text: deterministic_cjk_fixture(&unit.protected.text),
                cached: false,
                validation_error: None,
            })
            .collect())
    }
}

#[derive(Debug, Serialize)]
struct SampleReport {
    sample: String,
    pages: usize,
    source_blocks: usize,
    protected_regions: usize,
    translated_blocks: usize,
    styled_runs: usize,
    layout_items: usize,
    layout_warnings: Vec<String>,
    output_pdf: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sample_directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("../translation-pdf-samples"));
    let output_directory = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/pdf-translation-visual"));
    fs::create_dir_all(&output_directory)?;

    let mut samples = fs::read_dir(&sample_directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "pdf"))
        .collect::<Vec<_>>();
    samples.sort();
    if samples.is_empty() {
        return Err(format!("no PDF samples found in {}", sample_directory.display()).into());
    }

    let cancellation = CancellationToken::new();
    let compiler = TypstCompiler::new(TypstCompilerConfig::default());
    let mut reports = Vec::with_capacity(samples.len());
    for source_path in samples {
        let sample = source_path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("sample path has no UTF-8 file stem")?;
        let job_directory = output_directory.join(sample);
        fs::create_dir_all(&job_directory)?;

        let inspection = inspect_pdf(&source_path, &cancellation)?;
        let mut document =
            extract_native_document(&source_path, format!("sample-{sample}"), &cancellation)?;
        if let Some(fixture_path) = paddle_fixture_for(sample) {
            let payload: Value = serde_json::from_slice(&fs::read(&fixture_path)?)?;
            let target_pages = inspection
                .pages
                .iter()
                .map(|page| glyphst_pdf_translation::PageGeometry::new(page.width, page.height))
                .collect::<Vec<_>>();
            let ocr = normalize_paddle_payload(
                &payload,
                source_path.clone(),
                format!("sample-{sample}-ocr"),
                "PaddleOCR-VL-1.6",
                &target_pages,
            )?;
            document = fuse_native_with_ocr(document, &ocr);
        }
        let source_blocks = document
            .pages
            .iter()
            .map(|page| page.blocks.len())
            .sum::<usize>();
        let protected_regions = document
            .pages
            .iter()
            .map(|page| page.protected_regions.len())
            .sum::<usize>();
        assert_no_translated_blocks_in_protected_regions(&document)?;
        write_json(job_directory.join("document.json"), &document)?;
        let translated =
            translate_document(document, &DeterministicCjkBackend, &cancellation, |_, _| {})?;
        let translated_blocks = translated.units.len();
        let styled_runs = translated
            .units
            .iter()
            .map(|unit| unit.styled_runs.len())
            .sum::<usize>();
        write_json(job_directory.join("translated-document.json"), &translated)?;

        let layout = build_layout_with_options(&translated, LayoutOptions::strict());
        let layout_items = layout
            .pages
            .iter()
            .map(|page| page.items.len())
            .sum::<usize>();
        if translated_blocks > 0 && layout_items == 0 {
            return Err(format!("sample {sample} produced no layout items").into());
        }
        write_json(job_directory.join("layout.json"), &layout)?;

        let sanitized = job_directory.join("source-sanitized.pdf");
        sanitize_source_pdf(&source_path, &sanitized, &layout, &cancellation)?;
        let typst =
            compiler.compile_translated_pdf(&sanitized, &layout, &job_directory, &cancellation)?;
        let output_pdf = output_directory.join(format!("{sample}-cjk-layout.pdf"));
        PdfComposer.compose(
            &source_path,
            &typst.pdf_path,
            &output_pdf,
            &layout,
            &inspection,
            &cancellation,
        )?;

        reports.push(SampleReport {
            sample: sample.to_string(),
            pages: layout.pages.len(),
            source_blocks,
            protected_regions,
            translated_blocks,
            styled_runs,
            layout_items,
            layout_warnings: layout
                .warnings
                .iter()
                .map(|warning| format!("{}: {}", warning.unit_id, warning.message))
                .collect(),
            output_pdf,
        });
    }
    write_json(output_directory.join("report.json"), &reports)?;
    for report in reports {
        println!(
            "{}: {} pages, {} translated blocks, {} styled runs, {} warnings -> {}",
            report.sample,
            report.pages,
            report.translated_blocks,
            report.styled_runs,
            report.layout_warnings.len(),
            report.output_pdf.display()
        );
    }
    Ok(())
}

fn paddle_fixture_for(sample: &str) -> Option<PathBuf> {
    let direct = std::env::var_os(format!(
        "GLYPHST_PADDLE_FIXTURE_{}",
        sample.replace('-', "_")
    ))
    .map(PathBuf::from);
    direct.filter(|path| path.is_file()).or_else(|| {
        std::env::var_os("GLYPHST_PADDLE_FIXTURE_DIR")
            .map(PathBuf::from)
            .map(|directory| directory.join(format!("{sample}.json")))
            .filter(|path| path.is_file())
    })
}

fn assert_no_translated_blocks_in_protected_regions(
    document: &glyphst_pdf_translation::DocumentV1,
) -> Result<(), Box<dyn std::error::Error>> {
    for page in &document.pages {
        for block in page.blocks.iter().filter(|block| block.policy.translate) {
            let x = (block.bbox.x0 + block.bbox.x1) * 0.5;
            let y = (block.bbox.y0 + block.bbox.y1) * 0.5;
            if let Some(region) = page.protected_regions.iter().find(|region| {
                x >= region.bbox.x0
                    && x <= region.bbox.x1
                    && y >= region.bbox.y0
                    && y <= region.bbox.y1
            }) {
                return Err(format!(
                    "page {} block {} is translatable inside protected {} region {}",
                    page.page_index + 1,
                    block.id,
                    region.kind.as_str(),
                    region.id
                )
                .into());
            }
        }
    }
    Ok(())
}

fn deterministic_cjk_fixture(source: &str) -> String {
    let mut translated = String::with_capacity(source.len());
    let mut cursor = 0usize;
    while cursor < source.len() {
        if source[cursor..].starts_with("[[GLYPHST_")
            && let Some(relative_end) = source[cursor..].find("]]")
        {
            let end = cursor + relative_end + 2;
            translated.push_str(&source[cursor..end]);
            cursor = end;
            continue;
        }
        let character = source[cursor..].chars().next().expect("valid UTF-8 cursor");
        if character.is_ascii_alphabetic() {
            let word_start = cursor;
            cursor += character.len_utf8();
            while cursor < source.len() {
                let next = source[cursor..].chars().next().expect("valid UTF-8 cursor");
                if !next.is_ascii_alphabetic() {
                    break;
                }
                cursor += next.len_utf8();
            }
            let characters = source[word_start..cursor].chars().count().div_ceil(5);
            for index in 0..characters.max(1) {
                translated.push(['译', '文', '测', '试'][index % 4]);
            }
            continue;
        }
        if !character.is_ascii_whitespace() || character == '\n' {
            translated.push(character);
        }
        cursor += character.len_utf8();
    }
    translated
}

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    fs::write(path, bytes)
}
