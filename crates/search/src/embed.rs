//! What meaning search needs from a model: vectors for passages and for
//! queries. The runtime (llama.cpp's server today) stays behind this, so
//! another can replace it without touching the index or the search.

use anyhow::Result;

/// Turns text into unit vectors of `dims()` numbers.
pub trait Embedder: Send + Sync {
    /// Names the model, its revision, the dimensions kept and the prompt;
    /// vectors made under another key are made again.
    fn model_key(&self) -> &str;

    /// Passages, each with its document's title.
    fn embed_documents(&self, passages: &[(&str, &str)]) -> Result<Vec<Vec<f32>>>;

    fn embed_query(&self, query: &str) -> Result<Vec<f32>>;
}

/// EmbeddingGemma's prompt for a passage.
pub fn document_prompt(title: &str, text: &str) -> String {
    let title = title.trim();
    format!(
        "title: {} | text: {text}",
        if title.is_empty() { "none" } else { title }
    )
}

/// EmbeddingGemma's prompt for a search.
pub fn query_prompt(query: &str) -> String {
    format!("task: search result | query: {query}")
}

/// The first `dims` numbers (Matryoshka), at unit length again. `None` for
/// a vector with NaN, infinity or no length, which a model must not give.
pub fn truncate(vector: &[f32], dims: usize) -> Option<Vec<f32>> {
    let kept = &vector[..dims.min(vector.len())];
    if kept.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let norm = kept.iter().map(|value| value * value).sum::<f32>().sqrt();
    (norm > 1e-6).then(|| kept.iter().map(|value| value / norm).collect())
}

/// Words hashed into a vector: no model, but the same text gives the same
/// vector and shared words make vectors close. For tests and screens.
#[cfg(any(test, feature = "test-support"))]
pub struct FakeEmbedder;

#[cfg(any(test, feature = "test-support"))]
impl FakeEmbedder {
    pub const DIMS: usize = 64;

    fn vector(text: &str) -> Vec<f32> {
        use std::hash::{DefaultHasher, Hash as _, Hasher as _};
        let mut vector = vec![0.0; Self::DIMS];
        for word in text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
        {
            let mut hasher = DefaultHasher::new();
            word.to_lowercase().hash(&mut hasher);
            vector[(hasher.finish() % Self::DIMS as u64) as usize] += 1.0;
        }
        vector[0] += 0.01;
        truncate(&vector, Self::DIMS).unwrap_or_default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Embedder for FakeEmbedder {
    fn model_key(&self) -> &str {
        "fake/64/1"
    }

    fn embed_documents(&self, passages: &[(&str, &str)]) -> Result<Vec<Vec<f32>>> {
        Ok(passages
            .iter()
            .map(|(title, text)| Self::vector(&format!("{title} {text}")))
            .collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        Ok(Self::vector(query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_and_truncation_follow_the_model_card() {
        assert_eq!(document_prompt("", "x"), "title: none | text: x");
        assert_eq!(document_prompt("Mamba", "x"), "title: Mamba | text: x");
        assert_eq!(
            query_prompt("long context"),
            "task: search result | query: long context"
        );
        let vector = truncate(&[3.0, 4.0, 100.0], 2).unwrap();
        assert!((vector[0] - 0.6).abs() < 1e-6 && (vector[1] - 0.8).abs() < 1e-6);
        assert_eq!(truncate(&[f32::NAN, 1.0], 2), None);
        assert_eq!(truncate(&[0.0, 0.0], 2), None);
    }
}
