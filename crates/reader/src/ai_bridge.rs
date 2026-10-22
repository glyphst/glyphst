//! Tools that need the app (tabs, the library, the view) run on the UI
//! thread: the agent's worker sends a [`UiCall`] and waits for the reply,
//! and the app answers calls between frames, also while its window is
//! hidden.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};

use glyphst_ai::{
    agent::{AgentToolCall, AgentToolDefinition, AgentToolResult},
    tools::{Tool, ToolContext, ToolEffect, ToolRegistry},
};
use glyphst_library::LibraryEdit;
use serde_json::{Value, json};

use crate::ai_tools::document_id_property;

/// How long a tool waits for the app.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// A tool call for the app to answer.
pub struct UiCall {
    pub tool: String,
    pub arguments: Value,
    pub context: ToolContext,
    pub reply: Sender<UiReply>,
}

/// What a call did: its result for the model, and the library edits it
/// made (for the change log).
#[derive(Debug, Default)]
pub struct UiOutput {
    pub text: String,
    pub edits: Vec<LibraryEdit>,
    /// Blocks it wrote in vault notes.
    pub vault: Vec<crate::vault_blocks::VaultBlockEdit>,
}

impl From<String> for UiOutput {
    fn from(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }
}

pub type UiReply = Result<UiOutput, String>;

/// The worker's end of the bridge.
#[derive(Clone)]
pub struct UiBridge {
    calls: Sender<UiCall>,
    ctx: egui::Context,
}

impl UiBridge {
    pub fn new(ctx: &egui::Context) -> (Self, Receiver<UiCall>) {
        let (calls, receiver) = mpsc::channel();
        (
            Self {
                calls,
                ctx: ctx.clone(),
            },
            receiver,
        )
    }

    fn request(&self, tool: &str, arguments: Value, context: &ToolContext) -> UiReply {
        let (reply, answer) = mpsc::channel();
        self.calls
            .send(UiCall {
                tool: tool.to_string(),
                arguments,
                context: context.clone(),
                reply,
            })
            .map_err(|_| "the reader closed".to_string())?;
        self.ctx.request_repaint();
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            if context.cancel.is_canceled() {
                return Err("agent run canceled".into());
            }
            match answer.recv_timeout(Duration::from_millis(100)) {
                Ok(reply) => return reply,
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(RecvTimeoutError::Timeout) => {
                    return Err("the reader did not answer in time".into());
                }
                Err(RecvTimeoutError::Disconnected) => return Err("the reader closed".into()),
            }
        }
    }

    /// Adds the app's tools to a run's tools.
    pub fn register(&self, registry: &mut ToolRegistry) {
        for spec in SPECS {
            registry.register(UiTool {
                spec,
                bridge: self.clone(),
            });
        }
    }
}

struct Spec {
    name: &'static str,
    description: &'static str,
    effect: ToolEffect,
    /// The properties of the input schema, besides `document_id`.
    properties: fn() -> Value,
    required: &'static [&'static str],
    /// Whether the tool takes a `document_id`.
    document: bool,
    describe: fn(&Value) -> String,
}

struct UiTool {
    spec: &'static Spec,
    bridge: UiBridge,
}

impl Tool for UiTool {
    fn definition(&self) -> AgentToolDefinition {
        let mut properties = (self.spec.properties)();
        if self.spec.document
            && let Some(properties) = properties.as_object_mut()
        {
            properties.insert("document_id".into(), document_id_property());
        }
        let mut definition = AgentToolDefinition::new(
            self.spec.name,
            self.spec.description,
            json!({
                "type": "object",
                "properties": properties,
                "required": self.spec.required,
                "additionalProperties": false
            }),
        );
        definition.strict = false;
        definition
    }

    fn effect(&self) -> ToolEffect {
        self.spec.effect
    }

    fn call(&self, call: &AgentToolCall, cx: &ToolContext) -> AgentToolResult {
        let reply = call
            .arguments_json()
            .map_err(|error| format!("{error:#}"))
            .and_then(|arguments| self.bridge.request(&call.name, arguments, cx));
        match reply {
            Ok(output) => AgentToolResult::text(&call.id, &call.name, output.text),
            Err(error) => AgentToolResult::error(&call.id, &call.name, error),
        }
    }

