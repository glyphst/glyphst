//! The real engine and model, by hand: set GLYPHST_EMBED_SERVER (the
//! llama-server) and GLYPHST_EMBED_MODEL (the GGUF), GLYPHST_EMBED_MODEL_ID
//! for a model other than EmbeddingGemma (its catalog id), and on NixOS
//! GLYPHST_EMBED_LIBS (the Vulkan loader's folder), then
//! `cargo test -p glyphst-embed --test real_model -- --ignored --nocapture`.

use std::{path::PathBuf, time::Instant};

use glyphst_embed::{llama::LlamaEmbedder, server::ServerConfig};
use glyphst_search::embed::Embedder as _;

#[test]
#[ignore = "needs the real engine and model"]
fn the_real_model_finds_the_passage_that_means_it() {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let (Some(exe), Some(model)) = (var("GLYPHST_EMBED_SERVER"), var("GLYPHST_EMBED_MODEL")) else {
        eprintln!("GLYPHST_EMBED_SERVER and GLYPHST_EMBED_MODEL are not set");
        return;
    };
    let started = Instant::now();
    let embedder = LlamaEmbedder::start(&ServerConfig {
        exe,
        model,
        library_dirs: var("GLYPHST_EMBED_LIBS").into_iter().collect(),
        pid_file: None,
        embedding: std::env::var("GLYPHST_EMBED_MODEL_ID")
            .map(|id| glyphst_embed::models::embedding(&id))
            .unwrap_or_default(),
    })
    .unwrap();
    eprintln!(
        "started in {:.1}s on {:?}",
        started.elapsed().as_secs_f32(),
        embedder.devices()
    );
    let passages = [
        (
            "Attention Is All You Need",
            "Multi-Head Attention allows the model to jointly attend to information from different representation subspaces.",
        ),
        (
            "Mamba",
            "Many subquadratic-time architectures address the computational inefficiency of Transformers on long sequences.",
        ),
        (
            "SICP",
            "Programs must be written for people to read, and only incidentally for machines to execute.",
        ),
    ];
    let vectors = embedder.embed_documents(&passages).unwrap();
    let query = embedder
        .embed_query("making transformers cheaper on very long inputs")
        .unwrap();
    assert_eq!(query.len(), glyphst_embed::manifest::DIMENSIONS);
    let scores = vectors
        .iter()
        .map(|vector| vector.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>())
        .collect::<Vec<_>>();
    eprintln!("scores {scores:?}");
    assert!(
        scores[1] > scores[0] && scores[1] > scores[2],
        "Mamba first"
    );

    let chunk =
        "The quick development of sequence models has led to many architectures. ".repeat(25);
    let batch = vec![("none", chunk.as_str()); 32];
    let timer = Instant::now();
    embedder.embed_documents(&batch).unwrap();
    eprintln!(
        "{:.1} passages a second",
        32.0 / timer.elapsed().as_secs_f32()
    );
}
