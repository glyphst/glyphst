//! The library's background work: looking up each new item's metadata,
//! then the library model's pass (summary, tags, a collection). Jobs wait
//! in the library's queue (`library_jobs_v1`), so they carry on after a
//! restart; one worker thread looks metadata up and two talk to the model.
//! Results come back to the UI thread and are applied a few a frame.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

use glyphst_ai::{
    agent::{AgentCancellationToken, AgentUsage},
    enrichment::{self, CollectionChoice, EnrichInput, EnrichOutput, ItemFacts},
    settings::ResolvedAiConfig,
};
use glyphst_library::{
    DocumentId, EnrichmentApply, EnrichmentStatus, ItemEnrichment, LibraryCreator,
    LibraryCreatorRole, LibraryItem, LibraryItemKind, LibraryJob, LibraryJobKind, LibraryJobState,
    LibraryStore, MetadataSource, current_timestamp,
};
use glyphst_metadata::{MetadataClient, MetadataEndpoints, Recognized, WorkMetadata};
use sha2::{Digest as _, Sha256};

use crate::prefs::LibraryPrefs;

/// Pages read for identifiers and for the model's excerpt.
const PROBE_PAGES: usize = 4;
const METADATA_WORKERS: usize = 1;
const ENRICH_WORKERS: usize = 2;
/// Results applied per frame.
const PER_FRAME: usize = 16;
/// Waits before the second and third attempts; then the job fails.
const RETRY_AFTER: [u64; 3] = [60, 600, 3_600];

pub(crate) enum Work {
    Metadata {
        job: LibraryJob,
        path: PathBuf,
        look_up: bool,
        mailto: Option<String>,
    },
    Enrich(Box<EnrichWork>),
}

pub(crate) struct EnrichWork {
    job: LibraryJob,
    path: Option<PathBuf>,
    settings: ResolvedAiConfig,
    input: EnrichInput,
    /// The last run's input, to skip an unchanged item.
    previous: Option<[u8; 32]>,
    cancel: AgentCancellationToken,
}

/// What a worker found for a job.
pub enum Outcome {
    Metadata {
        job: LibraryJob,
        result: Result<Box<Recognized>, String>,
    },
    Enrich {
        job: LibraryJob,
        result: Result<Box<EnrichDone>, String>,
    },
}

pub struct EnrichDone {
    pub output: Option<EnrichOutput>,
    pub usage: AgentUsage,
    pub model: String,
    pub input_sha256: [u8; 32],
}

/// The queue's workers and what they are doing.
pub struct LibraryJobs {
    metadata: Sender<Work>,
    enrich: Sender<Work>,
    outcomes: Receiver<Outcome>,
    running_metadata: usize,
    running_enrich: Vec<AgentCancellationToken>,
    /// The library model refused (no key, a bad key): the AI pass waits
    /// until settings change.
    paused: Option<String>,
    started: bool,
}

impl LibraryJobs {
    pub fn new(ctx: &egui::Context) -> Self {
        let (outcomes_sender, outcomes) = channel();
        let metadata = spawn_workers("library-metadata", METADATA_WORKERS, &outcomes_sender, ctx);
        let enrich = spawn_workers("library-ai", ENRICH_WORKERS, &outcomes_sender, ctx);
        Self {
            metadata,
            enrich,
            outcomes,
            running_metadata: 0,
            running_enrich: Vec::new(),
            paused: None,
            started: false,
        }
    }

    /// Jobs being worked on now.
    pub fn running(&self) -> usize {
        self.running_metadata + self.running_enrich.len()
    }

    /// Lets a paused AI pass go on (after a settings change).
    pub fn resume(&mut self) {
        self.paused = None;
    }

