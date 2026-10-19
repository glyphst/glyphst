//! Importing a Zotero library (frame 04j). A worker reads a copy of
//! Zotero's database, copies (or links) each item's PDF or EPUB and places
//! its PDF annotations on the page; the UI thread adds the items a batch a
//! frame (one catalog write each) with their highlights. Run again, it
//! skips what it brought in before.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
    time::Instant,
};

use egui::{Rect, Sense, Stroke, pos2, vec2};
use glyphst_library::{
    DocumentId, HighlightColor, HighlightRect, ImportEntry, ImportEntryOutcome, ImportFile,
    LibraryCreator, LibraryCreatorRole, LibraryIdentifiers, LibraryItem, LibraryItemKind,
    LibraryItemMetadata, LibraryStore, MetadataRecord, MetadataSource,
};
use glyphst_ui::{button::Button, icons, toggle::Checkbox};
use glyphst_zotero_import as zotero;

use crate::{
    library_dialogs::{facts_keyed, foot, head},
    prefs::ImportMode,
    theme::{self, Tokens},
};

/// Items added per frame, in one catalog write.
const BATCH: usize = 24;

/// What the wizard brings in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    pub mode: ImportMode,
    pub catalog: bool,
    pub notes: bool,
    pub highlights: bool,
    pub trash: bool,
    /// Ask to run the AI pass on what came in, when it is done.
    pub enrich: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            mode: ImportMode::Copy,
            catalog: true,
            notes: true,
            highlights: true,
            trash: false,
            enrich: false,
        }
    }
}

/// The library's size, read before importing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    pub items: usize,
    pub papers: usize,
    pub books: usize,
    pub collections: usize,
    pub depth: usize,
    pub tags: usize,
    pub coloured: usize,
    pub automatic: usize,
    pub pdf: usize,
    pub epub: usize,
    pub missing: usize,
    pub notes: usize,
    pub annotations: usize,
    pub highlights: usize,
    pub skipped: usize,
}

/// A page annotation, placed.
enum Mark {
    Highlight(Vec<HighlightRect>, HighlightColor, String, String),
    Note(Vec<HighlightRect>, HighlightColor, String, String),
}

/// One item made ready on the worker.
struct Ready {
    entry: ImportEntry,
    /// The same work already in the library (by DOI, ISBN or title).
    same_work: Option<DocumentId>,
    marks: Vec<Mark>,
    skipped: usize,
    skipped_epub: usize,
    /// The file Zotero names but that is not there: whose, and where.
    missing: Option<String>,
}

enum Event {
    Read(Preview),
    Ready(Box<Ready>),
    Already,
    Failed(String),
    Done,
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub total: usize,
    pub processed: usize,
    pub added: usize,
    pub merged: usize,
    pub already: usize,
    pub collections: usize,
    pub tags: usize,
    pub highlights: usize,
    pub notes: usize,
    pub skipped: usize,
    pub skipped_epub: usize,
    pub missing: Vec<String>,
    pub failed: Vec<String>,
    pub finished: bool,
    pub seconds: u64,
}

/// An import under way.
struct Run {
    events: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    report: Report,
    added: Vec<DocumentId>,
    /// The library's collections and tags before, to count what came.
    before: (usize, usize),
    pending: Vec<Ready>,
    standalone: Receiver<Vec<(String, String)>>,
}