    fn describe(&self, arguments: &Value) -> Option<String> {
        Some((self.spec.describe)(arguments))
    }
}

fn none() -> Value {
    json!({})
}

fn page_property() -> Value {
    json!({"type": "integer", "minimum": 1, "description": "1-based page number"})
}

const COLORS: [&str; 5] = ["yellow", "green", "blue", "pink", "orange"];

fn text_of(arguments: &Value, key: &str) -> String {
    let text = arguments[key].as_str().unwrap_or_default();
    let short = text.chars().take(40).collect::<String>();
    if text.chars().count() > 40 {
        format!("{short}…")
    } else {
        short
    }
}

static SPECS: &[Spec] = &[
    Spec {
        name: "get_app_state",
        description: "Get the reader's state: open documents with their ids, the active one and its page, panes, the theme, and the assistant's model.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: false,
        describe: |_| "Looked at the reader".into(),
    },
    Spec {
        name: "list_documents",
        description: "List the open documents (with ids for other tools) and the library's recently read books (with library ids for open_document).",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: false,
        describe: |_| "Listed the documents".into(),
    },
    Spec {
        name: "get_document_info",
        description: "Get a document's details: title, authors and other metadata, pages, the current page and section, its library entry (status, year, abstract, note), and how many annotations it has.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: true,
        describe: |_| "Looked at the document's details".into(),
    },
    Spec {
        name: "get_outline",
        description: "Get a document's table of contents with the 1-based page of each entry.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: true,
        describe: |_| "Read the table of contents".into(),
    },
    Spec {
        name: "get_selection",
        description: "Get the text the user selected in the active document, with its page.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: false,
        describe: |_| "Read the selection".into(),
    },
    Spec {
        name: "list_annotations",
        description: "List a document's highlights, notes and bookmarks with their ids, pages, colors, quoted text and note bodies.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: true,
        describe: |_| "Listed the annotations".into(),
    },
    Spec {
        name: "open_at",
        description: "Open a library item at a page, or at a passage search_library found (chunk_id), and show it to the user.",
        effect: ToolEffect::Navigate,
        properties: || {
            json!({
                "library_id": {"type": "string", "description": "lib_12; not needed with chunk_id"},
                "page": {"type": "integer", "minimum": 1},
                "chunk_id": {"type": "string", "description": "c_381"}
            })
        },
        required: &[],
        document: false,
        describe: |arguments| match arguments["page"].as_u64() {
            Some(page) => format!("Opened page {page}"),
            None => "Opened the passage".into(),
        },
    },
    Spec {
        name: "list_commands",
        description: "List the reader's commands (ids for run_command), with whether each can run now.",
        effect: ToolEffect::Read,
        properties: none,
        required: &[],
        document: false,
        describe: |_| "Listed the commands".into(),
    },
    Spec {
        name: "run_command",
        description: "Run a reader command by id, such as view.zoom_in or navigation.next_page. Only view, navigation, find and theme commands can run.",
        effect: ToolEffect::Navigate,
        properties: || json!({"id": {"type": "string"}}),
        required: &["id"],
        document: false,
        describe: |arguments| format!("Ran {}", text_of(arguments, "id")),
    },
    Spec {
        name: "go_to_page",
        description: "Show a page of a document, making the document active.",
        effect: ToolEffect::Navigate,
        properties: || json!({"page": page_property()}),
        required: &["page"],
        document: true,
        describe: |arguments| format!("Went to page {}", arguments["page"].as_u64().unwrap_or(0)),
    },
    Spec {
        name: "open_document",
        description: "Open a library book (library_id from list_documents or search_library) or a file path in a tab, and make it active. Returns its document id.",
        effect: ToolEffect::Navigate,
        properties: || {
            json!({
                "library_id": {"type": "string"},
                "path": {"type": "string"}
            })
        },
        required: &[],
        document: false,
        describe: |_| "Opened a document".into(),
    },
    Spec {
        name: "add_highlight",
        description: "Highlight a quote on a page; with a note, add a note on it instead. The quote must match the page's text. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "page": page_property(),
                "quote": {"type": "string", "description": "Exact text on the page"},
                "occurrence": {"type": "integer", "minimum": 1, "description": "Which match on the page, from 1"},
                "color": {"type": "string", "enum": COLORS},
                "note": {"type": "string"}
            })
        },
        required: &["page", "quote"],
        document: true,
        describe: |arguments| {
            if arguments["note"]
                .as_str()
                .is_some_and(|note| !note.is_empty())
            {
                format!("Added a note on “{}”", text_of(arguments, "quote"))
            } else {
                format!("Highlighted “{}”", text_of(arguments, "quote"))
            }
        },
    },
    Spec {
        name: "update_annotation",
        description: "Change a highlight's or note's color, or a note's text, by its id from list_annotations. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "annotation_id": {"type": "string"},
                "color": {"type": "string", "enum": COLORS},
                "note": {"type": "string"}
            })
        },
        required: &["annotation_id"],
        document: true,
        describe: |_| "Changed an annotation".into(),
    },
    Spec {
        name: "remove_annotation",
        description: "Remove a highlight, note or bookmark by its id from list_annotations. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || json!({"annotation_id": {"type": "string"}}),
        required: &["annotation_id"],
        document: true,
        describe: |_| "Removed an annotation".into(),
    },
    Spec {
        name: "add_bookmark",
        description: "Bookmark a page, optionally with a title. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "page": page_property(),
                "title": {"type": "string"}
            })
        },
        required: &["page"],
        document: true,
        describe: |arguments| {
            format!(
                "Bookmarked page {}",
                arguments["page"].as_u64().unwrap_or(0)
            )
        },
    },
    Spec {
        name: "set_reading_status",
        description: "Set a library book's reading status. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": {"type": "string", "description": "The book's library id; the document's book when omitted"},
                "status": {"type": "string", "enum": ["to_read", "reading", "read"]}
            })
        },
        required: &["status"],
        document: true,
        describe: |arguments| {
            format!(
                "Marked the book as {}",
                text_of(arguments, "status").replace('_', " ")
            )
        },
    },
    Spec {
        name: "set_library_note",
        description: "Replace a library book's own note (not an annotation). The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": {"type": "string", "description": "The book's library id; the document's book when omitted"},
                "note": {"type": "string"}
            })
        },
        required: &["note"],
        document: true,
        describe: |_| "Edited the book's note".into(),
    },
    Spec {
        name: "get_library_item",
        description: "Get a library item in full: its metadata and identifiers, where the metadata came from, tags and collections (the AI's marked with whether the user kept them), the AI summary and key points, its file and its note in the vault.",
        effect: ToolEffect::Read,
        properties: || json!({"library_id": library_id_property()}),
        required: &[],
        document: true,
        describe: |_| "Read the library item".into(),
    },
    Spec {
        name: "list_library",
        description: "List library items, newest first or as the view sorts them. `query` takes words plus by:author, tag:name, year:2020 or year:>2019. Returns library ids with title, authors, year, status and tags.",
        effect: ToolEffect::Read,
        properties: || {
            json!({
                "view": {"type": "string", "enum": ["all", "recent", "unfiled", "missing", "trash"]},
                "collection": {"type": "string", "description": "A collection path such as Machine learning/LLMs"},
                "query": {"type": "string"},
                "offset": {"type": "integer", "minimum": 0},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100}
            })
        },
        required: &[],
        document: false,
        describe: |arguments| match arguments["collection"].as_str() {
            Some(collection) => format!("Listed {collection}"),
            None => "Listed the library".into(),
        },
    },
    Spec {
        name: "list_library_vocabulary",
        description: "List the library's tags and collection paths with how many items each has, to reuse their spelling before tagging or filing.",
        effect: ToolEffect::Read,
        properties: || json!({}),
        required: &[],
        document: false,
        describe: |_| "Read the library's tags and collections".into(),
    },
    Spec {
        name: "read_vault_note",
        description: "Read a library item's Markdown note in the vault (frontmatter, the AI summary block and the user's notes).",
        effect: ToolEffect::Read,
        properties: || json!({"library_id": library_id_property()}),
        required: &[],
        document: true,
        describe: |_| "Read the item's note".into(),
    },
    Spec {
        name: "update_library_item",
        description: "Correct a library item's metadata. Only the fields given change, and they are then kept from lookups and the AI pass. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": library_id_property(),
                "title": {"type": "string"},
                "authors": {"type": "array", "items": {"type": "string"}},
                "year": {"type": "integer"},
                "kind": {"type": "string", "enum": ["paper", "book", "other"]},
                "publication": {"type": "string", "description": "The journal, proceedings or repository"},
                "publisher": {"type": "string"},
                "abstract": {"type": "string"},
                "language": {"type": "string"},
                "doi": {"type": "string"},
                "arxiv": {"type": "string"},
                "isbn": {"type": "string"}
            })
        },
        required: &[],
        document: true,
        describe: |_| "Edited the item's details".into(),
    },
    Spec {
        name: "set_item_tags",
        description: "Set a library item's tags (names; missing ones are created). Tags the user added stay whatever you pass; only tags an AI added are removed when left out. Prefer the spellings list_library_vocabulary returns. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": library_id_property(),
                "tags": {"type": "array", "items": {"type": "string"}}
            })
        },
        required: &["tags"],
        document: true,
        describe: |arguments| {
            let tags = string_list(arguments, "tags");
            format!("Tagged it {}", tags.join(", "))
        },
    },
    Spec {
        name: "set_item_collections",
        description: "Replace the collections a library item is in, as paths such as Machine learning/LLMs (missing ones are created). The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": library_id_property(),
                "collections": {"type": "array", "items": {"type": "string"}}
            })
        },
        required: &["collections"],
        document: true,
        describe: |arguments| {
            let collections = string_list(arguments, "collections");
            match collections.as_slice() {
                [] => "Took it out of its collections".into(),
                _ => format!("Filed it in {}", collections.join(", ")),
            }
        },
    },
    Spec {
        name: "add_library_item",
        description: "Add a PDF or EPUB on this computer to the library, linked where it is. Its metadata is looked up and the AI pass runs as the user set them. Returns its library id.",
        effect: ToolEffect::Mutate,
        properties: || json!({"path": {"type": "string", "description": "An absolute path"}}),
        required: &["path"],
        document: false,
        describe: |arguments| format!("Added {} to the library", text_of(arguments, "path")),
    },
    Spec {
        name: "enrich_library_items",
        description: "Queue the library model's pass (summary, key points, tags, collection) for up to 20 items, run again even when it ran before. Returns the estimated tokens; the work happens in the background.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_ids": {"type": "array", "items": {"type": "string"}, "maxItems": 20}
            })
        },
        required: &["library_ids"],
        document: false,
        describe: |arguments| {
            format!(
                "Queued the AI pass for {} items",
                string_list(arguments, "library_ids").len()
            )
        },
    },
    Spec {
        name: "merge_library_items",
        description: "Merge a duplicate into the item it duplicates: `keep` takes the other's files, tags, collections, related items and empty fields, and the other goes. get_library_item names likely duplicates. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "keep": {"type": "string", "description": "The library id that stays (lib_12)"},
                "absorb": {"type": "string", "description": "The library id merged into it"}
            })
        },
        required: &["keep", "absorb"],
        document: false,
        describe: |arguments| {
            format!(
                "Merged {} into {}",
                text_of(arguments, "absorb"),
                text_of(arguments, "keep")
            )
        },
    },
    Spec {
        name: "set_related_items",
        description: "Replace the items related to a library item (library ids; the link goes both ways). The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": library_id_property(),
                "related": {"type": "array", "items": {"type": "string"}}
            })
        },
        required: &["related"],
        document: true,
        describe: |arguments| {
            format!(
                "Linked it to {} related items",
                string_list(arguments, "related").len()
            )
        },
    },
    Spec {
        name: "update_vault_note_block",
        description: "Write your own block in a library item's vault note, such as agent-claude: Markdown placed before the user's notes, between glyphst markers. Only agent- blocks; empty content takes yours out. Refused when the user edited the block. The user can undo it.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "library_id": library_id_property(),
                "block": {"type": "string", "description": "agent- and a short name, such as agent-claude"},
                "content": {"type": "string", "description": "Markdown; empty removes the block"}
            })
        },
        required: &["block", "content"],
        document: true,
        describe: |arguments| format!("Wrote the {} block", text_of(arguments, "block")),
    },
    Spec {
        name: "write_concept_page",
        description: "Ask to write a concept page in the user's vault: a topic researched across library items, every claim citing a passage. The user sees the cost and confirms first; the page is written in the background and appears in Library › pages. Give library_ids to write it from those items, else the library's search finds them.",
        effect: ToolEffect::Mutate,
        properties: || {
            json!({
                "topic": {"type": "string", "description": "The page's title, such as attention mechanisms"},
                "library_ids": {"type": "array", "items": {"type": "string"}, "description": "Items to write it from (lib_12)"}
            })
        },
        required: &["topic"],
        document: false,
        describe: |arguments| format!("Asked to write a page on “{}”", text_of(arguments, "topic")),
    },
];

