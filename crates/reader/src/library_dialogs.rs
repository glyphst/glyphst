//! The library's dialogs: enriching the existing library (frame 04h),
//! choosing the library folder and moving the files there (04k), and
//! adding a work by its DOI, arXiv id or ISBN (06d).

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
};

use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, vec2};
use glyphst_library::{DocumentId, FilesMovePlan, LibraryStore};
use glyphst_metadata::{Identifiers, MetadataClient, MetadataEndpoints, WorkMetadata};
use glyphst_ui::{button::Button, field::TextField, icons, key::Key, text, toggle::Checkbox};

use crate::theme::{self, Tokens};

/// What the dialog asks the app to do.
pub enum DialogAction {
    /// Queue the AI pass for these items.
    Enrich(Vec<DocumentId>),
    /// Move the library's files into this folder.
    UseFolder(PathBuf),
    /// Not now: don't ask about the folder again.
    LaterFolder,
    /// Add this work (downloading its PDF when it has an open one).
    AddWork(Box<WorkMetadata>),
    StopMove,
}

pub enum Lookup {
    Idle,
    Running {
        query: String,
        results: Receiver<Result<Option<WorkMetadata>, String>>,
    },
    Found(Box<WorkMetadata>),
    NotFound,
    Failed(String),
}

pub enum Dialog {
    Backfill {
        items: Vec<DocumentId>,
        model: String,
        tokens_in: u64,
        tokens_out: u64,
        create_tags: bool,
        suggest_collections: bool,
    },
    ChooseFolder {
        path: String,
        files: usize,
        bytes: u64,
        linked: usize,
        error: Option<String>,
    },
    Moving {
        done: usize,
        total: usize,
    },
    AddByIdentifier {
        text: String,
        lookup: Lookup,
        endpoints: MetadataEndpoints,
        mailto: Option<String>,
        looked_up: Option<String>,
    },
}

impl Dialog {
    pub fn add_by_identifier(mailto: Option<String>) -> Self {
        Self::AddByIdentifier {
            text: String::new(),
            lookup: Lookup::Idle,
            endpoints: MetadataEndpoints::default(),
            mailto,
            looked_up: None,
        }
    }

