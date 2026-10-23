//! The agents' library tools: reading items, their vocabulary and notes,
//! and correcting, tagging, filing, adding and enriching them. Edits go
//! through `record_edits`, so the change log can undo them.

use std::path::{Path, PathBuf};

use glyphst_core::DocumentId;
use glyphst_library::{
    AssignmentOrigin, AssignmentState, AssignmentTarget, LibraryCreator, LibraryCreatorRole,
    LibraryImportOutcome, LibraryItemKind, LibraryItemMetadata, LibraryJobKind, LibraryJobOrigin,
    LibraryStore,
};
use serde_json::Value;

use super::ReaderApp;
use crate::{
    ai_bridge::{UiOutput, UiReply, string_list},
    library_tab::{LibraryTab, View},
};

/// Items `enrich_library_items` queues at most.
const MAX_ENRICH: usize = 20;

impl ReaderApp {
    /// `write_concept_page`: the cost dialog, for the user to confirm.
    fn agent_concept_page(&mut self, arguments: &Value) -> Result<String, String> {
        let topic = arguments["topic"]
            .as_str()
            .map(str::trim)
            .filter(|topic| !topic.is_empty())
            .ok_or("topic must not be empty")?
            .to_string();
        let items = string_list(arguments, "library_ids")
            .iter()
            .filter_map(|id| id.trim().trim_start_matches("lib_").parse().ok())
            .map(DocumentId)
            .filter(|id| self.library.library_item(*id).is_some())
            .collect::<Vec<_>>();
        let ask = if items.is_empty() {
            crate::knowledge::PageAsk::Asked(topic.clone())
        } else {
            crate::knowledge::PageAsk::Topic(crate::knowledge::TopicSuggestion {
                key: format!("asked:{topic}"),
                title: topic.clone(),
                kind: crate::knowledge::TopicKind::Cluster,
                items,
            })
        };
        self.library_dialog = Some(self.page_dialog(&ask)?);
        Ok(format!(
            "Showed the user the cost of a page on “{topic}”. If they confirm, it is written in the background and appears in Library › pages; it is not written yet."
        ))
    }

    /// Answers a library tool, or `None` when `tool` is not one.
    pub(super) fn agent_library_call(&mut self, tool: &str, arguments: &Value) -> Option<UiReply> {
        let vault = self
            .prefs
            .library
            .vault_root(self.library.files_root(), self.library.files_root_chosen());
        let text = |result: Result<String, String>| result.map(UiOutput::from);
        let item = |app: &Self| app.agent_item_id(arguments);
        Some(match tool {
            "get_library_item" => {
                text(item(self).map(|id| item_details(&self.library, vault.as_deref(), id)))
            }
            "list_library" => text(list(&self.library, arguments)),
            "list_library_vocabulary" => Ok(UiOutput::from(vocabulary(&self.library))),
            "read_vault_note" => {
                text(item(self).and_then(|id| read_note(&self.library, vault.as_deref(), id)))
            }
            "update_library_item" => {
                item(self).and_then(|id| update_item(&mut self.library, id, arguments))
            }
            "set_item_tags" => item(self).and_then(|id| set_tags(&mut self.library, id, arguments)),
            "set_item_collections" => {
                item(self).and_then(|id| set_collections(&mut self.library, id, arguments))
            }
            "add_library_item" => add_item(&mut self.library, arguments),
            "merge_library_items" => merge(&mut self.library, arguments),
            "set_related_items" => {
                item(self).and_then(|id| set_related(&mut self.library, id, arguments))
            }
            "write_concept_page" => self.agent_concept_page(arguments).map(UiOutput::from),
            "update_vault_note_block" => item(self).and_then(|id| {
                if self.vault_sync.in_flight(id) {
                    return Err("Glyphst is writing that note now; try again in a moment".into());
                }
                let path = note_path(&self.library, vault.as_deref(), id)
                    .ok_or("the item has no note in the vault yet")?;
                let edit = crate::vault_blocks::agent_write(
                    &mut self.library,
                    id,
                    &path,
                    arguments["block"].as_str().unwrap_or_default(),
                    arguments["content"].as_str().unwrap_or_default(),
                )?;
                Ok(UiOutput {
                    text: match &edit.after {
                        Some(_) => format!("Wrote the {} block in {}.", edit.name, path.display()),
                        None => format!("Took the {} block out.", edit.name),
                    },
                    vault: vec![edit],
                    ..UiOutput::default()
                })
            }),
            "enrich_library_items" => {
                let model = self
                    .user_settings
                    .get()
                    .and_then(|settings| settings.ai.resolved_library())
                    .map(|model| model.model);
                enrich(&mut self.library, model.as_deref(), arguments)
            }
            _ => return None,
        })
    }

