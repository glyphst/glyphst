//! Document tools for the assistant: search the document, read pages, and
//! render pages as images, over MuPDF and `pdf::text`. Each tool opens the
//! document itself, so tools never wait for the UI and reads run in
//! parallel.

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use anyhow::{Context as _, Result, anyhow, bail};
use glyphst_ai::{
    agent::{
        AgentCancellationToken, AgentImage, AgentToolCall, AgentToolDefinition, AgentToolResult,
        AgentToolResultPart,
    },
    tools::{Tool, ToolContext, ToolEffect, ToolRegistry},
};
use glyphst_core::DocumentLayout;
use glyphst_pdf::{DocumentSnapshot, text::PageText};
use mupdf::{Colorspace, Matrix};
use serde_json::{Value, json};

const MAX_SEARCH_RESULTS: u64 = 20;
const MAX_READ_PAGES: usize = 12;
const MAX_READ_CHARS: usize = 12_000;
const MAX_RENDER_PAGES: usize = 4;
/// Longest side of a rendered page image, in pixels.
const RENDER_PIXELS: f32 = 1600.0;
const SNIPPET_CHARS: usize = 60;

/// The documents open in the reader, by the ids tools take ("doc_7"); the
/// app updates it as tabs open, close and turn pages.
#[derive(Clone, Default)]
pub struct DocumentDirectory(Arc<RwLock<Directory>>);

#[derive(Default)]
struct Directory {
    documents: Vec<OpenDocument>,
    active: Option<String>,
}

impl DocumentDirectory {
    pub fn update(&self, documents: Vec<OpenDocument>, active: Option<String>) {
        if let Ok(mut directory) = self.0.write() {
            directory.documents = documents;
            directory.active = active;
        }
    }

    /// The document `id` names, or the active one.
    fn resolve(&self, id: Option<&str>) -> Result<OpenDocument> {
        let directory = self
            .0
            .read()
            .map_err(|_| anyhow!("the document list is unavailable"))?;
        let id = id
            .or(directory.active.as_deref())
            .ok_or_else(|| anyhow!("no document is open"))?;
        directory
            .documents
            .iter()
            .find(|document| document.id == id)
            .cloned()
            .ok_or_else(|| anyhow!("no open document has the id {id}; call list_documents"))
    }

    /// Adds the document tools to a run's tools.
    pub fn register(&self, registry: &mut ToolRegistry) {
        registry.register(SearchDocument(self.clone()));
        registry.register(ReadDocument(self.clone()));
        registry.register(RenderPages(self.clone()));
        registry.register(ListLinks(self.clone()));
    }
}

/// The id tools use for the tab `tab_id`.
pub fn document_id(tab_id: u64) -> String {
    format!("doc_{tab_id}")
}

fn document_argument(arguments: &Value) -> Option<&str> {
    arguments["document_id"]
        .as_str()
        .filter(|id| !id.is_empty())
}

/// A JSON schema property for the optional document id.
pub fn document_id_property() -> Value {
    json!({
        "type": "string",
        "description": "The document's id from <reader_state> or list_documents; the active document when omitted"
    })
}

/// An open document, as the assistant's tools see it.
#[derive(Clone, Debug)]
pub struct OpenDocument {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
    pub layout: DocumentLayout,
    pub page_count: usize,
    /// The page shown, from 0; reading without pages reads it.
    pub current_page: usize,
}

impl OpenDocument {
    fn document(&self) -> Result<mupdf::Document> {
        Ok(DocumentSnapshot::read(&self.path)?
            .open_with_layout(self.layout)?
            .into_inner())
    }

    fn page_text(document: &mupdf::Document, page: usize) -> Result<PageText> {
        let loaded = document
            .load_page(i32::try_from(page)?)
            .with_context(|| format!("failed to load page {}", page + 1))?;
        PageText::from_page(&loaded)
    }

