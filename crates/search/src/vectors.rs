//! Meaning search's vectors: one per chunk and model, stored as float16,
//! and searched in memory by brute force. A personal library is about
//! 100,000 chunks; at 256 numbers that is 100 MB of float32 and a few
//! milliseconds a query, so no approximate index is needed.

use anyhow::Result;
use half::f16;
use rusqlite::params;

use crate::store::Store;

/// Chunks to embed next, with their document's title.
pub struct Pending {
    pub chunk_id: i64,
    pub title: String,
    pub text: String,
}

pub fn encode(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| f16::from_f32(*value).to_le_bytes())
        .collect()
}

pub fn decode(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f16::from_le_bytes(*pair).to_f32())
        .collect()
}

impl Store {
    /// Chunks without a vector from `model_key`: every document's first 30
    /// pages before the rest, new and small documents first.
    pub fn next_to_embed(&self, model_key: &str, limit: usize) -> Result<Vec<Pending>> {
        let mut statement = self.conn.prepare(
            "SELECT c.id, COALESCE((SELECT f.title FROM item_files i JOIN items_fts f
                                    ON f.rowid = i.item_id WHERE i.doc_id = c.doc_id LIMIT 1), ''),
                    c.text
             FROM chunks c JOIN docs d ON d.id = c.doc_id
             LEFT JOIN vectors v ON v.chunk_id = c.id AND v.model_key = ?1
             WHERE v.chunk_id IS NULL
             ORDER BY c.page >= 30, d.priority DESC, d.size ASC, c.doc_id, c.ord
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![model_key, limit as i64], |row| {
            Ok(Pending {
                chunk_id: row.get(0)?,
                title: row.get(1)?,
                text: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn save_vectors(
        &mut self,
        model_key: &str,
        vectors: &[(i64, String, Vec<f32>)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for (chunk_id, title, vector) in vectors {
            tx.execute(
                "INSERT OR REPLACE INTO vectors (chunk_id, model_key, vec, title) VALUES (?1, ?2, ?3, ?4)",
                params![chunk_id, model_key, encode(vector), title],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drops vectors another model (or prompt, or size) made.
    pub fn drop_other_vectors(&mut self, model_key: &str) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM vectors WHERE model_key != ?1", [model_key])?)
    }

    pub fn vector_count(&self, model_key: &str) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM vectors WHERE model_key = ?1",
            [model_key],
            |row| row.get::<_, i64>(0),
        )? as usize)
    }
}

/// All of one model's vectors in memory.
#[derive(Default)]
pub struct VectorIndex {
    model_key: String,
    dims: usize,
    ids: Vec<i64>,
    data: Vec<f32>,
    rows: std::collections::HashMap<i64, usize>,
}

/// How the whole collection scored, to say how unusual a score is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spread {
    pub mean: f32,
    pub std: f32,
}

impl VectorIndex {
    pub fn load(store: &Store, model_key: &str) -> Result<Self> {
        let mut index = Self {
            model_key: model_key.to_string(),
            ..Self::default()
        };
        let mut statement = store
            .conn
            .prepare("SELECT chunk_id, vec FROM vectors WHERE model_key = ?1")?;
        let rows = statement.query_map([model_key], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (id, bytes) = row?;
            let vector = decode(&bytes);
            if index.dims == 0 {
                index.dims = vector.len();
            }
            if vector.len() == index.dims {
                index.rows.insert(id, index.ids.len());
                index.ids.push(id);
                index.data.extend(vector);
            }
        }
        Ok(index)
    }

    /// Loads again when the store has another number of vectors.
    pub fn refresh(&mut self, store: &Store) -> Result<bool> {
        let count = store.vector_count(&self.model_key)?;
        if count == self.ids.len() {
            return Ok(false);
        }
        *self = Self::load(store, &self.model_key.clone())?;
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn model_key(&self) -> &str {
        &self.model_key
    }

    /// How close one chunk is to `query`, if it has a vector.
    pub fn score(&self, chunk_id: i64, query: &[f32]) -> Option<f32> {
        let row = *self.rows.get(&chunk_id)?;
        let vector = self.data.get(row * self.dims..(row + 1) * self.dims)?;
        (query.len() == self.dims).then(|| vector.iter().zip(query).map(|(a, b)| a * b).sum())
    }

    /// The `limit` chunks closest to `query` (a unit vector), by cosine,
    /// and how all of them scored.
    pub fn nearest(&self, query: &[f32], limit: usize) -> (Vec<(i64, f32)>, Spread) {
        if self.dims == 0 || query.len() != self.dims {
            return (Vec::new(), Spread::default());
        }
        let scores = self
            .data
            .chunks_exact(self.dims)
            .map(|vector| vector.iter().zip(query).map(|(a, b)| a * b).sum::<f32>())
            .collect::<Vec<_>>();
        let count = scores.len().max(1) as f32;
        let mean = scores.iter().sum::<f32>() / count;
        let std = (scores
            .iter()
            .map(|score| (score - mean).powi(2))
            .sum::<f32>()
            / count)
            .sqrt();
        let mut ranked = self.ids.iter().copied().zip(scores).collect::<Vec<_>>();
        let limit = limit.min(ranked.len());
        if limit > 0 {
            ranked.select_nth_unstable_by(limit - 1, |a, b| b.1.total_cmp(&a.1));
            ranked.truncate(limit);
            ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        (ranked, Spread { mean, std })
    }
}