fn library_id_property() -> Value {
    json!({"type": "string", "description": "The item's library id (lib_12); the document's item when omitted"})
}

/// The strings of an array argument.
pub fn string_list(arguments: &Value, key: &str) -> Vec<String> {
    arguments[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    /// The library tools are in the registry the assistant and the MCP
    /// server list, the edits as changes the user can undo.
    #[test]
    fn the_library_tools_are_offered_with_their_schemas() {
        let ctx = egui::Context::default();
        let (bridge, _calls) = UiBridge::new(&ctx);
        let mut registry = ToolRegistry::new();
        bridge.register(&mut registry);
        for (name, effect) in [
            ("get_library_item", ToolEffect::Read),
            ("list_library", ToolEffect::Read),
            ("list_library_vocabulary", ToolEffect::Read),
            ("read_vault_note", ToolEffect::Read),
            ("update_library_item", ToolEffect::Mutate),
            ("set_item_tags", ToolEffect::Mutate),
            ("set_item_collections", ToolEffect::Mutate),
            ("add_library_item", ToolEffect::Mutate),
            ("enrich_library_items", ToolEffect::Mutate),
            ("merge_library_items", ToolEffect::Mutate),
            ("set_related_items", ToolEffect::Mutate),
            ("update_vault_note_block", ToolEffect::Mutate),
            ("write_concept_page", ToolEffect::Mutate),
        ] {
            let tool = registry
                .get(name)
                .unwrap_or_else(|| panic!("{name} is not offered"));
            assert_eq!(tool.effect(), effect, "{name}");
        }
        let schema = registry
            .get("set_item_tags")
            .unwrap()
            .definition()
            .input_schema;
        assert!(schema["properties"]["library_id"].is_object());
        assert!(schema["properties"]["document_id"].is_object());
        assert_eq!(schema["required"], json!(["tags"]));
        assert_eq!(
            registry.describe("set_item_tags", r#"{"tags": ["nlp", "rag"]}"#),
            "Tagged it nlp, rag"
        );
        assert_eq!(
            registry.describe("set_item_collections", r#"{"collections": []}"#),
            "Took it out of its collections"
        );
        assert!(
            registry
                .get("list_library")
                .unwrap()
                .definition()
                .input_schema["properties"]
                .get("document_id")
                .is_none()
        );
    }

    #[test]
    fn calls_reach_the_app_and_return_its_reply() {
        let ctx = egui::Context::default();
        let (bridge, calls) = UiBridge::new(&ctx);
        let mut registry = ToolRegistry::new();
        bridge.register(&mut registry);
        let names = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"add_highlight".to_string()));
        assert!(names.iter().all(|name| registry.get(name).is_some()));
        let schema = registry
            .get("go_to_page")
            .unwrap()
            .definition()
            .input_schema;
        assert!(schema["properties"]["document_id"].is_object());
        assert_eq!(schema["required"], json!(["page"]));
        assert_eq!(
            registry.describe("go_to_page", r#"{"page": 4}"#),
            "Went to page 4"
        );

        let app = thread::spawn(move || {
            let call = calls.recv().unwrap();
            assert_eq!(call.tool, "go_to_page");
            assert_eq!(call.arguments["page"], 4);
            call.reply
                .send(Ok(UiOutput::from("Showing page 4.".to_string())))
                .unwrap();
        });
        let call = AgentToolCall {
            id: "1".into(),
            name: "go_to_page".into(),
            arguments: r#"{"page": 4}"#.into(),
        };
        let result = registry.execute(&call, &ToolContext::default());
        app.join().unwrap();
        assert!(!result.is_error);
        assert_eq!(
            result.content,
            vec![glyphst_ai::agent::AgentToolResultPart::Text(
                "Showing page 4.".into()
            )]
        );

        let cancel = ToolContext::default();
        cancel.cancel.cancel();
        let canceled = registry.execute(&call, &cancel);
        assert!(canceled.is_error);
    }
}
