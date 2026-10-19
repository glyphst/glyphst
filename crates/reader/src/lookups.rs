//! The one metadata client the library's jobs and the agents share, so
//! each catalogue's pace (arXiv's 3 s) holds across all of them; and the
//! agents' `lookup_metadata` tool, which runs on the agent's thread since a
//! lookup can take longer than the UI bridge waits.

use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow, bail};
use glyphst_ai::{
    agent::{AgentToolCall, AgentToolDefinition, AgentToolResult},
    tools::{Tool, ToolContext, ToolEffect, ToolRegistry},
};
use glyphst_metadata::{MetadataClient, MetadataEndpoints, WorkMetadata};
use serde_json::{Value, json};

/// The settings the client was made with, and the client.
#[derive(Clone, Default)]
pub struct Lookups(Arc<RwLock<State>>);

#[derive(Default)]
struct State {
    online: bool,
    mailto: Option<String>,
    client: Option<Arc<MetadataClient>>,
}

impl Lookups {
    /// Follows Settings › Library; makes a new client when they change.
    pub fn update(&self, online: bool, mailto: Option<String>) {
        let Ok(mut state) = self.0.write() else {
            return;
        };
        if state.online == online && state.mailto == mailto && state.client.is_some() == online {
            return;
        }
        state.online = online;
        state.mailto = mailto.clone();
        state.client = online
            .then(|| MetadataClient::new(MetadataEndpoints::default(), mailto))
            .and_then(|client| {
                client
                    .map_err(|error| log::warn!("no metadata client: {error:#}"))
                    .ok()
            })
            .map(Arc::new);
    }

    /// The client, when looking up online is on.
    pub fn client(&self) -> Option<Arc<MetadataClient>> {
        self.0.read().ok().and_then(|state| state.client.clone())
    }

    /// Adds `lookup_metadata` to a run's tools.
    pub fn register(&self, registry: &mut ToolRegistry) {
        registry.register(LookupMetadata(self.clone()));
    }
}

struct LookupMetadata(Lookups);

impl Tool for LookupMetadata {
    fn definition(&self) -> AgentToolDefinition {
        let mut lookup = AgentToolDefinition::new(
            "lookup_metadata",
            "Look a work up in Crossref, arXiv, OpenAlex and Open Library by a DOI, arXiv id, ISBN or URL, or by its title (and an author's surname). Returns what they know; it changes nothing (use update_library_item or add_library_item for that).",
            json!({
                "type": "object",
                "properties": {
                    "identifier": {"type": "string", "description": "A DOI, arXiv id, ISBN, or a URL with one"},
                    "title": {"type": "string", "description": "When there is no identifier"},
                    "author": {"type": "string", "description": "An author's surname, with title"}
                },
                "additionalProperties": false
            }),
        );
        lookup.strict = false;
        lookup
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    fn open_world(&self) -> bool {
        true
    }

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        let result = call
            .arguments_json()
            .and_then(|arguments| self.look_up(&arguments, cx));
        match result {
            Ok(text) => AgentToolResult::text(&call.id, &call.name, text),
            Err(error) => AgentToolResult::error(&call.id, &call.name, format!("{error:#}")),
        }
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        let what = arguments["identifier"]
            .as_str()
            .or(arguments["title"].as_str())
            .unwrap_or("a work");
        Some(format!("Looked up {what}"))
    }
}

impl LookupMetadata {
    fn look_up(&self, arguments: &Value, cx: &ToolContext) -> Result<String> {
        let client = self.0.client().ok_or_else(|| {
            anyhow!("looking up online is off (Settings › Library › look up details online)")
        })?;
        let text = |key: &str| {
            arguments[key]
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let mut work = None;
        if let Some(identifier) = text("identifier") {
            let identifiers = crate::library_dialogs::parse_identifier(identifier);
            if identifiers.doi.is_none()
                && identifiers.arxiv.is_none()
                && identifiers.isbn.is_none()
            {
                bail!("{identifier} is not a DOI, arXiv id or ISBN");
            }
            work = client.lookup(&identifiers)?;
        }
        if work.is_none()
            && let Some(title) = text("title")
        {
            if cx.cancel.is_canceled() {
                bail!("agent run canceled");
            }
            work = client.search_title(title, text("author"))?;
        }
        if text("identifier").is_none() && text("title").is_none() {
            bail!("give an identifier or a title");
        }
        Ok(match work {
            Some(work) => describe_work(&work),
            None => "No catalogue knows that work.".to_string(),
        })
    }
}

/// The work as lines, for the model.
pub fn describe_work(work: &WorkMetadata) -> String {
    let mut lines = vec![format!("Title: {}", work.title)];
    if !work.creators.is_empty() {
        lines.push(format!("Authors: {}", work.creators.join(", ")));
    }
    if let Some(year) = work.year {
        lines.push(format!("Year: {year}"));
    }
    for (label, value) in [
        ("Publication", &work.publication),
        ("Publisher", &work.publisher),
        ("Language", &work.language),
        ("Abstract", &work.abstract_text),
    ] {
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    for (label, value) in [
        ("DOI", &work.doi),
        ("arXiv", &work.arxiv),
        ("ISBN", &work.isbn),
        ("Open PDF", &work.pdf_url),
    ] {
        if let Some(value) = value {
            lines.push(format!("{label}: {value}"));
        }
    }
    lines.push(format!("Source: {}", work.source.label()));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lookup_needs_the_setting_and_something_to_look_for() {
        let lookups = Lookups::default();
        let tool = LookupMetadata(lookups.clone());
        let cx = ToolContext::default();
        let error = tool
            .look_up(&json!({"identifier": "10.1/x"}), &cx)
            .unwrap_err();
        assert!(error.to_string().contains("off"), "{error:#}");

        lookups.update(true, None);
        assert!(lookups.client().is_some());
        let error = tool.look_up(&json!({}), &cx).unwrap_err();
        assert!(error.to_string().contains("identifier or a title"));
        let error = tool
            .look_up(&json!({"identifier": "not one"}), &cx)
            .unwrap_err();
        assert!(error.to_string().contains("not a DOI"));
        lookups.update(false, None);
        assert!(lookups.client().is_none());
    }

    #[test]
    fn a_work_reads_as_lines() {
        let work = WorkMetadata {
            title: "Attention Is All You Need".to_string(),
            creators: vec!["Ashish Vaswani".to_string()],
            year: Some(2017),
            arxiv: Some("1706.03762".to_string()),
            ..WorkMetadata::default()
        };
        let text = describe_work(&work);
        assert!(
            text.starts_with(
                "Title: Attention Is All You Need\nAuthors: Ashish Vaswani\nYear: 2017"
            )
        );
        assert!(text.contains("arXiv: 1706.03762"));
    }
}
