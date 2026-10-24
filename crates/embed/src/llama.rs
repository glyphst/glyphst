//! An embedder (EmbeddingGemma 2, Qwen3 Embedding) through llama.cpp's
//! server: passages and queries with the model card's prompts, in batches,
//! cut to the dimensions the index keeps and made unit length again.

use anyhow::{Result, anyhow};
use glyphst_search::embed::{
    Embedder, Reranker, document_prompt, query_prompt, qwen3_document_prompt, qwen3_query_prompt,
    truncate,
};

use crate::{
    manifest::DIMENSIONS,
    models::{Embedding, Prompts},
    server::{Mode, Server, ServerConfig, ServerError},
};

/// Passages sent at once.
const BATCH: usize = 16;

pub struct LlamaEmbedder {
    server: Server,
    embedding: Embedding,
}

impl LlamaEmbedder {
    pub fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        Ok(Self {
            server: Server::start(config)?,
            embedding: config.embedding,
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
        self.embedding.model_key
    }

    fn embed_documents(&self, passages: &[(&str, &str)]) -> Result<Vec<Vec<f32>>> {
        let prompt = match self.embedding.prompts {
            Prompts::Gemma => document_prompt,
            Prompts::Qwen3 => qwen3_document_prompt,
        };
        self.vectors(
            passages
                .iter()
                .map(|(title, text)| prompt(title, text))
                .collect(),
        )
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let prompt = match self.embedding.prompts {
            Prompts::Gemma => query_prompt(query),
            Prompts::Qwen3 => qwen3_query_prompt(query),
        };
        self.vectors(vec![prompt])?
            .pop()
            .ok_or_else(|| anyhow!("the engine gave no vector"))
    }
}

/// A cross-encoder through llama.cpp's server (`--rerank`): rereads the
/// best passages against the query.
pub struct LlamaReranker {
    server: Server,
}

impl LlamaReranker {
    pub fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        Ok(Self {
            server: Server::start_as(config, Mode::Rerank)?,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.server.is_alive()
    }

    pub fn log_tail(&self) -> String {
        self.server.log_tail()
    }
}

impl Reranker for LlamaReranker {
    fn rerank(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>> {
        let mut scores = Vec::with_capacity(passages.len());
        for batch in passages.chunks(BATCH) {
            scores.extend(self.server.client().rerank(query, batch)?);
        }
        Ok(scores)
    }
}
