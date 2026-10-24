//! What local OCR downloads besides its models (pinned in the
//! `glyphst-models` catalog): ONNX Runtime's release build for this
//! computer, and for an NVIDIA card its CUDA 12 build, which needs the
//! system's CUDA 12 and cuDNN 9. A Mac's build has CoreML in it already.

use glyphst_models::{Asset, Engine};

use crate::Device;

pub const RUNTIME_VERSION: &str = "1.27.0";

/// ONNX Runtime's archive for this computer and `device`, and the library
/// inside it; `None` where there is no such build.
pub fn runtime(device: Device) -> Option<Engine> {
    let (name, archive, url, sha256, size, library) = match device {
        Device::Cpu => cpu_build()?,
        Device::Gpu => gpu_build().or_else(|| cfg!(target_os = "macos").then(cpu_build)?)?,
    };
    Some(Engine {
        name,
        build: RUNTIME_VERSION,
        archive: Asset {
            name: archive,
            url,
            sha256,
            size,
        },
        exe: library,
    })
}

/// An engine name, archive, its URL, SHA-256 and size, and the library
/// inside it.
type Build = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    u64,
    &'static str,
);

fn cpu_build() -> Option<Build> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "onnxruntime",
            "onnxruntime-linux-x64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-x64-1.27.0.tgz",
            "547e40a48f1fe73e3f812d7c88a948612c23f896b91e4e2ee1e232d7b468246f",
            8_831_605,
            "onnxruntime-linux-x64-1.27.0/lib/libonnxruntime.so.1.27.0",
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some((
            "onnxruntime",
            "onnxruntime-linux-aarch64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-aarch64-1.27.0.tgz",
            "3e4d83ac06924a32a07b6d7f91ce6f852876153fc0bbdf931bf517a140bfbe48",
            7_797_972,
            "onnxruntime-linux-aarch64-1.27.0/lib/libonnxruntime.so.1.27.0",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "onnxruntime",
            "onnxruntime-osx-arm64-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-osx-arm64-1.27.0.tgz",
            "545e81c58152353acb0d1e8bd6ce4b62f830c0961f5b3acfedc790ffd76e477a",
            32_485_368,
            "onnxruntime-osx-arm64-1.27.0/lib/libonnxruntime.1.27.0.dylib",
        ))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some((
            "onnxruntime",
            "onnxruntime-win-x64-1.27.0.zip",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-x64-1.27.0.zip",
            "c5c81710938e68079ff1a192b04897faabe4b43830d48f39f27ecd4e16138bfc",
            77_086_915,
            "onnxruntime-win-x64-1.27.0/lib/onnxruntime.dll",
        ))
    } else {
        None
    }
}

/// The CUDA 12 builds (Linux and Windows on x86-64).
fn gpu_build() -> Option<Build> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "onnxruntime-cuda",
            "onnxruntime-linux-x64-gpu_cuda12-1.27.0.tgz",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-linux-x64-gpu_cuda12-1.27.0.tgz",
            "3fed2d2f45f01f8bc1c1597a31afe29efd692c7ea4648d58e1844a8a0d0a48cb",
            239_349_446,
            "onnxruntime-linux-x64-gpu_cuda12-1.27.0/lib/libonnxruntime.so.1.27.0",
        ))
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some((
            "onnxruntime-cuda",
            "onnxruntime-win-x64-gpu_cuda12-1.27.0.zip",
            "https://github.com/microsoft/onnxruntime/releases/download/v1.27.0/onnxruntime-win-x64-gpu_cuda12-1.27.0.zip",
            "5174869c00f2f8c2ece31fe431821ac38773296f57962fec49dbb14ad5faca97",
            320_611_114,
            "onnxruntime-win-x64-gpu_cuda12-1.27.0/lib/onnxruntime.dll",
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtimes_name_their_files() {
        for device in [Device::Cpu, Device::Gpu] {
            if let Some(runtime) = runtime(device) {
                assert!(runtime.archive.url.ends_with(runtime.archive.name));
                assert_eq!(runtime.archive.sha256.len(), 64);
                assert!(runtime.exe.contains(RUNTIME_VERSION));
            }
        }
        // CPU and GPU builds unpack side by side.
        if let (Some(cpu), Some(gpu)) = (runtime(Device::Cpu), runtime(Device::Gpu))
            && cfg!(not(target_os = "macos"))
        {
            assert_ne!(cpu.name, gpu.name);
        }
    }
}
