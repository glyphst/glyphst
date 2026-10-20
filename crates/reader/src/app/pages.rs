//! Concept pages from the app's side: what a page is written from, the
//! cost asked first (04s), the writer's thread started, and its pages put
//! into the vault as they come back.

use glyphst_ai::pages::{PageInput, PageSource, estimate_tokens};
use glyphst_core::DocumentId;
use glyphst_library::PageTopic;

use super::ReaderApp;
use crate::{
    knowledge::{PageAsk, TopicKind},
    page_writer::{PageJob, PageResult},
};

/// Sources a page draws on, at most.
const PAGE_SOURCES: usize = 8;

impl ReaderApp {
    /// An item as a page's source: its note's name, title and summary.
    fn page_source(&self, id: DocumentId) -> Option<PageSource> {
        let item = self.library.library_item(id)?;
        let slug = self
            .library
            .vault_note(id)
            .and_then(|note| {
                std::path::Path::new(&note.relative_path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| {
                let authors = item
                    .creators
                    .iter()
                    .map(|creator| creator.name.clone())
                    .collect::<Vec<_>>();
                glyphst_vault::slug(&authors, item.year, &item.title)
            });
        Some(PageSource {
            id: id.0,
            slug,
            title: item.title.clone(),
            year: item.year,
            tldr: self
                .library
                .item_enrichment(id)
                .map(|enrichment| enrichment.tldr.clone())
                .unwrap_or_default(),
        })
    }

    /// A page id no page has, nor one being written.
    fn new_page_id(&self) -> u64 {
        let writing = self.page_writing.iter().map(|(id, _)| id + 1).max();
        self.library.next_page_id().max(writing.unwrap_or(0))
    }

    /// What a page asked for is written from.
    fn page_job(&self, ask: &PageAsk) -> Result<PageJob, String> {
        let (page_id, title, about, topic, items) = match ask {
            PageAsk::Topic(topic) => (
                self.new_page_id(),
                topic.title.clone(),
                match topic.kind {
                    TopicKind::Tag(_) => format!("The papers tagged #{}.", topic.title),
                    TopicKind::Cluster => {
                        "A group of papers in the library close in meaning; name what they share."
                            .to_string()
                    }
                },
                topic.topic(),
                topic.items.clone(),
            ),
            PageAsk::Again(id) => {
                let page = self.library.page(*id).ok_or("the page is gone")?;
                (
                    *id,
                    page.title.clone(),
                    "Write the page again from its sources.".to_string(),
                    page.topic.clone(),
                    page.sources.clone(),
                )
            }
            PageAsk::Asked(text) => (
                self.new_page_id(),
                text.clone(),
                "What the user asked for a page on.".to_string(),
                PageTopic::Asked(text.clone()),
                Vec::new(),
            ),
        };
        let sources = items
            .iter()
            .take(PAGE_SOURCES)
            .filter_map(|id| self.page_source(*id))
            .collect();
        let known_notes = self
            .vault_sync
            .index()
            .notes
            .iter()
            .map(|note| note.key.clone())
            .collect();
        Ok(PageJob {
            page_id,
            topic,
            input: PageInput {
                title,
                about,
                sources,
            },
            known_notes,
        })
    }

    /// The cost dialog for a page, or why it cannot be written now.
    pub(super) fn page_dialog(
        &self,
        ask: &PageAsk,
    ) -> Result<crate::library_dialogs::Dialog, String> {
        if self.vault_root().is_none() {
            return Err(
                "Choose a library folder first: pages are written into its notes/ folder."
                    .to_string(),
            );
        }
        let settings = self
            .user_settings
            .get()
            .ok_or("Settings are not loaded yet.")?;
        let model = settings.ai.resolved_main();
        if !model.can_request() {
            return Err("Choose a main model in Settings › AI first.".to_string());
        }
        let job = self.page_job(ask)?;
        let mut estimate = job.input.clone();
        if estimate.sources.is_empty() {
            // Asked: about as many as the search will find.
            estimate.sources = vec![PageSource::default(); PAGE_SOURCES];
        }
        let (tokens_in, tokens_out) = estimate_tokens(&estimate);
        Ok(crate::library_dialogs::Dialog::WritePage {
            asked: matches!(ask, PageAsk::Asked(_)),
            job: Box::new(job),
            model: model.model,
            tokens_in,
            tokens_out,
        })
    }

    /// Starts writing `job`; an asked page finds its sources first.
    pub(super) fn start_page(&mut self, mut job: PageJob, ctx: &egui::Context) {
        if job.input.sources.is_empty() {
            let found = self
                .search_index
                .open()
                .and_then(|store| {
                    store.search(&glyphst_search::SearchRequest {
                        text: job.input.title.clone(),
                        limit: PAGE_SOURCES,
                        ..Default::default()
                    })
                })
                .map(|results| {
                    results
                        .items
                        .iter()
                        .map(|item| DocumentId(item.item_id))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            job.input.sources = found
                .into_iter()
                .filter_map(|id| self.page_source(id))
                .collect();
            if job.input.sources.is_empty() {
                self.notices.push(format!(
                    "Nothing in the library matched “{}”; no page was written.",
                    job.input.title
                ));
                return;
            }
            job.topic = PageTopic::Asked(job.input.title.clone());
        }
        let Some(settings) = self.user_settings.get() else {
            return;
        };
        self.page_writing
            .push((job.page_id, job.input.title.clone()));
        self.page_writer.start(
            job,
            settings.ai.resolved_main(),
            self.search_index.clone(),
            ctx,
        );
    }

    /// Puts pages written into the vault, and says how it went.
    pub(super) fn poll_pages(&mut self) {
        for result in self.page_writer.take() {
            match result {
                PageResult::Written(page) => {
                    let id = page.job.page_id;
                    self.page_writing.retain(|(writing, _)| *writing != id);
                    let Some(root) = self.vault_root() else {
                        self.notices.push(
                            "The page was written, but there is no vault to keep it.".to_string(),
                        );
                        continue;
                    };
                    let index = self.vault_sync.index();
                    match crate::page_writer::apply(&mut self.library, &root, &index, &page) {
                        Ok(applied) => {
                            let dropped = if page.dropped > 0 {
                                format!("; {} it could not show were dropped", page.dropped)
                            } else {
                                String::new()
                            };
                            let beside = if applied.beside_edit {
                                " Your edit stays; the new text waits beside it."
                            } else {
                                ""
                            };
                            self.notices.push(format!(
                                "Wrote the page “{}”: {} citations{dropped}.{beside}",
                                page.job.input.title, page.cited
                            ));
                        }
                        Err(error) => self
                            .notices
                            .push(format!("Could not write the page: {error:#}")),
                    }
                }
                PageResult::Failed {
                    page_id,
                    title,
                    error,
                } => {
                    self.page_writing.retain(|(writing, _)| *writing != page_id);
                    self.notices
                        .push(format!("Could not write the page “{title}”: {error}"));
                }
            }
        }
    }

    /// The vault, when there is one.
    pub(super) fn vault_root(&self) -> Option<std::path::PathBuf> {
        self.prefs
            .library
            .vault_root(self.library.files_root(), self.library.files_root_chosen())
    }
}
