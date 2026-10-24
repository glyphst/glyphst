//! Settings, after pdfcraft's preferences dialog: an in-app window with a
//! grouped section list at the left and the section's settings at the
//! right, in titled groups of rows. Changes apply at once and are saved
//! with the preferences (or the settings file). Keyboard shortcuts have
//! their own tab (`keymap_page.rs`).

use egui::{RichText, Sense, vec2};
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_effects::EffectDef;
use glyphst_ui::{
    Tip,
    button::Button,
    field::TextField,
    motif,
    row::ListRow,
    scheme::{Contrast, Scheme, SchemeStyle},
    settings as ui_settings,
    slider::Slider,
    theme::{Accent, Density},
    toggle::Switch,
};

use crate::{
    icons,
    prefs::{Colours, Prefs},
    settings_providers::ProviderPages,
    theme::{self, ThemeKind, ThemePreference, Tokens},
    user_settings::{Persistence, UserSettings},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Reading,
    Presenting,
    Library,
    Effects,
    Translation,
    Ocr,
    Ai,
    Models,
}

/// The section list's groups, top to bottom.
const GROUPS: [(&str, &[Section]); 4] = [
    (
        "general",
        &[
            Section::Appearance,
            Section::Reading,
            Section::Presenting,
            Section::Library,
        ],
    ),
    ("look", &[Section::Effects]),
    (
        "services",
        &[Section::Translation, Section::Ocr, Section::Ai],
    ),
    ("this computer", &[Section::Models]),
];

const SIDEBAR_WIDTH: f32 = 220.0;
const HEADER_HEIGHT: f32 = 40.0;

impl Section {
    pub const ALL: [Self; 9] = [
        Self::Appearance,
        Self::Reading,
        Self::Presenting,
        Self::Library,
        Self::Effects,
        Self::Translation,
        Self::Ocr,
        Self::Ai,
        Self::Models,
    ];

    /// The section a scripted run names: `appearance`, `ai`…
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| {
            section
                .label()
                .split_whitespace()
                .any(|word| word.eq_ignore_ascii_case(name))
        })
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Reading => "book-open",
            Self::Presenting => "presentation",
            Self::Library => "library-big",
            Self::Effects => "wand-sparkles",
            Self::Translation => "languages",
            Self::Ocr => "scan-text",
            Self::Ai => "sparkles",
            Self::Models => "cpu",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Reading => "Reading",
            Self::Presenting => "Presenting",
            Self::Library => "Library",
            Self::Effects => "Effects",
            Self::Translation => "Translation",
            Self::Ocr => "OCR",
            Self::Ai => "AI & agents",
            Self::Models => "Models",
        }
    }

    /// Words its settings answer to, for the settings search.
    fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "theme dark light system accent colour color scheme palette generate material \
                 font chrome prose ui scale size density transparency opacity blur glyph \
                 ornaments background wallpaper image dim parallax animation tint sepia night \
                 paper grain"
            }
            Self::Reading => "session reopen sidebar drawer vim keys motion",
            Self::Presenting => {
                "present presentation slides full screen fullscreen timer clock talk length \
                 countdown ring tools right click draw pen eraser laser page number"
            }
            Self::Library => {
                "library folder vault notes obsidian import zotero metadata lookup crossref \
                 arxiv openalex doi isbn enrich summary tags collections"
            }
            Self::Effects => "transition folder shader",
            Self::Translation => "translate deepl google microsoft language engine glossary",
            Self::Ocr => "ocr scan recognize text",
            Self::Ai => "ai model provider key agent claude codex mcp assistant",
            Self::Models => {
                "models download local gpu cuda coreml ocr pp-ocr indra embedding \
                 embeddinggemma qwen rerank reranker disk"
            }
        }
    }

    /// The line under the section's title.
    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "How Glyphst looks. Changes apply to every window immediately.",
            Self::Reading => "What opens at start, and what shows while you read.",
            Self::Presenting => "Full screen, the pointer, ink and the timer while you present.",
            Self::Library => {
                "Where your papers and books live, how they come in, and what the AI does \
                 with them."
            }
            Self::Effects => {
                "Book switches, page turns and your own effects. The background and \
                 the page tint are in Appearance."
            }
            Self::Translation => "The service that translates selections and documents.",
            Self::Ocr => "Recognizing the text of scanned pages.",
            Self::Ai => "Models and providers for the assistant and AI translation.",
            Self::Models => {
                "Models that run on this computer, for reading scans and searching by \
                 meaning. Downloads are pinned and checked; pages and passages never leave \
                 the computer."
            }
        }
    }

    /// Whether its settings live in the settings file (and keys in the
    /// keychain) rather than the app's preferences.
    fn in_settings_file(self) -> bool {
        matches!(
            self,
            Self::Appearance | Self::Translation | Self::Ocr | Self::Ai
        )
    }
}

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    section: Option<Section>,
    providers: ProviderPages,
    /// Settings · Models' downloads.
    models: crate::models_page::ModelsPage,
    /// What the Effects page asked for, for the app to do.
    pub effects_actions: Vec<crate::effects_page::Action>,
    /// What Settings › Library asked the app to do.
    pub library_actions: Vec<LibraryAction>,
    /// The Keyboard shortcuts row was clicked: the app opens their tab.
    pub open_shortcuts: bool,
    /// A group of the page to scroll to once drawn (scripted runs).
    pub scroll_to: Option<String>,
    /// Settings › Library lists the sync's conflicts.
    pub show_conflicts: bool,
    /// The store's secret as typed, until tested (then in the keychain).
    pub sync_secret: String,
    /// The settings search.
    query: String,
    /// Installed families for the font pickers (chrome, prose), read once.
    families: Option<(Vec<String>, Vec<String>)>,
}

/// What the window needs from the app.
pub struct SettingsContext<'a> {
    pub prefs: &'a mut Prefs,
    pub effects: &'a [EffectDef],
    /// Effects that failed to load, and the user's effects folder.
    pub effect_errors: &'a [String],
    pub effects_folder: Option<&'a std::path::Path>,
    /// Seconds, for previews.
    pub time: f32,
    pub user: &'a mut UserSettings,
    pub chatgpt: &'a ChatGptSubscription,
    /// The theme the desktop last asked for, if it ever did.
    pub system_theme: Option<ThemeKind>,
    /// Whether the compositor can blur behind the window; `None` until
    /// known.
    pub blur_supported: Option<bool>,
    /// The background's own colour, for a scheme generated from it.
    pub background_seed: Option<[u8; 3]>,
    /// Facts about the library for its page.
    pub library: LibraryFacts,
    /// This computer, for the models that suit it.
    pub machine: &'a glyphst_models::machine::Machine,
}

/// What Settings › Library shows about the library.
#[derive(Clone, Debug, Default)]
pub struct LibraryFacts {
    pub folder: Option<std::path::PathBuf>,
    pub folder_chosen: bool,
    pub unenriched: usize,
    pub model: Option<String>,
    pub zotero: Option<std::path::PathBuf>,
    /// The search index, when it runs: how far, and its size on disk.
    pub index: Option<(glyphst_search::IndexStatus, u64)>,
    /// Search by meaning's download and state.
    pub meaning: Option<crate::meaning::MeaningCard>,
    /// The reranker's.
    pub rerank: Option<crate::meaning::MeaningCard>,
    /// The desktop entry that opens `glyphst://` links, when known.
    pub link_handler: Option<String>,
    /// How the library's sync stands.
    pub sync: crate::library_sync::SyncStatus,
    /// Sync tools' copies of notes in the vault.
    pub note_copies: Vec<std::path::PathBuf>,
    /// Items whose files are linked from outside the folder, and their
    /// size.
    pub linked: (usize, u64),
}

