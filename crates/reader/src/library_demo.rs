//! A canned library for screenshots of the library frames (04d, 04f, 04i):
//! papers and books with collections, tags, the AI's summaries and
//! suggestions, in a memory store so a run never touches the user's.

use std::path::PathBuf;

use glyphst_library::{
    DocumentId, EnrichmentApply, EnrichmentStatus, HighlightColor, ItemEnrichment, LibraryCreator,
    LibraryCreatorRole, LibraryImportOutcome, LibraryItemKind, LibraryItemMetadata, LibraryStore,
    MetadataRecord, MetadataSource,
};

/// The states `--library-demo` takes.
pub const STATES: [&str; 22] = [
    "all",
    "item",
    "pages",
    "page",
    "page-cost",
    "duplicate",
    "merged",
    "search",
    "search-meaning",
    "search-downloading",
    "search-failed",
    "history",
    "backfill",
    "folder",
    "identifier",
    "zotero",
    "zotero-done",
    "sync",
    "sync-conflicts",
    "sync-s3",
    "sync-webdav",
    "join",
];

/// A sample Zotero data folder for frame 04j.
pub fn zotero_folder() -> PathBuf {
    let folder = std::env::temp_dir().join(format!("glyphst-zotero-demo-{}", std::process::id()));
    if let Err(error) = glyphst_zotero_import::sample::write(&folder) {
        eprintln!("failed to write the sample Zotero library: {error:#}");
    }
    folder
}

struct Paper {
    title: &'static str,
    authors: &'static [&'static str],
    year: i32,
    venue: &'static str,
    collections: &'static [&'static [&'static str]],
    tags: &'static [&'static str],
    /// AI tags still to review.
    ai_tags: &'static [&'static str],
    suggested: Option<&'static [&'static str]>,
    tldr: Option<&'static str>,
    source: MetadataSource,
}