    /// `open_at`: a passage, or an item at a page.
    pub(super) fn agent_open_at(
        &mut self,
        arguments: &Value,
        ctx: &egui::Context,
    ) -> Result<String, String> {
        let chunk = arguments["chunk_id"]
            .as_str()
            .and_then(|id| id.trim().trim_start_matches("c_").parse().ok())
            .or_else(|| arguments["chunk_id"].as_i64());
        let item = match arguments["library_id"].as_str().filter(|id| !id.is_empty()) {
            Some(id) => Some(super::agent::parse_library_id(id)?.0),
            None => None,
        };
        if chunk.is_none() && item.is_none() {
            return Err("give a library_id or a chunk_id".into());
        }
        self.open_link(
            crate::links::OpenLink {
                item,
                uid: None,
                page: arguments["page"].as_u64().map(|page| page as usize),
                chunk,
            },
            ctx,
        )
    }

    /// The item a call names with `library_id`, else the document's.
    pub(super) fn agent_item_id(&self, arguments: &Value) -> Result<DocumentId, String> {
        let id = match arguments["library_id"].as_str().filter(|id| !id.is_empty()) {
            // A merged item's id finds the item it went into.
            Some(id) => self
                .library
                .resolve_item_id(super::agent::parse_library_id(id)?),
            None => {
                let index = self.agent_tab(arguments)?;
                return self
                    .library
                    .library_item_for_path(&self.tabs[index].path)
                    .map(|item| item.id)
                    .ok_or_else(|| "the document is not in the library".to_string());
            }
        };
        self.library
            .library_item(id)
            .map(|item| item.id)
            .ok_or_else(|| format!("lib_{} is not in the library; call list_library", id.0))
    }
}