/// `path` from the home folder, cut at the front to `chars`.
fn short_path(path: &std::path::Path, chars: usize) -> String {
    let shown = match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    };
    let count = shown.chars().count();
    if count <= chars {
        shown
    } else {
        let tail = shown.chars().skip(count - chars + 1).collect::<String>();
        format!("…{tail}")
    }
}

/// What Settings › Library asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryAction {
    ChooseFolder,
    Enrich,
    ImportZotero,
    /// Read the library's text again from the start.
    Reindex,
    Meaning(crate::meaning::MeaningAction),
    Rerank(crate::meaning::MeaningAction),
    /// Forget the searches and the passages opened from them.
    ForgetOpened,
    /// Make this Reader open `glyphst://` links from other apps.
    RegisterLinks,
    /// Read the other computers' changes now.
    SyncNow,
    /// Keep the typed secret in the keychain and try the store.
    TestSync,
    /// Copy linked files into the library folder.
    CopyLinked,
    /// Open a note's copy in the system's editor.
    OpenFile(std::path::PathBuf),
}

/// The caption of a downloaded model's row (search by meaning, the
/// reranker), as its card says.
fn model_caption(card: &crate::meaning::MeaningCard, model: &str) -> String {
    use crate::meaning::{MeaningCard, megabytes, speed};
    match card {
        MeaningCard::Offer { kept, total } if *kept > 0 => {
            format!("off · {} downloaded", megabytes(*kept, *total))
        }
        MeaningCard::Offer { total, .. } => format!(
            "off · {model} · {} MB, downloaded once",
            (*total as f64 / 1_000_000.0).round()
        ),
        MeaningCard::Downloading {
            what,
            done,
            total,
            rate,
            seconds_left,
        } => format!(
            "downloading the {what} · {} · {}",
            megabytes(*done, *total),
            speed(*rate, *seconds_left)
        ),
        MeaningCard::Retrying { seconds, .. } => {
            format!("network lost · trying again in {seconds} s")
        }
        MeaningCard::Paused { done, total } => {
            format!("paused · {} kept", megabytes(*done, *total))
        }
        MeaningCard::Failed { title, .. } => title.clone(),
        MeaningCard::Starting => "starting the engine".to_string(),
        MeaningCard::EngineFailed { log } => format!("the engine did not start · {log}"),
        MeaningCard::OnCpu => "on · on the CPU (no GPU it can use)".to_string(),
        MeaningCard::Ready => format!("on · {model} on this computer"),
    }
}

/// The buttons of a downloaded model's row.
fn model_buttons(
    ui: &mut egui::Ui,
    card: &crate::meaning::MeaningCard,
    mut act: impl FnMut(crate::meaning::MeaningAction),
) {
    use crate::meaning::{MeaningAction, MeaningCard};
    let mut button = |label: &str, accent: bool, action: MeaningAction| {
        let button = if accent {
            Button::bracket(label)
        } else {
            Button::bracket_muted(label)
        };
        if ui.add(button).clicked() {
            act(action);
        }
    };
    match card {
        MeaningCard::Offer { kept, .. } => button(
            if *kept > 0 { "resume" } else { "download…" },
            true,
            MeaningAction::Download,
        ),
        MeaningCard::Downloading { .. } | MeaningCard::Retrying { .. } => {
            button("pause", false, MeaningAction::Pause);
        }
        MeaningCard::Paused { .. } => {
            button("discard", false, MeaningAction::Discard);
            button("resume", true, MeaningAction::Download);
        }
        MeaningCard::Failed { .. } | MeaningCard::EngineFailed { .. } => {
            button("retry", true, MeaningAction::Download);
        }
        MeaningCard::Starting | MeaningCard::OnCpu | MeaningCard::Ready => {}
    }
}

