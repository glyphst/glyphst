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

    /// The items each passage belongs to (items in the trash left out).
    pub fn chunk_owners(&self) -> Result<std::collections::HashMap<i64, Vec<u64>>> {
        let mut statement = self.conn.prepare(
            "SELECT c.id, f.item_id FROM chunks c
             JOIN item_files f ON f.doc_id = c.doc_id
             JOIN items i ON i.item_id = f.item_id WHERE i.trashed = 0",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let mut owners: std::collections::HashMap<i64, Vec<u64>> = Default::default();
        for row in rows {
            let (chunk, item) = row?;
            owners.entry(chunk).or_default().push(item);
        }
        Ok(owners)
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

/// One vector per item: the mean of its passages' vectors, at unit length.
/// Close items are related; groups of them are topics.
#[derive(Clone, Debug, Default)]
pub struct ItemVectors {
    dims: usize,
    ids: Vec<u64>,
    data: Vec<f32>,
}

/// How unusual a pair's closeness has to be (standard deviations over all
/// pairs) to be related, or in one topic.
pub const RELATED_Z: f32 = 2.5;

impl VectorIndex {
    /// Each item's mean vector, from `owners` (passage → items).
    pub fn item_means(&self, owners: &std::collections::HashMap<i64, Vec<u64>>) -> ItemVectors {
        let mut sums: std::collections::BTreeMap<u64, Vec<f32>> = Default::default();
        if self.dims == 0 {
            return ItemVectors::default();
        }
        for (row, chunk) in self.ids.iter().enumerate() {
            let vector = &self.data[row * self.dims..(row + 1) * self.dims];
            for item in owners.get(chunk).into_iter().flatten() {
                let sum = sums.entry(*item).or_insert_with(|| vec![0.0; self.dims]);
                for (total, value) in sum.iter_mut().zip(vector) {
                    *total += value;
                }
            }
        }
        let mut items = ItemVectors {
            dims: self.dims,
            ..ItemVectors::default()
        };
        for (id, sum) in sums {
            let norm = sum.iter().map(|value| value * value).sum::<f32>().sqrt();
            if norm > 1e-6 {
                items.ids.push(id);
                items.data.extend(sum.iter().map(|value| value / norm));
            }
        }
        items
    }
}

impl ItemVectors {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn row(&self, index: usize) -> &[f32] {
        &self.data[index * self.dims..(index + 1) * self.dims]
    }

    /// Every pair's closeness (row-major, `len × len`), and how all pairs
    /// scored.
    fn pairs(&self) -> (Vec<f32>, Spread) {
        let count = self.ids.len();
        let mut matrix = vec![0.0; count * count];
        let (mut sum, mut squares, mut pairs) = (0.0_f64, 0.0_f64, 0_u64);
        for a in 0..count {
            for b in a + 1..count {
                let cosine = self
                    .row(a)
                    .iter()
                    .zip(self.row(b))
                    .map(|(x, y)| x * y)
                    .sum::<f32>();
                matrix[a * count + b] = cosine;
                matrix[b * count + a] = cosine;
                sum += cosine as f64;
                squares += (cosine as f64).powi(2);
                pairs += 1;
            }
        }
        let pairs = pairs.max(1) as f64;
        let mean = sum / pairs;
        let std = (squares / pairs - mean * mean).max(0.0).sqrt();
        (
            matrix,
            Spread {
                mean: mean as f32,
                std: std as f32,
            },
        )
    }

    /// For each item, up to `k` others unusually close to it (z ≥
    /// `RELATED_Z`), closest first, with their cosine.
    pub fn neighbours(&self, k: usize) -> Vec<(u64, Vec<(u64, f32)>)> {
        let (matrix, spread) = self.pairs();
        let std = spread.std.max(0.02);
        let count = self.ids.len();
        (0..count)
            .filter_map(|a| {
                let row = &matrix[a * count..(a + 1) * count];
                let mut close = (0..count)
                    .filter(|b| *b != a && (row[*b] - spread.mean) / std >= RELATED_Z)
                    .map(|b| (self.ids[b], row[b]))
                    .collect::<Vec<_>>();
                close.sort_by(|x, y| y.1.total_cmp(&x.1));
                close.truncate(k);
                (!close.is_empty()).then(|| (self.ids[a], close))
            })
            .collect()
    }

    /// Topics: groups of at least `min_size` items joined by pairs that are
    /// each other's close neighbours (within each other's `k` closest, and
    /// unusually close), largest first.
    pub fn clusters(&self, k: usize, min_size: usize) -> Vec<Vec<u64>> {
        let neighbours = self
            .neighbours(k)
            .into_iter()
            .collect::<std::collections::HashMap<_, _>>();
        let close = |a: u64, b: u64| {
            neighbours
                .get(&a)
                .is_some_and(|list| list.iter().any(|(other, _)| *other == b))
        };
        let mut seen = std::collections::HashSet::new();
        let mut groups = Vec::new();
        for start in &self.ids {
            if !seen.insert(*start) {
                continue;
            }
            let mut group = vec![*start];
            let mut index = 0;
            while index < group.len() {
                let current = group[index];
                for (other, _) in neighbours.get(&current).into_iter().flatten() {
                    if close(*other, current) && seen.insert(*other) {
                        group.push(*other);
                    }
                }
                index += 1;
            }
            if group.len() >= min_size {
                group.sort();
                groups.push(group);
            }
        }
        groups.sort_by_key(|group| std::cmp::Reverse(group.len()));
        groups
    }
}
