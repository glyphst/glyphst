//! How far the library is with each paper (frame 04w): its details looked
//! up, its text read for search, its passages embedded for search by
//! meaning, and its summary, tags and collection. Built once a frame from
//! the job queue and the search index's last report; asked only for the
//! rows and the item shown.

use std::collections::{HashMap, HashSet};

use glyphst_library::{
    DocumentId, LibraryItem, LibraryJobKind, LibraryJobState, LibraryStore, MetadataSource,
};
use glyphst_search::ItemTextState;

/// One stage of a paper.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    Done(String),
    /// Under way now.
    Active(String),
    /// Waiting its turn (or to try again).
    Waiting(String),
    Failed(String),
    /// Not done and not coming, with why when there is something to say.
    Off(Option<String>),
}

impl Step {
    fn busy(&self) -> bool {
        matches!(self, Step::Active(_) | Step::Waiting(_))
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            Step::Done(label)
            | Step::Active(label)
            | Step::Waiting(label)
            | Step::Failed(label) => Some(label),
            Step::Off(label) => label.as_deref(),
        }
    }
}

/// A paper's stages, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemProgress {
    pub details: Step,
    pub text: Step,
    pub meaning: Step,
    pub summary: Step,
}

impl ItemProgress {
    pub fn steps(&self) -> [&Step; 4] {
        [&self.details, &self.text, &self.meaning, &self.summary]
    }

    /// Something is still being worked out.
    pub fn busy(&self) -> bool {
        self.steps().iter().any(|step| step.busy())
    }

    /// What is happening now (or went wrong), for a row's hover.
    pub fn now(&self) -> Option<&str> {
        self.steps()
            .into_iter()
            .find(|step| step.busy() || matches!(step, Step::Failed(_)))
            .and_then(Step::label)
    }

    pub fn finding_details(&self) -> bool {
        self.details.busy()
    }

    pub fn summarising(&self) -> bool {
        self.summary.busy()
    }

    pub fn failed(&self) -> Option<&str> {
        self.steps().into_iter().find_map(|step| match step {
            Step::Failed(error) => Some(error.as_str()),
            _ => None,
        })
    }
}

/// Whether summaries can be made now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Summaries {
    #[default]
    On,
    /// The AI pass is paused (after an error, or by the user).
    Paused,
    /// No library model is chosen in Settings › AI.
    NoModel,
}

/// What the app knows this frame.
#[derive(Clone, Default)]
pub struct Progress {
    jobs: HashMap<DocumentId, Vec<(LibraryJobKind, LibraryJobState)>>,
    /// The search index's report, when it runs.
    text: Option<glyphst_search::indexer::ItemStates>,
    /// Passages are embedded for search by meaning.
    meaning: bool,
    summaries: Summaries,
    busy: HashSet<DocumentId>,
}

impl Progress {
    pub fn new(
        library: &LibraryStore,
        text: Option<glyphst_search::indexer::ItemStates>,
        meaning: bool,
        summaries: Summaries,
    ) -> Self {
        let mut jobs: HashMap<DocumentId, Vec<_>> = HashMap::new();
        for job in library.library_jobs() {
            jobs.entry(job.item_id)
                .or_default()
                .push((job.kind, job.state.clone()));
        }
        let mut progress = Progress {
            jobs,
            text,
            meaning,
            summaries,
            busy: HashSet::new(),
        };
        progress.busy = progress
            .jobs
            .iter()
            .filter(|(_, jobs)| {
                jobs.iter().any(|(_, state)| {
                    !matches!(state, LibraryJobState::Failed { .. })
                        && (summaries == Summaries::On || state == &LibraryJobState::Running)
                })
            })
            .map(|(id, _)| *id)
            .collect();
        if let Some(text) = &progress.text {
            for (item, state) in text.iter() {
                let reading =
                    matches!(state, ItemTextState::Queued | ItemTextState::Reading { .. })
                        || (meaning && matches!(state, ItemTextState::Embedding { .. }));
                if reading {
                    progress.busy.insert(DocumentId(*item));
                }
            }
        }
        progress
    }