    /// Holds the AI pass until [`Self::resume`].
    pub fn pause(&mut self) {
        self.paused = Some("paused".to_string());
    }

    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    /// Applies what the workers found and hands them the next jobs.
    /// Returns notices for the user.
    pub fn poll(
        &mut self,
        library: &mut LibraryStore,
        prefs: &LibraryPrefs,
        model: Option<ResolvedAiConfig>,
    ) -> Vec<String> {
        let mut notices = Vec::new();
        if !self.started {
            // Jobs left running by a crash wait in the queue again.
            let _ = library.requeue_running_library_jobs();
            self.started = true;
        }
        for _ in 0..PER_FRAME {
            let Ok(outcome) = self.outcomes.try_recv() else {
                break;
            };
            match outcome {
                Outcome::Metadata { job, result } => {
                    self.running_metadata = self.running_metadata.saturating_sub(1);
                    match result {
                        Ok(recognized) => {
                            apply_metadata(library, job.item_id, &recognized);
                            let _ = library.finish_library_job(job.id, None);
                            if prefs.enrich_new && model.is_some() {
                                let _ = library.enqueue_library_jobs(
                                    &[job.item_id],
                                    LibraryJobKind::Enrich,
                                    job.origin,
                                    false,
                                );
                            }
                        }
                        Err(error) => {
                            let state = retry_state(&job, &error);
                            // A lookup that keeps failing still lets the
                            // model read the metadata off the text.
                            if matches!(state, LibraryJobState::Failed { .. })
                                && prefs.enrich_new
                                && model.is_some()
                            {
                                let _ = library.enqueue_library_jobs(
                                    &[job.item_id],
                                    LibraryJobKind::Enrich,
                                    job.origin,
                                    false,
                                );
                            }
                            let _ = library.finish_library_job(job.id, Some(state));
                        }
                    }
                }
                Outcome::Enrich { job, result } => {
                    self.running_enrich.pop();
                    match result {
                        Ok(done) => {
                            if let Some(output) = &done.output
                                && let Err(error) =
                                    apply_enrichment(library, job.item_id, output, &done, prefs)
                            {
                                notices.push(format!("Could not apply the AI's pass: {error:#}"));
                            }
                            let _ = library.finish_library_job(job.id, None);
                        }
                        Err(error) => {
                            if refused(&error) {
                                self.paused = Some(error.clone());
                                notices.push(format!(
                                    "The library model is paused: {error}. Check Settings › AI."
                                ));
                                let _ = library
                                    .finish_library_job(job.id, Some(LibraryJobState::Queued));
                            } else {
                                let state = retry_state(&job, &error);
                                let _ = library.finish_library_job(job.id, Some(state));
                            }
                        }
                    }
                }
            }
        }
        self.dispatch(library, prefs, model);
        notices
    }