impl Run {
    fn start(
        data_dir: PathBuf,
        library: &LibraryStore,
        options: ImportOptions,
        ctx: &egui::Context,
    ) -> Self {
        let (sender, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let ctx = ctx.clone();
        let files_root = library.files_root().map(Path::to_path_buf);
        let existing = library.library_items().to_vec();
        let known = library
            .library_items()
            .iter()
            .filter_map(|item| library.metadata_record(item.id))
            .flat_map(|record| record.external_ids.iter().cloned())
            .collect::<HashSet<_>>();
        let (standalone_sender, standalone) = channel();
        std::thread::spawn(move || {
            let scratch = scratch_dir("import");
            let read = zotero::read_data_dir(&data_dir, &scratch, None);
            let _ = std::fs::remove_dir_all(&scratch);
            let zotero_library = match read {
                Ok(library) => library,
                Err(error) => {
                    let _ = sender.send(Event::Failed(format!("{error:#}")));
                    let _ = sender.send(Event::Done);
                    ctx.request_repaint();
                    return;
                }
            };
            let items = zotero_library
                .items
                .iter()
                .filter(|item| options.trash || !item.trashed)
                .collect::<Vec<_>>();
            let _ = sender.send(Event::Read(Preview {
                items: items.len(),
                ..preview(&zotero_library)
            }));
            let _ = standalone_sender.send(if options.notes {
                zotero_library.standalone_notes.clone()
            } else {
                Vec::new()
            });
            for item in items {
                if worker_cancel.load(Ordering::Acquire) {
                    break;
                }
                let event = if known.contains(&external_id(item)) {
                    Event::Already
                } else {
                    Event::Ready(Box::new(prepare(
                        item,
                        &zotero_library,
                        &existing,
                        files_root.as_deref(),
                        options,
                        &worker_cancel,
                    )))
                };
                if sender.send(event).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            let _ = sender.send(Event::Done);
            ctx.request_repaint();
        });
        Self {
            events,
            cancel,
            started: Instant::now(),
            report: Report::default(),
            added: Vec::new(),
            before: (
                library.library_collections().len(),
                library.library_tags().len(),
            ),
            pending: Vec::new(),
            standalone,
        }
    }

    /// Adds what the worker made ready since the last frame.
    fn poll(&mut self, library: &mut LibraryStore, vault: Option<&Path>) {
        if self.report.finished {
            return;
        }
        let mut done = false;
        while self.pending.len() < BATCH {
            let Ok(event) = self.events.try_recv() else {
                break;
            };
            match event {
                Event::Read(preview) => self.report.total = preview.items,
                Event::Already => {
                    self.report.processed += 1;
                    self.report.already += 1;
                }
                Event::Failed(error) => self.report.failed.push(error),
                Event::Ready(ready) => self.pending.push(*ready),
                Event::Done => {
                    done = true;
                    break;
                }
            }
        }
        if !self.pending.is_empty() {
            self.add(library);
        }
        self.report.collections = library
            .library_collections()
            .len()
            .saturating_sub(self.before.0);
        self.report.tags = library.library_tags().len().saturating_sub(self.before.1);
        self.report.seconds = self.started.elapsed().as_secs();
        if done {
            let standalone = self.standalone.try_recv().unwrap_or_default();
            self.report.notes += write_standalone(&standalone, vault);
            self.report.finished = true;
        }
    }

    fn add(&mut self, library: &mut LibraryStore) {
        let mut entries = Vec::new();
        let mut marks = Vec::new();
        for ready in self.pending.drain(..) {
            let report = &mut self.report;
            report.processed += 1;
            report.skipped += ready.skipped;
            report.skipped_epub += ready.skipped_epub;
            report.missing.extend(ready.missing);
            if let Some(existing) = ready.same_work {
                // The work is there already: it gets Zotero's id, so a
                // second run skips it too.
                let mut record = library
                    .metadata_record(existing)
                    .cloned()
                    .unwrap_or_default();
                record.external_ids.extend(ready.entry.record.external_ids);
                if let Err(error) = library.set_metadata_record(existing, record) {
                    report.failed.push(format!("{error:#}"));
                }
                report.merged += 1;
                continue;
            }
            if !ready.entry.note.is_empty() {
                report.notes += 1;
            }
            marks.push(ready.marks);
            entries.push(ready.entry);
        }
        if entries.is_empty() {
            return;
        }
        let outcomes = match library.import_entries(entries, "zotero") {
            Ok(outcomes) => outcomes,
            Err(error) => {
                self.report.failed.push(format!("{error:#}"));
                return;
            }
        };
        for (outcome, marks) in outcomes.into_iter().zip(marks) {
            let id = match outcome {
                ImportEntryOutcome::Added(id) => id,
                ImportEntryOutcome::Duplicate(_) => {
                    self.report.merged += 1;
                    continue;
                }
            };
            self.report.added += 1;
            self.added.push(id);
            let Some(path) = library
                .primary_attachment(id)
                .map(|attachment| attachment.path.clone())
            else {
                continue;
            };
            for mark in marks {
                let placed = match mark {
                    Mark::Highlight(rects, color, text, comment) => library
                        .add_highlight(&path, color, rects, text, comment)
                        .map(drop),
                    Mark::Note(rects, color, text, body) => {
                        library.add_note(&path, color, rects, text, body).map(drop)
                    }
                };
                match placed {
                    Ok(()) => self.report.highlights += 1,
                    Err(error) => self.report.failed.push(format!("{error:#}")),
                }
            }
        }
    }
}

fn scratch_dir(purpose: &str) -> PathBuf {
    std::env::temp_dir().join(format!("glyphst-zotero-{purpose}-{}", std::process::id()))
}

fn external_id(item: &zotero::Item) -> String {
    format!("zotero:{}/{}", item.library_id, item.key)
}

/// Counts what `library` holds, for the wizard.
pub fn preview(library: &zotero::Library) -> Preview {
    let mut preview = Preview {
        items: library.items.len(),
        collections: library.collections.len(),
        depth: library
            .collections
            .iter()
            .map(|collection| library.collection_path(collection.id).len())
            .max()
            .unwrap_or(0),
        coloured: library.tag_colors.len(),
        notes: library.standalone_notes.len(),
        ..Preview::default()
    };
    let mut tags = HashSet::new();
    for item in &library.items {
        match kind(&item.item_type) {
            LibraryItemKind::Book => preview.books += 1,
            LibraryItemKind::Paper => preview.papers += 1,
            LibraryItemKind::Other => {}
        }
        for (tag, automatic) in &item.tags {
            if *automatic {
                preview.automatic += 1;
            } else {
                tags.insert(tag.to_lowercase());
            }
        }
        preview.notes += item.notes.len();
        for attachment in item.attachments.iter().filter(|a| is_document(a)) {
            match &attachment.file {
                None => preview.missing += 1,
                Some(_) if is_epub(attachment) => preview.epub += 1,
                Some(_) => preview.pdf += 1,
            }
            for annotation in &attachment.annotations {
                preview.annotations += 1;
                match annotation.kind {
                    zotero::AnnotationKind::Highlight | zotero::AnnotationKind::Underline => {
                        preview.highlights += 1;
                    }
                    zotero::AnnotationKind::Ink | zotero::AnnotationKind::Image => {
                        preview.skipped += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    preview.tags = tags.len();
    preview
}

fn kind(item_type: &str) -> LibraryItemKind {
    match item_type {
        "journalArticle" | "preprint" | "conferencePaper" | "thesis" | "report" | "manuscript" => {
            LibraryItemKind::Paper
        }
        "book" | "bookSection" => LibraryItemKind::Book,
        _ => LibraryItemKind::Other,
    }
}

fn is_epub(attachment: &zotero::Attachment) -> bool {
    attachment.content_type == "application/epub+zip"
        || attachment
            .file
            .as_deref()
            .and_then(Path::extension)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
}

fn is_document(attachment: &zotero::Attachment) -> bool {
    attachment.content_type == "application/pdf"
        || attachment.content_type == "application/epub+zip"
        || attachment
            .file
            .as_deref()
            .is_some_and(crate::import::is_library_document)
}

/// The colour of Glyphst's five nearest to a Zotero colour.
pub fn nearest_color(hex: &str) -> HighlightColor {
    let parse = |hex: &str| {
        let hex = hex.trim_start_matches('#');
        let value = u32::from_str_radix(hex.get(..6)?, 16).ok()?;
        Some((
            (value >> 16) as i32,
            ((value >> 8) & 0xff) as i32,
            (value & 0xff) as i32,
        ))
    };
    let Some((r, g, b)) = parse(hex) else {
        return HighlightColor::Yellow;
    };
    HighlightColor::ALL
        .into_iter()
        .min_by_key(|color| {
            let c = crate::annotations::color(*color);
            let (dr, dg, db) = (r - c.r() as i32, g - c.g() as i32, b - c.b() as i32);
            dr * dr + dg * dg + db * db
        })
        .unwrap_or(HighlightColor::Yellow)
}

/// Copies or links the item's document and places its annotations.
fn prepare(
    item: &zotero::Item,
    library: &zotero::Library,
    existing: &[LibraryItem],
    files_root: Option<&Path>,
    options: ImportOptions,
    cancel: &AtomicBool,
) -> Ready {
    let metadata = metadata_of(item);
    let attachment = item
        .attachments
        .iter()
        .find(|attachment| is_document(attachment) && attachment.file.is_some());
    let missing = match attachment {
        Some(_) => None,
        None => item
            .attachments
            .iter()
            .find(|attachment| is_document(attachment))
            .map(|attachment| format!("{} · {}", byline(item), attachment.zotero_path)),
    };
    let mut ready = Ready {
        entry: ImportEntry {
            note: if options.notes {
                item.notes
                    .iter()
                    .map(|note| zotero::html_to_markdown(note))
                    .filter(|note| !note.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n")
            } else {
                String::new()
            },
            tags: tags_of(item, options, false),
            source_tags: tags_of(item, options, true),
            collections: if options.catalog {
                item.collections
                    .iter()
                    .map(|id| library.collection_path(*id))
                    .filter(|path| !path.is_empty())
                    .collect()
            } else {
                Vec::new()
            },
            record: MetadataRecord {
                source: MetadataSource::Zotero,
                fetched_at: glyphst_library::current_timestamp(),
                external_ids: vec![external_id(item)],
                ..MetadataRecord::default()
            },
            trashed: item.trashed,
            metadata: metadata.clone(),
            file: None,
        },
        same_work: None,
        marks: Vec::new(),
        skipped: 0,
        skipped_epub: 0,
        missing,
    };
    // A work already in the library keeps its file; Zotero's is not copied.
    if let Some((id, _)) = glyphst_metadata::find_duplicate(&candidate(&metadata), existing) {
        ready.same_work = Some(id);
        return ready;
    }
    let Some(attachment) = attachment else {
        return ready;
    };
    let Some(path) = attachment.file.as_deref() else {
        return ready;
    };
    let copy = options.mode == ImportMode::Copy && attachment.link_mode == zotero::LinkMode::Stored;
    ready.entry.file = match (copy, files_root) {
        (true, Some(root)) => glyphst_library::prepare_managed_import(root, path, cancel)
            .ok()
            .map(ImportFile::Managed),
        _ => glyphst_library::prepare_linked_import(path, cancel)
            .ok()
            .map(ImportFile::Linked),
    };
    if options.highlights {
        place_marks(path, attachment, &mut ready);
    }
    ready
}

/// Zotero's PDF annotations in Glyphst's page space: highlights and
/// underlines as highlights, notes and text boxes as notes. Ink, images,
/// EPUB positions and rotated pages are counted and left out.
fn place_marks(path: &Path, attachment: &zotero::Attachment, ready: &mut Ready) {
    if attachment.annotations.is_empty() {
        return;
    }
    if is_epub(attachment) {
        ready.skipped_epub += attachment.annotations.len();
        return;
    }
    let pages = attachment
        .annotations
        .iter()
        .filter_map(|annotation| annotation.position.as_ref().map(|(page, _)| *page))
        .collect::<Vec<_>>();
    let boxes = glyphst_pdf::page_boxes(path, &pages).unwrap_or_default();
    for annotation in &attachment.annotations {
        let placed = annotation.position.as_ref().and_then(|(page, rects)| {
            let page_box = boxes.get(page).filter(|page_box| page_box.rotate == 0)?;
            let height = page_box.y1 - page_box.y0;
            Some(
                rects
                    .iter()
                    .map(|rect| {
                        let [x0, y0, x1, y1] =
                            zotero::to_top_left(*rect, height, (page_box.x0, page_box.y0));
                        HighlightRect {
                            page_idx: *page,
                            x0,
                            y0,
                            x1,
                            y1,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let color = nearest_color(&annotation.color);
        let (text, comment) = (annotation.text.clone(), annotation.comment.clone());
        match (annotation.kind, placed) {
            (
                zotero::AnnotationKind::Highlight | zotero::AnnotationKind::Underline,
                Some(rects),
            ) => {
                ready
                    .marks
                    .push(Mark::Highlight(rects, color, text, comment));
            }
            (zotero::AnnotationKind::Note | zotero::AnnotationKind::Text, Some(rects)) => {
                let body = if comment.is_empty() {
                    text.clone()
                } else {
                    comment
                };
                ready.marks.push(Mark::Note(rects, color, text, body));
            }
            _ => ready.skipped += 1,
        }
    }
}

fn tags_of(item: &zotero::Item, options: ImportOptions, automatic: bool) -> Vec<String> {
    if !options.catalog {
        return Vec::new();
    }
    item.tags
        .iter()
        .filter(|(_, from_zotero)| *from_zotero == automatic)
        .map(|(name, _)| name.clone())
        .collect()
}

/// `Vaswani et al. 2017`.
fn byline(item: &zotero::Item) -> String {
    let authors = match item.creators.as_slice() {
        [] => item.field("title").unwrap_or(&item.key).to_string(),
        [one] => one.last.clone(),
        [first, ..] => format!("{} et al.", first.last),
    };
    match item.year() {
        Some(year) => format!("{authors} {year}"),
        None => authors,
    }
}

fn candidate(metadata: &LibraryItemMetadata) -> LibraryItem {
    LibraryItem {
        id: DocumentId(0),
        kind: metadata.kind,
        title: metadata.title.clone(),
        creators: metadata.creators.clone(),
        year: metadata.year,
        publication: String::new(),
        publisher: String::new(),
        abstract_text: String::new(),
        subject: String::new(),
        language: String::new(),
        identifiers: metadata.identifiers.clone(),
        embedded_keywords: Vec::new(),
        favorite: false,
        status: glyphst_library::ReadingStatus::ToRead,
        work_note: String::new(),
        created: 0,
        updated: 0,
        trashed_at: None,
    }
}

fn metadata_of(item: &zotero::Item) -> LibraryItemMetadata {
    let field = |name: &str| item.field(name).unwrap_or_default().trim().to_string();
    let publication = [
        "publicationTitle",
        "proceedingsTitle",
        "bookTitle",
        "repository",
        "websiteTitle",
    ]
    .iter()
    .map(|name| field(name))
    .find(|value| !value.is_empty())
    .unwrap_or_default();
    LibraryItemMetadata {
        kind: kind(&item.item_type),
        title: field("title"),
        creators: item
            .creators
            .iter()
            .filter(|creator| !creator.name().is_empty())
            .map(|creator| LibraryCreator {
                name: creator.name(),
                role: match creator.role.as_str() {
                    "author" => LibraryCreatorRole::Author,
                    "editor" | "seriesEditor" | "bookAuthor" => LibraryCreatorRole::Editor,
                    _ => LibraryCreatorRole::Other,
                },
            })
            .collect(),
        year: item.year(),
        publication,
        publisher: field("publisher"),
        abstract_text: field("abstractNote"),
        subject: String::new(),
        language: field("language"),
        identifiers: LibraryIdentifiers {
            doi: item
                .field("DOI")
                .map(str::to_lowercase)
                .filter(|doi| !doi.starts_with("10.48550/arxiv.")),
            isbn: item
                .field("ISBN")
                .and_then(glyphst_metadata::identifiers::normalize_isbn),
            arxiv: item.arxiv(),
        },
        embedded_keywords: Vec::new(),
    }
}

/// Writes notes that belong to no item into the vault's `zotero/` folder.
fn write_standalone(notes: &[(String, String)], vault: Option<&Path>) -> usize {
    let Some(vault) = vault else {
        return 0;
    };
    let folder = vault.join("zotero");
    if notes.is_empty() || std::fs::create_dir_all(&folder).is_err() {
        return 0;
    }
    let mut written = 0;
    for (index, (title, html)) in notes.iter().enumerate() {
        let name = title
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let name = if name.is_empty() {
            format!("note {}", index + 1)
        } else {
            name.chars().take(80).collect()
        };
        let path = folder.join(format!("{name}.md"));
        if path.exists() {
            continue;
        }
        if std::fs::write(&path, zotero::html_to_markdown(html) + "\n").is_ok() {
            written += 1;
        }
    }
    written
}

/// `1,284`.
fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn home_relative(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// What the dialog asks of the app.
pub enum ZoteroAction {
    OpenLibrary,
    /// Run the AI pass over these (asks with an estimate first).
    Enrich(Vec<DocumentId>),
    SaveReport(String),
}

/// The import dialog (frame 04j): the data folder, its size and the
/// options, then the progress and the report.
pub struct ZoteroImport {
    data_dir: Option<PathBuf>,
    preview: Option<Result<Preview, String>>,
    reading: Option<Receiver<Result<Preview, String>>>,
    picked: Arc<Mutex<Option<PathBuf>>>,
    options: ImportOptions,
    run: Option<Run>,
}

impl ZoteroImport {
    pub fn new(ctx: &egui::Context) -> Self {
        let mut wizard = Self::new_empty();
        wizard.read(zotero::find_data_dir(), ctx);
        wizard
    }

    /// The dialog for the Zotero data folder `data_dir`.
    pub fn with_data_dir(data_dir: PathBuf, ctx: &egui::Context) -> Self {
        let mut wizard = Self::new_empty();
        wizard.read(Some(data_dir), ctx);
        wizard
    }

    fn new_empty() -> Self {
        Self {
            data_dir: None,
            preview: None,
            reading: None,
            picked: Arc::default(),
            options: ImportOptions::default(),
            run: None,
        }
    }

    /// Starts importing with the options as they are.
    pub fn import_now(&mut self, library: &LibraryStore, ctx: &egui::Context) {
        if let Some(data_dir) = self.data_dir.clone() {
            self.run = Some(Run::start(data_dir, library, self.options, ctx));
        }
    }

    /// Still reading the library or importing it.
    pub fn busy(&self) -> bool {
        self.reading.is_some() || self.run.as_ref().is_some_and(|run| !run.report.finished)
    }

    /// Reads the size of the library in `data_dir`, on a worker.
    fn read(&mut self, data_dir: Option<PathBuf>, ctx: &egui::Context) {
        self.data_dir = data_dir.clone();
        self.preview = None;
        self.reading = data_dir.map(|data_dir| {
            let (sender, receiver) = channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let scratch = scratch_dir("preview");
                let result = zotero::read_data_dir(&data_dir, &scratch, None)
                    .map(|library| preview(&library))
                    .map_err(|error| format!("{error:#}"));
                let _ = std::fs::remove_dir_all(&scratch);
                let _ = sender.send(result);
                ctx.request_repaint();
            });
            receiver
        });
    }

    /// Shows a folder dialog for a data folder Glyphst did not find.
    fn pick(&self, ctx: &egui::Context) {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("Zotero's data folder (with zotero.sqlite)")
            .pick_folder();
        let (picked, ctx) = (self.picked.clone(), ctx.clone());
        std::thread::spawn(move || {
            if let Some(folder) = pollster::block_on(dialog)
                && let Ok(mut picked) = picked.lock()
            {
                *picked = Some(folder.path().to_path_buf());
            }
            ctx.request_repaint();
        });
    }

    /// Adds what came in since the last frame and draws the dialog.
    /// Returns what it asks, and whether it closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        library: &mut LibraryStore,
        vault: Option<&Path>,
    ) -> (Option<ZoteroAction>, bool) {
        if let Some(reading) = &self.reading
            && let Ok(preview) = reading.try_recv()
        {
            self.preview = Some(preview);
            self.reading = None;
        }
        let picked = self.picked.lock().ok().and_then(|mut picked| picked.take());
        if let Some(folder) = picked {
            self.read(Some(folder), ctx);
        }
        if let Some(run) = &mut self.run {
            run.poll(library, vault);
        }
        let tokens = Tokens::get(ctx);
        let scrim = glyphst_ui::overlay::scrim(ctx, "zotero-scrim");
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let busy = self.run.as_ref().is_some_and(|run| !run.report.finished);
        if (escape || scrim.clicked()) && !busy {
            return (self.closing(), true);
        }
        let width = if self.run.is_some() { 560.0 } else { 620.0 };
        let screen = ctx.content_rect();
        let mut result = (None, false);
        egui::Area::new(egui::Id::unique("zotero-import"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - width / 2.0, screen.top() + 70.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(tokens.raised)
                    .stroke(Stroke::new(1.0, tokens.rule_strong))
                    .corner_radius(tokens.pop_radius())
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_width(width - 40.0);
                        ui.spacing_mut().item_spacing.y = 14.0;
                        result = if self.run.is_some() {
                            self.report(ui, &tokens)
                        } else {
                            self.setup(ui, &tokens, library)
                        };
                    });
            });
        if result.1 && result.0.is_none() {
            result.0 = self.closing();
        }
        result
    }

    /// On closing after an import that asked for the AI pass, that pass.
    fn closing(&self) -> Option<ZoteroAction> {
        let run = self.run.as_ref()?;
        (self.options.enrich && run.report.finished && !run.added.is_empty())
            .then(|| ZoteroAction::Enrich(run.added.clone()))
    }

    fn setup(
        &mut self,
        ui: &mut egui::Ui,
        tokens: &Tokens,
        library: &LibraryStore,
    ) -> (Option<ZoteroAction>, bool) {
        head(ui, tokens, "download", "Import from Zotero");
        let mut close = false;
        self.source(ui, tokens);
        let Some(data_dir) = self.data_dir.clone() else {
            foot(ui, tokens, None, |ui| {
                close = ui.add(Button::ghost("cancel")).clicked();
            });
            return (None, close);
        };
        if zotero::is_running(&data_dir) {
            banner(
                ui,
                tokens,
                "zotero is running. glyphst reads a copy (with its write-ahead log), so nothing in zotero changes. close zotero for the most recent edits.",
            );
        }
        let preview = match &self.preview {
            None => {
                line(ui, "reading the library…", tokens.muted);
                return (None, false);
            }
            Some(Err(error)) => {
                line(ui, &format!("could not read it: {error}"), tokens.err);
                foot(ui, tokens, None, |ui| {
                    close = ui.add(Button::ghost("cancel")).clicked();
                });
                return (None, close);
            }
            Some(Ok(preview)) => preview.clone(),
        };
        let other = preview.items.saturating_sub(preview.papers + preview.books);
        facts_keyed(
            ui,
            tokens,
            100.0,
            &[
                (
                    "items",
                    format!(
                        "{}  · {} · {} · {} other",
                        thousands(preview.items),
                        plural(preview.papers, "paper"),
                        plural(preview.books, "book"),
                        thousands(other)
                    ),
                    None,
                ),
                (
                    "collections",
                    format!(
                        "{}  (nested {} deep)",
                        thousands(preview.collections),
                        preview.depth
                    ),
                    None,
                ),
                (
                    "tags",
                    format!(
                        "{}  · {} coloured · {} automatic",
                        thousands(preview.tags),
                        preview.coloured,
                        thousands(preview.automatic)
                    ),
                    None,
                ),
                (
                    "files",
                    format!(
                        "{} pdf · {} epub · {} missing",
                        thousands(preview.pdf),
                        thousands(preview.epub),
                        thousands(preview.missing)
                    ),
                    None,
                ),
                (
                    "notes",
                    format!("{}  → markdown in notes/", thousands(preview.notes)),
                    None,
                ),
                (
                    "annotations",
                    format!(
                        "{}  · {} highlights · {} ink & image (skipped)",
                        thousands(preview.annotations),
                        thousands(preview.highlights),
                        thousands(preview.skipped)
                    ),
                    None,
                ),
            ],
        );
        label(ui, tokens, "files");
        let root = library
            .files_root()
            .map(home_relative)
            .unwrap_or_else(|| "the library".to_string());
        let copy_label = format!("copy into {root}");
        glyphst_ui::segmented::labels(
            ui,
            &mut self.options.mode,
            &[
                (ImportMode::Copy, copy_label.as_str()),
                (ImportMode::Link, "link in place"),
            ],
        );
        label(ui, tokens, "include");
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(Checkbox::new(
                &mut self.options.catalog,
                "collections and tags (automatic tags marked import, easy to remove)",
            ));
            ui.add(Checkbox::new(
                &mut self.options.notes,
                "child notes and standalone notes",
            ));
            ui.add(Checkbox::new(
                &mut self.options.highlights,
                "pdf highlights and their comments",
            ));
            ui.add(Checkbox::new(
                &mut self.options.trash,
                "items in zotero's trash",
            ));
            ui.add(Checkbox::new(
                &mut self.options.enrich,
                "run the ai pass on imported items (asks with a cost estimate)",
            ));
        });
        let start = format!("import {} items", thousands(preview.items));
        let mut import = false;
        foot(
            ui,
            tokens,
            Some("run it again later: it updates, never duplicates"),
            |ui| {
                import = ui.add(Button::primary(&start)).clicked();
                close = ui.add(Button::ghost("cancel")).clicked();
            },
        );
        if import {
            self.run = Some(Run::start(data_dir, library, self.options, ui.ctx()));
        }
        (None, close)
    }

    /// The data folder found, and `[ other… ]`.
    fn source(&mut self, ui: &mut egui::Ui, tokens: &Tokens) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
        ui.painter().rect_filled(rect, 8.0, tokens.surface);
        icons::paint(
            ui,
            Rect::from_center_size(pos2(rect.left() + 17.0, rect.center().y), vec2(14.0, 14.0)),
            "database",
            14.0,
            tokens.muted,
        );
        let (path, found) = match &self.data_dir {
            Some(dir) => (
                home_relative(&dir.join("zotero.sqlite")),
                match &self.preview {
                    Some(Err(_)) => "unreadable",
                    _ => "found",
                },
            ),
            None => ("no zotero data folder in ~/Zotero".to_string(), ""),
        };
        // The path gives way to `found` and the button.
        let font = theme::mono(13.0);
        let room = width - 32.0 - 190.0;
        let path_width = glyphst_ui::text::width(ui, &path, &font).min(room);
        glyphst_ui::text::row(
            ui,
            pos2(rect.left() + 32.0, rect.center().y),
            &path,
            font,
            tokens.text,
            room,
        );
        ui.painter().text(
            pos2(rect.left() + 32.0 + path_width + 8.0, rect.center().y + 1.0),
            egui::Align2::LEFT_CENTER,
            found,
            theme::mono(10.0),
            if found == "found" {
                tokens.ok
            } else {
                tokens.err
            },
        );
        let mut button_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(vec2(10.0, 0.0)))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        if button_ui.add(Button::bracket_muted("other…")).clicked() {
            self.pick(ui.ctx());
        }
    }

    fn report(&mut self, ui: &mut egui::Ui, tokens: &Tokens) -> (Option<ZoteroAction>, bool) {
        let Some(run) = &self.run else {
            return (None, false);
        };
        let report = &run.report;
        let (icon, color, title) = if report.finished {
            ("check", tokens.ok, "Imported from Zotero")
        } else {
            ("download", tokens.accent_text, "Importing from Zotero")
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        icons::paint(
            ui,
            Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(18.0, 18.0)),
            icon,
            18.0,
            color,
        );
        ui.painter().text(
            pos2(rect.left() + 28.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            title,
            theme::semibold(17.0),
            tokens.text,
        );
        ui.painter().text(
            pos2(rect.right(), rect.center().y),
            egui::Align2::RIGHT_CENTER,
            duration(report.seconds),
            theme::mono(11.0),
            tokens.faint,
        );
        let fraction = if report.finished {
            1.0
        } else if report.total == 0 {
            0.0
        } else {
            report.processed as f32 / report.total as f32
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 15.0), Sense::hover());
        glyphst_ui::meter::Meter::new(fraction)
            .bar()
            .width(rect.width() * 0.62)
            .label(&if report.finished {
                "done".to_string()
            } else {
                format!(
                    "{} / {}",
                    thousands(report.processed),
                    thousands(report.total)
                )
            })
            .paint(ui, rect);
        let mut rows = vec![
            (
                "added",
                format!(
                    "{} items · {} collections · {} tags",
                    thousands(report.added),
                    thousands(report.collections),
                    thousands(report.tags)
                ),
                Some(tokens.text),
            ),
            (
                "merged",
                format!(
                    "{} already in the library (same doi or file){}",
                    thousands(report.merged),
                    if report.already > 0 {
                        format!(" · {} imported before", thousands(report.already))
                    } else {
                        String::new()
                    }
                ),
                None,
            ),
            (
                "notes",
                format!(
                    "{} written to notes/items/ and notes/zotero/",
                    thousands(report.notes)
                ),
                None,
            ),
            (
                "highlights",
                format!("{} placed on their pages", thousands(report.highlights)),
                None,
            ),
            (
                "skipped",
                format!(
                    "{} ink and image annotations · {} epub highlights",
                    thousands(report.skipped),
                    thousands(report.skipped_epub)
                ),
                Some(tokens.muted),
            ),
        ];
        if !report.missing.is_empty() {
            rows.push((
                "missing",
                format!("{} not found", plural(report.missing.len(), "file")),
                Some(tokens.err),
            ));
        }
        if let Some(error) = report.failed.first() {
            rows.push(("failed", error.clone(), Some(tokens.err)));
        }
        facts_keyed(ui, tokens, 100.0, &rows);
        if !report.missing.is_empty() {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for missing in report.missing.iter().take(2) {
                    line(ui, missing, tokens.faint);
                }
                if report.missing.len() > 2 {
                    line(
                        ui,
                        &format!("+{} more", report.missing.len() - 2),
                        tokens.faint,
                    );
                }
            });
        }
        let mut action = None;
        let mut close = false;
        let enrich = format!("enrich {}…", thousands(run.added.len()));
        foot(ui, tokens, None, |ui| {
            if !report.finished {
                if ui.add(Button::ghost("stop")).clicked() {
                    run.cancel.store(true, Ordering::Release);
                }
                return;
            }
            if ui.add(Button::primary("open library")).clicked() {
                action = Some(ZoteroAction::OpenLibrary);
                close = true;
            }
            if !run.added.is_empty() && ui.add(Button::ghost(&enrich)).clicked() {
                action = Some(ZoteroAction::Enrich(run.added.clone()));
                close = true;
            }
            if ui.add(Button::ghost("save report")).clicked() {
                action = Some(ZoteroAction::SaveReport(report_text(report)));
            }
        });
        (action, close)
    }
}

/// The report as Markdown, to keep.
fn report_text(report: &Report) -> String {
    let mut text = format!(
        "# Imported from Zotero\n\n- added: {} items, {} collections, {} tags\n- merged: {} already in the library, {} imported before\n- notes: {}\n- highlights: {}\n- skipped: {} ink and image annotations, {} epub highlights\n- took: {}\n",
        report.added,
        report.collections,
        report.tags,
        report.merged,
        report.already,
        report.notes,
        report.highlights,
        report.skipped,
        report.skipped_epub,
        duration(report.seconds),
    );
    if !report.missing.is_empty() {
        text.push_str(&format!(
            "\n## {} files not found\n\n",
            report.missing.len()
        ));
        for missing in &report.missing {
            text.push_str(&format!("- {missing}\n"));
        }
    }
    if !report.failed.is_empty() {
        text.push_str("\n## Failed\n\n");
        for failed in &report.failed {
            text.push_str(&format!("- {failed}\n"));
        }
    }
    text
}

/// `1 file`, `1,284 files`.
fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{} {noun}s", thousands(count)),
    }
}

/// `2 min 14 s`.
fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        _ => format!("{} min {} s", seconds / 60, seconds % 60),
    }
}

