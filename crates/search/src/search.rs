//! Keyword search over the index: the words (and, for Chinese, Japanese
//! and Korean, their characters) found in chunks and in the items' own
//! text, scored by how many of the query's words a passage has and how well
//! FTS5 ranks it, grouped by item with their best pages.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::PathBuf,
};

use anyhow::Result;
use rusqlite::{OptionalExtension as _, params_from_iter};

use crate::{index::is_cjk, store::Store};

/// Chunks read from each full-text index per query.
const CANDIDATES: usize = 300;
/// Chunks the meaning pass adds.
const NEAREST: usize = 200;
/// A passage this unusual in meaning qualifies without the words
/// (DigUp's zGate).
const MEANING_GATE: f32 = 2.0;
/// The floor of the spread meaning scores are measured against.
const MIN_SPREAD: f32 = 0.02;
/// Pages shown per item.
pub const PAGES_PER_ITEM: usize = 3;
/// Snippet markers around matched words.
const OPEN: char = '\u{1}';
const CLOSE: char = '\u{2}';

#[derive(Clone, Debug, Default)]
pub struct SearchRequest {
    pub text: String,
    /// Only these items (the tab's `tag:`/`year:`/`by:` filter), if given.
    pub items: Option<HashSet<u64>>,
    /// The last word is still being typed: match it as a prefix.
    pub typing: bool,
    pub limit: usize,
}

/// A passage that matched.
#[derive(Clone, Debug, PartialEq)]
pub struct PageHit {
    pub chunk_id: i64,
    /// The file the passage is in.
    pub path: PathBuf,
    /// 0-based.
    pub page: usize,
    pub section: String,
    pub snippet: String,
    /// Character ranges of `snippet` that matched.
    pub highlights: Vec<Range<usize>>,
    /// Where it is, for opening it: document offset, quote, prefix, suffix.
    pub doc_offset: usize,
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
    /// Share of the query's words it has (0 to 1).
    pub coverage: f32,
    /// FTS5's rank, higher better.
    pub rank: f32,
    /// How unusual its closeness in meaning is (standard deviations above
    /// the collection's mean), when the meaning pass ran.
    pub meaning: Option<f32>,
    pub score: f32,
}

/// The meaning pass: the vectors and the query's.
pub struct Meaning<'a> {
    pub index: &'a crate::vectors::VectorIndex,
    pub query: &'a [f32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemHit {
    pub item_id: u64,
    pub score: f32,
    /// The item's title, authors, tags or note matched.
    pub in_details: bool,
    pub pages: Vec<PageHit>,
    /// Further pages that matched.
    pub more_pages: usize,
}

/// A chunk to read or open: its file, page, anchor, and the items that
/// have its file.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkLocation {
    pub chunk_id: i64,
    pub path: PathBuf,
    /// 0-based.
    pub page: usize,
    pub section: String,
    pub text: String,
    pub doc_offset: usize,
    pub quote: String,
    pub prefix: String,
    pub suffix: String,
    pub items: Vec<u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResults {
    /// Strong matches first.
    pub items: Vec<ItemHit>,
    /// How many of `items`, from the end, are weaker: under half the best
    /// score, or with under half the words.
    pub weaker: usize,
    /// The words searched for.
    pub words: Vec<String>,
}

/// A query's parts: words for the word index, runs of CJK characters for
/// the trigram one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub words: Vec<String>,
    pub cjk: Vec<String>,
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let mut query = Self::default();
        let mut word = String::new();
        let mut run = String::new();
        let flush = |word: &mut String, run: &mut String, query: &mut Self| {
            if !word.is_empty() {
                query.words.push(std::mem::take(word).to_lowercase());
            }
            if !run.is_empty() {
                query.cjk.push(std::mem::take(run));
            }
        };
        for ch in text.chars() {
            if is_cjk(ch) {
                if !word.is_empty() {
                    query.words.push(std::mem::take(&mut word).to_lowercase());
                }
                run.push(ch);
            } else if ch.is_alphanumeric() {
                if !run.is_empty() {
                    query.cjk.push(std::mem::take(&mut run));
                }
                word.push(ch);
            } else {
                flush(&mut word, &mut run, &mut query);
            }
        }
        flush(&mut word, &mut run, &mut query);
        query.words.dedup();
        query
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty() && self.cjk.is_empty()
    }

    fn terms(&self) -> usize {
        self.words.len() + self.cjk.len()
    }

    /// The FTS5 expression for the words: any of them (coverage sorts out
    /// how many), the last as a prefix while typing.
    fn match_words(&self, typing: bool) -> Option<String> {
        if self.words.is_empty() {
            return None;
        }
        let last = self.words.len() - 1;
        Some(
            self.words
                .iter()
                .enumerate()
                .map(|(index, word)| {
                    let quoted = format!("\"{}\"", word.replace('"', "\"\""));
                    if typing && index == last {
                        format!("{quoted}*")
                    } else {
                        quoted
                    }
                })
                .collect::<Vec<_>>()
                .join(" OR "),
        )
    }

    /// How many of the query's terms `text` has, as a share.
    pub fn coverage(&self, text: &str) -> f32 {
        if self.terms() == 0 {
            return 0.0;
        }
        let folded = text.to_lowercase();
        let found = self
            .words
            .iter()
            .filter(|word| folded.contains(stem(word)))
            .count()
            + self
                .cjk
                .iter()
                .filter(|run| folded.contains(run.as_str()))
                .count();
        found as f32 / self.terms() as f32
    }
}

