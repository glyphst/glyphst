use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use glyphst_pdf_translation_app::{
    CancellationToken, PdfTranslationProgress, PdfTranslationRequest, PdfTranslationService,
};
use glyphst_settings::{
    OcrMode, OcrProvider, Settings, TranslationProvider, app_paths, parse_settings,
};
use glyphst_translation::{LanguageCode, TranslationService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
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
    let sample_directory = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("../translation-pdf-samples"));
    let output_directory = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target/pdf-translation-live"));
    fs::create_dir_all(&output_directory)?;

    let settings_path = env_value("GLYPHST_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let managed = app_paths::config_dir().join("config.kdl");
            if managed.is_file() {
                managed
            } else {
                app_paths::state_dir().join("config.kdl")
            }
        });
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
