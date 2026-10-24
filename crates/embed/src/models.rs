//! The models meaning search can run, by `glyphst-models` catalog id: how
//! llama.cpp pools each embedder's vectors, the prompts it was trained
//! with, and what its vectors are called in the index (a new model makes
//! new vectors, so switching re-embeds the library).

use glyphst_models::catalog::{self, ModelEntry, Task};

/// The prompts an embedder was trained with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prompts {
    /// EmbeddingGemma: `title: … | text: …` and `task: search result | query: …`.
    Gemma,
    /// Qwen3 Embedding: plain passages, an instruction before each query.
    Qwen3,
}

/// An embedder as meaning search runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Embedding {
    pub entry: &'static ModelEntry,
    /// llama.cpp's `--pooling`.
    pub pooling: &'static str,
    pub prompts: Prompts,
    /// The vectors' name in the index: the model, revision, size and
    /// prompt version.
    pub model_key: &'static str,
}

/// The embedder `id` (the default for an unknown one).
pub fn embedding(id: &str) -> Embedding {
    let (pooling, prompts, model_key) = match id {
        "qwen3-embedding-0.6b" => (
            "last",
            Prompts::Qwen3,
            "Qwen/Qwen3-Embedding-0.6B-GGUF@370f27d7#Q8_0/256/1",
        ),
        "qwen3-embedding-4b" => (
            "last",
            Prompts::Qwen3,
            "Qwen/Qwen3-Embedding-4B-GGUF@f4602530#Q4_K_M/256/1",
        ),
        _ => ("mean", Prompts::Gemma, crate::manifest::MODEL_KEY),
    };
    let entry = catalog::entry(id)
        .filter(|entry| entry.task == Task::Embedding && prompts != Prompts::Gemma)
        .unwrap_or_else(|| catalog::default_for(Task::Embedding));
    Embedding {
        entry,
        pooling,
        prompts,
        model_key,
    }
}

impl Default for Embedding {
    fn default() -> Self {
        embedding(catalog::default_for(Task::Embedding).id)
    }
}

/// The reranker `id` (the default for an unknown one).
pub fn reranker(id: &str) -> &'static ModelEntry {
    catalog::entry(id)
        .filter(|entry| entry.task == Task::Rerank)
        .unwrap_or_else(|| catalog::default_for(Task::Rerank))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_embedder_names_its_own_vectors() {
        let gemma = embedding("embeddinggemma-2");
        assert_eq!(gemma.model_key, crate::manifest::MODEL_KEY);
        assert_eq!(gemma.entry.files[0], crate::manifest::MODEL);
        let qwen = embedding("qwen3-embedding-0.6b");
        assert_eq!(qwen.pooling, "last");
        assert_ne!(qwen.model_key, gemma.model_key);
        assert_eq!(qwen.entry.id, "qwen3-embedding-0.6b");
        assert_eq!(embedding("nonsense"), gemma);
        assert_eq!(reranker("qwen3-reranker-0.6b").id, "qwen3-reranker-0.6b");
        assert_eq!(reranker("embeddinggemma-2").id, "bge-reranker-v2-m3");
    }
}