/// A word without its common English endings, so "models" finds "model"
/// the way the porter tokenizer does.
fn stem(word: &str) -> &str {
    if word.chars().count() <= 4 {
        return word;
    }
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(stem) = word.strip_suffix(suffix)
            && stem.chars().count() >= 3
        {
            return stem;
        }
    }
    word
}

struct Candidate {
    chunk_id: i64,
    path: String,
    doc_id: i64,
    page: usize,
    section: String,
    snippet: String,
    text: String,
    doc_offset: usize,
    quote: String,
    prefix: String,
    suffix: String,
    rank: f32,
}

impl Store {
    /// Keywords only.
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResults> {
        self.search_with(request, None)
    }

    /// Keywords, and meaning when given: each passage's closeness is put
    /// against the whole collection's (z-scores), and mixed with how many
    /// of the words it has, as DigUp does.
    pub fn search_with(
        &self,
        request: &SearchRequest,
        meaning: Option<Meaning<'_>>,
    ) -> Result<SearchResults> {
        let query = Query::parse(&request.text);
        if query.is_empty() {
            return Ok(SearchResults::default());
        }
        let mut candidates = Vec::new();
        if let Some(expression) = query.match_words(request.typing) {
            self.collect(
                "chunks_fts",
                &expression,
                "bm25(chunks_fts, 1.0, 0.4)",
                &mut candidates,
            )?;
        }
        for run in &query.cjk {
            if run.chars().count() >= 3 {
                let expression = format!("\"{}\"", run.replace('"', "\"\""));
                self.collect(
                    "chunks_tri",
                    &expression,
                    "bm25(chunks_tri)",
                    &mut candidates,
                )?;
            } else {
                self.collect_like(run, &mut candidates)?;
            }
        }
        // Each chunk once.
        let mut seen = HashSet::new();
        candidates.retain(|candidate| seen.insert(candidate.chunk_id));
        // The passages closest in meaning, and how all of them scored.
        let mut spread = None;
        if let Some(meaning) = &meaning {
            let (nearest, all) = meaning.index.nearest(meaning.query, NEAREST);
            spread = Some(all);
            let extra = nearest
                .iter()
                .map(|(chunk_id, _)| *chunk_id)
                .filter(|chunk_id| seen.insert(*chunk_id))
                .collect::<Vec<_>>();
            for chunk in self.chunks_by_id(&extra)? {
                candidates.push(chunk.into_candidate(&query));
            }
        }
        // Codes and numbers ("1706.03762") are found by their words.
        let lookup = !query.words.is_empty()
            && query
                .words
                .iter()
                .all(|word| word.chars().any(|c| c.is_ascii_digit()));
        let (meaning_weight, words_weight) = if lookup { (0.5, 4.0) } else { (1.0, 2.0) };

        let owners = self.owners()?;
        let mut by_item: HashMap<u64, ItemHit> = HashMap::new();
        for candidate in candidates {
            let coverage = query.coverage(&candidate.text);
            let z = meaning.as_ref().zip(spread).and_then(|(meaning, spread)| {
                let cosine = meaning.index.score(candidate.chunk_id, meaning.query)?;
                Some((cosine - spread.mean) / spread.std.max(MIN_SPREAD))
            });
            let score = words_weight * coverage.powf(1.5)
                + 0.05 * candidate.rank.max(0.0).ln_1p()
                + meaning_weight * z.unwrap_or(0.0).max(0.0);
            let (snippet, highlights) = marked(&candidate.snippet);
            let hit = PageHit {
                chunk_id: candidate.chunk_id,
                path: PathBuf::from(&candidate.path),
                page: candidate.page,
                section: candidate.section,
                snippet,
                highlights,
                doc_offset: candidate.doc_offset,
                quote: candidate.quote,
                prefix: candidate.prefix,
                suffix: candidate.suffix,
                coverage,
                rank: candidate.rank,
                meaning: z,
                score,
            };
            for item_id in owners.get(&candidate.doc_id).into_iter().flatten() {
                if request
                    .items
                    .as_ref()
                    .is_some_and(|allowed| !allowed.contains(item_id))
                {
                    continue;
                }
                let entry = by_item.entry(*item_id).or_insert_with(|| ItemHit {
                    item_id: *item_id,
                    score: 0.0,
                    in_details: false,
                    pages: Vec::new(),
                    more_pages: 0,
                });
                entry.pages.push(hit.clone());
            }
        }
        // The items' own text.
        for (item_id, coverage) in self.detail_hits(&query, request.typing)? {
            if request
                .items
                .as_ref()
                .is_some_and(|allowed| !allowed.contains(&item_id))
            {
                continue;
            }
            let entry = by_item.entry(item_id).or_insert_with(|| ItemHit {
                item_id,
                score: 0.0,
                in_details: false,
                pages: Vec::new(),
                more_pages: 0,
            });
            entry.in_details = true;
            // Added to its best passage's score below.
            entry.score = 0.25 + 0.5 * coverage;
        }

        let mut items = by_item
            .into_values()
            .map(|mut item| {
                item.pages
                    .sort_by(|a, b| b.score.total_cmp(&a.score).then(a.page.cmp(&b.page)));
                // One passage per page.
                let mut pages_seen = HashSet::new();
                item.pages.retain(|hit| pages_seen.insert(hit.page));
                // Its best passage, more for each further page that matched
                // and for its own text matching.
                if let Some(best) = item.pages.first() {
                    let others = item.pages.len().saturating_sub(1) as f32;
                    item.score += best.score + 0.15 * others.ln_1p();
                }
                item.more_pages = item.pages.len().saturating_sub(PAGES_PER_ITEM);
                item.pages.truncate(PAGES_PER_ITEM);
                item
            })
            .collect::<Vec<_>>();
        // Strong: half the words at least, or the item's own text. The rest,
        // and those under half the best score, fold as weaker matches.
        let qualifies = |item: &ItemHit| {
            item.in_details
                || query.terms() == 1
                || item.pages.iter().any(|hit| {
                    hit.coverage >= 0.5 || hit.meaning.is_some_and(|z| z >= MEANING_GATE)
                })
        };
        let best = items
            .iter()
            .filter(|item| qualifies(item))
            .map(|item| item.score)
            .fold(0.0_f32, f32::max);
        let weak = |item: &ItemHit| !qualifies(item) || item.score < best * 0.5;
        items.sort_by(|a, b| {
            weak(a)
                .cmp(&weak(b))
                .then(b.score.total_cmp(&a.score))
                .then(a.item_id.cmp(&b.item_id))
        });
        if request.limit > 0 {
            items.truncate(request.limit);
        }
        let weaker = items.iter().filter(|item| weak(item)).count();
        Ok(SearchResults {
            items,
            weaker,
            words: query.words.iter().chain(&query.cjk).cloned().collect(),
        })
    }