/// The library's sync (frame 07s): how, how it stands, each computer and
/// what could not be kept.
fn sync_group(
    ui: &mut egui::Ui,
    library: &mut crate::prefs::LibraryPrefs,
    facts: &LibraryFacts,
    actions: &mut Vec<LibraryAction>,
    show_conflicts: &mut bool,
    sync_secret: &mut String,
) {
    use crate::library_sync::{ago, ahead};
    use crate::prefs::SyncMode;
    let status = &facts.sync;
    let mode = &mut library.sync;
    ui_settings::group(ui, "sync", |group| {
        let caption = match *mode {
            SyncMode::Folder | SyncMode::Off if !facts.folder_chosen => {
                "through a library folder a sync tool carries, or a webdav server or s3 bucket"
            }
            SyncMode::Off | SyncMode::Folder => {
                "a sync tool (syncthing, icloud, dropbox) carries the library folder"
            }
            SyncMode::WebDav => "nextcloud, a nas, any webdav · glyphst mirrors the library there",
            SyncMode::S3 => {
                "cloudflare r2, backblaze b2, any s3 · glyphst mirrors the library there"
            }
        };
        group.row("sync with", Some(caption), |ui| {
            let before = *mode;
            glyphst_ui::segmented::labels(
                ui,
                mode,
                &[
                    (SyncMode::Off, "off"),
                    (SyncMode::Folder, "library folder"),
                    (SyncMode::WebDav, "webdav"),
                    (SyncMode::S3, "s3"),
                ],
            );
            if *mode != before && *mode == SyncMode::Folder && !facts.folder_chosen {
                *mode = SyncMode::Off;
                actions.push(LibraryAction::ChooseFolder);
            }
        });
        if *mode == SyncMode::Off {
            return;
        }
        let tokens = theme::Tokens::get(group.ctx());
        let remote = &mut library.sync_remote;
        let field = |group: &mut ui_settings::Group<'_>,
                     label: &str,
                     caption: Option<&str>,
                     value: &mut String,
                     hint: &str| {
            group.row(label, caption, |ui| {
                TextField::new(value, hint).width(320.0).show(ui);
            });
        };
        match *mode {
            SyncMode::S3 => {
                field(
                    group,
                    "endpoint",
                    None,
                    &mut remote.s3_endpoint,
                    "https://<account>.r2.cloudflarestorage.com",
                );
                field(
                    group,
                    "region",
                    Some("r2: auto · b2: the bucket's, like us-west-004"),
                    &mut remote.s3_region,
                    "auto",
                );
                field(group, "bucket", None, &mut remote.s3_bucket, "glyphst");
                field(
                    group,
                    "prefix",
                    Some("a folder in the bucket, or empty"),
                    &mut remote.s3_prefix,
                    "library/",
                );
                field(group, "access key id", None, &mut remote.s3_key_id, "");
            }
            SyncMode::WebDav => {
                field(
                    group,
                    "url",
                    Some("the library's folder on the server"),
                    &mut remote.webdav_url,
                    "https://cloud.example.org/remote.php/dav/files/me/Glyphst",
                );
                field(group, "user", None, &mut remote.webdav_user, "");
            }
            SyncMode::Off | SyncMode::Folder => {}
        }
        if matches!(*mode, SyncMode::S3 | SyncMode::WebDav) {
            let label = if *mode == SyncMode::S3 {
                "secret"
            } else {
                "password"
            };
            group.row(
                label,
                Some("kept in the system keychain once tested · an app password works best"),
                |ui| {
                    glyphst_ui::text::fitted(ui, |ui| {
                        ui.add(
                            egui::TextEdit::singleline(sync_secret)
                                .margin(glyphst_ui::text::FIELD_MARGIN)
                                .password(true)
                                .hint_text("unchanged")
                                .desired_width(320.0),
                        )
                    });
                },
            );
            let (text, colour) = match &status.probe {
                None => ("not tried yet".to_string(), tokens.muted),
                Some(Ok(files)) => (
                    format!("● connected · {files} files of other computers' changes"),
                    tokens.ok,
                ),
                Some(Err(error)) => (format!("▲ {error}"), tokens.err),
            };
            group.row_with(
                "connection",
                Some(
                    egui::RichText::new(text)
                        .font(theme::regular(12.0))
                        .color(colour),
                ),
                |ui| {
                    if ui.add(Button::bracket("test connection")).clicked() {
                        actions.push(LibraryAction::TestSync);
                    }
                },
            );
        }
        let (text, colour) = match (&status.error, status.last_synced) {
            (Some(error), _) => (format!("▲ {error}"), tokens.err),
            (None, Some(at)) => {
                let mut text = format!("● synced {}", ago(at));
                if status.files_waiting > 0 {
                    text.push_str(&format!(" · {} files on their way", status.files_waiting));
                }
                (text, tokens.ok)
            }
            (None, None) => ("○ not synced yet".to_string(), tokens.muted),
        };
        group.row_with(
            "status",
            Some(
                egui::RichText::new(text)
                    .font(theme::regular(12.0))
                    .color(colour),
            ),
            |ui| {
                if ui.add(Button::bracket("sync now")).clicked() {
                    actions.push(LibraryAction::SyncNow);
                }
            },
        );
        for device in &status.devices {
            let (text, colour) = match (device.this, device.clock_ahead) {
                (true, _) => ("this computer".to_string(), tokens.muted),
                (false, Some(by)) => (
                    format!(
                        "▲ its clock runs {} ahead · changes still merge by time",
                        ahead(by)
                    ),
                    tokens.err,
                ),
                (false, None) => (
                    match device.last_written {
                        Some(at) => format!("last wrote {}", ago(at)),
                        None => "has not written yet".to_string(),
                    },
                    tokens.muted,
                ),
            };
            group.row_with(
                &device.name,
                Some(
                    egui::RichText::new(text)
                        .font(theme::regular(12.0))
                        .color(colour),
                ),
                |_| {},
            );
        }
        let problems = status.conflicts.len() + facts.note_copies.len();
        if problems > 0 || status.parked > 0 {
            let mut parts = Vec::new();
            if !facts.note_copies.is_empty() {
                parts.push(match facts.note_copies.len() {
                    1 => "1 note kept twice".to_string(),
                    count => format!("{count} notes kept twice"),
                });
            }
            if status.parked > 0 {
                parts.push(match status.parked {
                    1 => "1 record waits for what it names".to_string(),
                    count => format!("{count} records wait for what they name"),
                });
            }
            if !status.conflicts.is_empty() {
                parts.push(format!("{} left out", status.conflicts.len()));
            }
            group.row("conflicts", Some(&*parts.join(" · ")), |ui| {
                let label = if *show_conflicts { "hide" } else { "show" };
                if problems > 0 && ui.add(Button::bracket_muted(label)).clicked() {
                    *show_conflicts = !*show_conflicts;
                }
            });
            if *show_conflicts {
                for copy in &facts.note_copies {
                    group.row(
                        "note kept twice",
                        Some(&*copy.display().to_string()),
                        |ui| {
                            if ui.add(Button::bracket_muted("open")).clicked() {
                                actions.push(LibraryAction::OpenFile(copy.clone()));
                            }
                        },
                    );
                }
                for conflict in &status.conflicts {
                    group.row("left out", Some(conflict), |_| {});
                }
            }
        }
    });
}