    fn search(&self, arguments: &Value, cancel: &AgentCancellationToken) -> Result<String> {
        let query = arguments["query"]
            .as_str()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| anyhow!("query must be a non-empty string"))?;
        let limit = arguments["max_results"]
            .as_u64()
            .unwrap_or(8)
            .clamp(1, MAX_SEARCH_RESULTS) as usize;
        let document = self.document()?;
        let mut lines = Vec::new();
        'pages: for page in 0..self.page_count {
            if cancel.is_canceled() {
                bail!("agent run canceled");
            }
            let text = Self::page_text(&document, page)?;
            for range in text.find(query, false, false) {
                let start = range.start.saturating_sub(SNIPPET_CHARS);
                let end = (range.end + SNIPPET_CHARS).min(text.len());
                let snippet = text.text_of(start..end).replace('\n', " ");
                lines.push(format!("- page {}: {snippet}", page + 1));
                if lines.len() >= limit {
                    break 'pages;
                }
            }
        }
        Ok(if lines.is_empty() {
            format!("No matches for \"{query}\".")
        } else {
            format!("Matches for \"{query}\":\n{}", lines.join("\n"))
        })
    }

    /// The 1-based pages asked for, as indices; the current page when none.
    fn pages(&self, arguments: &Value, limit: usize) -> Result<Vec<usize>> {
        let mut pages: Vec<usize> = match (
            arguments["pages"].as_array(),
            arguments["start_page"].as_u64(),
        ) {
            (Some(pages), _) => pages
                .iter()
                .filter_map(Value::as_u64)
                .map(|p| p as usize)
                .collect(),
            (None, Some(start)) => {
                let end = arguments["end_page"].as_u64().unwrap_or(start).max(start);
                (start..=end).map(|page| page as usize).collect()
            }
            (None, None) => vec![self.current_page + 1],
        };
        pages.retain(|&page| page >= 1 && page <= self.page_count);
        pages.dedup();
        if pages.is_empty() {
            bail!("no pages between 1 and {} were asked for", self.page_count);
        }
        pages.truncate(limit);
        Ok(pages.into_iter().map(|page| page - 1).collect())
    }

    fn read(&self, arguments: &Value, cancel: &AgentCancellationToken) -> Result<String> {
        let max_chars = arguments["max_chars"]
            .as_u64()
            .map_or(MAX_READ_CHARS, |chars| (chars as usize).min(MAX_READ_CHARS));
        self.page_texts(&self.pages(arguments, MAX_READ_PAGES)?, max_chars, cancel)
    }

    /// The text of 0-based `pages`, each under a heading, cut at
    /// `max_chars`.
    pub fn page_texts(
        &self,
        pages: &[usize],
        max_chars: usize,
        cancel: &AgentCancellationToken,
    ) -> Result<String> {
        let document = self.document()?;
        let mut out = String::new();
        for &page in pages {
            if cancel.is_canceled() {
                bail!("agent run canceled");
            }
            let text = Self::page_text(&document, page)?.plain_text();
            out.push_str(&format!("## Page {}\n{text}\n\n", page + 1));
            if out.chars().count() >= max_chars {
                break;
            }
        }
        Ok(truncate(&out, max_chars))
    }

    fn render(
        &self,
        arguments: &Value,
        cancel: &AgentCancellationToken,
    ) -> Result<Vec<AgentToolResultPart>> {
        let pages = self.pages(arguments, MAX_RENDER_PAGES)?;
        let images = self.page_images(&pages, cancel)?;
        Ok(pages
            .into_iter()
            .zip(images)
            .flat_map(|(page, image)| {
                [
                    AgentToolResultPart::Text(format!("Page {}:", page + 1)),
                    AgentToolResultPart::Image(image),
                ]
            })
            .collect())
    }

    /// 0-based `pages` as JPEG images (PDFs only).
    pub fn page_images(
        &self,
        pages: &[usize],
        cancel: &AgentCancellationToken,
    ) -> Result<Vec<AgentImage>> {
        if !self.renders() {
            bail!(
                "{} is a reflowed book; its pages have no images",
                self.title
            );
        }
        let document = self.document()?;
        let mut images = Vec::new();
        for &page in pages {
            if cancel.is_canceled() {
                bail!("agent run canceled");
            }
            let loaded = document.load_page(i32::try_from(page)?)?;
            let bounds = loaded.bounds()?;
            let longest = (bounds.x1 - bounds.x0).max(bounds.y1 - bounds.y0).max(1.0);
            let scale = (RENDER_PIXELS / longest).min(3.0);
            let pixmap = loaded.to_pixmap(
                &Matrix::new_scale(scale, scale),
                &Colorspace::device_rgb(),
                false,
                true,
            )?;
            let (width, height) = (pixmap.width(), pixmap.height());
            let mut jpeg = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 82)
                .encode(
                    pixmap.samples(),
                    width,
                    height,
                    image::ExtendedColorType::Rgb8,
                )
                .context("failed to encode the page image")?;
            images.push(
                AgentImage::new("image/jpeg", width, height, jpeg)
                    .with_name(format!("page-{}.jpg", page + 1)),
            );
        }
        Ok(images)
    }

    pub fn renders(&self) -> bool {
        matches!(self.layout, DocumentLayout::Fixed)
    }

    fn links(&self, arguments: &Value) -> Result<String> {
        let page = arguments["page"]
            .as_u64()
            .map_or(self.current_page, |page| (page as usize).saturating_sub(1));
        if page >= self.page_count {
            bail!("page {} is not between 1 and {}", page + 1, self.page_count);
        }
        let document = self.document()?;
        let loaded = document.load_page(i32::try_from(page)?)?;
        let links = glyphst_pdf::text::page_links(&loaded);
        if links.is_empty() {
            return Ok(format!("Page {} has no links.", page + 1));
        }
        let text = PageText::from_page(&loaded)?;
        let lines = links
            .iter()
            .map(|link| {
                let label = text.text_of_glyphs(&text.glyphs_in(link.rect));
                let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
                let target = match &link.target {
                    glyphst_pdf::text::LinkTarget::Page { page, .. } => {
                        format!("page {}", page + 1)
                    }
                    glyphst_pdf::text::LinkTarget::Uri(uri) => uri.clone(),
                };
                if label.is_empty() {
                    format!("- {target}")
                } else {
                    format!("- \"{label}\" → {target}")
                }
            })
            .collect::<Vec<_>>();
        Ok(format!("Links on page {}:\n{}", page + 1, lines.join("\n")))
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max_chars).collect::<String>())
    }
}