    fn collect(
        &self,
        table: &str,
        expression: &str,
        rank: &str,
        out: &mut Vec<Candidate>,
    ) -> Result<()> {
        let sql = format!(
            "SELECT c.id, c.doc_id, c.page, c.section,
                    snippet({table}, 0, '{OPEN}', '{CLOSE}', '…', 24),
                    c.text, c.doc_offset, c.quote, c.prefix, c.suffix, {rank}, d.path
             FROM {table} JOIN chunks c ON c.id = {table}.rowid JOIN docs d ON d.id = c.doc_id
             WHERE {table} MATCH ?1 ORDER BY {rank} LIMIT {CANDIDATES}"
        );
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([expression], |row| {
            Ok(Candidate {
                chunk_id: row.get(0)?,
                doc_id: row.get(1)?,
                page: row.get::<_, i64>(2)? as usize,
                section: row.get(3)?,
                snippet: row.get(4)?,
                text: row.get(5)?,
                doc_offset: row.get::<_, i64>(6)? as usize,
                quote: row.get(7)?,
                prefix: row.get(8)?,
                suffix: row.get(9)?,
                // bm25 is lower for better matches.
                rank: -(row.get::<_, f64>(10)? as f32),
                path: row.get(11)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(())
    }

    /// One or two CJK characters, below what trigrams can match.
    fn collect_like(&self, run: &str, out: &mut Vec<Candidate>) -> Result<()> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT c.id, c.doc_id, c.page, c.section, c.text, c.doc_offset, c.quote, c.prefix,
                    c.suffix, d.path
             FROM chunks c JOIN docs d ON d.id = c.doc_id
             WHERE instr(c.text, ?1) > 0 LIMIT {CANDIDATES}"
        ))?;
        let rows = statement.query_map([run], |row| {
            let text: String = row.get(4)?;
            Ok(Candidate {
                chunk_id: row.get(0)?,
                doc_id: row.get(1)?,
                page: row.get::<_, i64>(2)? as usize,
                section: row.get(3)?,
                snippet: snippet_around(&text, run),
                text,
                doc_offset: row.get::<_, i64>(5)? as usize,
                quote: row.get(6)?,
                prefix: row.get(7)?,
                suffix: row.get(8)?,
                rank: 1.0,
                path: row.get(9)?,
            })
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(())
    }

    /// Items whose own text matched, with its coverage.
    fn detail_hits(&self, query: &Query, typing: bool) -> Result<Vec<(u64, f32)>> {
        let Some(expression) = query.match_words(typing) else {
            return Ok(Vec::new());
        };
        let mut statement = self.conn.prepare(&format!(
            "SELECT f.rowid, f.title || ' ' || f.creators || ' ' || f.tags || ' ' || f.abstract || ' ' || f.note
             FROM items_fts f JOIN items i ON i.item_id = f.rowid
             WHERE items_fts MATCH ?1 AND i.trashed = 0 LIMIT {CANDIDATES}"
        ))?;
        let rows = statement.query_map([expression], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (item_id, text) = row?;
            hits.push((item_id, query.coverage(&text)));
        }
        Ok(hits)
    }

    /// The live (not trashed) items of each document.
    fn owners(&self) -> Result<HashMap<i64, Vec<u64>>> {
        let mut statement = self.conn.prepare(
            "SELECT f.doc_id, f.item_id FROM item_files f
             JOIN items i ON i.item_id = f.item_id WHERE i.trashed = 0",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let mut owners: HashMap<i64, Vec<u64>> = HashMap::new();
        for row in rows {
            let (doc_id, item_id) = row?;
            owners.entry(doc_id).or_default().push(item_id);
        }
        Ok(owners)
    }

    /// A chunk and where it is, with the items that have its file.
    pub fn chunk(&self, chunk_id: i64) -> Result<Option<ChunkLocation>> {
        Ok(self
            .chunks_where("c.id = ?1", &[chunk_id])?
            .into_iter()
            .next())
    }

    /// A chunk and the `around` chunks before and after it in its document.
    pub fn chunk_with_neighbours(
        &self,
        chunk_id: i64,
        around: usize,
    ) -> Result<Vec<ChunkLocation>> {
        let Some((doc_id, ord)) = self
            .conn
            .query_row(
                "SELECT doc_id, ord FROM chunks WHERE id = ?1",
                [chunk_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
        else {
            return Ok(Vec::new());
        };
        let around = around as i64;
        self.chunks_where(
            "c.doc_id = ?1 AND c.ord BETWEEN ?2 AND ?3",
            &[doc_id, ord - around, ord + around],
        )
    }

    fn chunks_where(&self, condition: &str, values: &[i64]) -> Result<Vec<ChunkLocation>> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT c.id, d.path, c.page, c.section, c.text, c.doc_offset, c.quote, c.prefix,
                    c.suffix, c.doc_id
             FROM chunks c JOIN docs d ON d.id = c.doc_id WHERE {condition} ORDER BY c.ord"
        ))?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((
                ChunkLocation {
                    chunk_id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    page: row.get::<_, i64>(2)? as usize,
                    section: row.get(3)?,
                    text: row.get(4)?,
                    doc_offset: row.get::<_, i64>(5)? as usize,
                    quote: row.get(6)?,
                    prefix: row.get(7)?,
                    suffix: row.get(8)?,
                    items: Vec::new(),
                },
                row.get::<_, i64>(9)?,
            ))
        })?;
        let owners = self.owners()?;
        let mut chunks = Vec::new();
        for row in rows {
            let (mut chunk, doc_id) = row?;
            chunk.items = owners.get(&doc_id).cloned().unwrap_or_default();
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    fn chunks_by_id(&self, ids: &[i64]) -> Result<Vec<ChunkRow>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; ids.len()].join(", ");
        let mut statement = self.conn.prepare(&format!(
            "SELECT c.id, c.doc_id, c.page, c.section, c.text, c.doc_offset, c.quote, c.prefix,
                    c.suffix, d.path
             FROM chunks c JOIN docs d ON d.id = c.doc_id WHERE c.id IN ({marks})"
        ))?;
        let rows = statement.query_map(params_from_iter(ids), |row| {
            Ok(ChunkRow {
                chunk_id: row.get(0)?,
                doc_id: row.get(1)?,
                page: row.get::<_, i64>(2)? as usize,
                section: row.get(3)?,
                text: row.get(4)?,
                doc_offset: row.get::<_, i64>(5)? as usize,
                quote: row.get(6)?,
                prefix: row.get(7)?,
                suffix: row.get(8)?,
                path: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Items' titles (and first author) as the index has them.
    pub fn item_titles(&self, ids: &[u64]) -> Result<HashMap<u64, String>> {
        let mut titles = HashMap::new();
        let mut statement = self
            .conn
            .prepare("SELECT title, creators FROM items_fts WHERE rowid = ?1")?;
        for id in ids {
            if let Some((title, creators)) = statement
                .query_row([*id as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .optional()?
            {
                let first = creators
                    .split(',')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                let label = if first.is_empty() {
                    title
                } else {
                    format!("{title} ({first})")
                };
                titles.insert(*id, label);
            }
        }
        Ok(titles)
    }
}

struct ChunkRow {
    chunk_id: i64,
    doc_id: i64,
    page: usize,
    section: String,
    text: String,
    doc_offset: usize,
    quote: String,
    prefix: String,
    suffix: String,
    path: String,
}

impl ChunkRow {
    /// A passage the meaning pass found: its snippet around the first word
    /// of the query it has, else its start.
    fn into_candidate(self, query: &Query) -> Candidate {
        let folded = self.text.to_lowercase();
        let snippet = query
            .words
            .iter()
            .map(|word| stem(word))
            .chain(query.cjk.iter().map(String::as_str))
            .find_map(|word| {
                let at = folded.find(word)?;
                let len = word.len();
                // Byte positions in the folded text match the original's
                // only for ASCII; fall back to the start otherwise.
                self.text
                    .get(at..at + len)
                    .map(|found| snippet_around(&self.text, found))
            })
            .unwrap_or_else(|| {
                let start = self.text.chars().take(160).collect::<String>();
                if self.text.chars().count() > 160 {
                    format!("{start}…")
                } else {
                    start
                }
            });
        Candidate {
            chunk_id: self.chunk_id,
            path: self.path,
            doc_id: self.doc_id,
            page: self.page,
            section: self.section,
            snippet,
            text: self.text,
            doc_offset: self.doc_offset,
            quote: self.quote,
            prefix: self.prefix,
            suffix: self.suffix,
            rank: 0.0,
        }
    }
}

/// A snippet with its markers taken out and their places kept.
fn marked(snippet: &str) -> (String, Vec<Range<usize>>) {
    let mut text = String::new();
    let mut highlights = Vec::new();
    let mut start = None;
    let mut count = 0;
    for ch in snippet.chars() {
        match ch {
            OPEN => start = Some(count),
            CLOSE => {
                if let Some(start) = start.take() {
                    highlights.push(start..count);
                }
            }
            ch => {
                text.push(ch);
                count += 1;
            }
        }
    }
    (text, highlights)
}

/// About 24 words around the first `needle`, marked.
fn snippet_around(text: &str, needle: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let needle_chars = needle.chars().collect::<Vec<_>>();
    let at = (0..chars.len().saturating_sub(needle_chars.len() - 1))
        .find(|index| chars[*index..*index + needle_chars.len()] == needle_chars[..])
        .unwrap_or(0);
    let start = at.saturating_sub(40);
    let end = (at + needle_chars.len() + 40).min(chars.len());
    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.extend(&chars[start..at]);
    snippet.push(OPEN);
    snippet.extend(&chars[at..at + needle_chars.len()]);
    snippet.push(CLOSE);
    snippet.extend(&chars[at + needle_chars.len()..end]);
    if end < chars.len() {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_split_into_words_and_cjk_runs() {
        let query = Query::parse("Attention, all you need: 注意力机制 and 学习");
        assert_eq!(query.words, vec!["attention", "all", "you", "need", "and"]);
        assert_eq!(query.cjk, vec!["注意力机制", "学习"]);
        assert_eq!(
            query.match_words(true).unwrap(),
            "\"attention\" OR \"all\" OR \"you\" OR \"need\" OR \"and\"*"
        );
        assert_eq!(query.coverage("Models of attention need data"), 2.0 / 7.0);
    }

    #[test]
    fn snippets_give_their_highlights() {
        let (text, highlights) = marked("…the \u{1}attention\u{2} is all");
        assert_eq!(text, "…the attention is all");
        assert_eq!(highlights, vec![5..14]);
    }
}
