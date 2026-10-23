//! The library model's pass over one catalog item: a short summary, key
//! points, tags from the library's vocabulary, a collection, the kind of
//! item, and the metadata printed in its text when the item has none.
//! One request per item, answered as JSON.

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use crate::{
    agent::{AgentCancellationToken, AgentMessage, AgentUsage},
    compaction::answer_with_usage,
    json::parse_json_object,
    settings::ResolvedAiConfig,
};

/// Bumped when the prompt changes, so items are enriched again on request.
pub const PROMPT_VERSION: u32 = 2;
/// Characters of the item's text the model reads.
pub const EXCERPT_CHARS: usize = 6_000;
const TLDR_CHARS: usize = 280;
const MAX_KEY_POINTS: usize = 5;
const MAX_TAGS: usize = 5;
const MAX_NEW_TAGS: usize = 2;
/// Titles shown from each collection, so the model sees what it holds.
pub const COLLECTION_SAMPLES: usize = 3;
/// A collection is kept only when the model is at least this sure of it.
pub const MIN_COLLECTION_CONFIDENCE: f32 = 0.75;
/// Tokens an answer takes, for estimates.
pub const ANSWER_TOKENS: u64 = 350;

/// What the model is told about the item.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ItemFacts {
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub kind: String,
    pub publication: String,
    #[serde(rename = "abstract")]
    pub abstract_text: String,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub isbn: Option<String>,
}

