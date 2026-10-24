//! Where a root (meaning search's, local OCR's) keeps its engines and
//! models, and putting an engine in place: the archive is unpacked next to
//! its folder and renamed into it, so a half-unpacked engine is never used.
//! Unpacking uses the system's `tar`, which reads `.tar.gz` everywhere and
//! `.zip` on Windows and macOS.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};

use crate::manifest::{Asset, Engine};

pub fn engine_dir(root: &Path, engine: &Engine) -> PathBuf {
    root.join("engines")
        .join(format!("{}-{}", engine.name, engine.build))
}

pub fn engine_archive(root: &Path, engine: &Engine) -> PathBuf {
    root.join("downloads").join(engine.archive.name)
}

/// The server, once installed.
pub fn engine_exe(root: &Path, engine: &Engine) -> Option<PathBuf> {
    let exe = engine_dir(root, engine).join(engine.exe);
    exe.is_file().then_some(exe)
}

pub fn model_path(root: &Path, model: &Asset) -> PathBuf {
    root.join("models").join(model.name)
}

/// A model, once downloaded whole.
pub fn model_ready(root: &Path, model: &Asset) -> bool {
    fs::metadata(model_path(root, model)).is_ok_and(|metadata| metadata.len() == model.size)
}

/// Unpacks the downloaded `archive` into the engine's folder, then deletes
/// the archive.
pub fn unpack(root: &Path, engine: &Engine, archive: &Path) -> Result<PathBuf> {
    let dir = engine_dir(root, engine);
    let staging = dir.with_extension("unpacking");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)
        .with_context(|| format!("failed to create {}", staging.display()))?;
    let output = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(&staging)
        .output()
        .context("failed to run tar to unpack the engine")?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&staging);
        bail!(
            "could not unpack the engine: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !staging.join(engine.exe).is_file() {
        let _ = fs::remove_dir_all(&staging);
        bail!("the engine's archive has no {}", engine.exe);
    }
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&staging, &dir)
        .with_context(|| format!("failed to move the engine to {}", dir.display()))?;
    let _ = fs::remove_file(archive);
    Ok(dir.join(engine.exe))
}

/// Removes other builds of `engine` (an update left them behind); other
/// engines in the root (a GPU build beside the CPU one) stay.
pub fn remove_old_engines(root: &Path, engine: &Engine) {
    let keep = engine_dir(root, engine);
    let prefix = format!("{}-", engine.name);
    if let Ok(entries) = fs::read_dir(root.join("engines")) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let older = name.to_str().is_some_and(|name| {
                name.strip_prefix(&prefix)
                    .is_some_and(|build| !build.contains('-') || build.ends_with(".unpacking"))
            });
            if entry.path() != keep && older {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Deletes `models` from `root` (and a folder they leave empty).
pub fn remove_models(root: &Path, models: &[Asset]) -> std::io::Result<()> {
    for model in models {
        let path = model_path(root, model);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = path.parent()
            && parent != root.join("models")
        {
            let _ = fs::remove_dir(parent);
        }
    }
    Ok(())
}

/// Bytes the files under `folder` take.
pub fn folder_size(folder: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
            Ok(_) => entry.metadata().map_or(0, |metadata| metadata.len()),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_removes_only_older_builds_of_the_same_engine() {
        let root =
            std::env::temp_dir().join(format!("glyphst-models-engines-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for name in [
            "onnxruntime-1.26.0",
            "onnxruntime-1.27.0",
            "onnxruntime-cuda-1.27.0",
        ] {
            fs::create_dir_all(root.join("engines").join(name)).unwrap();
        }
        let engine = Engine {
            name: "onnxruntime",
            build: "1.27.0",
            archive: Asset {
                name: "",
                url: "",
                sha256: "",
                size: 0,
            },
            exe: "",
        };
        remove_old_engines(&root, &engine);
        let left = |name: &str| root.join("engines").join(name).exists();
        assert!(!left("onnxruntime-1.26.0"));
        assert!(left("onnxruntime-1.27.0"));
        assert!(left("onnxruntime-cuda-1.27.0"), "the GPU build stays");

        // A model in its own folder goes with the folder.
        let model = Asset {
            name: "indra/det.onnx",
            ..engine.archive
        };
        fs::create_dir_all(root.join("models/indra")).unwrap();
        fs::write(model_path(&root, &model), b"weights").unwrap();
        assert_eq!(folder_size(&root.join("models")), 7);
        remove_models(&root, &[model]).unwrap();
        assert!(!root.join("models/indra").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_archive_unpacks_into_place_and_a_bad_one_leaves_nothing() {
        let root =
            std::env::temp_dir().join(format!("glyphst-embed-install-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source/llama-b1");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("llama-server"), b"#!/bin/sh\n").unwrap();
        let archive = root.join("engine.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root.join("source"))
            .arg("llama-b1")
            .status()
            .unwrap();
        assert!(status.success());
        let engine = Engine {
            name: "llama.cpp",
            build: "b1",
            archive: Asset {
                name: "engine.tar.gz",
                url: "",
                sha256: "",
                size: 0,
            },
            exe: "llama-b1/llama-server",
        };
        let exe = unpack(&root, &engine, &archive).unwrap();
        assert!(exe.is_file());
        assert_eq!(engine_exe(&root, &engine), Some(exe));
        assert!(!archive.exists(), "the archive goes once unpacked");

        let wrong = Engine {
            exe: "llama-b1/missing",
            ..engine
        };
        fs::write(&archive, b"not an archive").unwrap();
        assert!(unpack(&root, &wrong, &archive).is_err());
        assert!(
            !engine_dir(&root, &wrong)
                .with_extension("unpacking")
                .exists()
        );
        let _ = fs::remove_dir_all(root);
    }
}
