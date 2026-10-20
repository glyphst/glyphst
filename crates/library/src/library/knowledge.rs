//! The knowledge layer's records: concept pages Glyphst wrote into the
//! vault, related items suggested from meaning search (and what the user
//! did with them), and topics the user dismissed. Each is a row of its
//! own, written alone, outside the catalog's revision.

use std::collections::{BTreeMap, BTreeSet};

use super::storage_helpers::{decode, encode};
use super::*;

/// A concept page by id.
pub(super) const LIBRARY_PAGES_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_pages_v1");
/// Under the lower item id of each pair: related items suggested for it.
pub(super) const LIBRARY_RELATED_SUGGESTIONS_V1_TABLE: TableDefinition<u64, &[u8]> =
    TableDefinition::new("library_related_suggestions_v1");
/// Suggested page topics the user dismissed, by key, with when.
pub(super) const LIBRARY_PAGE_TOPICS_V1_TABLE: TableDefinition<&str, u64> =
    TableDefinition::new("library_page_topics_v1");
const KNOWLEDGE_STORAGE_VERSION: u8 = 1;

/// What a page was written about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageTopic {
    /// A tag's items.
    Tag(LibraryTagId),
    /// Items found close in meaning.
    Items(Vec<DocumentId>),
    /// What the user (or the assistant) asked for.
    Asked(String),
}

/// A concept page and what Glyphst last wrote there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRecord {
    pub id: u64,
    pub title: String,
    /// Relative to the vault, with `/`.
    pub relative_path: String,
    pub topic: PageTopic,
    /// The items it was written from.
    pub sources: Vec<DocumentId>,
    pub frontmatter_sha256: [u8; 32],
    pub blocks: Vec<(String, [u8; 32])>,
    /// The file's modification time after the last write (seconds).
    pub modified: u64,
    pub written_at: u64,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// The page's text before the last write, to undo it; `None` for a
    /// new page.
    pub previous: Option<String>,
    /// Written and not yet kept or undone by the user.
    pub unreviewed: bool,
}

/// A related item suggested for another.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RelatedSuggestion {
    pub other: DocumentId,
    /// How close the two are in meaning (cosine of their mean vectors).
    pub score: f32,
    /// `Suggested` until the user keeps it (`Accepted`) or not
    /// (`Rejected`, never suggested again).
    pub state: AssignmentState,
    pub at: u64,
}

#[derive(Serialize, Deserialize)]
struct StoredPageV1 {
    version: u8,
    page: PageRecord,
}

#[derive(Serialize, Deserialize)]
struct StoredSuggestionsV1 {
    version: u8,
    suggestions: Vec<RelatedSuggestion>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct KnowledgeState {
    pub(super) pages: BTreeMap<u64, PageRecord>,
    /// Under the pair's lower id.
    pub(super) suggestions: BTreeMap<u64, Vec<RelatedSuggestion>>,
    pub(super) dismissed: BTreeSet<String>,
}

pub(super) fn ensure_knowledge_tables(write_txn: &redb::WriteTransaction) -> Result<()> {
    write_txn.open_table(LIBRARY_PAGES_V1_TABLE)?;
    write_txn.open_table(LIBRARY_RELATED_SUGGESTIONS_V1_TABLE)?;
    write_txn.open_table(LIBRARY_PAGE_TOPICS_V1_TABLE)?;
    Ok(())
}

pub(super) fn load_knowledge(database: &Database) -> Result<KnowledgeState> {
    let read_txn = database.begin_read()?;
    let mut state = KnowledgeState::default();
    {
        let table = read_txn.open_table(LIBRARY_PAGES_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredPageV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, KNOWLEDGE_STORAGE_VERSION)?;
                Ok(stored.page)
            }) {
                Ok(page) => {
                    state.pages.insert(id.value(), page);
                }
                Err(error) => eprintln!("failed to decode a page: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_RELATED_SUGGESTIONS_V1_TABLE)?;
        for entry in table.iter()? {
            let (id, value) = entry?;
            match decode::<StoredSuggestionsV1>(value.value()).and_then(|stored| {
                require_storage_version(stored.version, KNOWLEDGE_STORAGE_VERSION)?;
                Ok(stored.suggestions)
            }) {
                Ok(suggestions) => {
                    state.suggestions.insert(id.value(), suggestions);
                }
                Err(error) => eprintln!("failed to decode related suggestions: {error:#}"),
            }
        }
    }
    {
        let table = read_txn.open_table(LIBRARY_PAGE_TOPICS_V1_TABLE)?;
        for entry in table.iter()? {
            let (key, _) = entry?;
            state.dismissed.insert(key.value().to_string());
        }
    }
    Ok(state)
}

impl LibraryStore {
    pub fn pages(&self) -> impl Iterator<Item = &PageRecord> {
        self.knowledge.pages.values()
    }