/// The Library page (frame 07 · library).
fn library(
    ui: &mut egui::Ui,
    prefs: &mut Prefs,
    facts: &LibraryFacts,
    actions: &mut Vec<LibraryAction>,
    scroll_to: &mut Option<String>,
    show_conflicts: &mut bool,
    sync_secret: &mut String,
) {
    let library = &mut prefs.library;
    ui_settings::group(ui, "folder", |group| {
        let folder = facts
            .folder
            .as_ref()
            .map(|folder| short_path(folder, 40))
            .unwrap_or_else(|| "in memory".to_string());
        let caption = if facts.folder_chosen {
            "files/ and notes/ live here · the database stays in the app's data folder"
        } else {
            "the app's data folder · choose a folder you can see and sync"
        };
        group.row("library folder", Some(caption), |ui| {
            // The row lays out from the right.
            if ui.add(Button::bracket_muted("move…")).clicked() {
                actions.push(LibraryAction::ChooseFolder);
            }
            ui.label(RichText::new(folder).font(theme::mono(12.0)));
        });
        let mut vault = library.vault.clone().unwrap_or_default();
        group.row(
            "notes vault",
            Some("empty: notes/ in the library folder · or point it at an obsidian vault"),
            |ui| {
                if TextField::new(&mut vault, "notes/ in the library folder")
                    .width(320.0)
                    .show(ui)
                    .changed()
                {
                    library.vault = Some(vault.clone()).filter(|vault| !vault.trim().is_empty());
                }
            },
        );
        let registered = facts.link_handler.as_deref() == Some(crate::instance::HANDLER_ENTRY);
        let caption = match &facts.link_handler {
            _ if registered => "citations clicked in obsidian and other apps open here".to_string(),
            Some(other) => format!("{other} opens them now"),
            None => "open citations from obsidian and other apps here".to_string(),
        };
        group.row("glyphst:// links", Some(&*caption), |ui| {
            let button = if registered {
                Button::bracket_muted("register again")
            } else {
                Button::bracket("register")
            };
            if ui.add(button).clicked() {
                actions.push(LibraryAction::RegisterLinks);
            }
        });
        // Items added before the library copied files in.
        let (linked, bytes) = facts.linked;
        if linked > 0 {
            let caption = format!(
                "{} point at files outside the folder · {} · the originals stay",
                if linked == 1 {
                    "1 item".to_string()
                } else {
                    format!("{linked} items")
                },
                crate::sidebar::format_size(bytes)
            );
            group.row("linked files", Some(&*caption), |ui| {
                if ui.add(Button::bracket("copy into the folder…")).clicked() {
                    actions.push(LibraryAction::CopyLinked);
                }
            });
        }
    });
    ui.add_space(10.0);
    if scroll_to.as_deref() == Some("sync") {
        let at = egui::Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(egui::Align::Min));
        *scroll_to = None;
    }
    sync_group(ui, library, facts, actions, show_conflicts, sync_secret);
    ui.add_space(10.0);
    ui_settings::group(ui, "adding", |group| {
        group.row(
            "look up metadata online",
            Some("arxiv · crossref · openalex · open library"),
            |ui| {
                ui.add(Switch(&mut library.look_up_online));
            },
        );
        group.row(
            "contact email",
            Some("optional · sent to crossref only, for its polite pool"),
            |ui| {
                TextField::new(&mut library.contact_email, "you@example.org")
                    .width(320.0)
                    .show(ui);
            },
        );
    });
    ui.add_space(10.0);
    if scroll_to.as_deref() == Some("ai") {
        let at = egui::Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(egui::Align::Min));
        *scroll_to = None;
    }
    ui_settings::group(ui, "ai", |group| {
        group.row(
            "enrich new items",
            Some("a summary and tags for each item added, and a collection when one fits"),
            |ui| {
                ui.add(Switch(&mut library.enrich_new));
            },
        );
        group.row(
            "ai may create tags",
            Some("up to 2 per item · otherwise only tags you already use"),
            |ui| {
                ui.add(Switch(&mut library.ai_create_tags));
            },
        );
        group.row(
            "new collections",
            Some("the ai only proposes them; your tree changes when you create one"),
            |ui| {
                ui.add(Switch(&mut library.suggest_collections));
            },
        );
        group.row(
            "existing collections",
            Some("file only when the ai is sure; none is required"),
            |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut library.auto_file_collections,
                    &[(false, "suggest"), (true, "file")],
                );
            },
        );
        let model = facts
            .model
            .clone()
            .unwrap_or_else(|| "none · choose one in ai & agents".to_string());
        group.row(
            "library model",
            Some(&*format!("{model} · set in ai & agents")),
            |ui| {
                if facts.unenriched > 0
                    && ui
                        .add(Button::bracket(&if facts.unenriched == 1 {
                            "enrich 1 item…".to_string()
                        } else {
                            format!("enrich {} items…", facts.unenriched)
                        }))
                        .clicked()
                {
                    actions.push(LibraryAction::Enrich);
                }
            },
        );
    });
    ui.add_space(10.0);
    if scroll_to.as_deref() == Some("search") {
        let at = egui::Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
        ui.scroll_to_rect(at, Some(egui::Align::Min));
        *scroll_to = None;
    }
    ui_settings::group(ui, "search", |group| {
        let caption = match &facts.index {
            Some((status, bytes)) => {
                let mut caption = format!(
                    "{} / {} read · {} passages · {}",
                    status.done,
                    status.docs,
                    status.chunks,
                    crate::sidebar::format_size(*bytes)
                );
                if status.waiting() > 0
                    && let Some(left) = crate::library_search::time_left(status)
                {
                    caption.push_str(&format!(" · {left}"));
                }
                if status.meaning && status.embedded < status.chunks {
                    caption.push_str(&format!(
                        " · meaning {} / {}",
                        status.embedded, status.chunks
                    ));
                }
                caption
            }
            None => "starts with the library".to_string(),
        };
        group.row("text index", Some(&*caption), |ui| {
            // The row lays out from the right.
            if facts.index.is_some() {
                if ui.add(Button::bracket_muted("read again")).clicked() {
                    actions.push(LibraryAction::Reindex);
                }
                let label = if library.index_paused {
                    "resume"
                } else {
                    "pause"
                };
                if ui.add(Button::bracket(label)).clicked() {
                    library.index_paused = !library.index_paused;
                }
            }
        });
        if let Some(card) = &facts.meaning {
            let caption = model_caption(card, "EmbeddingGemma 2");
            group.row("search by meaning", Some(&*caption), |ui| {
                model_buttons(ui, card, |action| {
                    actions.push(LibraryAction::Meaning(action));
                });
            });
        }
        if let Some(card) = &facts.rerank {
            use crate::meaning::MeaningCard;
            let downloaded = matches!(
                card,
                MeaningCard::Starting
                    | MeaningCard::EngineFailed { .. }
                    | MeaningCard::OnCpu
                    | MeaningCard::Ready
            );
            let caption = if downloaded && !library.rerank {
                "off · downloaded".to_string()
            } else {
                model_caption(card, "bge-reranker-v2-m3")
            };
            group.row("rerank the best matches", Some(&*caption), |ui| {
                if downloaded {
                    ui.add(Switch(&mut library.rerank));
                } else {
                    model_buttons(ui, card, |action| {
                        actions.push(LibraryAction::Rerank(action));
                    });
                }
            });
        }
        group.row(
            "llama-server",
            Some("empty: the one downloaded with the model · or a path to your own"),
            |ui| {
                TextField::new(&mut library.llama_server, "the downloaded one")
                    .width(320.0)
                    .show(ui);
            },
        );
        group.row(
            "learn from what I open",
            Some("to measure search · on this computer only"),
            |ui| {
                if ui.add(Button::bracket_muted("forget")).clicked() {
                    actions.push(LibraryAction::ForgetOpened);
                }
                ui.add(Switch(&mut library.learn_from_opened));
            },
        );
    });
    ui.add_space(10.0);
    ui_settings::group(ui, "import", |group| {
        let caption = facts
            .zotero
            .as_ref()
            .map(|path| format!("{} found", path.display()))
            .unwrap_or_else(|| "no zotero data folder found".to_string());
        group.row("zotero", Some(&*caption), |ui| {
            if ui.add(Button::bracket("import from zotero…")).clicked() {
                actions.push(LibraryAction::ImportZotero);
            }
        });
    });
}

impl SettingsWindow {
    pub fn show_section(&mut self, section: Section) {
        self.open = true;
        self.section = Some(section);
    }

    /// Opens Settings · Models and starts downloading the local OCR model
    /// `settings` use.
    /// Shows Models' downloads in each state, for screenshots.
    pub fn show_model_downloads(&mut self) {
        self.models.show_downloads();
    }

    pub fn install_local_ocr(
        &mut self,
        ctx: &egui::Context,
        settings: &glyphst_settings::Settings,
    ) {
        self.show_section(Section::Models);
        self.models.start_local_ocr(settings, ctx);
    }

    /// Lists a provider's models, as its Fetch models button does.
    pub fn fetch_models(
        &mut self,
        settings: &glyphst_settings::Settings,
        provider: glyphst_settings::AiProvider,
        ctx: &egui::Context,
    ) {
        self.providers.fetch_models(settings, provider, ctx);
    }