fn tool_result(
    call: &AgentToolCall,
    run: impl FnOnce(&Value) -> Result<Vec<AgentToolResultPart>>,
) -> AgentToolResult {
    match call.arguments_json().and_then(|arguments| run(&arguments)) {
        Ok(content) => AgentToolResult {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            content,
            is_error: false,
        },
        Err(error) => AgentToolResult::error(&call.id, &call.name, format!("{error:#}")),
    }
}

struct SearchDocument(DocumentDirectory);

impl Tool for SearchDocument {
    fn definition(&self) -> AgentToolDefinition {
        let mut search = AgentToolDefinition::new(
            "search_document",
            "Search a document for text. Returns concise matches with page numbers.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": document_id_property(),
                    "query": {"type": "string", "description": "Text to search for"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_RESULTS}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        );
        search.strict = false;
        search
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        tool_result(call, |arguments| {
            let document = self.0.resolve(document_argument(arguments))?;
            Ok(vec![AgentToolResultPart::Text(
                document.search(arguments, &cx.cancel)?,
            )])
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call("search_document", &arguments.to_string()))
    }
}

struct ReadDocument(DocumentDirectory);

impl Tool for ReadDocument {
    fn definition(&self) -> AgentToolDefinition {
        let mut read = AgentToolDefinition::new(
            "read_document",
            "Read a document's text by 1-based pages. If no pages are given, reads the current page.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": document_id_property(),
                    "pages": {
                        "type": "array",
                        "items": {"type": "integer", "minimum": 1},
                        "maxItems": MAX_READ_PAGES
                    },
                    "start_page": {"type": "integer", "minimum": 1},
                    "end_page": {"type": "integer", "minimum": 1},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": MAX_READ_CHARS}
                },
                "additionalProperties": false
            }),
        );
        read.strict = false;
        read
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        tool_result(call, |arguments| {
            let document = self.0.resolve(document_argument(arguments))?;
            Ok(vec![AgentToolResultPart::Text(
                document.read(arguments, &cx.cancel)?,
            )])
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call("read_document", &arguments.to_string()))
    }
}