fn label(ui: &mut egui::Ui, tokens: &Tokens, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::mono(11.0))
            .color(tokens.muted),
    );
}

fn line(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::mono(11.0))
                .color(color),
        )
        .wrap(),
    );
}

/// The accent note that Zotero is running.
fn banner(ui: &mut egui::Ui, tokens: &Tokens, text: &str) {
    egui::Frame::new()
        .fill(tokens.accent_soft)
        .corner_radius(8.0)
        .inner_margin(egui::Margin {
            left: 32,
            right: 10,
            top: 8,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let top = ui.cursor().min;
            icons::paint(
                ui,
                Rect::from_min_size(pos2(top.x - 22.0, top.y), vec2(14.0, 14.0)),
                "info",
                14.0,
                tokens.accent_text,
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(theme::mono(11.0))
                        .color(tokens.accent_text),
                )
                .wrap(),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zotero_colours_snap_to_the_nearest_of_five() {
        assert_eq!(nearest_color("#ffd400"), HighlightColor::Yellow);
        assert_eq!(nearest_color("#5fb236"), HighlightColor::Green);
        assert_eq!(nearest_color("#2ea8e5"), HighlightColor::Blue);
        assert_eq!(nearest_color("#e56eee"), HighlightColor::Pink);
        assert_eq!(nearest_color("#f19837"), HighlightColor::Orange);
        assert_eq!(nearest_color("nonsense"), HighlightColor::Yellow);
    }

    #[test]
    fn a_zotero_item_maps_to_library_metadata() {
        let mut item = zotero::Item {
            item_type: "preprint".to_string(),
            ..Default::default()
        };
        for (field, value) in [
            ("title", "Attention Is All You Need"),
            ("date", "2017-06-12 2017-06-12"),
            ("archiveID", "arXiv:1706.03762"),
            ("DOI", "10.48550/arXiv.1706.03762"),
            ("repository", "arXiv"),
        ] {
            item.fields.insert(field.to_string(), value.to_string());
        }
        item.creators.push(zotero::Creator {
            first: "Ashish".to_string(),
            last: "Vaswani".to_string(),
            role: "author".to_string(),
        });
        let metadata = metadata_of(&item);
        assert_eq!(metadata.kind, LibraryItemKind::Paper);
        assert_eq!(metadata.year, Some(2017));
        assert_eq!(metadata.publication, "arXiv");
        assert_eq!(metadata.identifiers.arxiv.as_deref(), Some("1706.03762"));
        assert_eq!(
            metadata.identifiers.doi, None,
            "an arxiv doi is the preprint itself"
        );
        assert_eq!(metadata.creators[0].name, "Ashish Vaswani");
        assert_eq!(byline(&item), "Vaswani 2017");
    }

    #[test]
    fn counts_read_with_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1284), "1,284");
        assert_eq!(thousands(1_000_000), "1,000,000");
        assert_eq!(duration(134), "2 min 14 s");
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(1203, "book"), "1,203 books");
    }
}
