//! What meaning search downloads, pinned: llama.cpp's own build of
//! `llama-server` for this machine, and EmbeddingGemma 2. A new build or
//! model is a code change with new hashes (`script/bump-llama.sh`).

pub use glyphst_models::{Asset, Engine};

pub const ENGINE_BUILD: &str = "b11724";

/// The embedder and reranker used until others are chosen (pinned in the
/// models catalog).
pub const MODEL: Asset = glyphst_models::catalog::EMBEDDINGGEMMA;
pub const RERANKER: Asset = glyphst_models::catalog::BGE_RERANKER;

/// What the vectors are, for the index: a new model, revision, size or
/// prompt makes new vectors.
pub const MODEL_KEY: &str = "ggml-org/embeddinggemma-2-GGUF@bfcd2987#embeddinggemma-2-Q8_0/256/1";

/// Dimensions kept of the model's 768 (Matryoshka), re-normalized.
pub const DIMENSIONS: usize = 256;

/// The engine built for this computer, if llama.cpp makes one: Vulkan on
/// Linux and Windows (any GPU, else the CPU), Metal on Apple silicon.
pub fn engine() -> Option<Engine> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(Engine {
            name: "llama.cpp",
            build: ENGINE_BUILD,
            archive: Asset {
                name: "llama-b11724-bin-ubuntu-vulkan-x64.tar.gz",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b11724/llama-b11724-bin-ubuntu-vulkan-x64.tar.gz",
                sha256: "947508489c07d2d47ba2c753cc92a9c1e3226405716fb396361f87c98a5928d1",
                size: 30_615_426,
            },
            exe: "llama-b11724/llama-server",
        })
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(Engine {
            name: "llama.cpp",
            build: ENGINE_BUILD,
            archive: Asset {
                name: "llama-b11724-bin-ubuntu-vulkan-arm64.tar.gz",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b11724/llama-b11724-bin-ubuntu-vulkan-arm64.tar.gz",
                sha256: "97afc80e0e0cc67ee180be221a201e8b2b5500993b564e29ca60aa6c050d57de",
                size: 24_310_214,
            },
            exe: "llama-b11724/llama-server",
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some(Engine {
            name: "llama.cpp",
            build: ENGINE_BUILD,
            archive: Asset {
                name: "llama-b11724-bin-win-vulkan-x64.zip",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b11724/llama-b11724-bin-win-vulkan-x64.zip",
                sha256: "82984b2d1bf1c80f033021b6c5173238ad7712cda02c5155b4cbedf859db24ba",
                size: 32_316_445,
            },
            exe: "llama-server.exe",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(Engine {
            name: "llama.cpp",
            build: ENGINE_BUILD,
            archive: Asset {
                name: "llama-b11724-bin-macos-arm64.tar.gz",
                url: "https://github.com/ggml-org/llama.cpp/releases/download/b11724/llama-b11724-bin-macos-arm64.tar.gz",
                sha256: "4e3c2951302b5fcee8d65327c6e2b28c382d4be87d9ca3af1346a7c092cd3335",
                size: 12_292_733,
            },
            exe: "llama-b11724/llama-server",
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pins_are_whole_and_never_float16() {
        assert!(!MODEL.name.contains("F16") && !MODEL.url.contains("F16"));
        assert_eq!(MODEL.sha256.len(), 64);
        assert!(MODEL.url.ends_with(MODEL.name));
        assert_eq!(RERANKER.sha256.len(), 64);
        assert!(RERANKER.url.ends_with(RERANKER.name));
        if let Some(engine) = engine() {
            assert_eq!(engine.archive.sha256.len(), 64);
            assert!(engine.archive.url.ends_with(engine.archive.name));
            assert!(engine.archive.name.contains(engine.build));
            assert!(engine.exe.contains("llama-server"));
        }
    }
}