    /// Papers still being worked on (trashed ones too; the heading counts
    /// the ones it lists).
    pub fn is_busy(&self, id: DocumentId) -> bool {
        self.busy.contains(&id)
    }

    fn job(&self, id: DocumentId, kind: LibraryJobKind) -> Option<&LibraryJobState> {
        self.jobs
            .get(&id)?
            .iter()
            .find(|(job_kind, _)| *job_kind == kind)
            .map(|(_, state)| state)
    }

    pub fn of(&self, library: &LibraryStore, item: &LibraryItem) -> ItemProgress {
        let id = item.id;
        let details = match (
            self.job(id, LibraryJobKind::Metadata),
            library.metadata_record(id),
        ) {
            (Some(LibraryJobState::Running), _) => Step::Active("finding its details".into()),
            (Some(LibraryJobState::Queued), _) => Step::Waiting("finding its details".into()),
            (Some(LibraryJobState::Waiting { .. }), _) => {
                Step::Waiting("finding its details · trying again soon".into())
            }
            (Some(LibraryJobState::Failed { error }), _) => {
                Step::Failed(format!("no details found · {error}"))
            }
            (None, Some(record)) if record.source == MetadataSource::Filename => {
                Step::Done("details from the file name".into())
            }
            (None, Some(record)) => Step::Done(format!("details from {}", record.source.label())),
            (None, None) => Step::Off(Some("no details found".into())),
        };
        let index = self.text.as_ref().and_then(|text| text.get(&id.0).copied());
        let (text, meaning) = match index {
            None => (Step::Off(None), Step::Off(None)),
            Some(ItemTextState::Queued) => (
                Step::Waiting("reading the text".into()),
                self.meaning_waits(),
            ),
            Some(ItemTextState::Reading { pages_done, pages }) => (
                Step::Active(if pages > 0 {
                    format!("reading the text · {pages_done} / {pages} pages")
                } else {
                    "reading the text".into()
                }),
                self.meaning_waits(),
            ),
            Some(ItemTextState::Embedding { done, passages }) if self.meaning => (
                Step::Done("text read".into()),
                Step::Active(format!("meaning for search · {done} / {passages} passages")),
            ),
            Some(ItemTextState::Embedding { .. } | ItemTextState::Done) => (
                Step::Done("text read".into()),
                if self.meaning {
                    Step::Done("meaning for search".into())
                } else {
                    Step::Off(None)
                },
            ),
            Some(ItemTextState::Failed) => (
                Step::Failed("its text could not be read".into()),
                Step::Off(None),
            ),
        };
        let summarised = library.item_enrichment(id).is_some();
        let summary = match self.job(id, LibraryJobKind::Enrich) {
            Some(LibraryJobState::Failed { error }) => {
                Step::Failed(format!("could not summarise · {error}"))
            }
            Some(_) if self.summaries == Summaries::NoModel => {
                Step::Off(Some("no library model · settings › ai".into()))
            }
            Some(LibraryJobState::Running) => Step::Active(if summarised {
                "summarising again".into()
            } else {
                "summary, tags and collection".into()
            }),
            Some(_) if self.summaries == Summaries::Paused => {
                Step::Off(Some("summaries are paused".into()))
            }
            Some(LibraryJobState::Waiting { .. }) => {
                Step::Waiting("summary, tags and collection · trying again soon".into())
            }
            Some(_) => Step::Waiting("summary, tags and collection".into()),
            None if summarised => Step::Done("summary, tags and collection".into()),
            None if self.summaries == Summaries::NoModel => {
                Step::Off(Some("no library model · settings › ai".into()))
            }
            None => Step::Off(Some("not summarised yet".into())),
        };
        ItemProgress {
            details,
            text,
            meaning,
            summary,
        }
    }

