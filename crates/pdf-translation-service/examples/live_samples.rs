use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use glyphst_pdf_translation_service::{
    CancellationToken, PdfTranslationConfig, PdfTranslationExportRequest,
    PdfTranslationPagePreview, PdfTranslationProgress, PdfTranslationRequest,
    PdfTranslationService, PdfTranslationSessionRequest, PdfTranslationSessionUpdate,
};
use glyphst_settings::{
    OcrMode, OcrProvider, Settings, TranslationProvider, app_paths, parse_settings,
};
use glyphst_translation::{LanguageCode, TranslationService};
use mupdf::{Colorspace, Document, Matrix};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HarnessMode {
    #[default]
    Batch,
    Session,
    Compare,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (mode, positional) = parse_arguments()?;
    let dotenv = read_dotenv(workspace.join(".env"))?;
    let env_value = |name: &str| {
        std::env::var(name)
            .ok()
            .or_else(|| dotenv.get(name).cloned())
    };
    if env_value("GLYPHST_LIVE_PDF_TRANSLATION").is_none() {
        return Err(
            "set GLYPHST_LIVE_PDF_TRANSLATION=1 in the environment or workspace .env to permit OCR and translation API requests".into(),
        );
    }
    let sample_directory = positional
        .first()
        .cloned()
        .unwrap_or_else(|| workspace.join("../translation-pdf-samples"));
    let output_directory = positional
        .get(1)
        .cloned()
        .unwrap_or_else(|| workspace.join("target/pdf-translation-live"));
    fs::create_dir_all(&output_directory)?;

    let settings_path = env_value("GLYPHST_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| app_paths::config_dir().join("config.kdl"));
    let mut settings: Settings = parse_settings(&fs::read_to_string(&settings_path)?)?;
    if let Some(api_key) = env_value("GLYPHST_BAIDU_OCR_API_KEY") {
        settings.ocr.unlimited.api_key = api_key;
    }
    if let Some(secret_key) = env_value("GLYPHST_BAIDU_OCR_SECRET_KEY") {
        settings.ocr.unlimited.secret_key = secret_key;
    }
    if let Some(mode) = env_value("GLYPHST_LIVE_OCR_MODE") {
        settings.ocr.mode = match mode.trim().to_ascii_lowercase().as_str() {
            "always" => OcrMode::Always,
            "auto" => OcrMode::Auto,
            "off" => OcrMode::Off,
            _ => return Err(format!("unsupported GLYPHST_LIVE_OCR_MODE {mode:?}").into()),
        };
    }
    if let Some(provider) = env_value("GLYPHST_LIVE_OCR_PROVIDER") {
        settings.ocr.provider = match provider.trim().to_ascii_lowercase().as_str() {
            "paddle" | "paddle-ocr" | "paddleocr" => OcrProvider::Paddle,
            "unlimited" | "unlimited-ocr" => OcrProvider::Unlimited,
            _ => {
                return Err(format!("unsupported GLYPHST_LIVE_OCR_PROVIDER {provider:?}").into());
            }
        };
    }
    if let Some(provider) = env_value("GLYPHST_LIVE_TRANSLATION_PROVIDER") {
        let Some(provider) = TranslationProvider::parse(&provider) else {
            return Err(
                format!("unsupported GLYPHST_LIVE_TRANSLATION_PROVIDER {provider:?}").into(),
            );
        };
        settings.translation.active_provider = provider;
    }
    settings.sanitize();
    let translation_config = PdfTranslationConfig::from(&settings);
    let resolved_ai = settings.ai.resolved_translation();
    println!(
        "OCR mode={}, OCR provider={}, translation provider={}, AI model={}",
        settings.ocr.mode.as_str(),
        settings.ocr.provider.as_str(),
        settings.translation.active_provider.as_str(),
        resolved_ai.model
    );

    let mut samples = if sample_directory.is_file() {
        vec![sample_directory.clone()]
    } else {
        fs::read_dir(&sample_directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "pdf"))
            .collect::<Vec<_>>()
    };
    samples.sort();
    if samples.is_empty() {
        return Err(format!("no PDF samples found in {}", sample_directory.display()).into());
    }

    let service = PdfTranslationService::new_in_process(TranslationService::new()?);
    let target_language = LanguageCode::new("zh-hans")?;
    let mut failures = Vec::new();
    for source_path in samples {
        let sample = source_path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("sample path has no UTF-8 file stem")?
            .to_string();
        let output_path = output_directory.join(format!("{sample}-zh-hans.pdf"));
        println!("{sample}: starting");
        let sample_for_progress = sample.clone();
        let result = match mode {
            HarnessMode::Batch => service.run(
                translation_config.clone(),
                PdfTranslationRequest {
                    source_path,
                    output_path: output_path.clone(),
                    source_language: None,
                    target_language: target_language.clone(),
                },
                CancellationToken::new(),
                progress_logger(sample_for_progress),
            ),
            HarnessMode::Session | HarnessMode::Compare => {
                let previews = Arc::new(Mutex::new(BTreeMap::new()));
                let captured_previews = Arc::clone(&previews);
                let sample_for_updates = sample_for_progress.clone();
                let session = service.run_session(
                    translation_config.clone(),
                    PdfTranslationSessionRequest {
                        source_path,
                        source_language: None,
                        target_language: target_language.clone(),
                    },
                    CancellationToken::new(),
                    progress_logger(sample_for_progress.clone()),
                    move |update| match update {
                        PdfTranslationSessionUpdate::PagePreviewReady(preview) => {
                            let mut previews = captured_previews.lock().unwrap();
                            let replace = previews.get(&preview.page_index).is_none_or(
                                |current: &PdfTranslationPagePreview| {
                                    preview.revision > current.revision
                                },
                            );
                            if replace {
                                previews.insert(preview.page_index, preview);
                            }
                        }
                        PdfTranslationSessionUpdate::PageFailed(failure) => eprintln!(
                            "{sample_for_updates}: page {} {:?} preview failure: {}",
                            failure.page_index + 1,
                            failure.phase,
                            failure.message
                        ),
                        _ => {}
                    },
                );
                match session {
                    Ok(session) => {
                        let exported = service.export_session(
                            translation_config.clone(),
                            session,
                            PdfTranslationExportRequest {
                                output_path: output_path.clone(),
                                original_unit_ids: Vec::new(),
                                layout: Default::default(),
                            },
                            CancellationToken::new(),
                            progress_logger(sample_for_progress.clone()),
                        );
                        if mode == HarnessMode::Compare
                            && let Ok(exported) = &exported
                        {
                            let previews = previews.lock().unwrap();
                            compare_live_previews(&previews, &exported.output_path)?;
                            println!(
                                "{sample}: {} live preview page(s) match the exported PDF",
                                previews.len()
                            );
                        }
                        exported
                    }
                    Err(error) => Err(error),
                }
            }
        };
        match result {
            Ok(result) => println!("{sample}: {}", result.output_path.display()),
            Err(error) => {
                eprintln!("{sample}: {:?}: {}", error.stage, error.message);
                failures.push(sample);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("live translation failed for: {}", failures.join(", ")).into())
    }
}

fn parse_arguments() -> Result<(HarnessMode, Vec<PathBuf>), Box<dyn std::error::Error>> {
    let mut mode = HarnessMode::Batch;
    let mut positional = Vec::new();
    for argument in std::env::args_os().skip(1) {
        match argument.to_str() {
            Some("--session") => mode = HarnessMode::Session,
            Some("--compare") => mode = HarnessMode::Compare,
            Some("--help" | "-h") => {
                println!(
                    "usage: live_samples [--session|--compare] [SAMPLE_PDF_OR_DIRECTORY] [OUTPUT_DIRECTORY]"
                );
                std::process::exit(0);
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unsupported live_samples option {value:?}").into());
            }
            _ => positional.push(PathBuf::from(argument)),
        }
    }
    if positional.len() > 2 {
        return Err("live_samples accepts at most a sample path and an output directory".into());
    }
    Ok((mode, positional))
}

fn progress_logger(sample: String) -> impl Fn(PdfTranslationProgress) + Send + 'static {
    move |progress| {
        println!(
            "{}: {:?} {:.0}% {}",
            sample,
            progress.stage,
            progress.overall_fraction * 100.0,
            progress.detail
        );
    }
}