const PAPERS: &[Paper] = &[
    Paper {
        title: "Attention Is All You Need",
        authors: &[
            "Ashish Vaswani",
            "Noam Shazeer",
            "Niki Parmar",
            "Jakob Uszkoreit",
            "Llion Jones",
            "Aidan N. Gomez",
            "Łukasz Kaiser",
            "Illia Polosukhin",
        ],
        year: 2017,
        venue: "NeurIPS",
        collections: &[&["Machine learning", "LLMs"]],
        tags: &["transformers", "to read"],
        ai_tags: &["attention", "nlp"],
        suggested: Some(&["Machine learning", "NLP", "Translation"]),
        tldr: Some(
            "Introduces the Transformer, a sequence model built only from attention. It drops recurrence and convolution, trains far faster in parallel, and sets new BLEU records on English–German and English–French translation.",
        ),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "BERT: Pre-training of Deep Bidirectional Transformers for Language Understanding",
        authors: &[
            "Jacob Devlin",
            "Ming-Wei Chang",
            "Kenton Lee",
            "Kristina Toutanova",
        ],
        year: 2018,
        venue: "NAACL",
        collections: &[&["Machine learning", "LLMs"]],
        tags: &["transformers", "nlp"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("Pre-trains a bidirectional Transformer with masked language modelling."),
        source: MetadataSource::Crossref,
    },
    Paper {
        title: "Language Models are Few-Shot Learners",
        authors: &["Tom B. Brown", "Benjamin Mann", "Nick Ryder"],
        year: 2020,
        venue: "NeurIPS",
        collections: &[&["Machine learning", "LLMs"]],
        tags: &["llms"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("GPT-3: scale makes in-context learning work."),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "Denoising Diffusion Probabilistic Models",
        authors: &["Jonathan Ho", "Ajay Jain", "Pieter Abbeel"],
        year: 2020,
        venue: "NeurIPS",
        collections: &[&["Machine learning", "Diffusion"]],
        tags: &["diffusion"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("Diffusion models reach GAN-level image quality."),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "AnimateDiff: Animate Your Personalized Text-to-Image Diffusion Models",
        authors: &["Yuwei Guo", "Ceyuan Yang", "Anyi Rao"],
        year: 2023,
        venue: "ICLR",
        collections: &[&["Machine learning", "Diffusion"]],
        tags: &["diffusion"],
        ai_tags: &["video"],
        suggested: Some(&["Machine learning", "Diffusion", "Video"]),
        tldr: Some("A motion module animates personalised text-to-image models."),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "Transformers are Inherently Succinct",
        authors: &["Pascal Bergsträßer", "Ryan Cotterell", "Anthony W. Lin"],
        year: 2025,
        venue: "arXiv",
        collections: &[],
        tags: &["transformers"],
        ai_tags: &["formal-languages"],
        suggested: None,
        tldr: Some("Transformers describe formal languages far more succinctly than automata."),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "Statistical Modeling: The Two Cultures",
        authors: &["Leo Breiman"],
        year: 2001,
        venue: "Statistical Science",
        collections: &[&["Statistics"]],
        tags: &["statistics"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("Argues for algorithmic models over data models."),
        source: MetadataSource::Crossref,
    },
    Paper {
        title: "Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks",
        authors: &["Patrick Lewis", "Ethan Perez"],
        year: 2020,
        venue: "NeurIPS",
        collections: &[&["Machine learning", "LLMs"], &["Reading list"]],
        tags: &["rag", "nlp"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("Couples a retriever with a generator for open-domain questions."),
        source: MetadataSource::Arxiv,
    },
    Paper {
        title: "Mamba: Linear-Time Sequence Modeling with Selective State Spaces",
        authors: &["Albert Gu", "Tri Dao"],
        year: 2023,
        venue: "arXiv",
        collections: &[],
        tags: &[],
        ai_tags: &[],
        suggested: None,
        tldr: None,
        source: MetadataSource::Filename,
    },
    Paper {
        title: "Designing Data-Intensive Applications",
        authors: &["Martin Kleppmann"],
        year: 2017,
        venue: "O'Reilly",
        collections: &[&["Systems"]],
        tags: &["databases"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("The ideas behind reliable, scalable and maintainable data systems."),
        source: MetadataSource::OpenLibrary,
    },
    Paper {
        title: "Structure and Interpretation of Computer Programs",
        authors: &["Harold Abelson", "Gerald Jay Sussman"],
        year: 1996,
        venue: "MIT Press",
        collections: &[&["Programming"], &["Reading list"]],
        tags: &["lisp"],
        ai_tags: &[],
        suggested: None,
        tldr: Some("Programs as a way of thinking, taught in Scheme."),
        source: MetadataSource::OpenLibrary,
    },
];

/// A memory library with the demo items. Their files are tiny PDFs in the
/// temporary folder; the first is `document` when one is given (a run's
/// document, so its sidebar shows the item).
pub fn library(document: Option<&std::path::Path>) -> LibraryStore {
    let mut library = LibraryStore::memory();
    let folder = std::env::temp_dir().join(format!("glyphst-library-demo-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&folder);
    let run = library.next_enrichment_run();
    for (index, paper) in PAPERS.iter().enumerate() {
        let path: PathBuf = match document.filter(|_| index == 0) {
            Some(document) => document.to_path_buf(),
            None => {
                let path = folder.join(format!("{index:02}.pdf"));
                let _ = std::fs::write(&path, format!("%PDF-1.4\n% {}\n", paper.title));
                path
            }
        };
        let Ok(LibraryImportOutcome::Added(id)) =
            library.link_item(&path, LibraryItemMetadata::for_path(&path), None)
        else {
            continue;
        };
        let Some(mut item) = library.library_item(id).cloned() else {
            continue;
        };
        item.title = paper.title.to_string();
        item.creators = paper
            .authors
            .iter()
            .map(|name| LibraryCreator {
                name: name.to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
        item.year = Some(paper.year);
        item.publication = paper.venue.to_string();
        item.kind = if paper.venue.contains("Press") || paper.venue.contains("O'Reilly") {
            LibraryItemKind::Book
        } else {
            LibraryItemKind::Paper
        };
        let _ = library.update_library_item_metadata(item);
        let tags = paper
            .tags
            .iter()
            .map(|tag| tag.to_string())
            .collect::<Vec<_>>();
        let _ = library.set_item_tag_names(id, &tags);
        let paths = paper
            .collections
            .iter()
            .map(|path| path.iter().map(|part| part.to_string()).collect())
            .collect::<Vec<Vec<String>>>();
        let _ = library.set_item_collection_paths(id, &paths);
        let _ = library.set_metadata_record(
            id,
            MetadataRecord {
                source: paper.source,
                matched_identifier: (paper.source == MetadataSource::Arxiv)
                    .then(|| "arxiv:1706.03762".to_string()),
                ..MetadataRecord::default()
            },
        );
        let enrichment = paper.tldr.map(|tldr| ItemEnrichment {
            prompt_version: glyphst_ai::enrichment::PROMPT_VERSION,
            model: "claude-haiku-5-5".to_string(),
            input_sha256: [0; 32],
            status: EnrichmentStatus::Done,
            attempts: 1,
            finished_at: 1_792_368_000,
            input_tokens: 0,
            output_tokens: 0,
            tldr: tldr.to_string(),
            key_points: if index == 0 {
                vec![
                    "scaled dot-product and multi-head attention".to_string(),
                    "sinusoidal positional encodings".to_string(),
                    "28.4 BLEU en–de after 3.5 days on 8 GPUs".to_string(),
                ]
            } else {
                Vec::new()
            },
            suggested_tags: Vec::new(),
            suggested_collection: None,
            kind: None,
            language: Some("en".to_string()),
            confidence: Some(0.9),
        });
        let _ = library.apply_enrichment(
            id,
            EnrichmentApply {
                run,
                tags: paper.ai_tags.iter().map(|tag| tag.to_string()).collect(),
                create_tags: true,
                collections: Vec::new(),
                suggested_collection: paper
                    .suggested
                    .map(|path| path.iter().map(|part| part.to_string()).collect()),
                metadata: None,
                enrichment,
            },
        );
    }
    library
}

/// For frames 04l and 04m: tag colours, the first paper's related items,
/// and a second copy of it, flagged as its duplicate. Returns the copy.
pub fn duplicate(library: &mut LibraryStore) -> Option<DocumentId> {
    for (name, color) in [
        ("transformers", HighlightColor::Blue),
        ("to read", HighlightColor::Yellow),
        ("nlp", HighlightColor::Green),
        ("diffusion", HighlightColor::Pink),
    ] {
        let tag = library
            .library_tags()
            .iter()
            .find(|tag| tag.name == name)
            .map(|tag| tag.id);
        if let Some(tag) = tag {
            let _ = library.set_tag_color(tag, Some(color));
        }
    }
    let first = first_item(library)?;
    let doi = "10.48550/arXiv.1706.03762";
    let mut item = library.library_item(first)?.clone();
    item.identifiers.doi = Some(doi.to_string());
    let _ = library.update_library_item_metadata(item.clone());

    let folder = std::env::temp_dir().join(format!("glyphst-library-demo-{}", std::process::id()));
    let path = folder.join("attention-is-all-you-need (1).pdf");
    let _ = std::fs::write(&path, "%PDF-1.4\n% a second copy\n");
    let Ok(LibraryImportOutcome::Added(copy)) =
        library.link_item(&path, LibraryItemMetadata::for_path(&path), None)
    else {
        return None;
    };
    let mut copy_item = library.library_item(copy)?.clone();
    copy_item.title = item.title.clone();
    copy_item.creators = item.creators.clone();
    copy_item.year = item.year;
    copy_item.publication = "NeurIPS".to_string();
    copy_item.identifiers.doi = Some(doi.to_string());
    let _ = library.update_library_item_metadata(copy_item);
    let _ = library.set_item_tag_names(copy, &["transformers".to_string(), "to read".to_string()]);
    let _ = library.set_item_collection_paths(copy, &library.item_collection_paths(first));
    let _ = library.set_metadata_record(
        copy,
        MetadataRecord {
            source: MetadataSource::Crossref,
            matched_identifier: Some(format!("doi:{doi}")),
            possible_duplicate_of: Some(first),
            ..MetadataRecord::default()
        },
    );
    let related = ["BERT", "Language Models are Few-Shot"]
        .iter()
        .filter_map(|start| {
            library
                .library_items()
                .iter()
                .find(|item| item.title.starts_with(start))
                .map(|item| item.id)
        })
        .collect::<Vec<_>>();
    let _ = library.set_related_items(copy, &related);
    Some(copy)
}

/// Passages of the demo papers for the search (frame 04n): title start,
/// 0-based page, section, text.
const PASSAGES: &[(&str, usize, &str, &str)] = &[
    (
        "Attention Is All",
        2,
        "3 Model Architecture",
        "Most competitive neural sequence transduction models have an encoder-decoder structure; the Transformer follows this overall architecture using stacked self-attention.",
    ),
    (
        "Attention Is All",
        3,
        "3.2 Attention",
        "We call our particular attention “Scaled Dot-Product Attention”. The input consists of queries and keys of dimension d_k, and values of dimension d_v.",
    ),
    (
        "Attention Is All",
        4,
        "3.2.2 Multi-Head Attention",
        "Multi-Head Attention allows the model to jointly attend to information from different representation subspaces at different positions. With a single attention head, averaging inhibits this.",
    ),
    (
        "Attention Is All",
        5,
        "3.2.3 Applications of Attention",
        "The Transformer uses multi-head attention in three different ways: encoder-decoder attention, encoder self-attention and masked decoder self-attention.",
    ),
    (
        "Attention Is All",
        8,
        "6.2 Model Variations",
        "In Table 3 rows (A), we vary the number of attention heads and the attention key and value dimensions.",
    ),
    (
        "BERT",
        2,
        "3 BERT",
        "BERT's model architecture is a multi-layer bidirectional Transformer encoder whose layers use multi-head self-attention over the whole input sequence.",
    ),
    (
        "BERT",
        5,
        "5 Ablation Studies",
        "We use 12 attention heads in BERT-base and 16 in BERT-large, with a hidden size of 768 and 1024.",
    ),
    (
        "Mamba",
        0,
        "1 Introduction",
        "Many subquadratic-time architectures address the computational inefficiency of Transformers on long sequences, replacing multi-head attention whose cost grows quadratically with sequence length.",
    ),
    (
        "Transformers are Inherently",
        5,
        "4 Succinctness",
        "We show that a single attention head can express any regular language recognised by a small automaton, and that heads compose.",
    ),
    (
        "Denoising Diffusion",
        1,
        "4 Experiments",
        "The U-Net backbone uses self-attention at the 16×16 feature map resolution, between the convolutional residual blocks.",
    ),
    (
        "Retrieval-Augmented",
        3,
        "2 Methods",
        "The generator attends to the retrieved passages; we use the attention of the decoder over the concatenated inputs.",
    ),
];

/// A search index of the demo library (frame 04n), its text partly read:
/// the papers with passages are done, the rest wait.
pub fn search_index(library: &LibraryStore) -> Option<PathBuf> {
    use glyphst_search::{
        Store,
        chunk::{Chunk, PageChunks},
        index::Step,
    };
    let path =
        std::env::temp_dir().join(format!("glyphst-search-demo-{}.sqlite", std::process::id()));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
    let mut store = Store::open(&path).ok()?;
    let snapshot = crate::library_search::snapshot(library);
    store.reconcile(&snapshot, 0).ok()?;
    for item in library.library_items() {
        let pages = PASSAGES
            .iter()
            .filter(|(title, ..)| item.title.starts_with(title))
            .map(|(_, page, section, text)| {
                (
                    *page,
                    PageChunks {
                        chunks: vec![Chunk {
                            page: *page,
                            glyph_start: 0,
                            glyph_end: text.chars().count(),
                            doc_offset: page * 2_000,
                            quote: text.chars().take(32).collect(),
                            prefix: String::new(),
                            suffix: String::new(),
                            rects: Vec::new(),
                            section: section.to_string(),
                            text: text.to_string(),
                        }],
                        chars: text.chars().count(),
                        image_only: false,
                    },
                )
            })
            .collect::<Vec<_>>();
        if pages.is_empty() {
            continue;
        }
        let Some(file) = snapshot.files.iter().find(|file| file.item_id == item.id.0) else {
            continue;
        };
        let Ok(Some(doc_id)) = store.doc_id(&file.sha256) else {
            continue;
        };
        let _ = store.save_step(
            &Step {
                doc_id,
                format: "pdf",
                page_count: 12,
                pages: &pages,
                pages_done: 12,
                glyphs_done: 24_000,
            },
            0,
        );
    }
    Some(path)
}

/// The demo's first paper, chosen in the `item` state.
pub fn first_item(library: &LibraryStore) -> Option<glyphst_library::DocumentId> {
    library
        .library_items()
        .iter()
        .find(|item| item.title == PAPERS[0].title)
        .map(|item| item.id)
}

/// The knowledge layer's demo (frames 04p, 04q, 04f): a vault in a temporary
/// folder with two concept pages (the first waiting for review), a note of
/// the user's linking a paper and a page, related items kept and suggested,
/// and papers close in meaning. Returns the vault and the neighbours.
pub fn knowledge(
    library: &mut LibraryStore,
) -> Option<(PathBuf, crate::library_search::Neighbours)> {
    use glyphst_library::{PageRecord, PageTopic};
    use glyphst_vault::pages::{PageSnapshot, render_page};
    let find = |library: &LibraryStore, start: &str| {
        library
            .library_items()
            .iter()
            .find(|item| item.title.starts_with(start))
            .map(|item| item.id)
    };
    let attention = find(library, "Attention Is All")?;
    let bert = find(library, "BERT")?;
    let mamba = find(library, "Mamba")?;
    let succinct = find(library, "Transformers are Inherently")?;
    let few_shot = find(library, "Language Models are Few-Shot");
    let root = std::env::temp_dir().join(format!("glyphst-vault-demo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("concepts")).ok()?;
    std::fs::create_dir_all(root.join("thesis")).ok()?;
    let slug = |library: &LibraryStore, id: DocumentId| {
        let item = library.library_item(id)?;
        let authors = item
            .creators
            .iter()
            .map(|creator| creator.name.clone())
            .collect::<Vec<_>>();
        Some(glyphst_vault::slug(&authors, item.year, &item.title))
    };
    let cite =
        |id: DocumentId, page: usize| format!("glyphst://open?id={}&page={page}&chunk=1", id.0);
    let pages = [
        (
            1,
            "Attention mechanisms",
            "attention-mechanisms",
            vec![attention, bert, mamba, succinct],
            format!(
                "Attention lets a model weigh every token of its input against every other, so a long-range link costs one step instead of many [p 3]({}). [[{}]] builds a whole translation model from it, and [[{}]] reads text in both directions with the same layers [p 4]({}).\n\n## Scaled dot-product and many heads\n\nEach head compares queries with keys, scales by the square root of their size and mixes the values; several heads look at different relations at once [p 4]({}) [p 5]({}).\n\n## Where the sources differ\n\nMamba replaces attention with a selective state space for long inputs [p 1]({}); a single head can already express any regular language [p 6]({}).",
                cite(attention, 3),
                slug(library, attention)?,
                slug(library, bert)?,
                cite(bert, 4),
                cite(attention, 4),
                cite(attention, 5),
                cite(mamba, 1),
                cite(succinct, 6),
            ),
            true,
        ),
        (
            2,
            "State space models",
            "state-space-models",
            vec![mamba, attention],
            format!(
                "State space models read a sequence in time linear in its length [p 1]({}), where [[attention-mechanisms|attention]] grows with its square.",
                cite(mamba, 1)
            ),
            false,
        ),
    ];
    for (id, title, file, sources, body, unreviewed) in pages {
        let snapshot = PageSnapshot {
            id,
            uid: String::new(),
            title: title.to_string(),
            sources: sources
                .iter()
                .filter_map(|source| slug(library, *source))
                .collect(),
            body,
            model: "haiku-5.5".to_string(),
            date: "2026-10-20".to_string(),
        };
        let written = render_page(&snapshot);
        let relative = format!("concepts/{file}.md");
        std::fs::write(root.join(&relative), &written.text).ok()?;
        library
            .set_page(
                id,
                Some(PageRecord {
                    id,
                    title: title.to_string(),
                    relative_path: relative,
                    topic: PageTopic::Items(sources.clone()),
                    sources,
                    frontmatter_sha256: written.written.frontmatter_sha256,
                    blocks: written.written.blocks,
                    modified: 0,
                    written_at: glyphst_library::current_timestamp(),
                    model: "haiku-5.5".to_string(),
                    tokens_in: 24_000,
                    tokens_out: 1_600,
                    previous: None,
                    unreviewed,
                }),
            )
            .ok()?;
    }
    std::fs::write(
        root.join("thesis/chapter 2.md"),
        format!(
            "# Chapter 2\n\nAs [[{}]] shows, and as [[attention-mechanisms]] sums up…\n",
            slug(library, attention)?
        ),
    )
    .ok()?;
    let _ = library.add_related_pairs(&[(attention, bert)]);
    if let Some(few_shot) = few_shot {
        let _ = library.add_related_pairs(&[(attention, few_shot)]);
    }
    let _ = library.suggest_related(&[(attention, succinct, 0.82)]);
    let neighbours = crate::library_search::Neighbours {
        related: vec![(attention.0, vec![(bert.0, 0.9), (succinct.0, 0.82)])],
        topics: vec![vec![attention.0, bert.0, succinct.0]],
    };
    Some((root, neighbours))
}

/// The demo's reranker: a passage scores the share of the query's words
/// (by their first five letters) it holds, a probability as a real
/// reranker's. No model; screens only.
pub struct DemoReranker;

impl glyphst_search::embed::Reranker for DemoReranker {
    fn rerank(&self, query: &str, passages: &[&str]) -> anyhow::Result<Vec<f32>> {
        let stems = |text: &str| {
            text.split(|c: char| !c.is_alphanumeric())
                .filter(|word| word.len() > 2)
                .map(|word| word.to_lowercase().chars().take(5).collect::<String>())
                .collect::<std::collections::HashSet<_>>()
        };
        let query = stems(query);
        let words = query.len().max(1) as f32;
        Ok(passages
            .iter()
            .map(|passage| stems(passage).intersection(&query).count() as f32 / words)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_has_collections_tags_and_suggestions() {
        let library = library(None);
        assert_eq!(library.library_items().len(), PAPERS.len());
        let id = first_item(&library).unwrap();
        assert!(library.item_needs_review(id));
        assert_eq!(library.collections_for_item(id).len(), 1);
        assert!(library.item_enrichment(id).is_some());
        assert!(
            library
                .library_collections()
                .iter()
                .any(|collection| collection.name == "LLMs")
        );
    }

    #[test]
    fn the_duplicate_merges_into_the_first_paper() {
        let mut library = library(None);
        let first = first_item(&library).unwrap();
        let copy = duplicate(&mut library).unwrap();
        assert_eq!(
            library.metadata_record(copy).unwrap().possible_duplicate_of,
            Some(first)
        );
        assert_eq!(library.related_items(copy).len(), 2);
        library.merge_library_items(first, copy).unwrap();
        assert_eq!(library.related_items(first).len(), 2);
        assert_eq!(
            library
                .library_attachments()
                .iter()
                .filter(|attachment| attachment.item_id == first)
                .count(),
            2
        );
    }
}