    pub fn page(&self, id: u64) -> Option<&PageRecord> {
        self.knowledge.pages.get(&id)
    }

    /// The next id for a page: never one given out before.
    pub fn next_page_id(&self) -> u64 {
        self.knowledge
            .pages
            .keys()
            .next_back()
            .map_or(1, |id| id + 1)
            .max(self.sync_ids.highest(SyncKind::Page) + 1)
    }

    /// Records a page, or forgets it with `None`.
    pub fn set_page(&mut self, id: u64, page: Option<PageRecord>) -> Result<()> {
        let previous = match &page {
            Some(page) => self.knowledge.pages.insert(id, page.clone()),
            None => self.knowledge.pages.remove(&id),
        };
        let result = self.write_knowledge(|write_txn| {
            let mut table = write_txn.open_table(LIBRARY_PAGES_V1_TABLE)?;
            match &page {
                Some(page) => {
                    let encoded = encode(&StoredPageV1 {
                        version: KNOWLEDGE_STORAGE_VERSION,
                        page: page.clone(),
                    })?;
                    table.insert(id, encoded.as_slice())?;
                }
                None => {
                    table.remove(id)?;
                }
            }
            Ok(())
        });
        if result.is_err() {
            match previous {
                Some(previous) => self.knowledge.pages.insert(id, previous),
                None => self.knowledge.pages.remove(&id),
            };
        }
        result
    }

    /// Related items suggested for `id` and not yet kept or rejected,
    /// closest first.
    pub fn related_suggestions(&self, id: DocumentId) -> Vec<(DocumentId, f32)> {
        let mut found = self
            .knowledge
            .suggestions
            .iter()
            .flat_map(|(low, suggestions)| {
                suggestions.iter().filter_map(move |suggestion| {
                    let pair = LibraryItemPair::new(DocumentId(*low), suggestion.other);
                    let other = pair.other(id)?;
                    (suggestion.state == AssignmentState::Suggested)
                        .then_some((other, suggestion.score))
                })
            })
            .filter(|(other, _)| self.library_item(*other).is_some())
            .collect::<Vec<_>>();
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        found
    }

    fn suggestion(&self, pair: LibraryItemPair) -> Option<&RelatedSuggestion> {
        self.knowledge
            .suggestions
            .get(&pair.0.0)?
            .iter()
            .find(|suggestion| suggestion.other == pair.1)
    }

    /// Suggests each pair (two items and how close) unless the two are
    /// related already, kept apart, merged, or the pair was suggested
    /// before (a pending one gets its new score). Returns how many are new.
    pub fn suggest_related(&mut self, pairs: &[(DocumentId, DocumentId, f32)]) -> Result<usize> {
        let at = current_timestamp();
        let mut changed = BTreeMap::<u64, Vec<RelatedSuggestion>>::new();
        let mut new = 0;
        for (a, b, score) in pairs {
            let pair = LibraryItemPair::new(*a, *b);
            if pair.0 == pair.1
                || self.library_item(pair.0).is_none()
                || self.library_item(pair.1).is_none()
                || self.library.library_related_items.contains(&pair)
                || self.library.library_distinct_items.contains(&pair)
            {
                continue;
            }
            let row = changed.entry(pair.0.0).or_insert_with(|| {
                self.knowledge
                    .suggestions
                    .get(&pair.0.0)
                    .cloned()
                    .unwrap_or_default()
            });
            match row.iter_mut().find(|suggestion| suggestion.other == pair.1) {
                Some(suggestion) if suggestion.state == AssignmentState::Suggested => {
                    suggestion.score = *score;
                }
                Some(_) => {}
                None => {
                    row.push(RelatedSuggestion {
                        other: pair.1,
                        score: *score,
                        state: AssignmentState::Suggested,
                        at,
                    });
                    new += 1;
                }
            }
        }
        self.write_suggestions(changed)?;
        Ok(new)
    }