    /// Draws the dialog over everything; returns what it asks, and whether
    /// it closed.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        library: &LibraryStore,
    ) -> (Option<DialogAction>, bool) {
        let tokens = Tokens::get(ctx);
        let scrim = glyphst_ui::overlay::scrim(ctx, "library-dialog-scrim");
        let escape =
            ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let moving = matches!(self, Self::Moving { .. });
        if (escape || scrim.clicked()) && !moving {
            let action =
                matches!(self, Self::ChooseFolder { .. }).then_some(DialogAction::LaterFolder);
            return (action, true);
        }
        let width = match self {
            Self::AddByIdentifier { .. } => 600.0,
            Self::ChooseFolder { .. } => 600.0,
            _ => 560.0,
        };
        let screen = ctx.content_rect();
        let mut result = (None, false);
        egui::Area::new(egui::Id::unique("library-dialog"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos2(screen.center().x - width / 2.0, screen.top() + 110.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(tokens.raised)
                    .stroke(Stroke::new(1.0, tokens.rule_strong))
                    .corner_radius(tokens.pop_radius())
                    .inner_margin(egui::Margin::same(20))
                    .show(ui, |ui| {
                        ui.set_width(width - 40.0);
                        ui.spacing_mut().item_spacing.y = 12.0;
                        result = match self {
                            Self::Backfill { .. } => self.backfill(ui, &tokens),
                            Self::ChooseFolder { .. } => self.choose_folder(ui, &tokens),
                            Self::Moving { .. } => self.moving(ui, &tokens),
                            Self::AddByIdentifier { .. } => self.identifier(ui, &tokens, library),
                        };
                    });
            });
        result
    }

    fn backfill(&mut self, ui: &mut Ui, tokens: &Tokens) -> (Option<DialogAction>, bool) {
        let Self::Backfill {
            items,
            model,
            tokens_in,
            tokens_out,
            create_tags,
            suggest_collections,
        } = self
        else {
            return (None, false);
        };
        head(ui, tokens, "sparkles", "Enrich the library?");
        para(
            ui,
            tokens,
            "The library model will summarise, tag and file the items that have no AI pass yet. Every change shows in needs review and can be undone there.",
        );
        facts(
            ui,
            tokens,
            &[
                ("items", format!("{} without an ai pass", items.len()), None),
                ("model", format!("{model} · the library model"), None),
                (
                    "tokens",
                    format!(
                        "≈ {} in · {} out",
                        short_count(*tokens_in),
                        short_count(*tokens_out)
                    ),
                    Some(tokens.accent_text),
                ),
                (
                    "time",
                    format!(
                        "≈ {} min · 2 at a time · in the background",
                        (items.len() as f32 * 6.0 / 2.0 / 60.0).ceil().max(1.0)
                    ),
                    None,
                ),
            ],
        );
        ui.add(Checkbox::new(
            create_tags,
            "let ai add up to 2 new tags per item",
        ));
        ui.add(Checkbox::new(
            suggest_collections,
            "let ai propose new collections",
        ));
        let mut action = None;
        let mut close = false;
        foot(ui, tokens, Some("settings › ai › library model"), |ui| {
            if ui
                .add(Button::primary(&format!("enrich {}", items.len())))
                .clicked()
            {
                action = Some(DialogAction::Enrich(items.clone()));
                close = true;
            }
            if ui.add(Button::ghost("cancel")).clicked() {
                close = true;
            }
        });
        (action, close)
    }

    fn choose_folder(&mut self, ui: &mut Ui, tokens: &Tokens) -> (Option<DialogAction>, bool) {
        let Self::ChooseFolder {
            path,
            files,
            bytes,
            linked,
            error,
        } = self
        else {
            return (None, false);
        };
        head(ui, tokens, "folder-open", "Where should your library live?");
        para(
            ui,
            tokens,
            "Glyphst keeps your papers and books in one folder you can see, back up or sync with Syncthing, Nextcloud or WebDAV. Notes are Markdown files Obsidian can open.",
        );
        TextField::new(path, "~/Documents/Glyphst")
            .icon("folder")
            .width(ui.available_width())
            .show(ui);
        let mut layout = String::new();
        let root = path.trim().trim_end_matches('/');
        layout.push_str(&format!("{root}/\n├─ files/      pdfs, epubs and books\n├─ notes/      one .md per item · the vault\n│  └─ items/\n└─ .staging/   while files come in"));
        ui.label(
            egui::RichText::new(layout)
                .font(theme::mono(11.0))
                .color(tokens.text_2),
        );
        facts(
            ui,
            tokens,
            &[
                (
                    "moving",
                    format!(
                        "{files} managed files · {}",
                        crate::sidebar::format_size(*bytes)
                    ),
                    None,
                ),
                (
                    "database",
                    "stays in the app's data folder (not synced)".to_string(),
                    None,
                ),
                (
                    "linked",
                    format!("{linked} linked files stay where they are"),
                    None,
                ),
            ],
        );
        if let Some(error) = error {
            para(ui, tokens, error);
        }
        let mut action = None;
        let mut close = false;
        foot(
            ui,
            tokens,
            Some("change it any time in settings › library"),
            |ui| {
                if ui.add(Button::primary("use this folder")).clicked() {
                    action = Some(DialogAction::UseFolder(expand_home(path.trim())));
                }
                if ui.add(Button::ghost("later")).clicked() {
                    action = Some(DialogAction::LaterFolder);
                    close = true;
                }
            },
        );
        (action, close)
    }

    fn moving(&mut self, ui: &mut Ui, tokens: &Tokens) -> (Option<DialogAction>, bool) {
        let Self::Moving { done, total } = self else {
            return (None, false);
        };
        head(ui, tokens, "folder-open", "Moving the library");
        let fraction = if *total == 0 {
            0.0
        } else {
            *done as f32 / *total as f32
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 14.0), Sense::hover());
        glyphst_ui::meter::Meter::new(fraction)
            .bar()
            .width(rect.width() - 80.0)
            .label(&format!("{done} / {total}"))
            .paint(ui, rect);
        para(
            ui,
            tokens,
            "Each file is copied and checked before the library points at it; the old copies go last.",
        );
        let mut action = None;
        foot(ui, tokens, None, |ui| {
            if ui.add(Button::ghost("stop")).clicked() {
                action = Some(DialogAction::StopMove);
            }
        });
        (action, false)
    }

    fn identifier(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &LibraryStore,
    ) -> (Option<DialogAction>, bool) {
        let Self::AddByIdentifier {
            text: query,
            lookup,
            endpoints,
            mailto,
            looked_up,
        } = self
        else {
            return (None, false);
        };
        head(ui, tokens, "hash", "Add by identifier");
        let field = TextField::new(query, "a doi, an arxiv id or url, or an isbn")
            .width(ui.available_width())
            .id(egui::Id::unique("library-identifier"))
            .show(ui);
        if !field.has_focus() && query.is_empty() {
            field.request_focus();
        }
        let identifiers = parse_identifier(query);
        let wanted = identifier_label(&identifiers);
        // Look the identifier up when it changes.
        if wanted != *looked_up {
            looked_up.clone_from(&wanted);
            *lookup = match &wanted {
                None => Lookup::Idle,
                Some(wanted) => {
                    let (sender, results) = channel();
                    let endpoints = endpoints.clone();
                    let mailto = mailto.clone();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let found = MetadataClient::new(endpoints, mailto)
                            .and_then(|client| client.lookup(&identifiers))
                            .map_err(|error| format!("{error:#}"));
                        let _ = sender.send(found);
                        ctx.request_repaint();
                    });
                    Lookup::Running {
                        query: wanted.clone(),
                        results,
                    }
                }
            };
        }
        if let Lookup::Running { results, .. } = lookup
            && let Ok(found) = results.try_recv()
        {
            *lookup = match found {
                Ok(Some(work)) => Lookup::Found(Box::new(work)),
                Ok(None) => Lookup::NotFound,
                Err(error) => Lookup::Failed(error),
            };
        }
        let mut action = None;
        let mut close = false;
        match lookup {
            Lookup::Idle => para(
                ui,
                tokens,
                "Paste a DOI (10.1145/…), an arXiv id or link, or an ISBN.",
            ),
            Lookup::Running { query, .. } => para(ui, tokens, &format!("looking up {query}…")),
            Lookup::NotFound => para(ui, tokens, "No catalogue knows it."),
            Lookup::Failed(error) => para(
                ui,
                tokens,
                &format!("The catalogues could not be reached: {error}"),
            ),
            Lookup::Found(work) => {
                ui.label(
                    egui::RichText::new(&work.title)
                        .font(theme::semibold(15.0))
                        .color(tokens.text),
                );
                if !work.creators.is_empty() {
                    para(ui, tokens, &work.creators.join(", "));
                }
                let mut venue = Vec::new();
                if let Some(year) = work.year {
                    venue.push(year.to_string());
                }
                if !work.publication.is_empty() {
                    venue.push(work.publication.clone());
                }
                if let Some(label) = work.identifier_label() {
                    venue.push(label);
                }
                ui.label(
                    egui::RichText::new(venue.join(" · "))
                        .font(theme::mono(11.0))
                        .color(tokens.muted),
                );
                let duplicate = library.library_items().iter().find(|item| {
                    (work.doi.is_some() && item.identifiers.doi == work.doi)
                        || (work.arxiv.is_some() && item.identifiers.arxiv == work.arxiv)
                        || (work.isbn.is_some() && item.identifiers.isbn == work.isbn)
                });
                let lines = [
                    match &work.pdf_url {
                        Some(_) => ("✓", "a pdf is open to download", tokens.ok),
                        None => ("·", "no open pdf · added without a file", tokens.faint),
                    },
                    match duplicate {
                        Some(_) => ("≡", "already in your library", tokens.accent_text),
                        None => ("✓", "not in your library", tokens.ok),
                    },
                    (
                        "◆",
                        "the ai files and tags it after adding",
                        tokens.accent_text,
                    ),
                ];
                for (glyph, line, color) in lines {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(glyph)
                                .font(theme::mono(11.0))
                                .color(color),
                        );
                        ui.label(
                            egui::RichText::new(line)
                                .font(theme::mono(11.0))
                                .color(tokens.text_2),
                        );
                    });
                }
                if duplicate.is_none() {
                    foot(ui, tokens, Some("doi · arxiv id or url · isbn"), |ui| {
                        if ui.add(Button::primary("add")).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            action = Some(DialogAction::AddWork(work.clone()));
                            close = true;
                        }
                    });
                }
            }
        }
        (action, close)
    }
}