    /// Draws the window (frame 07): a header with the section and the
    /// settings search, the sections grouped at the left with the file's
    /// state at their foot, and the section's settings at the right.
    pub fn show(&mut self, ctx: &egui::Context, app: SettingsContext<'_>) {
        if !self.open {
            return;
        }
        self.providers.poll();
        let tokens = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let size = vec2(
            (screen.width() - 48.0).clamp(360.0, 960.0),
            (screen.height() - 80.0).clamp(280.0, 680.0),
        );
        let response = egui::Modal::new(egui::Id::unique("settings"))
            .backdrop_color(tokens.scrim)
            .frame(glyphst_ui::pane::popover(ctx).inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                let (full, _) = ui.allocate_exact_size(size, Sense::hover());
                let header = egui::Rect::from_min_size(full.min, vec2(size.x, HEADER_HEIGHT));
                let sidebar = egui::Rect::from_min_max(
                    egui::pos2(full.left(), header.bottom()),
                    egui::pos2(full.left() + SIDEBAR_WIDTH, full.bottom()),
                );
                let content = egui::Rect::from_min_max(
                    egui::pos2(sidebar.right(), header.bottom()),
                    full.max,
                );
                ui.painter()
                    .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
                ui.painter()
                    .vline(sidebar.right() - 0.5, sidebar.y_range(), tokens.hairline());
                ui.painter().rect_filled(
                    sidebar
                        .shrink2(vec2(0.5, 0.0))
                        .with_min_x(sidebar.left() + 1.0),
                    0,
                    tokens.surface.to_opaque(),
                );
                let current = self.section.unwrap_or(Section::Appearance);
                ui.scope_builder(egui::UiBuilder::new().max_rect(header), |ui| {
                    self.header(ui, current, &tokens)
                });
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(sidebar.shrink2(vec2(0.0, 10.0))),
                    |ui| self.sidebar(ui, current, app.user, &tokens),
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(content.shrink2(vec2(28.0, 20.0))),
                    |ui| self.content(ui, current, app, &tokens),
                );
            });
        if response.should_close() {
            self.open = false;
        }
    }

    /// `settings — section`, the search and the close button.
    fn header(&mut self, ui: &mut egui::Ui, current: Section, tokens: &Tokens) {
        let rect = ui.max_rect();
        let y = rect.center().y;
        let title = ui.painter().text(
            egui::pos2(rect.left() + 16.0, y),
            egui::Align2::LEFT_CENTER,
            "settings",
            theme::mono_strong(13.0),
            tokens.text,
        );
        let x = title.right() + 10.0;
        ui.painter()
            .hline(x..=x + 12.0, y, egui::Stroke::new(1.0, tokens.rule_strong));
        ui.painter().text(
            egui::pos2(x + 20.0, y),
            egui::Align2::LEFT_CENTER,
            current.label().to_lowercase(),
            theme::mono(12.0),
            tokens.faint,
        );
        let close =
            egui::Rect::from_center_size(egui::pos2(rect.right() - 22.0, y), vec2(24.0, 24.0));
        let mut close_ui = ui.new_child(egui::UiBuilder::new().max_rect(close));
        if icons::button(&mut close_ui, "x", 24.0, false, "close (esc)").clicked() {
            self.open = false;
        }
        let field = egui::Rect::from_min_size(
            egui::pos2(close.left() - 12.0 - 220.0, y - 14.0),
            vec2(220.0, 28.0),
        );
        let mut field_ui = ui.new_child(egui::UiBuilder::new().max_rect(field));
        TextField::search(&mut self.query, "search settings")
            .shortcut("/")
            .width(220.0)
            .id(egui::Id::unique("settings-search"))
            .show(&mut field_ui);
        if ui.input(|i| i.key_pressed(egui::Key::Slash))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(egui::Id::unique("settings-search")));
        }
    }

    fn sidebar(
        &mut self,
        ui: &mut egui::Ui,
        current: Section,
        user: &UserSettings,
        tokens: &Tokens,
    ) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let query = self.query.trim().to_lowercase();
        let matches = |section: Section| {
            query.is_empty()
                || section.label().to_lowercase().contains(&query)
                || section.description().to_lowercase().contains(&query)
                || section.keywords().contains(&query)
        };
        for (title, sections) in GROUPS {
            let shown: Vec<Section> = sections
                .iter()
                .copied()
                .filter(|&section| matches(section))
                .collect();
            if shown.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.set_width(SIDEBAR_WIDTH - 28.0);
                motif::rule_title(ui, title, None);
            });
            for section in shown {
                let label = section.label().to_lowercase();
                let response = ListRow::new(&label)
                    .icon(section.icon())
                    .active(current == section)
                    .indent(14.0)
                    .show(ui);
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.section = Some(section);
                }
            }
        }
        if query.is_empty() || "keys keyboard shortcuts".contains(&query) {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.set_width(SIDEBAR_WIDTH - 28.0);
                motif::rule_title(ui, "keys", None);
            });
            if ListRow::new("shortcuts")
                .icon("keyboard")
                .indent(14.0)
                .show(ui)
                .tip("open the keyboard shortcuts in a tab")
                .clicked()
            {
                self.open_shortcuts = true;
                self.open = false;
            }
        }
        // The settings file's state at the foot.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let file = glyphst_settings::settings_file();
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                if ui.add(Button::bracket_muted("open file")).clicked() {
                    let _ = std::process::Command::new("xdg-open").arg(&file).spawn();
                }
            });
            let shown = match dirs::home_dir() {
                Some(home) => file
                    .strip_prefix(&home)
                    .map(|rest| format!("~/{}", rest.display()))
                    .unwrap_or_else(|_| file.display().to_string()),
                None => file.display().to_string(),
            };
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(shown)
                            .font(theme::mono(10.0))
                            .color(tokens.faint),
                    )
                    .truncate(),
                );
            });
            let (state, color) = match user.persistence() {
                Persistence::User => ("● saved · applies live", tokens.ok),
                Persistence::Memory => ("● not saved · scripted run", tokens.warn),
            };
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(state).font(theme::mono(10.0)).color(color));
            });
        });
    }

    fn content(
        &mut self,
        ui: &mut egui::Ui,
        current: Section,
        mut app: SettingsContext<'_>,
        _tokens: &Tokens,
    ) {
        let mut description = current.description().to_string();
        if current.in_settings_file() && app.user.persistence() == Persistence::Memory {
            description.push_str(" Scripted run: changes are not saved.");
        }
        let title = match current {
            Section::Ai => "AI & agents",
            other => other.label(),
        };
        ui_settings::heading(ui, title, &description);
        let families = self.families.get_or_insert_with(|| {
            (
                glyphst_ui::fonts::installed_families(true),
                glyphst_ui::fonts::installed_families(false),
            )
        });
        egui::ScrollArea::vertical()
            .id_salt(("settings-page", current.label()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                glyphst_ui::settings::form_style(ui);
                ui.set_width(ui.available_width() - 4.0);
                match current {
                    Section::Appearance => {
                        // The effect backgrounds' previews are animated.
                        ui.ctx().request_repaint();
                        if appearance(
                            ui,
                            &mut app,
                            families,
                            &mut self.effects_actions,
                            &mut self.scroll_to,
                        ) {
                            self.section = Some(Section::Effects);
                        }
                    }
                    Section::Reading => reading(ui, app.prefs),
                    Section::Presenting => presenting(ui, app.prefs),
                    Section::Library => library(
                        ui,
                        app.prefs,
                        &app.library,
                        &mut self.library_actions,
                        &mut self.scroll_to,
                        &mut self.show_conflicts,
                        &mut self.sync_secret,
                    ),
                    Section::Effects => {
                        // The previews are animated.
                        ui.ctx().request_repaint();
                        crate::effects_page::settings_page(
                            ui,
                            crate::effects_page::SettingsPage {
                                prefs: app.prefs,
                                effects: app.effects,
                                errors: app.effect_errors,
                                folder: app.effects_folder,
                                time: app.time,
                                actions: &mut self.effects_actions,
                            },
                        );
                    }
                    Section::Translation => self.providers.translation(ui, app.user),
                    Section::Ocr => {
                        if self.providers.ocr(ui, app.user) {
                            self.section = Some(Section::Models);
                        }
                    }
                    Section::Ai => self.providers.ai(ui, app.user, app.chatgpt),
                    Section::Models => self.models.show(
                        ui,
                        crate::models_page::ModelsContext {
                            user: app.user,
                            library: &mut app.prefs.library,
                            facts: &app.library,
                            machine: app.machine,
                            actions: &mut self.library_actions,
                        },
                    ),
                }
                ui.add_space(16.0);
            });
    }
}

