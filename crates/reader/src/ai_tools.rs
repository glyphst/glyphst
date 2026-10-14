//! Document tools for the assistant: search the document, read pages, and
//! render pages as images, over MuPDF and `pdf::text`. Each tool opens the
//! document itself, so tools never wait for the UI and reads run in
//! parallel.

use std::{path::PathBuf, sync::Arc};

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

/// The open document, as the assistant's tools see it.
pub struct OpenDocument {
    pub path: PathBuf,
    pub layout: DocumentLayout,
    pub page_count: usize,
    /// The page shown, from 0; reading without pages reads it.
    pub current_page: usize,
}

impl OpenDocument {
    /// Adds the tools for this document to a run's tools.
    pub fn register(self, registry: &mut ToolRegistry) {
        let document = Arc::new(self);
        registry.register(SearchDocument(document.clone()));
        registry.register(ReadDocument(document.clone()));
        if document.renders() {
            registry.register(RenderPages(document));
        }
    }

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
        let document = self.document()?;
        let mut out = String::new();
        for page in self.pages(arguments, MAX_READ_PAGES)? {
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
        let document = self.document()?;
        let mut parts = Vec::new();
        for page in self.pages(arguments, MAX_RENDER_PAGES)? {
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
            parts.push(AgentToolResultPart::Text(format!("Page {}:", page + 1)));
            parts.push(AgentToolResultPart::Image(
                AgentImage::new("image/jpeg", width, height, jpeg)
                    .with_name(format!("page-{}.jpg", page + 1)),
            ));
        }
        Ok(parts)
    }

    fn renders(&self) -> bool {
        matches!(self.layout, DocumentLayout::Fixed)
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

struct SearchDocument(Arc<OpenDocument>);

impl Tool for SearchDocument {
    fn definition(&self) -> AgentToolDefinition {
        let mut search = AgentToolDefinition::new(
            "search_document",
            "Search the open document for text. Returns concise matches with page numbers.",
            json!({
                "type": "object",
                "properties": {
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
            Ok(vec![AgentToolResultPart::Text(
                self.0.search(arguments, &cx.cancel)?,
            )])
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call("search_document", &arguments.to_string()))
    }
}

struct ReadDocument(Arc<OpenDocument>);

impl Tool for ReadDocument {
    fn definition(&self) -> AgentToolDefinition {
        let mut read = AgentToolDefinition::new(
            "read_document",
            "Read text from the open document by 1-based pages. If no pages are given, reads the current page.",
            json!({
                "type": "object",
                "properties": {
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
            Ok(vec![AgentToolResultPart::Text(
                self.0.read(arguments, &cx.cancel)?,
            )])
        })
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call("read_document", &arguments.to_string()))
    }
}

struct RenderPages(Arc<OpenDocument>);

impl Tool for RenderPages {
    fn definition(&self) -> AgentToolDefinition {
        let mut render = AgentToolDefinition::new(
            "render_document_pages",
            "Render one or more 1-based PDF pages as images. Use it when layout, figures, tables, or equations matter.",
            json!({
                "type": "object",
                "properties": {
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
        tool_result(call, |arguments| self.0.render(arguments, &cx.cancel))
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some(describe_call(
            "render_document_pages",
            &arguments.to_string(),
        ))
    }
}

/// A one-line description of a tool call for the chat.
pub fn describe_call(name: &str, arguments: &str) -> String {
    let arguments: Value = serde_json::from_str(arguments).unwrap_or_default();
    let pages = |arguments: &Value| -> String {
        match (
            arguments["pages"].as_array(),
            arguments["start_page"].as_u64(),
        ) {
            (Some(pages), _) => pages
                .iter()
                .filter_map(Value::as_u64)
                .map(|page| page.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            (None, Some(start)) => match arguments["end_page"].as_u64() {
                Some(end) if end != start => format!("{start}–{end}"),
                _ => start.to_string(),
            },
            (None, None) => "the current page".into(),
        }
    };
    match name {
        "search_document" => format!(
            "Searched for “{}”",
            arguments["query"].as_str().unwrap_or_default()
        ),
        "read_document" => format!("Read page {}", pages(&arguments)),
        "render_document_pages" => format!("Looked at page {}", pages(&arguments)),
        other => format!("Used {other}"),
    }
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
        OpenDocument {
            path,
            layout: DocumentLayout::Fixed,
            page_count: 1,
            current_page: 0,
        }
        .register(&mut tools);
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
            ["search_document", "read_document", "render_document_pages"]
        );

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
            "Searched for “dog”"
        );
        assert_eq!(
            describe_call("read_document", r#"{"start_page":2,"end_page":4}"#),
            "Read page 2–4"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
