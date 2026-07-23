use std::fs;
use std::path::PathBuf;

use glyphst_pdf_translation_app::{
    CancellationToken, PdfTranslationProgress, PdfTranslationRequest, PdfTranslationService,
};
use glyphst_settings::{Settings, app_paths, parse_settings};
use glyphst_translation::{LanguageCode, TranslationService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("GLYPHST_LIVE_PDF_TRANSLATION").is_none() {
        return Err(
            "set GLYPHST_LIVE_PDF_TRANSLATION=1 to permit OCR and translation API requests".into(),
        );
    }
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sample_directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("../translation-pdf-samples"));
    let output_directory = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/pdf-translation-live"));
    fs::create_dir_all(&output_directory)?;

    let settings_path = std::env::var_os("GLYPHST_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| app_paths::config_dir().join("config.kdl"));
    let mut settings: Settings = parse_settings(&fs::read_to_string(&settings_path)?)?;
    settings.sanitize();
    let resolved_ai = settings.ai.resolved_translation();
    println!(
        "OCR mode={}, OCR model={}, translation provider={}, AI model={}",
        settings.ocr.mode.as_str(),
        settings.ocr.model,
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

    let service = PdfTranslationService::new(TranslationService::new()?);
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
        let result = service.run(
            settings.clone(),
            PdfTranslationRequest {
                source_path,
                output_path: output_path.clone(),
                source_language: None,
                target_language: target_language.clone(),
            },
            CancellationToken::new(),
            move |progress: PdfTranslationProgress| {
                println!(
                    "{}: {:?} {:.0}% {}",
                    sample_for_progress,
                    progress.stage,
                    progress.overall_fraction * 100.0,
                    progress.detail
                );
            },
        );
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