    /// Keeps a suggested related item: the two are related from now on.
    pub fn accept_related(&mut self, a: DocumentId, b: DocumentId) -> Result<()> {
        self.set_suggestion_state(LibraryItemPair::new(a, b), AssignmentState::Accepted)?;
        self.add_related_pairs(&[(a, b)])?;
        Ok(())
    }

    /// Dismisses a suggested related item; it is never suggested again.
    pub fn reject_related(&mut self, a: DocumentId, b: DocumentId) -> Result<()> {
        self.set_suggestion_state(LibraryItemPair::new(a, b), AssignmentState::Rejected)
    }

    /// Unrelates two items by hand: the pair is not suggested again.
    pub fn remove_related(&mut self, a: DocumentId, b: DocumentId) -> Result<()> {
        let others = self
            .related_items(a)
            .into_iter()
            .filter(|other| *other != b)
            .collect::<Vec<_>>();
        self.set_related_items(a, &others)?;
        self.set_suggestion_state(LibraryItemPair::new(a, b), AssignmentState::Rejected)
    }

    fn set_suggestion_state(
        &mut self,
        pair: LibraryItemPair,
        state: AssignmentState,
    ) -> Result<()> {
        let mut row = self
            .knowledge
            .suggestions
            .get(&pair.0.0)
            .cloned()
            .unwrap_or_default();
        match row.iter_mut().find(|suggestion| suggestion.other == pair.1) {
            Some(suggestion) => suggestion.state = state,
            None => row.push(RelatedSuggestion {
                other: pair.1,
                score: 0.0,
                state,
                at: current_timestamp(),
            }),
        }
        self.write_suggestions(BTreeMap::from([(pair.0.0, row)]))
    }

    fn write_suggestions(&mut self, rows: BTreeMap<u64, Vec<RelatedSuggestion>>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        self.write_knowledge(|write_txn| {
            let mut table = write_txn.open_table(LIBRARY_RELATED_SUGGESTIONS_V1_TABLE)?;
            for (id, suggestions) in &rows {
                let encoded = encode(&StoredSuggestionsV1 {
                    version: KNOWLEDGE_STORAGE_VERSION,
                    suggestions: suggestions.clone(),
                })?;
                table.insert(*id, encoded.as_slice())?;
            }
            Ok(())
        })?;
        self.knowledge.suggestions.extend(rows);
        Ok(())
    }

    /// Whether a suggested pair was kept or rejected (for tests and the
    /// suggester).
    pub fn related_suggestion_state(
        &self,
        a: DocumentId,
        b: DocumentId,
    ) -> Option<AssignmentState> {
        self.suggestion(LibraryItemPair::new(a, b))
            .map(|suggestion| suggestion.state)
    }

    pub fn topic_dismissed(&self, key: &str) -> bool {
        self.knowledge.dismissed.contains(key)
    }

    /// Stops suggesting a page topic.
    pub fn dismiss_topic(&mut self, key: &str) -> Result<()> {
        if self.knowledge.dismissed.contains(key) {
            return Ok(());
        }
        self.write_knowledge(|write_txn| {
            let mut table = write_txn.open_table(LIBRARY_PAGE_TOPICS_V1_TABLE)?;
            table.insert(key, current_timestamp())?;
            Ok(())
        })?;
        self.knowledge.dismissed.insert(key.to_string());
        Ok(())
    }