/// Everything about an item, for `get_library_item`.
pub fn item_details(library: &LibraryStore, vault: Option<&Path>, id: DocumentId) -> String {
    let Some(item) = library.library_item(id) else {
        return format!("lib_{} is gone", id.0);
    };
    let mut lines = vec![super::agent::describe_item(item)];
    let kind = match item.kind {
        LibraryItemKind::Paper => "paper",
        LibraryItemKind::Book => "book",
        LibraryItemKind::Other => "other",
    };
    lines.push(format!("Kind: {kind}"));
    for (label, value) in [
        ("Publication", &item.publication),
        ("Publisher", &item.publisher),
        ("Language", &item.language),
        ("Abstract", &item.abstract_text),
    ] {
        if !value.is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    let ids = &item.identifiers;
    for (label, value) in [
        ("DOI", &ids.doi),
        ("arXiv", &ids.arxiv),
        ("ISBN", &ids.isbn),
    ] {
        if let Some(value) = value {
            lines.push(format!("{label}: {value}"));
        }
    }
    if let Some(record) = library.metadata_record(id) {
        let mut source = format!("Metadata from: {}", record.source.label());
        if let Some(matched) = &record.matched_identifier {
            source.push_str(&format!(" ({matched})"));
        }
        if !record.locked_fields.is_empty() {
            source.push_str(&format!(
                "; edited by the user: {}",
                record
                    .locked_fields
                    .iter()
                    .map(|field| format!("{field:?}").to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        lines.push(source);
    }
    let assignments = library.ai_assignments(id);
    let mark = |target: &AssignmentTarget| {
        let assignment = assignments
            .iter()
            .find(|assignment| &assignment.target == target)?;
        Some(match (&assignment.origin, assignment.state) {
            (AssignmentOrigin::Import { source }, _) => format!(" (from {source})"),
            (_, AssignmentState::Accepted) => " (the user's, first added by an ai)".to_string(),
            (_, AssignmentState::Applied) => " (added by an ai)".to_string(),
            (_, AssignmentState::Suggested) => " (an ai's suggestion)".to_string(),
            _ => String::new(),
        })
    };
    let tags = library
        .tags_for_item(id)
        .into_iter()
        .map(|tag| {
            format!(
                "{}{}",
                tag.name,
                mark(&AssignmentTarget::Tag(tag.id)).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!(
        "Tags: {}",
        if tags.is_empty() {
            "none".to_string()
        } else {
            tags.join(", ")
        }
    ));
    let collections = library
        .collections_for_item(id)
        .into_iter()
        .map(|collection| {
            format!(
                "{}{}",
                library.collection_path_label(collection.id),
                mark(&AssignmentTarget::Collection(collection.id)).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    lines.push(format!(
        "Collections: {}",
        if collections.is_empty() {
            "none (unfiled)".to_string()
        } else {
            collections.join(", ")
        }
    ));
    for assignment in assignments {
        if assignment.state != AssignmentState::Suggested {
            continue;
        }
        match &assignment.target {
            AssignmentTarget::NewCollection(path) => {
                lines.push(format!("Suggested new collection: {}", path.join("/")));
            }
            AssignmentTarget::Collection(_) => {
                lines.push(format!("Suggested collection: {}", assignment.label));
            }
            AssignmentTarget::Tag(_) => {}
        }
    }
    if let Some(enrichment) = library.item_enrichment(id)
        && !enrichment.tldr.is_empty()
    {
        lines.push(format!(
            "AI summary ({}): {}",
            enrichment.model, enrichment.tldr
        ));
        for point in &enrichment.key_points {
            lines.push(format!("- {point}"));
        }
    }
    let files = library
        .library_attachments()
        .iter()
        .filter(|attachment| attachment.item_id == id)
        .map(|attachment| {
            format!(
                "{}{}",
                attachment.path.display(),
                if attachment.path.is_file() {
                    ""
                } else {
                    " (missing)"
                }
            )
        })
        .collect::<Vec<_>>();
    match files.as_slice() {
        [] => lines.push("File: none".to_string()),
        [file] => lines.push(format!("File: {file}")),
        files => lines.push(format!("Files: {}", files.join(", "))),
    }
    let related = library
        .related_items(id)
        .into_iter()
        .filter_map(|other| library.library_item(other))
        .map(super::agent::describe_item)
        .collect::<Vec<_>>();
    if !related.is_empty() {
        lines.push(format!("Related: {}", related.join("; ")));
    }
    if let Some(other) = library
        .metadata_record(id)
        .and_then(|record| record.possible_duplicate_of)
        .and_then(|other| library.library_item(other))
    {
        lines.push(format!(
            "Possibly the same work as: {} (merge_library_items can merge them)",
            super::agent::describe_item(other)
        ));
    }
    if let Some(note) = note_path(library, vault, id) {
        lines.push(format!("Note: {}", note.display()));
    }
    if !item.work_note.is_empty() {
        lines.push(format!("Notes:\n{}", item.work_note));
    }
    lines.join("\n")
}

/// `list_library`.
pub fn list(library: &LibraryStore, arguments: &Value) -> Result<String, String> {
    let mut tab = LibraryTab::default();
    tab.view = match arguments["view"].as_str().unwrap_or("all") {
        "all" => View::All,
        "recent" => View::Recent,
        "unfiled" => View::Unfiled,
        "missing" => View::Missing,
        "trash" => View::Trash,
        other => return Err(format!("{other} is not a view")),
    };
    if let Some(path) = arguments["collection"]
        .as_str()
        .filter(|path| !path.is_empty())
    {
        let wanted = path.trim_matches('/').to_lowercase();
        let collection = library
            .library_collections()
            .iter()
            .find(|collection| {
                library.collection_path_label(collection.id).to_lowercase() == wanted
            })
            .ok_or_else(|| {
                format!("no collection is called {path}; call list_library_vocabulary")
            })?;
        tab.view = View::Collection(collection.id);
    }
    tab.filter = arguments["query"].as_str().unwrap_or_default().to_string();
    let ids = tab.query(library);
    let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
    let limit = arguments["limit"].as_u64().unwrap_or(30).clamp(1, 100) as usize;
    let lines = ids
        .iter()
        .skip(offset)
        .take(limit)
        .filter_map(|id| library.library_item(*id))
        .map(|item| {
            let tags = library
                .tags_for_item(item.id)
                .into_iter()
                .map(|tag| format!("#{}", tag.name))
                .collect::<Vec<_>>();
            let mut line = format!("- {}", super::agent::describe_item(item));
            if !tags.is_empty() {
                line.push_str(&format!(" {}", tags.join(" ")));
            }
            if library.item_needs_review(item.id) {
                line.push_str(" [ai to review]");
            }
            line
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok("No items match.".to_string());
    }
    let shown = offset + lines.len();
    let mut text = lines.join("\n");
    if shown < ids.len() {
        text.push_str(&format!(
            "\n({shown} of {}; ask again with offset {shown} for more)",
            ids.len()
        ));
    }
    Ok(text)
}

/// The tags and collection paths with their counts.
pub fn vocabulary(library: &LibraryStore) -> String {
    let mut tags = library
        .library_tags()
        .iter()
        .map(|tag| {
            let count = library
                .library_items()
                .iter()
                .filter(|item| item.trashed_at.is_none())
                .filter(|item| {
                    library
                        .tags_for_item(item.id)
                        .iter()
                        .any(|candidate| candidate.id == tag.id)
                })
                .count();
            (tag.name.clone(), count)
        })
        .collect::<Vec<_>>();
    tags.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
    let mut collections = library
        .library_collections()
        .iter()
        .map(|collection| {
            // Counted with the collections under it, as the tree shows.
            let label = library.collection_path_label(collection.id);
            let within = format!("{label}/");
            let count = library
                .library_items()
                .iter()
                .filter(|item| item.trashed_at.is_none())
                .filter(|item| {
                    library
                        .collections_for_item(item.id)
                        .iter()
                        .any(|candidate| {
                            let path = library.collection_path_label(candidate.id);
                            path == label || path.starts_with(&within)
                        })
                })
                .count();
            (label, count)
        })
        .collect::<Vec<_>>();
    collections.sort();
    let list = |entries: &[(String, usize)]| {
        if entries.is_empty() {
            "none".to_string()
        } else {
            entries
                .iter()
                .map(|(name, count)| format!("{name} ({count})"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    format!("Tags: {}\nCollections: {}", list(&tags), list(&collections))
}

fn note_path(library: &LibraryStore, vault: Option<&Path>, id: DocumentId) -> Option<PathBuf> {
    let record = library.vault_note(id)?;
    Some(vault?.join(&record.relative_path))
}

pub fn read_note(
    library: &LibraryStore,
    vault: Option<&Path>,
    id: DocumentId,
) -> Result<String, String> {
    let path = note_path(library, vault, id).ok_or("the item has no note in the vault yet")?;
    std::fs::read_to_string(&path).map_err(|error| format!("could not read the note: {error}"))
}

/// `update_library_item`: the fields given change, and are then locked.
pub fn update_item(library: &mut LibraryStore, id: DocumentId, arguments: &Value) -> UiReply {
    let mut item = library
        .library_item(id)
        .cloned()
        .ok_or("the item is gone")?;
    let text = |key: &str| {
        arguments[key]
            .as_str()
            .map(|value| value.trim().to_string())
    };
    let optional = |key: &str| text(key).map(|value| Some(value).filter(|v| !v.is_empty()));
    if let Some(title) = text("title").filter(|title| !title.is_empty()) {
        item.title = title;
    }
    if arguments["authors"].is_array() {
        item.creators = string_list(arguments, "authors")
            .into_iter()
            .filter(|name| !name.trim().is_empty())
            .map(|name| LibraryCreator {
                name: name.trim().to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
    }
    if let Some(year) = arguments["year"].as_i64() {
        item.year = i32::try_from(year).ok().filter(|year| *year > 0);
    }
    if let Some(kind) = arguments["kind"].as_str() {
        item.kind = match kind {
            "paper" => LibraryItemKind::Paper,
            "book" => LibraryItemKind::Book,
            "other" => LibraryItemKind::Other,
            _ => return Err("kind must be paper, book or other".into()),
        };
    }
    for (key, field) in [
        ("publication", &mut item.publication),
        ("publisher", &mut item.publisher),
        ("abstract", &mut item.abstract_text),
        ("language", &mut item.language),
    ] {
        if let Some(value) = text(key) {
            *field = value;
        }
    }
    if let Some(doi) = optional("doi") {
        item.identifiers.doi = doi.map(|doi| doi.to_lowercase());
    }
    if let Some(arxiv) = optional("arxiv") {
        item.identifiers.arxiv = arxiv;
    }
    if let Some(isbn) = optional("isbn") {
        item.identifiers.isbn = match isbn {
            Some(isbn) => Some(
                glyphst_metadata::identifiers::normalize_isbn(&isbn)
                    .ok_or_else(|| format!("{isbn} is not a valid ISBN"))?,
            ),
            None => None,
        };
    }
    let title = item.title.clone();
    let (result, edits) = library.record_edits(|library| library.edit_library_item_metadata(item));
    result.map_err(|error| format!("could not save: {error:#}"))?;
    Ok(UiOutput {
        text: if edits.is_empty() {
            format!("\"{title}\" already had those details.")
        } else {
            format!("Updated \"{title}\"; those fields stay as set.")
        },
        edits,
        ..UiOutput::default()
    })
}

pub fn set_tags(library: &mut LibraryStore, id: DocumentId, arguments: &Value) -> UiReply {
    let tags = string_list(arguments, "tags")
        .into_iter()
        .map(|tag| tag.trim().trim_start_matches('#').to_string())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    // The user's own tags stay whatever the assistant asks.
    let (result, edits) = library.record_edits(|library| {
        library.set_item_tags_by_ai(
            id,
            &tags,
            glyphst_library::AssignmentOrigin::Agent {
                name: "assistant".to_string(),
            },
        )
    });
    let done = result.map_err(|error| format!("could not save: {error:#}"))?;
    let mut text = match (done.added.as_slice(), done.removed.as_slice()) {
        ([], []) => "Its tags were already so.".to_string(),
        (added, []) => format!("Tagged it {}.", added.join(", ")),
        ([], removed) => format!("Took off {}.", removed.join(", ")),
        (added, removed) => format!(
            "Tagged it {}; took off {}.",
            added.join(", "),
            removed.join(", ")
        ),
    };
    if !done.kept.is_empty() {
        text.push_str(&format!(
            " Kept {}: the user's own tags, which only the user changes.",
            done.kept.join(", ")
        ));
    }
    Ok(UiOutput {
        text,
        edits,
        ..UiOutput::default()
    })
}

pub fn set_collections(library: &mut LibraryStore, id: DocumentId, arguments: &Value) -> UiReply {
    let paths = string_list(arguments, "collections")
        .iter()
        .map(|path| {
            path.split('/')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    let (result, edits) =
        library.record_edits(|library| library.set_item_collection_paths(id, &paths));
    result.map_err(|error| format!("could not save: {error:#}"))?;
    Ok(UiOutput {
        text: match paths.as_slice() {
            [] => "It is in no collection now.".to_string(),
            _ => format!(
                "Filed it in {}.",
                paths
                    .iter()
                    .map(|path| path.join("/"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
        edits,
        ..UiOutput::default()
    })
}

/// `add_library_item`: linked where it is, its metadata then looked up.
pub fn add_item(library: &mut LibraryStore, arguments: &Value) -> UiReply {
    let path = Path::new(arguments["path"].as_str().unwrap_or_default().trim());
    if !path.is_absolute() {
        return Err("path must be absolute".into());
    }
    if !path.is_file() || !crate::import::is_library_document(path) {
        return Err(format!("{} is not a PDF or EPUB", path.display()));
    }
    let outcome = library
        .link_item(path, LibraryItemMetadata::for_path(path), None)
        .map_err(|error| format!("could not add it: {error:#}"))?;
    let id = match outcome {
        LibraryImportOutcome::Added(id) => id,
        LibraryImportOutcome::Duplicate(id) | LibraryImportOutcome::DuplicateInTrash(id) => {
            return Ok(UiOutput::from(format!(
                "It is in the library already as lib_{}.",
                id.0
            )));
        }
    };
    // Its metadata is looked up, and the AI pass follows as set.
    library
        .enqueue_library_jobs(
            &[id],
            LibraryJobKind::Metadata,
            LibraryJobOrigin::Agent,
            false,
        )
        .map_err(|error| format!("added as lib_{}, but: {error:#}", id.0))?;
    Ok(UiOutput::from(format!(
        "Added it as lib_{}; its details are being looked up.",
        id.0
    )))
}

/// `merge_library_items`.
pub fn merge(library: &mut LibraryStore, arguments: &Value) -> UiReply {
    let id = |key: &str| {
        let id = super::agent::parse_library_id(arguments[key].as_str().unwrap_or_default())?;
        let id = library.resolve_item_id(id);
        library
            .library_item(id)
            .map(|item| (item.id, item.title.clone()))
            .ok_or_else(|| format!("lib_{} is not in the library", id.0))
    };
    let (keep, title) = id("keep")?;
    let (absorb, absorbed) = id("absorb")?;
    let (result, edits) = library.record_edits(|library| library.merge_library_items(keep, absorb));
    result.map_err(|error| format!("could not merge them: {error:#}"))?;
    Ok(UiOutput {
        text: format!(
            "Merged \"{absorbed}\" (lib_{}) into \"{title}\" (lib_{}); its files, tags, collections and links are lib_{}'s now.",
            absorb.0, keep.0, keep.0
        ),
        edits,
        ..UiOutput::default()
    })
}

/// `set_related_items`.
pub fn set_related(library: &mut LibraryStore, id: DocumentId, arguments: &Value) -> UiReply {
    let related = string_list(arguments, "related")
        .iter()
        .map(|other| {
            super::agent::parse_library_id(other).map(|other| library.resolve_item_id(other))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (result, edits) = library.record_edits(|library| library.set_related_items(id, &related));
    result.map_err(|error| format!("could not save: {error:#}"))?;
    Ok(UiOutput {
        text: match related.len() {
            0 => "It has no related items now.".to_string(),
            count => format!("Linked it to {count} related items."),
        },
        edits,
        ..UiOutput::default()
    })
}

/// `enrich_library_items`, with the library model `model`.
pub fn enrich(library: &mut LibraryStore, model: Option<&str>, arguments: &Value) -> UiReply {
    let ids = string_list(arguments, "library_ids")
        .iter()
        .map(|id| super::agent::parse_library_id(id))
        .collect::<Result<Vec<_>, _>>()?;
    if ids.is_empty() {
        return Err("library_ids must name at least one item".into());
    }
    if ids.len() > MAX_ENRICH {
        return Err(format!("at most {MAX_ENRICH} items a call"));
    }
    let model = model.ok_or("no library model is set (Settings › AI)")?;
    let items = ids
        .iter()
        .map(|id| {
            library
                .library_item(*id)
                .ok_or_else(|| format!("lib_{} is not in the library", id.0))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (tokens_in, tokens_out) = crate::library_jobs::enrich_estimate(library, &items);
    library
        .enqueue_library_jobs(&ids, LibraryJobKind::Enrich, LibraryJobOrigin::Agent, true)
        .map_err(|error| format!("could not queue them: {error:#}"))?;
    Ok(UiOutput::from(format!(
        "Queued {} items for {} (about {tokens_in} tokens in, {tokens_out} out). Their tags and summaries arrive in the background; read them with get_library_item.",
        ids.len(),
        model
    )))
}

#[cfg(test)]
mod tests {
    use glyphst_library::{EnrichmentApply, VaultNoteRecord};
    use serde_json::json;

    use super::*;

    /// A memory library with two linked papers, one tagged and filed, with
    /// an AI tag to review; and the folder holding them.
    fn library(name: &str) -> (LibraryStore, PathBuf, DocumentId, DocumentId) {
        let folder = std::env::temp_dir().join(format!(
            "glyphst-agent-library-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let mut library = LibraryStore::memory();
        let mut add = |file: &str, title: &str| {
            let path = folder.join(file);
            std::fs::write(&path, format!("%PDF-1.4\n% {title}\n")).unwrap();
            let LibraryImportOutcome::Added(id) = library
                .link_item(&path, LibraryItemMetadata::for_path(&path), None)
                .unwrap()
            else {
                panic!("expected {file} to be added");
            };
            let mut item = library.library_item(id).unwrap().clone();
            item.title = title.to_string();
            library.update_library_item_metadata(item).unwrap();
            id
        };
        let attention = add("attention.pdf", "Attention Is All You Need");
        let bert = add("bert.pdf", "BERT");
        library
            .set_item_tag_names(attention, &["transformers".to_string()])
            .unwrap();
        library
            .set_item_collection_paths(
                attention,
                &[vec!["Machine learning".to_string(), "LLMs".to_string()]],
            )
            .unwrap();
        let run = library.next_enrichment_run();
        library
            .apply_enrichment(
                attention,
                EnrichmentApply {
                    run,
                    tags: vec!["attention".to_string()],
                    create_tags: true,
                    collections: Vec::new(),
                    suggested_collection: Some(vec!["NLP".to_string()]),
                    metadata: None,
                    enrichment: None,
                },
            )
            .unwrap();
        (library, folder, attention, bert)
    }

    #[test]
    fn the_read_tools_describe_items_lists_and_the_vocabulary() {
        let (mut library, folder, attention, bert) = library("read");
        let details = item_details(&library, None, attention);
        assert!(
            details.contains("lib_1 \"Attention Is All You Need\""),
            "{details}"
        );
        assert!(
            details.contains("Tags: attention (added by an ai), transformers"),
            "{details}"
        );
        assert!(
            details.contains("Collections: Machine learning/LLMs"),
            "{details}"
        );
        assert!(
            details.contains("Suggested new collection: NLP"),
            "{details}"
        );
        assert!(details.contains("attention.pdf"), "{details}");

        let all = list(&library, &json!({})).unwrap();
        assert!(
            all.contains("#transformers") && all.contains("[ai to review]"),
            "{all}"
        );
        assert!(all.contains("BERT"), "{all}");
        let filed = list(&library, &json!({"collection": "machine learning/llms"})).unwrap();
        assert!(
            filed.contains("Attention") && !filed.contains("BERT"),
            "{filed}"
        );
        let paged = list(&library, &json!({"limit": 1})).unwrap();
        assert!(
            paged.contains("(1 of 2; ask again with offset 1"),
            "{paged}"
        );
        assert!(list(&library, &json!({"collection": "nope"})).is_err());
        assert!(list(&library, &json!({"view": "nope"})).is_err());
        let unfiled = list(&library, &json!({"view": "unfiled"})).unwrap();
        assert!(
            unfiled.contains("BERT") && !unfiled.contains("Attention"),
            "{unfiled}"
        );

        let words = vocabulary(&library);
        assert!(
            words.contains("attention (1)") && words.contains("transformers (1)"),
            "{words}"
        );
        assert!(
            words.contains("Machine learning (1)") && words.contains("Machine learning/LLMs (1)"),
            "{words}"
        );

        assert!(
            read_note(&library, Some(&folder), bert).is_err(),
            "no note yet"
        );
        std::fs::create_dir_all(folder.join("items")).unwrap();
        std::fs::write(folder.join("items/bert.md"), "# BERT\n").unwrap();
        library
            .set_vault_note(
                bert,
                Some(VaultNoteRecord {
                    relative_path: "items/bert.md".to_string(),
                    ..VaultNoteRecord::default()
                }),
            )
            .unwrap();
        assert_eq!(
            read_note(&library, Some(&folder), bert).unwrap(),
            "# BERT\n"
        );
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn the_edit_tools_record_what_they_change() {
        let (mut library, folder, attention, bert) = library("edit");
        let output = update_item(
            &mut library,
            bert,
            &json!({"title": "BERT: Pre-training", "authors": ["Jacob Devlin"], "year": 2018, "isbn": ""}),
        )
        .unwrap();
        let item = library.library_item(bert).unwrap();
        assert_eq!(item.title, "BERT: Pre-training");
        assert_eq!(item.creators[0].name, "Jacob Devlin");
        assert_eq!(item.year, Some(2018));
        assert_eq!(output.edits.len(), 1);
        assert!(
            !library
                .metadata_record(bert)
                .unwrap()
                .locked_fields
                .is_empty(),
            "what an agent set stays"
        );
        assert!(update_item(&mut library, bert, &json!({"isbn": "123"})).is_err());
        assert!(update_item(&mut library, bert, &json!({"kind": "poem"})).is_err());
        library.revert_edit(&output.edits[0]).unwrap();
        assert_eq!(library.library_item(bert).unwrap().title, "BERT");

        let output = set_tags(
            &mut library,
            bert,
            &json!({"tags": ["#nlp", " ", "Transformers"]}),
        )
        .unwrap();
        assert_eq!(output.text, "Tagged it nlp, Transformers.");
        let names = library
            .tags_for_item(bert)
            .into_iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(
            names.contains(&"transformers".to_string()),
            "an existing tag is reused"
        );
        assert_eq!(output.edits.len(), 1);

        // The assistant takes an AI's tag off, and cannot take the
        // user's.
        let output = set_tags(&mut library, attention, &json!({"tags": []})).unwrap();
        assert_eq!(
            output.text,
            "Took off attention. Kept transformers: the user's own tags, which only the user changes."
        );
        let names = library
            .tags_for_item(attention)
            .into_iter()
            .map(|tag| tag.normalized_name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["transformers".to_string()]);

        let output = set_collections(
            &mut library,
            bert,
            &json!({"collections": ["Machine learning / NLP", "Reading"]}),
        )
        .unwrap();
        assert_eq!(output.text, "Filed it in Machine learning/NLP, Reading.");
        assert_eq!(output.edits.len(), 1);
        assert_eq!(library.collections_for_item(bert).len(), 2);
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn agents_relate_and_merge_items_and_the_merged_id_still_answers() {
        let (mut library, folder, attention, bert) = library("merge");
        let output = set_related(
            &mut library,
            attention,
            &json!({"related": [format!("lib_{}", bert.0)]}),
        )
        .unwrap();
        assert_eq!(output.text, "Linked it to 1 related items.");
        assert_eq!(output.edits.len(), 1);
        assert_eq!(library.related_items(bert), vec![attention]);
        assert!(item_details(&library, None, attention).contains("Related: "));
        library.revert_edit(&output.edits[0]).unwrap();
        assert!(library.related_items(attention).is_empty());

        assert!(
            merge(&mut library, &json!({"keep": "lib_1", "absorb": "lib_1"})).is_err(),
            "not into itself"
        );
        let output = merge(
            &mut library,
            &json!({"keep": format!("lib_{}", attention.0), "absorb": format!("lib_{}", bert.0)}),
        )
        .unwrap();
        assert_eq!(output.edits.len(), 1);
        assert!(library.library_item(bert).is_none());
        assert_eq!(library.resolve_item_id(bert), attention);
        assert!(item_details(&library, None, attention).contains("Files: "));
        library.revert_edit(&output.edits[0]).unwrap();
        assert!(library.library_item(bert).is_some());
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn adding_and_enriching_queue_background_work() {
        let (mut library, folder, attention, bert) = library("add");
        assert!(add_item(&mut library, &json!({"path": "relative.pdf"})).is_err());
        let text_file = folder.join("notes.txt");
        std::fs::write(&text_file, "hi").unwrap();
        assert!(add_item(&mut library, &json!({"path": text_file})).is_err());
        let new = folder.join("ddpm.pdf");
        std::fs::write(&new, "%PDF-1.4\n% ddpm\n").unwrap();
        let output = add_item(&mut library, &json!({"path": new})).unwrap();
        assert!(
            output.text.starts_with("Added it as lib_3"),
            "{}",
            output.text
        );
        assert!(library.library_jobs().iter().any(|job| {
            job.item_id == DocumentId(3)
                && job.kind == LibraryJobKind::Metadata
                && job.origin == LibraryJobOrigin::Agent
        }));
        let again = add_item(&mut library, &json!({"path": new})).unwrap();
        assert_eq!(again.text, "It is in the library already as lib_3.");

        let ids =
            json!({"library_ids": [format!("lib_{}", attention.0), format!("lib_{}", bert.0)]});
        assert!(
            enrich(&mut library, None, &ids).is_err(),
            "no model, no pass"
        );
        let many = json!({"library_ids": (0..21).map(|n| format!("lib_{n}")).collect::<Vec<_>>()});
        assert!(enrich(&mut library, Some("small"), &many).is_err());
        let output = enrich(&mut library, Some("small"), &ids).unwrap();
        assert!(
            output.text.starts_with("Queued 2 items for small"),
            "{}",
            output.text
        );
        let queued = library
            .library_jobs()
            .iter()
            .filter(|job| job.kind == LibraryJobKind::Enrich)
            .count();
        assert_eq!(queued, 2);
        let _ = std::fs::remove_dir_all(folder);
    }
}