    fn dispatch(
        &mut self,
        library: &mut LibraryStore,
        prefs: &LibraryPrefs,
        model: Option<ResolvedAiConfig>,
    ) {
        let free = METADATA_WORKERS.saturating_sub(self.running_metadata);
        if free > 0 {
            for job in library.take_library_jobs(LibraryJobKind::Metadata, free) {
                let Some(path) = library
                    .primary_attachment(job.item_id)
                    .map(|attachment| attachment.path.clone())
                else {
                    let _ = library.finish_library_job(job.id, None);
                    continue;
                };
                let work = Work::Metadata {
                    job,
                    path,
                    look_up: prefs.look_up_online,
                    mailto: Some(prefs.contact_email.trim().to_string())
                        .filter(|address| !address.is_empty()),
                };
                if self.metadata.send(work).is_ok() {
                    self.running_metadata += 1;
                }
            }
        }
        let Some(model) = model.filter(|_| self.paused.is_none()) else {
            return;
        };
        let free = ENRICH_WORKERS.saturating_sub(self.running_enrich.len());
        if free == 0 {
            return;
        }
        let jobs = library.take_library_jobs(LibraryJobKind::Enrich, free);
        if jobs.is_empty() {
            return;
        }
        let tags = library
            .library_tags()
            .iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        let collections = library
            .library_collections()
            .iter()
            .map(|collection| {
                library
                    .collection_ancestors(collection.id)
                    .into_iter()
                    .map(|ancestor| ancestor.name.clone())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for job in jobs {
            let Some(item) = library.library_item(job.item_id).cloned() else {
                let _ = library.finish_library_job(job.id, None);
                continue;
            };
            let metadata_needed = library
                .metadata_record(item.id)
                .is_none_or(|record| record.source == MetadataSource::Filename)
                || item.creators.is_empty();
            let input = EnrichInput {
                item: item_facts(&item),
                excerpt: item.abstract_text.clone(),
                tags: tags.clone(),
                collections: collections.clone(),
                metadata_needed,
                create_tags: prefs.ai_create_tags,
                suggest_collections: prefs.suggest_collections,
            };
            let previous = library
                .item_enrichment(item.id)
                .filter(|enrichment| {
                    !job.force
                        && enrichment.status == EnrichmentStatus::Done
                        && enrichment.prompt_version == enrichment::PROMPT_VERSION
                })
                .map(|enrichment| enrichment.input_sha256);
            let cancel = AgentCancellationToken::default();
            let work = Work::Enrich(Box::new(EnrichWork {
                path: library
                    .primary_attachment(item.id)
                    .map(|attachment| attachment.path.clone()),
                job,
                settings: model.clone(),
                input,
                previous,
                cancel: cancel.clone(),
            }));
            if self.enrich.send(work).is_ok() {
                self.running_enrich.push(cancel);
            }
        }
    }
}

impl Drop for LibraryJobs {
    fn drop(&mut self) {
        for cancel in &self.running_enrich {
            cancel.cancel();
        }
    }
}

fn spawn_workers(
    name: &str,
    count: usize,
    outcomes: &Sender<Outcome>,
    ctx: &egui::Context,
) -> Sender<Work> {
    let (sender, receiver) = channel::<Work>();
    let receiver = Arc::new(Mutex::new(receiver));
    for index in 0..count {
        let receiver = receiver.clone();
        let outcomes = outcomes.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("{name}-{index}"))
            .spawn(move || {
                loop {
                    let work = {
                        let Ok(receiver) = receiver.lock() else {
                            return;
                        };
                        match receiver.recv() {
                            Ok(work) => work,
                            Err(_) => return,
                        }
                    };
                    if outcomes.send(run(work)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        if let Err(error) = spawned {
            log::warn!("no {name} thread: {error}");
        }
    }
    sender
}

/// Does one job on a worker thread.
fn run(work: Work) -> Outcome {
    match work {
        Work::Metadata {
            job,
            path,
            look_up,
            mailto,
        } => {
            let result = recognize(&path, look_up, mailto)
                .map(Box::new)
                .map_err(|error| format!("{error:#}"));
            Outcome::Metadata { job, result }
        }
        Work::Enrich(work) => {
            let EnrichWork {
                job,
                path,
                settings,
                mut input,
                previous,
                cancel,
            } = *work;
            if input.excerpt.trim().is_empty()
                && let Some(path) = &path
                && let Ok(probe) = glyphst_pdf::probe_document(path, PROBE_PAGES)
            {
                input.excerpt = probe.page_text.join("\n");
            }
            let input_sha256 = input_hash(&settings.model, &input);
            let model = settings.model.clone();
            let result = if previous == Some(input_sha256) {
                Ok(Box::new(EnrichDone {
                    output: None,
                    usage: AgentUsage::default(),
                    model,
                    input_sha256,
                }))
            } else {
                enrichment::enrich(settings, &input, cancel)
                    .map(|(output, usage)| {
                        Box::new(EnrichDone {
                            output: Some(output),
                            usage,
                            model,
                            input_sha256,
                        })
                    })
                    .map_err(|error| format!("{error:#}"))
            };
            Outcome::Enrich { job, result }
        }
    }
}

fn recognize(
    path: &std::path::Path,
    look_up: bool,
    mailto: Option<String>,
) -> anyhow::Result<Recognized> {
    let probe = glyphst_pdf::probe_document(path, PROBE_PAGES)?;
    let client = look_up
        .then(|| MetadataClient::new(MetadataEndpoints::default(), mailto))
        .transpose()?;
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    glyphst_metadata::recognize(client.as_ref(), &probe.page_text, &probe.info, &stem)
}

/// The hash an enrichment's input is remembered by.
/// The tokens the library model's pass over `items` would read and
/// write, roughly.
pub fn enrich_estimate(
    library: &LibraryStore,
    items: &[&glyphst_library::LibraryItem],
) -> (u64, u64) {
    let tags = library
        .library_tags()
        .iter()
        .map(|tag| tag.name.clone())
        .collect::<Vec<_>>();
    let (mut tokens_in, mut tokens_out) = (0, 0);
    for item in items {
        let input = glyphst_ai::enrichment::EnrichInput {
            item: glyphst_ai::enrichment::ItemFacts {
                title: item.title.clone(),
                ..Default::default()
            },
            // Without an abstract the model reads the first pages.
            excerpt: if item.abstract_text.is_empty() {
                "x".repeat(glyphst_ai::enrichment::EXCERPT_CHARS)
            } else {
                item.abstract_text.clone()
            },
            tags: tags.clone(),
            ..Default::default()
        };
        let (input_tokens, output_tokens) = glyphst_ai::enrichment::estimate_tokens(&input);
        tokens_in += input_tokens;
        tokens_out += output_tokens;
    }
    (tokens_in, tokens_out)
}

pub fn input_hash(model: &str, input: &EnrichInput) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(enrichment::PROMPT_VERSION.to_le_bytes());
    hasher.update(model.as_bytes());
    hasher.update(serde_json::to_vec(&input.item).unwrap_or_default());
    let excerpt = input
        .excerpt
        .chars()
        .take(enrichment::EXCERPT_CHARS)
        .collect::<String>();
    hasher.update(excerpt.as_bytes());
    hasher.finalize().into()
}

fn item_facts(item: &LibraryItem) -> ItemFacts {
    ItemFacts {
        title: item.title.clone(),
        authors: item
            .creators
            .iter()
            .map(|creator| creator.name.clone())
            .collect(),
        year: item.year,
        kind: kind_name(item.kind).to_string(),
        publication: item.publication.clone(),
        abstract_text: item.abstract_text.chars().take(1_500).collect(),
        doi: item.identifiers.doi.clone(),
        arxiv: item.identifiers.arxiv.clone(),
        isbn: item.identifiers.isbn.clone(),
    }
}

fn kind_name(kind: LibraryItemKind) -> &'static str {
    match kind {
        LibraryItemKind::Paper => "paper",
        LibraryItemKind::Book => "book",
        LibraryItemKind::Other => "other",
    }
}

/// What to do with a job that failed this time.
fn retry_state(job: &LibraryJob, error: &str) -> LibraryJobState {
    match RETRY_AFTER.get(job.attempts.saturating_sub(1) as usize) {
        Some(wait) if job.attempts < RETRY_AFTER.len() as u32 => LibraryJobState::Waiting {
            until: current_timestamp() + wait,
        },
        _ => LibraryJobState::Failed {
            error: error.to_string(),
        },
    }
}

/// Whether the model's provider turned the request down for good (no key,
/// a wrong key, no access), so retrying won't help.
fn refused(error: &str) -> bool {
    let error = error.to_lowercase();
    [
        "401",
        "403",
        "unauthorized",
        "forbidden",
        "api key",
        "invalid_api_key",
    ]
    .iter()
    .any(|sign| error.contains(sign))
}

/// Puts what recognition found into the item: a catalogue's answer over
/// the file's own info, never over fields the user edited.
pub fn apply_metadata(library: &mut LibraryStore, id: DocumentId, recognized: &Recognized) {
    let Some(mut item) = library.library_item(id).cloned() else {
        return;
    };
    let mut record = library.metadata_record(id).cloned().unwrap_or_default();
    let locked = record.locked_fields.clone();
    let mut changed = Vec::new();
    // The file's info first, into what is still a guess or empty.
    let embedded = &recognized.embedded;
    if record.source == MetadataSource::Filename {
        let mut weak = WorkMetadata {
            source: MetadataSource::Embedded,
            title: embedded.title.clone(),
            keywords: embedded.keywords.clone(),
            ..WorkMetadata::default()
        };
        if item.creators.is_empty() {
            weak.creators = embedded.creators.clone();
        }
        changed.extend(glyphst_metadata::merge_work(&mut item, &weak, &locked));
    }
    let mut ids = recognized.identifiers.clone();
    if let Some(work) = &recognized.work {
        changed.extend(glyphst_metadata::merge_work(&mut item, work, &locked));
        record.source = work.source;
        record.matched_identifier = work.identifier_label();
    } else {
        if !changed.is_empty() {
            record.source = MetadataSource::Embedded;
        }
        // Identifiers the text prints still count.
        let identifiers = WorkMetadata {
            doi: ids.doi.take(),
            arxiv: ids.arxiv.take(),
            isbn: ids.isbn.take(),
            ..WorkMetadata::default()
        };
        changed.extend(glyphst_metadata::merge_work(
            &mut item,
            &identifiers,
            &locked,
        ));
    }
    record.fetched_at = current_timestamp();
    record.possible_duplicate_of =
        glyphst_metadata::find_duplicate(&item, library.library_items()).map(|(other, _)| other);
    if !changed.is_empty()
        && let Err(error) = library.update_library_item_metadata(item)
    {
        log::warn!("could not save metadata for item {}: {error:#}", id.0);
        return;
    }
    if let Err(error) = library.set_metadata_record(id, record) {
        log::warn!(
            "could not save the metadata record of item {}: {error:#}",
            id.0
        );
    }
}

/// Gives the item what the model found.
pub fn apply_enrichment(
    library: &mut LibraryStore,
    id: DocumentId,
    output: &EnrichOutput,
    done: &EnrichDone,
    prefs: &LibraryPrefs,
) -> anyhow::Result<()> {
    let Some(item) = library.library_item(id).cloned() else {
        return Ok(());
    };
    let (collections, suggested_collection) = match &output.collection {
        CollectionChoice::Existing(path) => (vec![path.clone()], None),
        CollectionChoice::New(path) if prefs.suggest_collections => {
            (Vec::new(), Some(path.clone()))
        }
        _ => (Vec::new(), None),
    };
    let kind = output.kind.as_deref().map(|kind| match kind {
        "book" => LibraryItemKind::Book,
        "other" => LibraryItemKind::Other,
        _ => LibraryItemKind::Paper,
    });
    let metadata = output.metadata.as_ref().map(|extracted| {
        let mut proposed = item.clone();
        if let Some(title) = &extracted.title {
            proposed.title = title.clone();
        }
        if !extracted.authors.is_empty() {
            proposed.creators = extracted
                .authors
                .iter()
                .map(|name| LibraryCreator {
                    name: name.clone(),
                    role: LibraryCreatorRole::Author,
                })
                .collect();
        }
        if extracted.year.is_some() {
            proposed.year = extracted.year;
        }
        if let Some(abstract_text) = &extracted.abstract_text {
            proposed.abstract_text = abstract_text.clone();
        }
        if let Some(doi) = &extracted.doi {
            proposed.identifiers.doi = Some(doi.clone());
        }
        if let Some(kind) = kind {
            proposed.kind = kind;
        }
        proposed
    });
    let run = library.next_enrichment_run();
    library.apply_enrichment(
        id,
        EnrichmentApply {
            run,
            tags: output.tags.clone(),
            create_tags: prefs.ai_create_tags,
            collections,
            suggested_collection,
            metadata,
            enrichment: Some(ItemEnrichment {
                prompt_version: enrichment::PROMPT_VERSION,
                model: done.model.clone(),
                input_sha256: done.input_sha256,
                status: EnrichmentStatus::Done,
                attempts: 1,
                finished_at: current_timestamp(),
                input_tokens: done.usage.input_tokens,
                output_tokens: done.usage.output_tokens,
                tldr: output.tldr.clone(),
                key_points: output.key_points.clone(),
                suggested_tags: output.tags.clone(),
                suggested_collection: match &output.collection {
                    CollectionChoice::Existing(path) | CollectionChoice::New(path) => {
                        Some(path.clone())
                    }
                    CollectionChoice::None => None,
                },
                kind,
                language: output.language.clone(),
                confidence: output.confidence,
            }),
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use glyphst_library::{
        AssignmentState, AssignmentTarget, LibraryItemMetadata, LibraryJobOrigin,
    };
    use glyphst_metadata::Identifiers;

    use super::*;

    fn linked(library: &mut LibraryStore, name: &str) -> (DocumentId, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("glyphst-jobs-{name}-{}.pdf", std::process::id()));
        std::fs::write(
            &path,
            glyphst_pdf::test_pdf::text_pdf(&["Attention Is All You Need", "arXiv:1706.03762v7"]),
        )
        .unwrap();
        let glyphst_library::LibraryImportOutcome::Added(id) = library
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        else {
            panic!("expected the file to be linked");
        };
        (id, path)
    }

    #[test]
    fn recognised_metadata_fills_the_item_and_records_its_source() {
        let mut library = LibraryStore::memory();
        let (id, path) = linked(&mut library, "metadata");
        let recognized = Recognized {
            identifiers: Identifiers {
                arxiv: Some("1706.03762".to_string()),
                ..Identifiers::default()
            },
            work: Some(WorkMetadata {
                source: MetadataSource::Arxiv,
                title: "Attention Is All You Need".to_string(),
                creators: vec!["Ashish Vaswani".to_string()],
                year: Some(2017),
                arxiv: Some("1706.03762".to_string()),
                ..WorkMetadata::default()
            }),
            embedded: WorkMetadata::default(),
        };
        apply_metadata(&mut library, id, &recognized);
        let item = library.library_item(id).unwrap();
        assert_eq!(item.title, "Attention Is All You Need");
        assert_eq!(item.year, Some(2017));
        let record = library.metadata_record(id).unwrap();
        assert_eq!(record.source, MetadataSource::Arxiv);
        assert_eq!(
            record.matched_identifier.as_deref(),
            Some("arxiv:1706.03762")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_fake_library_model_tags_and_proposes_a_collection() {
        let port = crate::fake_providers::start_ai_server().unwrap();
        let mut settings = glyphst_settings::Settings::default();
        crate::fake_providers::use_fake_ai(&mut settings, port);
        let model = settings
            .ai
            .resolved_library()
            .expect("the fake model resolves");

        let mut library = LibraryStore::memory();
        let (id, path) = linked(&mut library, "enrich");
        library
            .enqueue_library_jobs(
                &[id],
                LibraryJobKind::Enrich,
                LibraryJobOrigin::OnAdd,
                false,
            )
            .unwrap();
        let job = library
            .take_library_jobs(LibraryJobKind::Enrich, 1)
            .pop()
            .unwrap();
        let input = EnrichInput {
            item: item_facts(library.library_item(id).unwrap()),
            create_tags: true,
            suggest_collections: true,
            ..EnrichInput::default()
        };
        let outcome = run(Work::Enrich(Box::new(EnrichWork {
            job,
            path: Some(path.clone()),
            settings: model,
            input,
            previous: None,
            cancel: AgentCancellationToken::default(),
        })));
        let Outcome::Enrich { job, result } = outcome else {
            panic!("expected an enrichment");
        };
        let done = result.unwrap();
        let output = done.output.as_ref().unwrap();
        assert_eq!(output.tldr, "A fake summary of the item.");
        apply_enrichment(
            &mut library,
            job.item_id,
            output,
            &done,
            &LibraryPrefs::default(),
        )
        .unwrap();
        library.finish_library_job(job.id, None).unwrap();

        assert_eq!(
            library
                .tags_for_item(id)
                .into_iter()
                .map(|tag| tag.name.clone())
                .collect::<Vec<_>>(),
            vec!["fake topic".to_string()]
        );
        let suggestion = library
            .ai_assignments(id)
            .iter()
            .find(|assignment| matches!(assignment.target, AssignmentTarget::NewCollection(_)))
            .unwrap();
        assert_eq!(suggestion.state, AssignmentState::Suggested);
        assert_eq!(suggestion.label, "Fake / Collection");
        assert_eq!(
            library.item_enrichment(id).unwrap().tldr,
            "A fake summary of the item."
        );
        assert!(library.library_jobs().is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failures_wait_longer_then_fail_and_refusals_pause() {
        let mut job = LibraryJob {
            id: glyphst_library::LibraryJobId(1),
            item_id: DocumentId(1),
            kind: LibraryJobKind::Enrich,
            origin: LibraryJobOrigin::OnAdd,
            state: LibraryJobState::Running,
            attempts: 1,
            force: false,
            created: 0,
        };
        assert!(matches!(
            retry_state(&job, "timeout"),
            LibraryJobState::Waiting { .. }
        ));
        job.attempts = 3;
        assert!(matches!(
            retry_state(&job, "timeout"),
            LibraryJobState::Failed { .. }
        ));
        assert!(refused("Fake answered 401 Unauthorized"));
        assert!(!refused("connection reset"));
    }
}