/// The identifier in what was typed: a DOI, an arXiv id or link, or an
/// ISBN.
pub fn parse_identifier(typed: &str) -> Identifiers {
    let typed = typed.trim();
    let doi = glyphst_metadata::identifiers::find_doi(typed);
    let arxiv = glyphst_metadata::identifiers::find_arxiv(typed).or_else(|| {
        // A bare new-style id, as people paste it.
        let bare = typed.trim_start_matches("arXiv:").trim();
        glyphst_metadata::identifiers::find_arxiv(&format!("arXiv:{bare}")).filter(|_| {
            bare.chars()
                .all(|c| c.is_ascii_digit() || c == '.' || c == 'v')
        })
    });
    let isbn = glyphst_metadata::identifiers::normalize_isbn(typed).filter(|_| {
        typed
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == ' ' || c == 'X')
    });
    Identifiers {
        // An arXiv DOI is the preprint.
        doi: doi.filter(|doi| !doi.starts_with("10.48550/arxiv.") || arxiv.is_none()),
        arxiv,
        isbn,
    }
}

fn identifier_label(identifiers: &Identifiers) -> Option<String> {
    identifiers
        .arxiv
        .as_ref()
        .map(|id| format!("arxiv {id}"))
        .or_else(|| identifiers.doi.as_ref().map(|doi| format!("doi {doi}")))
        .or_else(|| identifiers.isbn.as_ref().map(|isbn| format!("isbn {isbn}")))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

fn short_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{}k", count / 1_000)
    } else {
        count.to_string()
    }
}