    fn write_knowledge(
        &self,
        write: impl FnOnce(&redb::WriteTransaction) -> Result<()>,
    ) -> Result<()> {
        self.synced_write();
        let Some(database) = &self.database else {
            return Ok(());
        };
        let write_txn = database.begin_write()?;
        write(&write_txn)?;
        write_txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(store: &mut LibraryStore, name: &str) -> DocumentId {
        let path = std::env::temp_dir().join(format!(
            "glyphst-knowledge-{}-{name}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, format!("%PDF-1.4 {name}")).unwrap();
        match store
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        {
            LibraryImportOutcome::Added(id) => id,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pages_suggestions_and_dismissed_topics_round_trip_and_rejections_stick() {
        let dir = std::env::temp_dir().join(format!("glyphst-knowledge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.redb");
        let (a, b, c) = {
            let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
            let (a, b, c) = (
                item(&mut store, "a"),
                item(&mut store, "b"),
                item(&mut store, "c"),
            );
            assert_eq!(store.next_page_id(), 1);
            let page = PageRecord {
                id: 1,
                title: "Attention".to_string(),
                relative_path: "concepts/attention.md".to_string(),
                topic: PageTopic::Items(vec![a, b]),
                sources: vec![a, b],
                frontmatter_sha256: [0; 32],
                blocks: Vec::new(),
                modified: 1,
                written_at: 1,
                model: "fake".to_string(),
                tokens_in: 10,
                tokens_out: 5,
                previous: None,
                unreviewed: true,
            };
            store.set_page(1, Some(page)).unwrap();
            assert_eq!(store.next_page_id(), 2);

            assert_eq!(
                store.suggest_related(&[(a, b, 0.9), (c, a, 0.4)]).unwrap(),
                2
            );
            assert_eq!(store.related_suggestions(a), vec![(b, 0.9), (c, 0.4)]);
            store.accept_related(b, a).unwrap();
            assert_eq!(store.related_items(a), vec![b]);
            store.reject_related(a, c).unwrap();
            assert!(store.related_suggestions(a).is_empty());
            store.dismiss_topic("tag:7").unwrap();
            (a, b, c)
        };
        let mut store = LibraryStore::open_for_test(path).unwrap();
        assert_eq!(
            store.page(1).map(|page| page.title.as_str()),
            Some("Attention")
        );
        assert!(store.topic_dismissed("tag:7") && !store.topic_dismissed("tag:8"));
        // Rejected and related pairs are never suggested again.
        assert_eq!(
            store
                .suggest_related(&[(a, c, 0.95), (a, b, 0.99)])
                .unwrap(),
            0
        );
        assert!(store.related_suggestions(a).is_empty());
        assert_eq!(
            store.related_suggestion_state(c, a),
            Some(AssignmentState::Rejected)
        );
        // Unrelating by hand rejects the pair too.
        store.remove_related(a, b).unwrap();
        assert!(store.related_items(a).is_empty());
        assert_eq!(store.suggest_related(&[(b, a, 0.9)]).unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_conversation_remembers_its_item_and_old_ones_have_none() {
        let dir = std::env::temp_dir().join(format!("glyphst-item-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.redb");
        let message = |role, content: &str| AiSessionMessage {
            role,
            content: content.to_string(),
            request_content: None,
            references: Vec::new(),
            occurrences: Vec::new(),
            images: Vec::new(),
            document_path: None,
            document_label: None,
            mentions: Vec::new(),
            sent_at: None,
            duration_ms: None,
            agent_parts: Vec::new(),
        };
        let a = {
            let mut store = LibraryStore::open_for_test(path.clone()).unwrap();
            let a = item(&mut store, "session");
            for (id, updated, about) in [(1, 10, Some(a)), (2, 30, Some(a)), (3, 50, None)] {
                store
                    .save_ai_session(AiSession {
                        id,
                        title: format!("chat {id}"),
                        messages: vec![
                            message(AiSessionRole::User, "What is it about?"),
                            message(AiSessionRole::Assistant, "Attention."),
                        ],
                        queued_messages: Vec::new(),
                        created: updated,
                        updated,
                        summary: None,
                        agent: None,
                        item: about,
                    })
                    .unwrap();
            }
            a
        };
        let store = LibraryStore::open_for_test(path).unwrap();
        assert_eq!(
            store.latest_item_session(a).map(|session| session.id),
            Some(2)
        );
        // A session saved before items were remembered reads with none.
        let old = r#"{"version":6,"id":9,"title":"old","messages":[],"queued_messages":[],"created":1,"updated":1}"#;
        let stored = decode::<StoredAiSessionV6>(old.as_bytes()).unwrap();
        assert_eq!(stored.into_domain().unwrap().item, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
