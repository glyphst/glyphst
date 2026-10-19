//! EmbeddingGemma 2 through llama.cpp's server: passages and queries with
//! the model card's prompts, in batches, cut to the dimensions the index
//! keeps and made unit length again.

use anyhow::{Result, anyhow};
use glyphst_search::embed::{Embedder, document_prompt, query_prompt, truncate};

use crate::{
    manifest::{DIMENSIONS, MODEL_KEY},
    server::{Server, ServerConfig, ServerError},
};

/// Passages sent at once.
const BATCH: usize = 16;

pub struct LlamaEmbedder {
    server: Server,
}

impl LlamaEmbedder {
    pub fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        Ok(Self {
            server: Server::start(config)?,
        })
    }

    /// Whether it found a GPU (else it runs on the CPU, more slowly).
    pub fn on_gpu(&self) -> bool {
        !self.server.devices.is_empty()
    }

    pub fn devices(&self) -> &[String] {
        &self.server.devices
    }

    pub fn is_alive(&self) -> bool {
        self.server.is_alive()
    }

    pub fn log_tail(&self) -> String {
        self.server.log_tail()
    }

    fn vectors(&self, inputs: Vec<String>) -> Result<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(inputs.len());
        for batch in inputs.chunks(BATCH) {
            for vector in self.server.client().embed(batch)? {
                vectors.push(
                    truncate(&vector, DIMENSIONS)
                        .ok_or_else(|| anyhow!("the engine gave a broken vector"))?,
                );
            }
        }
        Ok(vectors)
    }
}

impl Embedder for LlamaEmbedder {
    fn model_key(&self) -> &str {
        MODEL_KEY
    }

    fn embed_documents(&self, passages: &[(&str, &str)]) -> Result<Vec<Vec<f32>>> {
        self.vectors(
            passages
                .iter()
                .map(|(title, text)| document_prompt(title, text))
                .collect(),
        )
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        self.vectors(vec![query_prompt(query)])?
            .pop()
            .ok_or_else(|| anyhow!("the engine gave no vector"))
    }
}