fn head(ui: &mut Ui, tokens: &Tokens, icon: &str, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    icons::paint(
        ui,
        Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(18.0, 18.0)),
        icon,
        18.0,
        tokens.accent_text,
    );
    ui.painter().text(
        pos2(rect.left() + 28.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(17.0),
        tokens.text,
    );
    let key = Rect::from_min_max(pos2(rect.right() - 40.0, rect.top()), rect.max);
    let mut key_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(key)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    key_ui.add(Key("esc"));
}

fn para(ui: &mut Ui, tokens: &Tokens, line: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(line)
                .font(theme::regular(13.0))
                .color(tokens.text_2),
        )
        .wrap(),
    );
}

fn facts(ui: &mut Ui, tokens: &Tokens, rows: &[(&str, String, Option<egui::Color32>)]) {
    let width = ui.available_width();
    let (top, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter()
        .hline(top.x_range(), top.top(), tokens.hairline());
    ui.spacing_mut().item_spacing.y = 0.0;
    for (key, value, color) in rows {
        let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
        text::row(
            ui,
            pos2(rect.left(), rect.center().y),
            *key,
            theme::mono(11.0),
            tokens.faint,
            90.0,
        );
        text::row(
            ui,
            pos2(rect.left() + 102.0, rect.center().y),
            value,
            theme::mono(12.0),
            color.unwrap_or(tokens.text_2),
            width - 102.0,
        );
    }
    let (bottom, _) = ui.allocate_exact_size(vec2(width, 1.0), Sense::hover());
    ui.painter()
        .hline(bottom.x_range(), bottom.top(), tokens.hairline());
    ui.spacing_mut().item_spacing.y = 12.0;
}

fn foot(ui: &mut Ui, tokens: &Tokens, hint: Option<&str>, buttons: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        if let Some(hint) = hint {
            ui.label(
                egui::RichText::new(hint)
                    .font(theme::mono(10.0))
                    .color(tokens.faint),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            buttons(ui);
        });
    });
}

