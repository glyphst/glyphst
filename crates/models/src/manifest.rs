//! What a model or engine is on the network: pinned files, each with its
//! SHA-256 and size.

/// A file to fetch: where from, its SHA-256 and size. A `name` with a
/// folder (`indra-v1.1.0/det.onnx`) keeps a model's files together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// An engine's archive for this machine, and the program (or library)
/// inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Engine {
    /// What it is (`llama.cpp`, `onnxruntime-cuda`), naming its folder
    /// with the build.
    pub name: &'static str,
    pub build: &'static str,
    pub archive: Asset,
    /// The program's path inside the archive.
    pub exe: &'static str,
}
