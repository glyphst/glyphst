//! The knowledge layer's suggestions, made without a model: related items
//! from meaning search (for the user to keep or not), and topics a concept
//! page could be written about, from tags and from groups of papers close
//! in meaning. Nothing is written by a model until the user picks a topic.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "topics are offered in M4's pages view")
)]

use std::collections::HashMap;

use glyphst_library::{DocumentId, LibraryStore, LibraryTagId, PageTopic};

use crate::library_search::Neighbours;

/// Tags with at least this many items are page topics.
const TOPIC_MIN_ITEMS: usize = 3;
/// Topics offered at once.
const TOPICS_SHOWN: usize = 6;

/// What a suggested page would be written from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopicKind {
    Tag(LibraryTagId),
    /// Papers close in meaning.
    Cluster,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopicSuggestion {
    /// Stable, to dismiss it: `tag:7`, `items:3,8,12`.
    pub key: String,
    pub title: String,
    pub kind: TopicKind,
    pub items: Vec<DocumentId>,
}

impl TopicSuggestion {
    /// The page topic it becomes when written.
    pub fn topic(&self) -> PageTopic {
        match self.kind {
            TopicKind::Tag(id) => PageTopic::Tag(id),
            TopicKind::Cluster => PageTopic::Items(self.items.clone()),
        }
    }
}

/// Suggests each item's close neighbours as related (each pair once; pairs
/// already related, kept apart or rejected are skipped by the library).
pub fn refresh_related(
    library: &mut LibraryStore,
    neighbours: &Neighbours,
) -> anyhow::Result<usize> {
    let mut pairs = Vec::new();
    for (item, close) in &neighbours.related {
        for (other, score) in close {
            if item < other {
                pairs.push((DocumentId(*item), DocumentId(*other), *score));
            }
        }
    }
    library.suggest_related(&pairs)
}

/// Topics for concept pages: groups of papers close in meaning first, then
/// tags with enough items, leaving out those with a page or dismissed.
pub fn suggest_topics(
    library: &LibraryStore,
    neighbours: Option<&Neighbours>,
) -> Vec<TopicSuggestion> {
    let live = |id: DocumentId| {
        library
            .library_item(id)
            .is_some_and(|item| item.trashed_at.is_none())
    };
    let written = library
        .pages()
        .map(|page| page.topic.clone())
        .collect::<Vec<_>>();
    let mut topics = Vec::new();
    for group in neighbours
        .map(|found| found.topics.as_slice())
        .unwrap_or_default()
    {
        let items = group
            .iter()
            .map(|id| DocumentId(*id))
            .filter(|id| live(*id))
            .collect::<Vec<_>>();
        if items.len() < TOPIC_MIN_ITEMS || written.contains(&PageTopic::Items(items.clone())) {
            continue;
        }
        let key = format!(
            "items:{}",
            items
                .iter()
                .map(|id| id.0.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        if library.topic_dismissed(&key) {
            continue;
        }
        topics.push(TopicSuggestion {
            key,
            title: cluster_title(library, &items, neighbours),
            kind: TopicKind::Cluster,
            items,
        });
    }
    let mut tags = library
        .library_tags()
        .iter()
        .map(|tag| {
            let items = library
                .library_items()
                .iter()
                .filter(|item| item.trashed_at.is_none())
                .filter(|item| {
                    library
                        .tags_for_item(item.id)
                        .iter()
                        .any(|t| t.id == tag.id)
                })
                .map(|item| item.id)
                .collect::<Vec<_>>();
            (tag, items)
        })
        .filter(|(tag, items)| {
            items.len() >= TOPIC_MIN_ITEMS
                && !written.contains(&PageTopic::Tag(tag.id))
                && !library.topic_dismissed(&format!("tag:{}", tag.id.0))
        })
        .collect::<Vec<_>>();
    tags.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.name.cmp(&b.0.name)));
    topics.extend(tags.into_iter().map(|(tag, items)| TopicSuggestion {
        key: format!("tag:{}", tag.id.0),
        title: tag.name.clone(),
        kind: TopicKind::Tag(tag.id),
        items,
    }));
    topics.truncate(TOPICS_SHOWN);
    topics
}