struct RenderPages(DocumentDirectory);

impl Tool for RenderPages {
    fn definition(&self) -> AgentToolDefinition {
        let mut render = AgentToolDefinition::new(
            "render_document_pages",
            "Render 1-based PDF pages as images. Use it when layout, figures, tables, or equations matter.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": document_id_property(),
                    "pages": {
                        "type": "array",
                        "items": {"type": "integer", "minimum": 1},
                        "minItems": 1,
                        "maxItems": MAX_RENDER_PAGES
                    }
                },
                "required": ["pages"],
                "additionalProperties": false
            }),
        );
        render.strict = false;
        render
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        tool_result(call, |arguments| {
            let document = self.0.resolve(document_argument(arguments))?;
            if !document.renders() {
                bail!(
                    "{} is a reflowed book; read its text instead",
                    document.title
                );
            }
            document.render(arguments, &cx.cancel)
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call(
            "render_document_pages",
            &arguments.to_string(),
        ))
    }
}

struct ListLinks(DocumentDirectory);

impl Tool for ListLinks {
    fn definition(&self) -> AgentToolDefinition {
        let mut links = AgentToolDefinition::new(
            "list_links",
            "List the links on a page: web addresses and the pages internal links go to.",
            json!({
                "type": "object",
                "properties": {
                    "document_id": document_id_property(),
                    "page": {"type": "integer", "minimum": 1, "description": "1-based; the current page when omitted"}
                },
                "additionalProperties": false
            }),
        );
        links.strict = false;
        links
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn call(&self, call: &AgentToolCall, _cx: &ToolContext) -> AgentToolResult {
        tool_result(call, |arguments| {
            let document = self.0.resolve(document_argument(arguments))?;
            Ok(vec![AgentToolResultPart::Text(document.links(arguments)?)])
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(match arguments["page"].as_u64() {
            Some(page) => format!("Listed the links on page {page}"),
            None => "Listed the links on the page".into(),
        })
    }
}

/// A tool call's line in the chat, after it ran: "Searched “cats”".
pub fn describe_call(name: &str, arguments: &str) -> String {
    describe_step(name, arguments).done()
}

/// What a step of the assistant's work does, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The verb while it runs ("Searching") and once done ("Searched").
    pub doing: &'static str,
    pub did: &'static str,
    /// What it acts on ("“cats”", "pages 3–5").
    pub object: String,
    pub kind: StepKind,
    /// Pages it read or looked at, for the summary.
    pub pages: usize,
}

/// Kinds of steps, counted in a finished answer's summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StepKind {
    Search,
    Read,
    Look,
    Check,
    Navigate,
    Change,
    /// An external agent's step, named by the agent.
    Other,
}

impl Step {
    pub fn doing(&self) -> String {
        join(self.doing, &self.object)
    }

