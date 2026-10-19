//! The assistant's library search: `search_library` finds passages in the
//! text of every item and `read_chunk` reads one with what is around it.
//! They run on the agent's thread: searches go through the library's
//! search service (with meaning and the reranker when they run), else by
//! keywords on their own read-only connection to the index. Passages are
//! cited with `glyphst://` links.

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use anyhow::{Result, anyhow, bail};
use glyphst_ai::{
    agent::{AgentToolCall, AgentToolDefinition, AgentToolResult},
    tools::{Tool, ToolContext, ToolEffect, ToolRegistry},
};
use glyphst_search::{SearchRequest, SearchResults, Store};

use crate::library_search::ToolSearcher;
use serde_json::{Value, json};

const DEFAULT_ITEMS: u64 = 8;
const MAX_ITEMS: u64 = 20;

/// Where the index is, and the search service, once the library's search
/// runs.
#[derive(Clone, Default)]
pub struct IndexHandle(Arc<RwLock<Option<Service>>>);

/// The index's file, and the service searching it.
type Service = (PathBuf, Option<ToolSearcher>);

impl IndexHandle {
    #[cfg(test)]
    pub fn set(&self, path: Option<PathBuf>) {
        self.set_service(path.map(|path| (path, None)));
    }

    /// The index and the service that searches it.
    pub fn set_service(&self, service: Option<Service>) {
        if let Ok(mut slot) = self.0.write() {
            *slot = service;
        }
    }

    fn searcher(&self) -> Option<ToolSearcher> {
        self.0.read().ok()?.as_ref()?.1.clone()
    }

    /// Through the service, with meaning and the reranker; by keywords
    /// on `store` when it does not run or answer.
    fn find(&self, store: &Store, request: &SearchRequest) -> Result<SearchResults> {
        match self
            .searcher()
            .map(|searcher| searcher.search(request.clone()))
        {
            Some(Ok(results)) => Ok(results),
            Some(Err(error)) => {
                log::warn!("search_library through the service: {error:#}");
                store.search(request)
            }
            None => store.search(request),
        }
    }

    fn open(&self) -> Result<Store> {
        let path = self
            .0
            .read()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(path, _)| path.clone()))
            .ok_or_else(|| anyhow!("the library's search is not running; use list_library"))?;
        Store::open_reader(&path)
    }

    pub fn register(&self, registry: &mut ToolRegistry) {
        registry.register(SearchLibrary(self.clone()));
        registry.register(ReadChunk(self.clone()));
    }
}

fn result(call: &AgentToolCall, run: impl FnOnce(&Value) -> Result<String>) -> AgentToolResult {
    match call.arguments_json().and_then(|arguments| run(&arguments)) {
        Ok(text) => AgentToolResult::text(&call.id, &call.name, text),
        Err(error) => AgentToolResult::error(&call.id, &call.name, format!("{error:#}")),
    }
}

/// `c_381` or `381`.
fn chunk_id(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str()?.trim().trim_start_matches("c_").parse().ok())
}

struct SearchLibrary(IndexHandle);