fn compare_live_previews(
    previews: &BTreeMap<usize, PdfTranslationPagePreview>,
    exported_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    if previews.is_empty() {
        return Err("compare mode did not receive any live page previews".into());
    }
    let exported = Document::open(exported_path.to_string_lossy().as_ref())?;
    for (page_index, preview) in previews {
        let live = Document::from_bytes(preview.pdf.as_ref(), "application/pdf")?;
        let live_page = live.load_page(0)?;
        let exported_page = exported.load_page(*page_index as i32)?;
        let colorspace = Colorspace::device_rgb();
        let live_pixels = live_page.to_pixmap(&Matrix::IDENTITY, &colorspace, false, false)?;
        let exported_pixels =
            exported_page.to_pixmap(&Matrix::IDENTITY, &colorspace, false, false)?;
        if live_pixels.width() != exported_pixels.width()
            || live_pixels.height() != exported_pixels.height()
            || live_pixels.n() != exported_pixels.n()
        {
            return Err(format!(
                "page {} live/export raster dimensions differ",
                page_index + 1
            )
            .into());
        }
        let components = usize::from(live_pixels.n());
        let mut changed_pixels = 0usize;
        let mut absolute_error = 0usize;
        for (live, exported) in live_pixels
            .samples()
            .chunks_exact(components)
            .zip(exported_pixels.samples().chunks_exact(components))
        {
            let mut changed = false;
            for (&left, &right) in live.iter().zip(exported) {
                let difference = left.abs_diff(right) as usize;
                absolute_error += difference;
                changed |= difference > 24;
            }
            changed_pixels += usize::from(changed);
        }
        let pixel_count = (live_pixels.width() as usize) * (live_pixels.height() as usize);
        let changed_fraction = changed_pixels as f64 / pixel_count.max(1) as f64;
        let mean_error = absolute_error as f64 / (pixel_count * components).max(1) as f64;
        if changed_fraction > 0.002 || mean_error > 0.5 {
            return Err(format!(
                "page {} live/export raster mismatch: {:.3}% changed pixels, {:.3} mean channel error",
                page_index + 1,
                changed_fraction * 100.0,
                mean_error
            )
            .into());
        }
    }
    Ok(())
}

fn read_dotenv(path: PathBuf) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut values = BTreeMap::new();
    for (line_index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!(
                "{}:{} is not a KEY=VALUE assignment",
                path.display(),
                line_index + 1
            )
            .into());
        };
        let name = name.trim();
        if name.is_empty() {
            return Err(format!(
                "{}:{} has an empty variable name",
                path.display(),
                line_index + 1
            )
            .into());
        }
        let value = value.trim();
        let value = if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            &value[1..value.len() - 1]
        } else {
            value
        };
        values.insert(name.to_string(), value.to_string());
    }
    Ok(values)
}