/// The Appearance page (frame 07a, and the background and colours below
/// it); returns whether the Effects link was clicked.
fn appearance(
    ui: &mut egui::Ui,
    app: &mut SettingsContext<'_>,
    families: &(Vec<String>, Vec<String>),
    actions: &mut Vec<crate::effects_page::Action>,
    scroll_to: &mut Option<String>,
) -> bool {
    let (system, blur_supported) = (app.system_theme, app.blur_supported);
    let prefs = &mut *app.prefs;
    let user = &mut *app.user;
    ui_settings::group(ui, "", |group| {
        let caption = match (prefs.theme, system) {
            (ThemePreference::System, Some(ThemeKind::Dark)) => {
                "follows the desktop, which asks for dark"
            }
            (ThemePreference::System, Some(ThemeKind::Light)) => {
                "follows the desktop, which asks for light"
            }
            (ThemePreference::System, None) => "the desktop states no preference: light",
            _ => "follows the desktop when set to system",
        };
        group.row("theme", Some(caption), |ui| {
            glyphst_ui::segmented::labels(
                ui,
                &mut prefs.theme,
                &[
                    (ThemePreference::System, "system"),
                    (ThemePreference::Light, "light"),
                    (ThemePreference::Dark, "dark"),
                ],
            );
        });
        group.row(
            "chrome font",
            Some("menus, tabs, labels and numbers"),
            |ui| {
                font_choice(ui, "chrome-font", &mut prefs.fonts.chrome, &families.0);
            },
        );
        group.row(
            "prose font",
            Some("assistant answers and help text"),
            |ui| {
                font_choice(ui, "prose-font", &mut prefs.fonts.prose, &families.1);
            },
        );
        group.row("ui scale", None, |ui| {
            let (low, high) = (
                *glyphst_ui::theme::UI_SCALE_RANGE.start(),
                *glyphst_ui::theme::UI_SCALE_RANGE.end(),
            );
            ui.add(
                Slider::new(&mut prefs.ui_scale, low..=high)
                    .step(1.0)
                    .width(196.0)
                    .label("ui scale")
                    .format(Some(Box::new(|value| format!("{value:.0} px")))),
            );
        });
        group.row("density", Some("row height and padding"), |ui| {
            glyphst_ui::segmented::labels(
                ui,
                &mut prefs.density,
                &[
                    (Density::Compact, "compact"),
                    (Density::Default, "default"),
                    (Density::Airy, "airy"),
                ],
            );
        });
        // The window's transparency, in the settings file.
        match user.draft().map(|settings| settings.window) {
            None => group.note("reading your settings…"),
            Some(mut window) => {
                let before = window;
                let mut transparent = window.transparent();
                let blur_hint = match blur_supported {
                    Some(false) => "show the desktop through panes (no blur from this compositor)",
                    _ => "show the desktop through panes (wayland blur when available)",
                };
                group.row("window transparency", Some(blur_hint), |ui| {
                    if ui.add(Switch(&mut transparent)).changed() {
                        window.set_transparent(transparent);
                    }
                    ui.add_enabled_ui(transparent, |ui| {
                        let mut opacity = window.opacity;
                        let min = glyphst_settings::WindowSettings::MIN_OPACITY;
                        if ui
                            .add(
                                Slider::new(&mut opacity, min..=1.0)
                                    .width(120.0)
                                    .label("opacity"),
                            )
                            .changed()
                        {
                            window.opacity = opacity;
                        }
                    });
                });
                let mut blur = window.blur();
                group.row("blur behind the window", None, |ui| {
                    ui.add_enabled_ui(transparent && blur_supported != Some(false), |ui| {
                        if ui.add(Switch(&mut blur)).changed() {
                            window.set_blur(blur);
                        }
                    });
                });
                let mut opaque = window.fullscreen_opaque;
                group.row(
                    "opaque in fullscreen",
                    Some(
                        "fullscreen hides the desktop behind the window, so show the theme instead",
                    ),
                    |ui| {
                        ui.add_enabled_ui(transparent, |ui| {
                            if ui.add(Switch(&mut opaque)).changed() {
                                window.fullscreen_opaque = opaque;
                            }
                        });
                    },
                );
                if window != before {
                    user.edit(|settings| settings.window = window);
                }
            }
        }
        group.row(
            "glyph ornaments",
            Some("rule titles, tree lines and dot fields; off = plain labels"),
            |ui| {
                ui.add(Switch(&mut prefs.glyph_ornaments));
            },
        );
    });
    let mut jump = |ui: &mut egui::Ui, group: &str| {
        if scroll_to.as_deref() == Some(group) {
            let at = egui::Rect::from_min_size(ui.cursor().min, vec2(1.0, 1.0));
            ui.scroll_to_rect(at, Some(egui::Align::Min));
            *scroll_to = None;
        }
    };
    jump(ui, "background");
    crate::effects_page::background_group(
        ui,
        crate::effects_page::BackgroundGroup {
            prefs,
            effects: app.effects,
            time: app.time,
            actions,
        },
    );
    jump(ui, "colours");
    colours(ui, prefs, app.background_seed);
    jump(ui, "tint");
    crate::effects_page::tint_group(
        ui,
        crate::effects_page::TintGroup {
            prefs,
            effects: app.effects,
            time: app.time,
        },
    );
    ui.add_space(8.0);
    ui.add(Button::bracket("page transitions and your effects →"))
        .clicked()
}

/// Settings › Appearance › colours: the accent swatches, or a scheme
/// generated from the background or a colour of the user's.
fn colours(ui: &mut egui::Ui, prefs: &mut Prefs, background_seed: Option<[u8; 3]>) {
    let tokens = Tokens::get(ui.ctx());
    ui_settings::group(ui, "colours", |group| {
        group.row(
            "colours from",
            Some(
                "a scheme from the background's colour or wallpaper, a preset accent, or a colour",
            ),
            |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut prefs.colours,
                    &[
                        (Colours::Background, "follow background"),
                        (Colours::Preset, "preset"),
                        (Colours::Custom, "custom"),
                    ],
                );
            },
        );
        let seed = match prefs.colours {
            Colours::Preset => {
                accent_row(group, prefs, &tokens);
                return;
            }
            Colours::Background => background_seed,
            Colours::Custom => {
                group.row("colour", Some("the seed the scheme grows from"), |ui| {
                    let [r, g, b] = prefs.custom_colour;
                    ui.label(
                        RichText::new(format!("#{r:02x}{g:02x}{b:02x}"))
                            .font(theme::mono(11.0))
                            .color(tokens.muted),
                    );
                    egui::widgets::color_picker::color_edit_button_srgb(
                        ui,
                        &mut prefs.custom_colour,
                    );
                });
                Some(prefs.custom_colour)
            }
        };
        let Some(seed) = seed else {
            group.note(
                "The background has no colour of its own (it is the theme's, an effect, or a \
                 wallpaper still being read), so the accent swatch colours the theme.",
            );
            accent_row(group, prefs, &tokens);
            return;
        };
        group.row(
            "style",
            Some("Material You schemes; the first ones are the newest"),
            |ui| {
                egui::ComboBox::from_id_salt("scheme-style")
                    .selected_text(
                        RichText::new(prefs.scheme_style.label()).font(theme::mono(12.0)),
                    )
                    .width(240.0)
                    .height(420.0)
                    .show_ui(ui, |ui| {
                        glyphst_ui::menu::style(ui);
                        // Rows of one height: the description is a cut
                        // second line, so the pointer never reflows them.
                        for style in SchemeStyle::ALL {
                            if glyphst_ui::menu::Item::new(style.label())
                                .detail(style.description())
                                .checked(prefs.scheme_style == style)
                                .width(340.0)
                                .show(ui)
                                .clicked()
                            {
                                prefs.scheme_style = style;
                            }
                        }
                    });
            },
        );
        group.row(
            "contrast",
            Some("secondary text and hairlines move towards the text"),
            |ui| {
                glyphst_ui::segmented::labels(
                    ui,
                    &mut prefs.scheme_contrast,
                    &Contrast::ALL.map(|contrast| (contrast, contrast.label())),
                );
            },
        );
        let scheme = Scheme {
            seed,
            style: prefs.scheme_style,
            contrast: prefs.scheme_contrast,
        };
        group.row(
            "scheme",
            Some("accent, secondary, tertiary, base, raised and hairline"),
            |ui| {
                for color in scheme.swatches(tokens.kind).into_iter().rev() {
                    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    ui.painter().rect(
                        rect.shrink(2.0),
                        tokens.ctl_radius(),
                        color,
                        tokens.edge(),
                        egui::StrokeKind::Outside,
                    );
                }
            },
        );
    });
}