/// Moving the files on a worker: copies, then hands back the plan to commit.
pub struct FolderMove {
    pub plan: FilesMovePlan,
    progress: Receiver<MoveEvent>,
    cancel: Arc<AtomicBool>,
}

pub enum MoveEvent {
    Copied(usize, usize),
    Done(Result<(), String>),
}

impl FolderMove {
    pub fn start(plan: FilesMovePlan, ctx: &egui::Context) -> Self {
        let (sender, progress) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_plan = plan.clone();
        let worker_cancel = cancel.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let copied =
                glyphst_library::copy_planned_files(&worker_plan, &worker_cancel, |done, total| {
                    let _ = sender.send(MoveEvent::Copied(done, total));
                    ctx.request_repaint();
                });
            let _ = sender.send(MoveEvent::Done(
                copied.map_err(|error| format!("{error:#}")),
            ));
            ctx.request_repaint();
        });
        Self {
            plan,
            progress,
            cancel,
        }
    }

    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// The events since the last frame.
    pub fn events(&self) -> Vec<MoveEvent> {
        self.progress.try_iter().collect()
    }
}

/// A work, and its downloaded PDF made ready to add (or why not).
pub type ReadyWork = (
    Box<WorkMetadata>,
    Result<Option<glyphst_library::PreparedManagedImport>, String>,
);

/// A work added by its identifier: its open PDF is downloaded and made
/// ready on a worker, then added on the UI thread.
pub struct WorkAdd {
    results: Receiver<ReadyWork>,
}

impl WorkAdd {
    pub fn start(work: WorkMetadata, files_root: Option<PathBuf>, ctx: &egui::Context) -> Self {
        let (sender, results) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let prepared = match (&work.pdf_url, &files_root) {
                (Some(url), Some(root)) => download(url, root)
                    .map(Some)
                    .map_err(|error| format!("{error:#}")),
                _ => Ok(None),
            };
            let _ = sender.send((Box::new(work), prepared));
            ctx.request_repaint();
        });
        Self { results }
    }

    /// The work and its prepared file once ready.
    pub fn take(&self) -> Option<ReadyWork> {
        self.results.try_recv().ok()
    }
}

/// Downloads `url` (a PDF) and makes it ready to add to the library at
/// `root`.
fn download(
    url: &str,
    root: &std::path::Path,
) -> anyhow::Result<glyphst_library::PreparedManagedImport> {
    use std::io::Read as _;
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("GlyphstReader/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let response = client.get(url).send()?.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(512 * 1024 * 1024).read_to_end(&mut bytes)?;
    if !bytes.starts_with(b"%PDF") {
        anyhow::bail!("{url} is not a pdf");
    }
    let name = url
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(|name| {
            if name.to_lowercase().ends_with(".pdf") {
                name.to_string()
            } else {
                format!("{name}.pdf")
            }
        })
        .unwrap_or_else(|| "paper.pdf".to_string());
    let folder = std::env::temp_dir().join(format!("glyphst-download-{}", std::process::id()));
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(name);
    std::fs::write(&path, &bytes)?;
    let prepared = glyphst_library::prepare_managed_import(root, &path, &AtomicBool::new(false));
    let _ = std::fs::remove_file(&path);
    prepared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_typed_becomes_one_identifier() {
        let found = parse_identifier("https://arxiv.org/abs/2205.14135v2");
        assert_eq!(found.arxiv.as_deref(), Some("2205.14135"));
        let found = parse_identifier("2205.14135");
        assert_eq!(found.arxiv.as_deref(), Some("2205.14135"));
        let found = parse_identifier("doi:10.1145/3292500.3330701");
        assert_eq!(found.doi.as_deref(), Some("10.1145/3292500.3330701"));
        let found = parse_identifier("978-1-4493-7332-0");
        assert_eq!(found.isbn.as_deref(), Some("9781449373320"));
        assert!(parse_identifier("attention is all you need").is_empty());
        assert_eq!(short_count(1_240_000), "1.2M");
        assert_eq!(short_count(15_400), "15k");
    }
}