    fn meaning_waits(&self) -> Step {
        if self.meaning {
            Step::Waiting("meaning for search".into())
        } else {
            Step::Off(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use glyphst_library::{
        LibraryImportOutcome, LibraryItemMetadata, LibraryJobOrigin, LibraryStore,
    };

    use super::*;

    fn library() -> (LibraryStore, DocumentId) {
        let mut library = LibraryStore::memory();
        let dir = std::env::temp_dir().join(format!("glyphst-progress-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("paper.pdf");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        let LibraryImportOutcome::Added(id) = library
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        else {
            panic!("expected an item");
        };
        (library, id)
    }

    fn text(id: DocumentId, state: ItemTextState) -> Option<glyphst_search::indexer::ItemStates> {
        Some(Arc::new(HashMap::from([(id.0, state)])))
    }

    #[test]
    fn a_new_paper_goes_through_its_stages() {
        let (mut library, id) = library();
        library
            .enqueue_library_jobs(
                &[id],
                LibraryJobKind::Metadata,
                LibraryJobOrigin::Import,
                false,
            )
            .unwrap();
        let item = library.library_item(id).unwrap().clone();
        let progress = Progress::new(
            &library,
            text(id, ItemTextState::Queued),
            true,
            Summaries::On,
        );
        let shown = progress.of(&library, &item);
        assert!(progress.is_busy(id));
        assert!(shown.finding_details());
        assert_eq!(shown.now(), Some("finding its details"));
        assert_eq!(shown.meaning, Step::Waiting("meaning for search".into()));

        let reading = Progress::new(
            &library,
            text(
                id,
                ItemTextState::Reading {
                    pages_done: 4,
                    pages: 12,
                },
            ),
            true,
            Summaries::On,
        )
        .of(&library, &item);
        assert_eq!(
            reading.text,
            Step::Active("reading the text · 4 / 12 pages".into())
        );
        let embedding = Progress::new(
            &library,
            text(
                id,
                ItemTextState::Embedding {
                    done: 3,
                    passages: 20,
                },
            ),
            true,
            Summaries::On,
        )
        .of(&library, &item);
        assert_eq!(embedding.text, Step::Done("text read".into()));
        assert!(matches!(embedding.meaning, Step::Active(_)));
    }

    #[test]
    fn failures_and_a_missing_model_say_so() {
        let (mut library, id) = library();
        library
            .enqueue_library_jobs(
                &[id],
                LibraryJobKind::Enrich,
                LibraryJobOrigin::Import,
                false,
            )
            .unwrap();
        let item = library.library_item(id).unwrap().clone();
        let none = Progress::new(&library, None, false, Summaries::NoModel);
        let shown = none.of(&library, &item);
        assert_eq!(
            shown.summary,
            Step::Off(Some("no library model · settings › ai".into()))
        );
        // A queue that cannot move is not "being processed".
        assert!(!none.is_busy(id));
        assert!(!shown.busy());

        let job = library
            .take_library_jobs(LibraryJobKind::Enrich, 1)
            .remove(0);
        library
            .finish_library_job(
                job.id,
                Some(LibraryJobState::Failed {
                    error: "the model said no".into(),
                }),
            )
            .unwrap();
        let failed = Progress::new(&library, None, false, Summaries::On);
        let shown = failed.of(&library, &item);
        assert!(!failed.is_busy(id), "a failed job is not work going on");
        assert!(
            shown
                .failed()
                .is_some_and(|error| error.contains("could not summarise"))
        );
    }

    #[test]
    fn a_read_paper_without_meaning_search_is_done_with_its_text() {
        let (library, id) = library();
        let item = library.library_item(id).unwrap().clone();
        let progress = Progress::new(
            &library,
            text(id, ItemTextState::Done),
            false,
            Summaries::On,
        );
        let shown = progress.of(&library, &item);
        assert_eq!(shown.text, Step::Done("text read".into()));
        assert_eq!(shown.meaning, Step::Off(None));
        assert!(!progress.is_busy(id));
    }
}