/// The accent swatches.
fn accent_row(group: &mut ui_settings::Group<'_>, prefs: &mut Prefs, tokens: &Tokens) {
    group.row(
        "accent",
        Some("used for focus, selection and progress"),
        |ui| {
            ui.label(
                RichText::new(prefs.accent.label())
                    .font(theme::mono(11.0))
                    .color(tokens.muted),
            );
            for accent in Accent::ALL.into_iter().rev() {
                let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                let chosen = prefs.accent == accent;
                ui.painter().rect(
                    rect.shrink(2.0),
                    tokens.ctl_radius(),
                    accent.swatch(),
                    if chosen {
                        egui::Stroke::new(1.5, tokens.text)
                    } else {
                        tokens.edge()
                    },
                    egui::StrokeKind::Outside,
                );
                if response.tip(accent.label()).clicked() {
                    prefs.accent = accent;
                }
            }
        },
    );
}

/// A family from `families` in a combo box.
fn font_choice(ui: &mut egui::Ui, id: &str, chosen: &mut String, families: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(chosen.as_str()).font(theme::mono(12.0)))
        .width(200.0)
        .height(320.0)
        .show_ui(ui, |ui| {
            glyphst_ui::menu::style(ui);
            for family in families {
                if glyphst_ui::menu::Item::new(family)
                    .checked(family == chosen)
                    .show(ui)
                    .clicked()
                {
                    *chosen = family.clone();
                }
            }
        });
}

fn reading(ui: &mut egui::Ui, prefs: &mut Prefs) {
    ui_settings::group(ui, "", |group| {
        group.row(
            "reopen the last session",
            Some("the documents, pages and panes you had when the app closed"),
            |ui| {
                ui.add(Switch(&mut prefs.reopen_last_session));
            },
        );
        group.row(
            "remember zoom",
            Some("reopen each document at its zoom; off opens at the default zoom, on its page"),
            |ui| {
                ui.add(Switch(&mut prefs.remember_zoom));
            },
        );
        group.row(
            "sidebar",
            Some("the outline, pages, bookmarks, notes and search beside the pages"),
            |ui| {
                ui.add(Switch(&mut prefs.show_sidebar));
            },
        );
        group.row(
            "vim keys",
            Some("j / k scroll, gg / G for pages; pending keys show in the corner"),
            |ui| {
                let mut vim = prefs.keymap == crate::keys::KeymapPreset::Vim;
                if ui.add(Switch(&mut vim)).changed() {
                    prefs.keymap = if vim {
                        crate::keys::KeymapPreset::Vim
                    } else {
                        crate::keys::KeymapPreset::Standard
                    };
                }
            },
        );
    });
    ui.add_space(10.0);
    motif::rule_title(ui, "gamepad ring", None);
    ui.add_space(2.0);
    ui.label(
        RichText::new(
            "Push the right stick toward a command and let go to run it; the bottom slot \
             and B cancel. While presenting the stick opens presenting's ring instead.",
        )
        .font(theme::regular(12.0))
        .color(Tokens::get(ui.ctx()).muted),
    );
    ui.add_space(8.0);
    ring_slots(
        ui,
        &mut prefs.reading_ring,
        crate::app::fits_reading_ring,
        true,
    );
}

/// The Presenting page (frame 07): full screen, the clock, and the tool
/// ring's slots.
fn presenting(ui: &mut egui::Ui, prefs: &mut Prefs) {
    ui_settings::group(ui, "", |group| {
        group.row(
            "enter full screen",
            Some("go full screen when a presentation starts; leaving restores the window"),
            |ui| {
                ui.add(Switch(&mut prefs.present_fullscreen));
            },
        );
        group.row(
            "show page number",
            Some("the page in the bottom-right corner; the timer tool (t) adds the time beside it"),
            |ui| {
                ui.add(Switch(&mut prefs.present_page_number));
            },
        );
        group.row(
            "talk length",
            Some("count down from this length: amber in the last 10%, red and +m:ss once over"),
            |ui| {
                let label = match prefs.talk_minutes {
                    0 => "off · count up".to_string(),
                    minutes => format!("{minutes} min"),
                };
                egui::ComboBox::from_id_salt("talk-length")
                    .selected_text(RichText::new(label).font(theme::mono(12.0)))
                    .width(200.0)
                    .height(320.0)
                    .show_ui(ui, |ui| {
                        glyphst_ui::menu::style(ui);
                        for minutes in TALK_LENGTHS {
                            let label = match minutes {
                                0 => "off · count up".to_string(),
                                minutes => format!("{minutes} min"),
                            };
                            if glyphst_ui::menu::Item::new(&label)
                                .checked(prefs.talk_minutes == minutes)
                                .show(ui)
                                .clicked()
                            {
                                prefs.talk_minutes = minutes;
                            }
                        }
                    });
            },
        );
    });
    ui.add_space(10.0);
    motif::rule_title(ui, "tool ring", None);
    ui.add_space(2.0);
    ui.label(
        RichText::new(
            "Hold the right button and flick toward a tool. A plain right-click goes to the \
             previous page; release on the hub to cancel. The pad's right stick opens it \
             too, with cancel in place of the bottom tool.",
        )
        .font(theme::regular(12.0))
        .color(Tokens::get(ui.ctx()).muted),
    );
    ui.add_space(8.0);
    ring_slots(
        ui,
        &mut prefs.present_ring,
        |command| command.category() == crate::commands::Category::Present,
        false,
    );
}

/// Talk lengths offered, in minutes.
const TALK_LENGTHS: [u32; 9] = [0, 5, 10, 15, 20, 30, 45, 60, 90];