    pub fn done(&self) -> String {
        join(self.did, &self.object)
    }
}

fn join(verb: &str, object: &str) -> String {
    match (verb.is_empty(), object.is_empty()) {
        (true, _) => object.to_string(),
        (false, true) => verb.to_string(),
        (false, false) => format!("{verb} {object}"),
    }
}

/// The step a tool call is.
pub fn describe_step(name: &str, arguments: &str) -> Step {
    let arguments: Value = serde_json::from_str(arguments).unwrap_or_default();
    let quoted = |key: &str| {
        let text = arguments[key].as_str().unwrap_or_default();
        let short = text.chars().take(48).collect::<String>();
        if text.chars().count() > 48 {
            format!("“{short}…”")
        } else {
            format!("“{short}”")
        }
    };
    let (pages, count) = match (
        arguments["pages"].as_array(),
        arguments["start_page"].as_u64(),
        arguments["page"].as_u64(),
    ) {
        (Some(pages), _, _) => {
            let numbers = pages
                .iter()
                .filter_map(Value::as_u64)
                .map(|page| page.to_string())
                .collect::<Vec<_>>();
            let label = match numbers.as_slice() {
                [one] => format!("page {one}"),
                _ => format!("pages {}", numbers.join(", ")),
            };
            (label, numbers.len().max(1))
        }
        (None, Some(start), _) => match arguments["end_page"].as_u64() {
            Some(end) if end > start => {
                (format!("pages {start}–{end}"), (end - start + 1) as usize)
            }
            _ => (format!("page {start}"), 1),
        },
        (None, None, Some(page)) => (format!("page {page}"), 1),
        (None, None, None) => ("the current page".into(), 1),
    };
    let step = |doing, did, object: String, kind| Step {
        doing,
        did,
        object,
        kind,
        pages: 0,
    };
    match name {
        "search_document" => step("Searching", "Searched", quoted("query"), StepKind::Search),
        "read_document" => Step {
            pages: count,
            ..step("Reading", "Read", pages, StepKind::Read)
        },
        "render_document_pages" => Step {
            pages: count,
            ..step("Looking at", "Looked at", pages, StepKind::Look)
        },
        "list_links" => step(
            "Reading",
            "Read",
            format!("the links on {pages}"),
            StepKind::Read,
        ),
        "get_app_state" => step("Checking", "Checked", "the reader".into(), StepKind::Check),
        "list_documents" => step("Listing", "Listed", "the documents".into(), StepKind::Check),
        "get_document_info" => step(
            "Reading",
            "Read",
            "the document's details".into(),
            StepKind::Check,
        ),
        "get_outline" => step("Reading", "Read", "the outline".into(), StepKind::Check),
        "get_selection" => step("Reading", "Read", "the selection".into(), StepKind::Check),
        "list_annotations" => step("Reading", "Read", "the annotations".into(), StepKind::Check),
        "search_library" => step(
            "Searching",
            "Searched",
            format!("the library for {}", quoted("query")),
            StepKind::Search,
        ),
        "list_commands" => step("Listing", "Listed", "the commands".into(), StepKind::Check),
        "run_command" => step(
            "Running",
            "Ran",
            arguments["id"].as_str().unwrap_or("a command").to_string(),
            StepKind::Navigate,
        ),
        "go_to_page" => step("Going to", "Went to", pages, StepKind::Navigate),
        "open_document" => step("Opening", "Opened", "a document".into(), StepKind::Navigate),
        "add_highlight" => {
            if arguments["note"]
                .as_str()
                .is_some_and(|note| !note.is_empty())
            {
                step(
                    "Adding a note on",
                    "Added a note on",
                    quoted("quote"),
                    StepKind::Change,
                )
            } else {
                step(
                    "Highlighting",
                    "Highlighted",
                    quoted("quote"),
                    StepKind::Change,
                )
            }
        }
        "update_annotation" => step(
            "Changing",
            "Changed",
            "an annotation".into(),
            StepKind::Change,
        ),
        "remove_annotation" => step(
            "Removing",
            "Removed",
            "an annotation".into(),
            StepKind::Change,
        ),
        "add_bookmark" => step("Bookmarking", "Bookmarked", pages, StepKind::Change),
        "set_reading_status" => step(
            "Marking",
            "Marked",
            format!(
                "the book as {}",
                arguments["status"]
                    .as_str()
                    .unwrap_or("read")
                    .replace('_', " ")
            ),
            StepKind::Change,
        ),
        "set_library_note" => step(
            "Editing",
            "Edited",
            "the book's note".into(),
            StepKind::Change,
        ),
        "get_library_item" => step("Reading", "Read", "the library item".into(), StepKind::Read),
        "list_library" => step(
            "Listing",
            "Listed",
            arguments["collection"]
                .as_str()
                .map_or("the library".to_string(), str::to_string),
            StepKind::Search,
        ),
        "list_library_vocabulary" => step(
            "Reading",
            "Read",
            "the library's tags and collections".into(),
            StepKind::Check,
        ),
        "read_vault_note" => step("Reading", "Read", "the item's note".into(), StepKind::Read),
        "update_library_item" => step(
            "Editing",
            "Edited",
            "the item's details".into(),
            StepKind::Change,
        ),
        "set_item_tags" => step("Tagging", "Tagged", "the item".into(), StepKind::Change),
        "set_item_collections" => step("Filing", "Filed", "the item".into(), StepKind::Change),
        "add_library_item" => step(
            "Adding",
            "Added",
            "a document to the library".into(),
            StepKind::Change,
        ),
        "enrich_library_items" => step("Queuing", "Queued", "the AI pass".into(), StepKind::Change),
        "merge_library_items" => step(
            "Merging",
            "Merged",
            "two library items".into(),
            StepKind::Change,
        ),
        "set_related_items" => step(
            "Linking",
            "Linked",
            "related items".into(),
            StepKind::Change,
        ),
        "update_vault_note_block" => step(
            "Writing",
            "Wrote",
            format!(
                "the {} block",
                arguments["block"].as_str().unwrap_or("agent")
            ),
            StepKind::Change,
        ),
        "lookup_metadata" => step(
            "Looking up",
            "Looked up",
            arguments["identifier"]
                .as_str()
                .or(arguments["title"].as_str())
                .unwrap_or("a work")
                .to_string(),
            StepKind::Search,
        ),
        // External agents' steps are already titles; the Reader's own tools
        // among them ("read document") count as theirs.
        other if other.contains(' ') => {
            let kind = describe_step(&other.replace(' ', "_"), "{}").kind;
            let mut title = other.to_string();
            if let Some(first) = title.get(..1) {
                title.replace_range(..1, &first.to_uppercase());
            }
            step("", "", title, kind)
        }
        other => step("Using", "Used", other.to_string(), StepKind::Other),
    }
}

/// A finished answer's work in a few words: "searched 2×, read 3 pages".
pub fn summarize_steps(steps: &[Step]) -> String {
    let count = |kind| steps.iter().filter(|step| step.kind == kind).count();
    let pages = |kind| {
        steps
            .iter()
            .filter(|step| step.kind == kind)
            .map(|step| step.pages)
            .sum::<usize>()
    };
    let times = |verb: &str, n: usize| match n {
        0 => None,
        1 => Some(verb.to_string()),
        n => Some(format!("{verb} {n}×")),
    };
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts: Vec<String> = Vec::new();
    parts.extend(times("searched", count(StepKind::Search)));
    match (count(StepKind::Read), pages(StepKind::Read)) {
        (0, _) => {}
        (_, 0) => parts.push("read".into()),
        (_, n) => parts.push(format!("read {}", plural(n, "page", "pages"))),
    }
    if count(StepKind::Look) > 0 {
        parts.push(format!(
            "looked at {}",
            plural(pages(StepKind::Look).max(1), "page", "pages")
        ));
    }
    if count(StepKind::Check) > 0 && parts.is_empty() {
        parts.push("checked the reader".into());
    }
    parts.extend(times("navigated", count(StepKind::Navigate)));
    match count(StepKind::Change) {
        0 => {}
        n => parts.push(format!("made {}", plural(n, "change", "changes"))),
    }
    match count(StepKind::Other) {
        0 => {}
        n => parts.push(plural(n, "step", "steps")),
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools() -> (ToolRegistry, PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("glyphst-reader-ai-tools-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("doc.pdf");
        std::fs::write(
            &path,
            glyphst_pdf::test_pdf::text_pdf(&["The cat sat on the mat.", "A dog barked."]),
        )
        .unwrap();
        let mut tools = ToolRegistry::new();
        let documents = DocumentDirectory::default();
        documents.update(
            vec![OpenDocument {
                id: "doc_1".into(),
                title: "Doc".into(),
                path,
                layout: DocumentLayout::Fixed,
                page_count: 1,
                current_page: 0,
            }],
            Some("doc_1".into()),
        );
        documents.register(&mut tools);
        (tools, directory)
    }

    fn call(name: &str, arguments: Value) -> AgentToolCall {
        AgentToolCall {
            id: "call".into(),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }

    fn text(result: &AgentToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|part| match part {
                AgentToolResultPart::Text(text) => Some(text.as_str()),
                AgentToolResultPart::Image(_) => None,
            })
            .collect()
    }

    #[test]
    fn tools_search_read_and_render_the_document() {
        let (tools, directory) = tools();
        let cancel = ToolContext::default();
        let names: Vec<String> = tools.definitions().into_iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "search_document",
                "read_document",
                "render_document_pages",
                "list_links"
            ]
        );
        let elsewhere = tools.execute(
            &call("read_document", json!({"document_id": "doc_9"})),
            &cancel,
        );
        assert!(text(&elsewhere).contains("no open document has the id doc_9"));
        let named = tools.execute(
            &call("read_document", json!({"document_id": "doc_1"})),
            &cancel,
        );
        assert!(!named.is_error);
        let links = tools.execute(&call("list_links", json!({})), &cancel);
        assert_eq!(text(&links), "Page 1 has no links.");

        let found = tools.execute(&call("search_document", json!({"query": "dog"})), &cancel);
        assert!(!found.is_error);
        assert!(
            text(&found).contains("page 1: The cat sat on the mat. A dog barked."),
            "{}",
            text(&found)
        );

        let read = tools.execute(&call("read_document", json!({})), &cancel);
        assert!(text(&read).starts_with("## Page 1\nThe cat sat on the mat.\nA dog barked."));
        let outside = tools.execute(&call("read_document", json!({"pages": [9]})), &cancel);
        assert!(outside.is_error);

        let render = tools.execute(
            &call("render_document_pages", json!({"pages": [1]})),
            &cancel,
        );
        let image = render.content.iter().find_map(|part| match part {
            AgentToolResultPart::Image(image) => Some(image),
            AgentToolResultPart::Text(_) => None,
        });
        let image = image.expect("a page image");
        assert_eq!(image.media_type, "image/jpeg");
        assert_eq!(
            (image.width, image.height),
            (600, 300),
            "at most 3 px per point"
        );

        let unknown = tools.execute(&call("delete_everything", json!({})), &cancel);
        assert!(unknown.is_error);
        assert_eq!(
            describe_call("search_document", r#"{"query":"dog"}"#),
            "Searched “dog”"
        );
        assert_eq!(
            describe_call("read_document", r#"{"start_page":2,"end_page":4}"#),
            "Read pages 2–4"
        );
        let steps = [
            describe_step("search_document", r#"{"query":"dog"}"#),
            describe_step("search_document", r#"{"query":"cat"}"#),
            describe_step("read_document", r#"{"start_page":2,"end_page":4}"#),
            describe_step("add_highlight", r#"{"page":2,"quote":"A dog"}"#),
        ];
        assert_eq!(steps[3].doing(), "Highlighting “A dog”");
        let external = describe_step("render document pages", "");
        assert_eq!(
            (external.done(), external.kind),
            ("Render document pages".into(), StepKind::Look)
        );
        assert_eq!(describe_step("Bash List files", "").kind, StepKind::Other);
        assert_eq!(
            summarize_steps(&steps),
            "searched 2×, read 3 pages, made 1 change"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