/// A collection of the library, as the model sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollectionFacts {
    pub path: Vec<String>,
    /// A few titles of items in it.
    pub titles: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnrichInput {
    pub item: ItemFacts,
    /// The start of the item's text (its abstract, else its first pages).
    pub excerpt: String,
    /// The library's tags.
    pub tags: Vec<String>,
    /// The library's collections.
    pub collections: Vec<CollectionFacts>,
    /// Ask for the title, authors and year printed in the text.
    pub metadata_needed: bool,
    pub create_tags: bool,
    pub suggest_collections: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CollectionChoice {
    #[default]
    None,
    Existing(Vec<String>),
    New(Vec<String>),
}

/// Metadata the model read off the item's text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedMetadata {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    pub abstract_text: Option<String>,
    pub doi: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnrichOutput {
    pub tldr: String,
    pub key_points: Vec<String>,
    pub tags: Vec<String>,
    pub collection: CollectionChoice,
    /// `paper`, `book` or `other`.
    pub kind: Option<String>,
    pub language: Option<String>,
    pub metadata: Option<ExtractedMetadata>,
    pub confidence: Option<f32>,
    /// How sure the model is of the collection; below
    /// [`MIN_COLLECTION_CONFIDENCE`] the collection is dropped.
    pub collection_confidence: Option<f32>,
}

/// Asks the model about one item.
pub fn enrich(
    settings: ResolvedAiConfig,
    input: &EnrichInput,
    cancel: AgentCancellationToken,
) -> Result<(EnrichOutput, AgentUsage)> {
    let (answer, usage) = answer_with_usage(
        settings,
        vec![AgentMessage::user_text(request(input))],
        cancel,
    )?;
    Ok((parse(&answer, input)?, usage))
}

/// Roughly the tokens a request for `input` takes in and out, for the
/// estimate shown before enriching many items.
pub fn estimate_tokens(input: &EnrichInput) -> (u64, u64) {
    let characters = request(input).chars().count() as u64;
    // The reader's system prompt goes with every request.
    (characters / 4 + 1_500, ANSWER_TOKENS)
}

/// The request: the rules and the library's vocabulary first (the same for
/// every item, so providers can cache them), then the item.
pub fn request(input: &EnrichInput) -> String {
    let tag_rule = if input.create_tags {
        format!(
            "up to {MAX_TAGS} short lowercase topics. Use the exact spelling of tags in <existing_tags> when one fits; at most {MAX_NEW_TAGS} tags may be new."
        )
    } else {
        format!("up to {MAX_TAGS} tags, only from <existing_tags>.")
    };
    // Filing is optional, so a library with one odd collection doesn't
    // get every new item.
    let collection_rule = format!(
        "null unless the item clearly belongs in one. A collection is never required: a paper or book may stay outside every collection, and null is the usual answer. Give {{\"existing\": \"A / B\"}} only when the item is plainly about the subject a collection in <collections> names and holds; never pick one whose name says nothing about a subject (\"test0\", \"misc\", \"todo\", \"new\"), however few collections there are.{}",
        if input.suggest_collections {
            " Give {\"new\": \"A / B\"} (at most three levels, reusing existing parents) only for a clear subject no collection covers."
        } else {
            ""
        }
    );
    let metadata_rule = if input.metadata_needed {
        "{\"title\": …, \"authors\": […], \"year\": …, \"abstract\": …, \"doi\": …} as printed in the text; null for what is not printed."
    } else {
        "null."
    };
    let tags = input.tags.join("\n");
    let collections = input
        .collections
        .iter()
        .map(|collection| {
            let path = collection.path.join(" / ");
            if collection.titles.is_empty() {
                format!("{path} (empty)")
            } else {
                format!("{path} (holds: {})", collection.titles.join("; "))
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let item = serde_json::to_string_pretty(&input.item).unwrap_or_default();
    let excerpt = input
        .excerpt
        .chars()
        .take(EXCERPT_CHARS)
        .collect::<String>();
    format!(
        "You catalogue documents in a personal research library. Reply with one JSON object and nothing else:\n\
{{\"tldr\": string, \"key_points\": [string], \"tags\": [string], \"collection\": object or null, \"collection_confidence\": number, \"kind\": \"paper\" | \"book\" | \"other\", \"language\": string, \"metadata\": object or null, \"confidence\": number}}\n\n\
- tldr: at most {TLDR_CHARS} characters in English: what the item is and its main contribution or argument.\n\
- key_points: 3 to {MAX_KEY_POINTS} short phrases.\n\
- tags: {tag_rule}\n\
- collection: {collection_rule}\n\
- collection_confidence: 0 to 1, how sure you are that the item belongs in that collection; 0 when collection is null.\n\
- kind: a paper (article, preprint, thesis, report), a book, or other.\n\
- language: the item's language as an ISO 639-1 code.\n\
- metadata: {metadata_rule}\n\
- confidence: 0 to 1, how sure you are of the tags.\n\n\
<existing_tags>\n{tags}\n</existing_tags>\n<collections>\n{collections}\n</collections>\n\n\
<item>\n{item}\n</item>\n<text>\n{excerpt}\n</text>"
    )
}

/// The model's answer, with its lists cut to size and, when new tags are
/// not allowed, its tags limited to the vocabulary.
pub fn parse(answer: &str, input: &EnrichInput) -> Result<EnrichOutput> {
    let value = parse_json_object(answer, "the library model's answer")?;
    let text = |value: &Value| {
        value
            .as_str()
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|text| !text.is_empty())
    };
    let tldr =
        text(&value["tldr"]).ok_or_else(|| anyhow!("the library model's answer has no tldr"))?;
    let tldr = if tldr.chars().count() > TLDR_CHARS {
        let mut cut = tldr.chars().take(TLDR_CHARS - 1).collect::<String>();
        cut.push('…');
        cut
    } else {
        tldr
    };
    let key_points = value["key_points"]
        .as_array()
        .map(|points| {
            points
                .iter()
                .filter_map(text)
                .take(MAX_KEY_POINTS)
                .collect()
        })
        .unwrap_or_default();
    let known = |tag: &str| {
        input
            .tags
            .iter()
            .find(|existing| existing.eq_ignore_ascii_case(tag))
            .cloned()
    };
    let mut tags = Vec::new();
    let mut new_tags = 0;
    for tag in value["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(text)
    {
        let tag = tag.trim_start_matches('#').to_string();
        if tags.len() == MAX_TAGS
            || tags
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&tag))
        {
            continue;
        }
        match known(&tag) {
            Some(existing) => tags.push(existing),
            None if input.create_tags && new_tags < MAX_NEW_TAGS => {
                new_tags += 1;
                tags.push(tag.to_lowercase());
            }
            None => {}
        }
    }
    let path = |value: &Value| {
        text(value).map(|path| {
            path.split('/')
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
        })
    };
    let collection_confidence = value["collection_confidence"]
        .as_f64()
        .map(|confidence| confidence.clamp(0.0, 1.0) as f32);
    let sure = collection_confidence.is_some_and(|c| c >= MIN_COLLECTION_CONFIDENCE);
    let collection = match (
        path(&value["collection"]["existing"]),
        path(&value["collection"]["new"]),
    ) {
        _ if !sure => CollectionChoice::None,
        (Some(existing), _)
            if input
                .collections
                .iter()
                .any(|collection| collection.path == existing) =>
        {
            CollectionChoice::Existing(existing)
        }
        (Some(other), _) | (None, Some(other))
            if input.suggest_collections && !other.is_empty() && other.len() <= 3 =>
        {
            CollectionChoice::New(other)
        }
        _ => CollectionChoice::None,
    };
    let kind = text(&value["kind"])
        .map(|kind| kind.to_lowercase())
        .filter(|kind| matches!(kind.as_str(), "paper" | "book" | "other"));
    let metadata = (input.metadata_needed && value["metadata"].is_object()).then(|| {
        let metadata = &value["metadata"];
        ExtractedMetadata {
            title: text(&metadata["title"]),
            authors: metadata["authors"]
                .as_array()
                .map(|authors| authors.iter().filter_map(text).collect())
                .unwrap_or_default(),
            year: metadata["year"]
                .as_i64()
                .or_else(|| metadata["year"].as_str().and_then(|year| year.parse().ok()))
                .and_then(|year| i32::try_from(year).ok())
                .filter(|year| (1400..=2100).contains(year)),
            abstract_text: text(&metadata["abstract"]),
            doi: text(&metadata["doi"]).map(|doi| doi.to_lowercase()),
        }
    });
    Ok(EnrichOutput {
        tldr,
        key_points,
        tags,
        collection,
        kind,
        language: text(&value["language"]).map(|language| language.to_lowercase()),
        metadata,
        confidence: value["confidence"]
            .as_f64()
            .map(|confidence| confidence.clamp(0.0, 1.0) as f32),
        collection_confidence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> EnrichInput {
        EnrichInput {
            item: ItemFacts {
                title: "1706.03762v7".to_string(),
                kind: "paper".to_string(),
                ..ItemFacts::default()
            },
            excerpt: "Attention Is All You Need …".to_string(),
            tags: vec!["Transformers".to_string(), "nlp".to_string()],
            collections: vec![
                CollectionFacts {
                    path: vec!["Machine learning".to_string()],
                    titles: Vec::new(),
                },
                CollectionFacts {
                    path: vec!["Machine learning".to_string(), "LLMs".to_string()],
                    titles: vec!["Language Models are Few-Shot Learners".to_string()],
                },
            ],
            metadata_needed: true,
            create_tags: true,
            suggest_collections: true,
        }
    }

    #[test]
    fn the_request_holds_the_vocabulary_and_the_item() {
        let request = request(&input());
        assert!(request.contains("<existing_tags>\nTransformers\nnlp\n</existing_tags>"));
        assert!(
            request
                .contains("Machine learning / LLMs (holds: Language Models are Few-Shot Learners)")
        );
        assert!(request.contains("Machine learning (empty)"));
        assert!(request.contains("A collection is never required"));
        assert!(request.contains("\"title\": \"1706.03762v7\""));
        assert!(request.contains("as printed in the text"));
        let (tokens_in, tokens_out) = estimate_tokens(&input());
        assert!(tokens_in > 1_500);
        assert_eq!(tokens_out, ANSWER_TOKENS);
    }

    #[test]
    fn answers_are_cut_to_size_and_keep_known_spellings() {
        let answer = format!(
            "```json\n{}\n```",
            serde_json::json!({
                "tldr": "x".repeat(400),
                "key_points": ["a", "b", "c", "d", "e", "f"],
                "tags": ["transformers", "#attention", "seq2seq", "translation", "NLP", "more"],
                "collection": {"existing": "Machine learning / LLMs"},
                "collection_confidence": 0.9,
                "kind": "Paper",
                "language": "EN",
                "metadata": {"title": "Attention Is All You Need", "authors": ["A. Vaswani"], "year": "2017", "doi": null},
                "confidence": 1.4
            })
        );
        let output = parse(&answer, &input()).unwrap();
        assert_eq!(output.tldr.chars().count(), TLDR_CHARS);
        assert_eq!(output.key_points.len(), MAX_KEY_POINTS);
        assert_eq!(
            output.tags,
            vec!["Transformers", "attention", "seq2seq", "nlp"],
            "two new tags at most, existing ones as spelled"
        );
        assert_eq!(
            output.collection,
            CollectionChoice::Existing(vec!["Machine learning".to_string(), "LLMs".to_string()])
        );
        assert_eq!(output.kind.as_deref(), Some("paper"));
        assert_eq!(output.language.as_deref(), Some("en"));
        assert_eq!(output.confidence, Some(1.0));
        let metadata = output.metadata.unwrap();
        assert_eq!(metadata.year, Some(2017));
        assert_eq!(metadata.doi, None);
    }

    #[test]
    fn without_permission_there_are_no_new_tags_or_collections() {
        let mut input = input();
        input.create_tags = false;
        input.suggest_collections = false;
        input.metadata_needed = false;
        let answer = r#"{"tldr":"t","tags":["nlp","brand-new"],"collection":{"new":"Physics"},"collection_confidence":0.9,"metadata":{"title":"x"}}"#;
        let output = parse(answer, &input).unwrap();
        assert_eq!(output.tags, vec!["nlp"]);
        assert_eq!(output.collection, CollectionChoice::None);
        assert_eq!(output.metadata, None);
        // An unknown existing path becomes a proposal only when allowed.
        let mut input = self::input();
        input.metadata_needed = false;
        let output = parse(
            r#"{"tldr":"t","collection":{"existing":"Physics / Optics"},"collection_confidence":0.9}"#,
            &input,
        )
        .unwrap();
        assert_eq!(
            output.collection,
            CollectionChoice::New(vec!["Physics".to_string(), "Optics".to_string()])
        );
        assert!(parse("not json", &input).is_err());
    }

    /// The bug where every new paper went into a collection named
    /// `test0`: a collection the model is not sure of, or gave no
    /// confidence for, is dropped.
    #[test]
    fn an_unsure_collection_is_dropped() {
        let input = input();
        for answer in [
            r#"{"tldr":"t","collection":{"existing":"Machine learning"},"collection_confidence":0.6}"#,
            r#"{"tldr":"t","collection":{"existing":"Machine learning"}}"#,
            r#"{"tldr":"t","collection":{"new":"Physics"},"collection_confidence":0.2}"#,
        ] {
            let output = parse(answer, &input).unwrap();
            assert_eq!(output.collection, CollectionChoice::None, "{answer}");
        }
        let output = parse(
            r#"{"tldr":"t","collection":{"existing":"Machine learning"},"collection_confidence":0.8}"#,
            &input,
        )
        .unwrap();
        assert_eq!(
            output.collection,
            CollectionChoice::Existing(vec!["Machine learning".to_string()])
        );
        assert_eq!(output.collection_confidence, Some(0.8));
    }
}