/// The ring's eight directions in two columns: up to down-right on the
/// left, down to up-left on the right.
/// `cancel_slot`: the ring only the stick opens, whose bottom slot is
/// cancel.
fn ring_slots(
    ui: &mut egui::Ui,
    slots: &mut crate::present::RingSlots,
    fits: impl Fn(crate::commands::Command) -> bool,
    cancel_slot: bool,
) {
    const ARROWS: [&str; 8] = ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"];
    let tokens = Tokens::get(ui.ctx());
    let tools: Vec<crate::commands::Command> = crate::commands::Command::ALL
        .into_iter()
        .filter(|&command| fits(command))
        .collect();
    let width = ((ui.available_width() - 32.0) / 2.0).max(160.0);
    for row in 0..4 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 32.0;
            for slot in [row, row + 4] {
                ui.allocate_ui_with_layout(
                    vec2(width, 28.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.label(
                            RichText::new(ARROWS[slot])
                                .font(theme::mono(13.0))
                                .color(tokens.accent_text),
                        );
                        if cancel_slot && slot == glyphst_ui::ring::CANCEL_SLOT {
                            ui.label(
                                RichText::new("cancel (a stick can't reach the hub)")
                                    .font(theme::mono(12.0))
                                    .color(tokens.muted),
                            );
                            return;
                        }
                        let chosen = slots[slot]
                            .as_deref()
                            .and_then(crate::commands::Command::from_id);
                        let label = chosen.map_or("none", |command| command.label());
                        egui::ComboBox::from_id_salt(("ring-slot", slot))
                            .selected_text(RichText::new(label).font(theme::mono(12.0)))
                            .width(width - 28.0)
                            .height(320.0)
                            .show_ui(ui, |ui| {
                                glyphst_ui::menu::style(ui);
                                if glyphst_ui::menu::Item::new("none")
                                    .checked(chosen.is_none())
                                    .show(ui)
                                    .clicked()
                                {
                                    slots[slot] = None;
                                }
                                for &tool in &tools {
                                    if glyphst_ui::menu::Item::new(tool.label())
                                        .icon(tool.icon())
                                        .checked(chosen == Some(tool))
                                        .show(ui)
                                        .clicked()
                                    {
                                        slots[slot] = Some(tool.id().to_string());
                                    }
                                }
                            });
                    },
                );
            }
        });
        ui.add_space(8.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(ctx: &egui::Context) -> UserSettings {
        UserSettings::in_memory(
            ctx,
            glyphst_settings::Settings::default(),
            Default::default(),
        )
    }

    fn chatgpt() -> ChatGptSubscription {
        ChatGptSubscription::start(
            std::sync::Arc::new(glyphst_settings::MemoryCredentials::default()),
            || {},
        )
    }

    /// Runs `frames` frames of the window and returns the last output.
    fn run(
        ctx: &egui::Context,
        window: &mut SettingsWindow,
        prefs: &mut Prefs,
        user: &mut UserSettings,
        effects: &[EffectDef],
        frames: usize,
    ) -> egui::FullOutput {
        let chatgpt = chatgpt();
        let errors = ["wave.wgsl: line 3".to_string()];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 900.0),
            )),
            ..Default::default()
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..frames {
            output = ctx.run_ui(input.clone(), |ui| {
                window.show(
                    ui.ctx(),
                    SettingsContext {
                        prefs: &mut *prefs,
                        effects,
                        effect_errors: &errors,
                        effects_folder: Some(std::path::Path::new("/config/glyphst/effects")),
                        time: 1.0,
                        user: &mut *user,
                        chatgpt: &chatgpt,
                        system_theme: Some(ThemeKind::Dark),
                        blur_supported: Some(false),
                        background_seed: None,
                        library: LibraryFacts::default(),
                        machine: &glyphst_models::machine::Machine::default(),
                    },
                );
            });
            output.textures_delta.clear();
        }
        output
    }

    #[test]
    fn sections_are_named_by_their_last_word() {
        assert_eq!(Section::parse("ai"), Some(Section::Ai));
        assert_eq!(Section::parse("Appearance"), Some(Section::Appearance));
        // Shortcuts have their own tab now.
        assert_eq!(Section::parse("shortcuts"), None);
        assert_eq!(Section::parse("nope"), None);
        let grouped: Vec<Section> = GROUPS
            .iter()
            .flat_map(|(_, sections)| sections.iter().copied())
            .collect();
        assert_eq!(grouped, Section::ALL, "every section is listed once");
    }

    #[test]
    fn every_section_draws_in_both_themes_and_transparent() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        for (kind, opacity) in [(ThemeKind::Light, 1.0), (ThemeKind::Dark, 0.7)] {
            theme::apply(
                &ctx,
                &theme::Appearance {
                    kind,
                    opacity,
                    ..Default::default()
                },
            );
            let mut prefs = Prefs::default();
            let mut user = user(&ctx);
            for section in Section::ALL {
                let mut window = SettingsWindow::default();
                window.show_section(section);
                run(&ctx, &mut window, &mut prefs, &mut user, &[], 3);
                assert!(window.open, "{section:?} stays open");
            }
        }
    }

    #[test]
    fn transparency_is_off_until_turned_on_and_saved_in_the_settings() {
        let ctx = egui::Context::default();
        let mut user = user(&ctx);
        let mut window = user.draft().unwrap().window;
        assert!(!window.transparent());
        // What the Appearance toggle does.
        window.set_transparent(true);
        user.edit(|settings| settings.window = window);
        let settings = user.get().unwrap();
        assert!(settings.window.transparent());
        assert!(settings.window.effective_opacity(true) < 1.0);
    }

    #[test]
    fn effect_galleries_show_a_live_card_per_effect() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let def = |id: &str, kind, user| EffectDef {
            id: id.into(),
            name: id.into(),
            kind,
            animated: false,
            duration_ms: 300,
            image: false,
            hidden: false,
            themed: false,
            params: vec![glyphst_effects::ParamDef {
                id: "amount".into(),
                label: "Amount".into(),
                kind: glyphst_effects::ParamKind::Float { min: 0.0, max: 1.0 },
                default: 0.5,
                random: false,
            }],
            source: format!("{id}.wgsl"),
            user,
        };
        use glyphst_effects::EffectKind::{Background, Overlay, Transition};
        let effects = [
            def("solid", Background, false),
            def("wave", Background, true),
            def("crossfade", Transition, false),
            def("night", Overlay, false),
        ];
        let mut prefs = Prefs {
            background: "wave".into(),
            tint_light: Some("night".into()),
            ..Prefs::default()
        };
        // The page itself, tall enough that every gallery is in view
        // (in the window, the tints are scrolled to).
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 2000.0),
            )),
            ..Default::default()
        };
        let errors = ["wave.wgsl: line 3".to_string()];
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(input.clone(), |ui| {
            crate::effects_page::settings_page(
                ui,
                crate::effects_page::SettingsPage {
                    prefs: &mut prefs,
                    effects: &effects,
                    errors: &errors,
                    folder: Some(std::path::Path::new("/config/glyphst/effects")),
                    time: 1.0,
                    actions: &mut actions,
                },
            );
        });
        output.textures_delta.clear();
        // Cards draw live previews: one per transition, none for "None";
        // the book switch's gallery adds Random, the page turn's doesn't.
        // Backgrounds and tints are in Appearance.
        let previews = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::Shape::Callback(_)))
                .count()
        };
        assert_eq!(previews(&output), 3);
        // Appearance › background: an effect background shows the gallery
        // of effects, without the solid colour and the wallpaper.
        let mut output = ctx.run_ui(input.clone(), |ui| {
            crate::effects_page::background_group(
                ui,
                crate::effects_page::BackgroundGroup {
                    prefs: &mut prefs,
                    effects: &effects,
                    time: 1.0,
                    actions: &mut actions,
                },
            );
        });
        output.textures_delta.clear();
        assert_eq!(previews(&output), 1);
        // Appearance › page tint: the tints, with "None".
        let mut output = ctx.run_ui(input.clone(), |ui| {
            crate::effects_page::tint_group(
                ui,
                crate::effects_page::TintGroup {
                    prefs: &mut prefs,
                    effects: &effects,
                    time: 1.0,
                },
            );
        });
        output.textures_delta.clear();
        assert_eq!(previews(&output), 1);
        assert_eq!(actions, []);
    }
}
