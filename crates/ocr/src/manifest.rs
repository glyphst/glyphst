//! What local OCR downloads, pinned: ONNX Runtime's own release for this
//! computer, and PP-OCRv6 Small's detection and recognition models and
//! dictionary (oar-ocr's v0.7.0 release, the files pdf-inspector pins).

use glyphst_models::{Asset, Engine};

pub const RUNTIME_VERSION: &str = "1.27.0";

/// The text detection model.
pub const DETECTION: Asset = Asset {
    name: "pp-ocrv6_small_det.onnx",
    url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/pp-ocrv6_small_det.onnx",
    sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
    size: 9_880_512,
};

/// The text recognition model.
pub const RECOGNITION: Asset = Asset {
    name: "pp-ocrv6_small_rec.onnx",
    url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/pp-ocrv6_small_rec.onnx",
    sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
    size: 21_159_378,
};

/// The recognition model's characters (Chinese, Latin, Japanese kana …).
pub const DICTIONARY: Asset = Asset {
    name: "ppocrv6_dict.txt",
    url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/ppocrv6_dict.txt",
    sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
    size: 74_947,
};

/// The models, in the order `LocalOcr::load` takes them.
pub const MODELS: [Asset; 3] = [DETECTION, RECOGNITION, DICTIONARY];

/// ONNX Runtime's archive for this computer and the library inside it.
pub fn runtime() -> Option<Engine> {
    let (name, url, sha256, size, library) = if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64"
    )) {
        (
            "onnxruntime-linux-x64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-x64-1.27.0.tgz",
            "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f",
            8_831_605,
            "onnxruntime-linux-x64-1.27.0/lib/libonnxruntime.so.1.27.0",
        )
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        (
            "onnxruntime-linux-aarch64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-aarch64-1.27.0.tgz",
            "3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48",
            7_797_972,
            "onnxruntime-linux-aarch64-1.27.0/lib/libonnxruntime.so.1.27.0",
        )
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        (
            "onnxruntime-osx-arm64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-osx-arm64-1.27.0.tgz",
            "545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a",
            32_485_368,
            "onnxruntime-osx-arm64-1.27.0/lib/libonnxruntime.1.27.0.dylib",
        )
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        (
            "onnxruntime-win-x64-1.27.0.zip",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-x64-1.27.0.zip",
            "c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc",
            77_086_915,
            "onnxruntime-win-x64-1.27.0/lib/onnxruntime.dll",
        )
    } else {
        return None;
    };
    Some(Engine {
        name: "onnxruntime",
        build: RUNTIME_VERSION,
        archive: Asset {
            name,
            url,
            sha256,
            size,
        },
        exe: library,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pins_name_their_files() {
        for asset in MODELS {
            assert!(asset.url.ends_with(asset.name));
            assert_eq!(asset.sha256.len(), 64);
        }
        if let Some(runtime) = runtime() {
            assert!(runtime.archive.url.ends_with(runtime.archive.name));
            assert!(runtime.exe.contains(RUNTIME_VERSION));
        }
    }
}
