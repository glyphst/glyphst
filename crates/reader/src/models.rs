//! The local models' state for the Reader: what is installed, set up from
//! the `glyphst-models` catalog and the chosen settings.

use glyphst_settings::{OcrDevice, OcrSettings};

/// The device `settings` run local OCR on.
pub fn ocr_device(settings: &OcrSettings) -> glyphst_ocr::Device {
    match settings.local.device {
        OcrDevice::Cpu => glyphst_ocr::Device::Cpu,
        OcrDevice::Gpu => glyphst_ocr::Device::Gpu,
    }
}

/// Whether a translation into `language` (a code) sets CJK text.
pub fn is_cjk_language(language: &str) -> bool {
    let language = language.to_ascii_lowercase();
    ["zh", "ja", "ko"]
        .iter()
        .any(|code| language == *code || language.starts_with(&format!("{code}-")))
}

/// Whether the local OCR model `settings` choose is downloaded, with its
/// runtime.
pub fn local_ocr_installed(settings: &OcrSettings) -> bool {
    glyphst_models::catalog::entry(&settings.local.model).is_some_and(|model| {
        glyphst_ocr::setup(model, ocr_device(settings))
            .all_paths()
            .is_some()
    })
}