/// A group's name: the tag most of it shares, else the title of the paper
/// most of the others are close to.
fn cluster_title(
    library: &LibraryStore,
    items: &[DocumentId],
    neighbours: Option<&Neighbours>,
) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for id in items {
        for tag in library.tags_for_item(*id) {
            *counts.entry(tag.name.as_str()).or_default() += 1;
        }
    }
    if let Some((name, count)) = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        && count * 2 > items.len()
    {
        return name.to_string();
    }
    let links = |id: DocumentId| {
        neighbours
            .and_then(|found| found.related.iter().find(|(item, _)| *item == id.0))
            .map_or(0, |(_, close)| {
                close
                    .iter()
                    .filter(|(other, _)| items.contains(&DocumentId(*other)))
                    .count()
            })
    };
    items
        .iter()
        .max_by_key(|id| (links(**id), std::cmp::Reverse(id.0)))
        .and_then(|id| library.library_item(*id))
        .map(|item| item.title.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use glyphst_library::{LibraryImportOutcome, LibraryItemMetadata};

    use super::*;

    fn item(library: &mut LibraryStore, title: &str, tags: &[&str]) -> DocumentId {
        let path = std::env::temp_dir().join(format!(
            "glyphst-knowledge-{}-{title}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, format!("%PDF-1.4 {title}")).unwrap();
        let LibraryImportOutcome::Added(id) = library
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        else {
            panic!("expected item");
        };
        let mut item = library.library_item(id).unwrap().clone();
        item.title = title.to_string();
        library.update_library_item_metadata(item).unwrap();
        let names = tags.iter().map(|tag| tag.to_string()).collect::<Vec<_>>();
        library.set_item_tag_names(id, &names).unwrap();
        id
    }

    #[test]
    fn neighbours_become_related_suggestions_and_topics() {
        let mut library = LibraryStore::memory();
        let attention = item(&mut library, "Attention Is All You Need", &["nlp"]);
        let bert = item(&mut library, "BERT", &["nlp"]);
        let t5 = item(&mut library, "T5", &["nlp"]);
        let mamba = item(&mut library, "Mamba", &["ssm"]);
        let bread = item(&mut library, "Bread", &["cooking"]);
        let neighbours = Neighbours {
            related: vec![
                (attention.0, vec![(bert.0, 0.9), (t5.0, 0.8)]),
                (bert.0, vec![(attention.0, 0.9), (t5.0, 0.7)]),
                (
                    t5.0,
                    vec![(attention.0, 0.8), (bert.0, 0.7), (mamba.0, 0.5)],
                ),
                (mamba.0, vec![(t5.0, 0.5)]),
            ],
            topics: vec![vec![attention.0, bert.0, t5.0]],
        };
        assert_eq!(refresh_related(&mut library, &neighbours).unwrap(), 4);
        let suggested = library.related_suggestions(attention);
        assert_eq!(suggested, vec![(bert, 0.9), (t5, 0.8)]);
        assert!(library.related_suggestions(bread).is_empty());

        // The group shares #nlp: one topic named for it, and #nlp itself
        // is a topic too (three items).
        let topics = suggest_topics(&library, Some(&neighbours));
        assert_eq!(topics.len(), 2, "{topics:#?}");
        assert_eq!(topics[0].title, "nlp");
        assert_eq!(topics[0].kind, TopicKind::Cluster);
        assert_eq!(topics[0].items, vec![attention, bert, t5]);
        assert_eq!(
            topics[0].topic(),
            PageTopic::Items(vec![attention, bert, t5])
        );
        assert!(matches!(topics[1].kind, TopicKind::Tag(_)));

        // Dismissed, a topic is not offered again.
        library.dismiss_topic(&topics[0].key).unwrap();
        let topics = suggest_topics(&library, Some(&neighbours));
        assert_eq!(topics.len(), 1);
        assert!(matches!(topics[0].kind, TopicKind::Tag(_)));
    }
}