impl Tool for SearchLibrary {
    fn definition(&self) -> AgentToolDefinition {
        let mut tool = AgentToolDefinition::new(
            "search_library",
            "Search the text of every library item (papers and books) and their titles, authors and notes. Returns items with their best passages: page, section, chunk id and a glyphst:// link to cite. It finds passages by their words, and by meaning when search by meaning is on (the reply says which passes ran); read_chunk gives a passage in full.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_ITEMS, "description": "Items, at most"}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        );
        tool.strict = false;
        tool
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
        result(call, |arguments| {
            let query = arguments["query"]
                .as_str()
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .ok_or_else(|| anyhow!("query must not be empty"))?;
            let limit = arguments["max_results"]
                .as_u64()
                .unwrap_or(DEFAULT_ITEMS)
                .clamp(1, MAX_ITEMS) as usize;
            let store = self.0.open()?;
            let request = SearchRequest {
                text: query.to_string(),
                limit: limit * 2,
                ..SearchRequest::default()
            };
            let results = self.0.find(&store, &request)?;
            describe_results(&store, query, limit, &results)
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(format!(
            "Searched the library for “{}”",
            arguments["query"].as_str().unwrap_or_default()
        ))
    }
}

/// Keyword results as text for the model.
#[cfg(test)]
pub fn search(store: &Store, query: &str, limit: usize) -> Result<String> {
    let results = store.search(&SearchRequest {
        text: query.to_string(),
        limit: limit * 2,
        ..SearchRequest::default()
    })?;
    describe_results(store, query, limit, &results)
}

/// The results as text for the model, saying which passes ran.
fn describe_results(
    store: &Store,
    query: &str,
    limit: usize,
    results: &SearchResults,
) -> Result<String> {
    let status = store.status()?;
    let strong = results.items.len() - results.weaker;
    let shown = &results.items[..strong.min(limit)];
    let ids = results
        .items
        .iter()
        .map(|item| item.item_id)
        .collect::<Vec<_>>();
    let titles = store.item_titles(&ids)?;
    let mut lines = Vec::new();
    let passes = match (results.meaning, results.reranked) {
        (_, true) if results.meaning => "keywords + meaning, reranked",
        (_, true) => "keywords, reranked",
        (true, false) => "keywords + meaning",
        (false, false) => "keywords only: use words that would appear in the text",
    };
    if shown.is_empty() {
        lines.push(format!("No good matches for \"{query}\" ({passes})."));
    } else {
        lines.push(format!("Items for \"{query}\" ({passes}):"));
    }
    for (index, item) in shown.iter().enumerate() {
        let title = titles
            .get(&item.item_id)
            .cloned()
            .unwrap_or_else(|| "untitled".to_string());
        lines.push(format!("{}. lib_{} {title}", index + 1, item.item_id));
        for page in &item.pages {
            let section = if page.section.is_empty() {
                String::new()
            } else {
                format!(" · {}", page.section)
            };
            lines.push(format!(
                "   - p{}{section} · c_{}: “{}” {}",
                page.page + 1,
                page.chunk_id,
                page.snippet.trim(),
                crate::links::passage(item.item_id, page.page, page.chunk_id)
            ));
        }
        if item.more_pages > 0 {
            lines.push(format!("   - and {} more pages", item.more_pages));
        }
    }
    let weaker = results.items[strong..]
        .iter()
        .take(10)
        .map(|item| format!("lib_{}", item.item_id))
        .collect::<Vec<_>>();
    if !weaker.is_empty() {
        lines.push(format!("Weaker matches: {}.", weaker.join(", ")));
    }
    if status.waiting() > 0 {
        lines.push(format!(
            "(The text of {} of {} documents is read so far; the rest are not searched yet.)",
            status.done, status.docs
        ));
    }
    Ok(lines.join("\n"))
}

struct ReadChunk(IndexHandle);

impl Tool for ReadChunk {
    fn definition(&self) -> AgentToolDefinition {
        let mut tool = AgentToolDefinition::new(
            "read_chunk",
            "Read a passage search_library found, in full, with the passages before and after it. Returns each with its page, section and glyphst:// link.",
            json!({
                "type": "object",
                "properties": {
                    "chunk_id": {"type": "string", "description": "c_381"},
                    "around": {"type": "integer", "minimum": 0, "maximum": 3, "description": "Passages before and after; 1 when omitted"}
                },
                "required": ["chunk_id"],
                "additionalProperties": false
            }),
        );
        tool.strict = false;
        tool
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
        result(call, |arguments| {
            let id = chunk_id(&arguments["chunk_id"])
                .ok_or_else(|| anyhow!("chunk_id must be like c_381"))?;
            let around = arguments["around"].as_u64().unwrap_or(1).min(3) as usize;
            let store = self.0.open()?;
            read(&store, id, around)
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(format!(
            "Read passage {}",
            arguments["chunk_id"].as_str().unwrap_or_default()
        ))
    }
}

pub fn read(store: &Store, id: i64, around: usize) -> Result<String> {
    let chunks = store.chunk_with_neighbours(id, around)?;
    if chunks.is_empty() {
        bail!("c_{id} is not in the index (it may have been read again); search again");
    }
    let item = chunks
        .iter()
        .find(|chunk| chunk.chunk_id == id)
        .and_then(|chunk| chunk.items.first().copied());
    let title = match item {
        Some(item) => store
            .item_titles(&[item])?
            .remove(&item)
            .map(|title| format!("lib_{item} {title}"))
            .unwrap_or_else(|| format!("lib_{item}")),
        None => "an item no longer in the library".to_string(),
    };
    let mut text = format!("From {title}:\n");
    for chunk in chunks {
        let section = if chunk.section.is_empty() {
            String::new()
        } else {
            format!(" · {}", chunk.section)
        };
        let marker = if chunk.chunk_id == id { " (asked)" } else { "" };
        text.push_str(&format!(
            "\n## p{}{section} · c_{}{marker}\n{}\n",
            chunk.page + 1,
            chunk.chunk_id,
            item.map(|item| crate::links::passage(item, chunk.page, chunk.chunk_id))
                .unwrap_or_default()
        ));
        text.push_str(&chunk.text);
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use glyphst_search::{ItemText, LibraryFile, Snapshot};

    use super::*;

    #[test]
    fn search_and_read_cite_passages_with_links() {
        let dir = std::env::temp_dir().join(format!("glyphst-search-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("paper.pdf");
        std::fs::write(
            &pdf,
            glyphst_pdf::test_pdf::text_pages_pdf(&[
                &["Introduction to sequence models"],
                &["Scaled dot product attention"],
            ]),
        )
        .unwrap();
        let path = dir.join("search.sqlite");
        let handle = IndexHandle::default();
        assert!(handle.open().is_err(), "not running yet");
        {
            let mut store = Store::open(&path).unwrap();
            store
                .reconcile(
                    &Snapshot {
                        files: vec![LibraryFile {
                            item_id: 3,
                            attachment_id: 30,
                            path: pdf,
                            sha256: [3; 32],
                            size: 10,
                            priority: 0,
                        }],
                        items: vec![ItemText {
                            item_id: 3,
                            title: "Attention Is All You Need".to_string(),
                            ..ItemText::default()
                        }],
                    },
                    0,
                )
                .unwrap();
            let indexer = glyphst_search::Indexer::start(path.clone(), || {}).unwrap();
            let started = std::time::Instant::now();
            while indexer.status().done < 1 {
                assert!(started.elapsed().as_secs() < 30);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        handle.set(Some(path));
        let store = handle.open().unwrap();
        let text = search(&store, "dot product", 5).unwrap();
        assert!(text.contains("lib_3 Attention Is All You Need"), "{text}");
        assert!(text.contains("glyphst://open?id=3&page=2&chunk="), "{text}");
        let chunk = text
            .split("c_")
            .nth(1)
            .and_then(|rest| rest.split(':').next())
            .and_then(|id| id.parse::<i64>().ok())
            .unwrap();
        let read_text = read(&store, chunk, 1).unwrap();
        assert!(read_text.contains("Scaled dot product attention"));
        assert!(read_text.contains("(asked)"));
        assert!(
            read_text.contains("Introduction to sequence models"),
            "the page before"
        );
        assert!(
            search(&store, "zebra crossing", 5)
                .unwrap()
                .starts_with("No good matches")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_assistant_searches_through_the_service_with_meaning() {
        use glyphst_search::embed::{FakeEmbedder, FakeReranker};
        let dir = std::env::temp_dir().join(format!("glyphst-tool-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("paper.pdf");
        std::fs::write(
            &pdf,
            glyphst_pdf::test_pdf::text_pages_pdf(&[&["Scaled dot product attention"]]),
        )
        .unwrap();
        let path = dir.join("search.sqlite");
        let ctx = egui::Context::default();
        let mut service = crate::library_search::SearchService::start(&path, &ctx).unwrap();
        service.indexer().reconcile(Snapshot {
            files: vec![LibraryFile {
                item_id: 3,
                attachment_id: 30,
                path: pdf,
                sha256: [3; 32],
                size: 10,
                priority: 0,
            }],
            items: vec![ItemText {
                item_id: 3,
                title: "Attention Is All You Need".to_string(),
                ..ItemText::default()
            }],
        });
        service.set_embedder(Some(Arc::new(FakeEmbedder)));
        let started = std::time::Instant::now();
        while service.status().embedded < 1 {
            assert!(started.elapsed().as_secs() < 30, "{:?}", service.status());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let request = SearchRequest {
            text: "dot product".to_string(),
            limit: 10,
            ..SearchRequest::default()
        };
        let handle = IndexHandle::default();
        // By keywords on its own connection while no service is named.
        handle.set(Some(path.clone()));
        let store = handle.open().unwrap();
        let plain = handle.find(&store, &request).unwrap();
        assert!(!plain.meaning);
        let text = describe_results(&store, "dot product", 5, &plain).unwrap();
        assert!(text.contains("keywords only"), "{text}");
        // Through the service: meaning, then the reranker too.
        handle.set_service(Some((path.clone(), Some(service.tool_searcher()))));
        let found = handle.find(&store, &request).unwrap();
        assert!(found.meaning && !found.reranked);
        assert_eq!(found.items[0].item_id, 3);
        service.set_reranker(Some(Arc::new(FakeReranker)));
        let found = handle.find(&store, &request).unwrap();
        assert!(found.reranked);
        let text = describe_results(&store, "dot product", 5, &found).unwrap();
        assert!(text.contains("keywords + meaning, reranked"), "{text}");
        // The service gone: keywords again.
        drop(service);
        assert!(!handle.find(&store, &request).unwrap().meaning);
        let _ = std::fs::remove_dir_all(dir);
    }
}
