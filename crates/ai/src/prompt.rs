//! The assistant's prompt, in two layers.
//!
//! The system prompt (who the assistant is, how to use the tools) stays the
//! same for a whole conversation, so providers can cache it and replay
//! reasoning against it. What changes between turns (the open document, the
//! page, the selection) goes into a `<reader_state>` block at the end of each
//! user message, where it stays in the history.

use crate::{agent::AgentToolDefinition, settings::AiThinkingLevel};

const IDENTITY: &str = "You are the assistant built into Glyphst Reader, a PDF and EPUB reader. Help the user read, understand, analyze, and work with their documents and library. Be conversational but precise. Refer to the user in the second person and yourself in the first person. Format answers in Markdown.

Ground answers in the documents: use the attached context and your tools to read them, cite 1-based page numbers, and say when something cannot be found instead of guessing. Each user message ends with a <reader_state> block that describes the app at the time of that message; it is not written by the user.";

const MENTIONS: &str = "The user can attach context with @ mentions (pages as text or images, the selection, whole documents, page regions, outline sections, annotations, and other documents). Attached context appears in a <context> block; treat it as current and authoritative.";

/// The stable part of the prompt.
pub fn system_prompt(
    tools: &[AgentToolDefinition],
    thinking_level: AiThinkingLevel,
    native_thinking: bool,
) -> String {
    let mut sections = vec![IDENTITY.to_string(), MENTIONS.to_string()];
    if let Some(guidance) = tool_guidance(tools) {
        sections.push(guidance);
    }
    if thinking_level != AiThinkingLevel::Off && !native_thinking {
        sections.push(format!(
            "Use {} internal reasoning effort before answering.",
            thinking_level.as_str()
        ));
    }
    sections.join("\n\n")
}

/// Describes the tools offered, so the prompt only ever names tools that
/// exist.
fn tool_guidance(tools: &[AgentToolDefinition]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let list = tools
        .iter()
        .map(|tool| {
            let summary = tool.description.split(". ").next().unwrap_or_default();
            format!("- {}: {}", tool.name, summary.trim_end_matches('.'))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "You can call these tools:\n{list}\n\nCall independent reading tools together in one turn. Prefer text tools before rendering pages. Tools that change the user's data (annotations, bookmarks, the library) take effect immediately and the user can undo them, so make the changes they ask for without asking for confirmation, then say what you changed. Do not claim tool results you did not receive."
    ))
}

/// A document as the reader shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentFacts {
    /// The id tools take for this document.
    pub id: Option<String>,
    pub title: String,
    /// "PDF" or "EPUB".
    pub format: String,
    pub authors: Option<String>,
    pub page_count: usize,
}

/// What the reader shows when the user sends a message.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReaderState {
    pub document: Option<DocumentFacts>,
    /// The page shown, from 1.
    pub current_page: Option<usize>,
    /// The outline entry of the current page.
    pub section: Option<String>,
    /// Characters of selected text.
    pub selection_chars: Option<usize>,
    /// Pages selected in the sidebar, from 1.
    pub selected_pages: Vec<usize>,
    /// The other open documents.
    pub other_documents: Vec<DocumentFacts>,
}

impl ReaderState {
    /// The block appended to the user's message.
    pub fn block(&self) -> String {
        let mut lines = Vec::new();
        match &self.document {
            Some(document) => {
                lines.push(format!("Active document: {}", describe(document)));
                if let Some(page) = self.current_page {
                    let mut line = format!("Current page: {page}");
                    if let Some(section) = &self.section {
                        line.push_str(&format!(" (in \"{section}\")"));
                    }
                    lines.push(line);
                }
            }
            None => lines.push("No document is open.".to_string()),
        }
        if let Some(chars) = self.selection_chars {
            lines.push(format!("Selected text: {chars} characters"));
        }
        if !self.selected_pages.is_empty() {
            let pages = self
                .selected_pages
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("Selected pages: {pages}"));
        }
        for document in &self.other_documents {
            lines.push(format!("Also open: {}", describe(document)));
        }
        format!("<reader_state>\n{}\n</reader_state>", lines.join("\n"))
    }
}

fn describe(document: &DocumentFacts) -> String {
    let mut text = format!("\"{}\"", document.title);
    if let Some(authors) = &document.authors {
        text.push_str(&format!(" by {authors}"));
    }
    text.push_str(&format!(
        " ({}, {} pages)",
        document.format, document.page_count
    ));
    if let Some(id) = &document.id {
        text.push_str(&format!(", id {id}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn tool_guidance_names_only_defined_tools() {
        let tools = vec![AgentToolDefinition {
            name: "read_document".to_string(),
            description: "Read pages. Long detail.".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        }];
        let prompt = system_prompt(&tools, AiThinkingLevel::Off, false);
        assert!(prompt.contains("- read_document: Read pages\n"));
        for stale in ["list_open_documents", "search_documents"] {
            assert!(!prompt.contains(stale));
        }
        assert!(!system_prompt(&[], AiThinkingLevel::Off, false).contains("You can call"));
    }

    #[test]
    fn unsupported_thinking_uses_a_prompt_instruction() {
        let prompt = system_prompt(&[], AiThinkingLevel::Low, false);
        assert!(prompt.contains("Use low internal reasoning effort"));
        let native = system_prompt(&[], AiThinkingLevel::Low, true);
        assert!(!native.contains("internal reasoning effort"));
    }

    #[test]
    fn reader_state_describes_the_view() {
        let state = ReaderState {
            document: Some(DocumentFacts {
                id: Some("doc_1".into()),
                title: "Paper".into(),
                format: "PDF".into(),
                authors: Some("Ada".into()),
                page_count: 42,
            }),
            current_page: Some(9),
            section: Some("Results".into()),
            selection_chars: Some(120),
            selected_pages: vec![3, 4],
            other_documents: Vec::new(),
        };
        assert_eq!(
            state.block(),
            "<reader_state>\nActive document: \"Paper\" by Ada (PDF, 42 pages), id doc_1\nCurrent page: 9 (in \"Results\")\nSelected text: 120 characters\nSelected pages: 3, 4\n</reader_state>"
        );
        assert!(
            ReaderState::default()
                .block()
                .contains("No document is open.")
        );
    }
}
