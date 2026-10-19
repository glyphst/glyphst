//! A canned library for screenshots of the library frames (04d, 04f, 04i):
//! papers and books with collections, tags, the AI's summaries and
//! suggestions, in a memory store so a run never touches the user's.

use std::path::PathBuf;

use glyphst_library::{
    EnrichmentApply, EnrichmentStatus, ItemEnrichment, LibraryCreator, LibraryCreatorRole,
    LibraryImportOutcome, LibraryItemKind, LibraryItemMetadata, LibraryStore, MetadataRecord,
    MetadataSource,
};

/// The states `--library-demo` takes.
pub const STATES: [&str; 6] = ["all", "item", "review", "backfill", "folder", "identifier"];

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
/// temporary folder.
pub fn library() -> LibraryStore {
    let mut library = LibraryStore::memory();
    let folder = std::env::temp_dir().join(format!("glyphst-library-demo-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&folder);
    let run = library.next_enrichment_run();
    for (index, paper) in PAPERS.iter().enumerate() {
        let path: PathBuf = folder.join(format!("{index:02}.pdf"));
        let _ = std::fs::write(&path, format!("%PDF-1.4\n% {}\n", paper.title));
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

/// The demo's first paper, chosen in the `item` state.
pub fn first_item(library: &LibraryStore) -> Option<glyphst_library::DocumentId> {
    library
        .library_items()
        .iter()
        .find(|item| item.title == PAPERS[0].title)
        .map(|item| item.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_has_collections_tags_and_suggestions() {
        let library = library();
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
}
