//! The new tab (frame 04b): `What do you want to read?` over a field that
//! takes a path (completed from the folder it names) or words that filter
//! the library table below, links to open a file or go elsewhere, and the
//! recent documents' first pages. Opening a document turns the tab into
//! it.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use glyphst_library::LibraryStore;
use glyphst_ui::{button::Button, field::TextField, key::Key, layout, motif, row::ListRow};

use crate::{
    covers::Covers,
    home::{self, Doc, HomeAction},
    library_view::{LibraryAction, LibraryView, Mode},
    theme::{self, Tokens},
};

/// The launcher's column.
const COLUMN: f32 = 720.0;
/// Matches listed under the field.
const MATCHES: usize = 8;

pub enum LauncherAction {
    Open(PathBuf),
    OpenDialog,
    Home,
    Library,
    Activity,
}

pub struct Launcher {
    query: String,
    /// The highlighted folder entry.
    selected: usize,
    /// The field has had the keys once (on the first frame).
    focused: bool,
    library: LibraryView,
}

impl Default for Launcher {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            focused: false,
            library: LibraryView::new("launcher-library"),
        }
    }
}

/// A folder entry matching a path typed.
#[derive(Clone, Debug, PartialEq)]
struct Match {
    path: PathBuf,
    title: String,
    meta: String,
    folder: bool,
}

pub struct LauncherContext<'a> {
    pub library: &'a LibraryStore,
    pub page_counts: &'a BTreeMap<String, usize>,
    pub covers: &'a mut Covers,
    pub picking: bool,
    pub open_key: Option<String>,
}

/// Whether `query` is a path rather than words.
fn is_path(query: &str) -> bool {
    query.starts_with(['/', '~', '.']) || query.contains(std::path::MAIN_SEPARATOR)
}

fn expand_home(query: &str) -> PathBuf {
    match query.strip_prefix('~') {
        Some(rest) => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest.trim_start_matches('/')),
        None => PathBuf::from(query),
    }
}

/// Entries of the folder `query` names that start with what follows its
/// last separator: folders, then documents the Reader opens.
fn path_matches(query: &str) -> Vec<Match> {
    let path = expand_home(query);
    let (folder, prefix) = if query.ends_with('/') {
        (path.clone(), String::new())
    } else {
        (
            path.parent().map(Path::to_path_buf).unwrap_or_default(),
            path.file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default(),
        )
    };
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut found: Vec<Match> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !name.to_lowercase().starts_with(&prefix) {
                return None;
            }
            let path = entry.path();
            let folder = path.is_dir();
            let kind = home::kind_of(&path);
            (folder || crate::pickers::DOCUMENT_EXTENSIONS.contains(&kind.as_str())).then(|| {
                Match {
                    title: if folder { format!("{name}/") } else { name },
                    meta: if folder { "folder".into() } else { kind },
                    path,
                    folder,
                }
            })
        })
        .collect();
    found.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.title.cmp(&b.title)));
    found.truncate(MATCHES);
    found
}

impl Launcher {
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        context: LauncherContext<'_>,
    ) -> Option<LauncherAction> {
        let tokens = Tokens::get(ui.ctx());
        let mut action = None;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, tokens.bg);
        let docs = home::documents(context.library, context.page_counts);
        let query = self.query.trim().to_owned();
        let path = is_path(&query);
        // Words filter the library below; a path lists its folder.
        self.library.filter = if path { String::new() } else { query.clone() };
        let matches = if path {
            path_matches(&self.query)
        } else {
            Vec::new()
        };
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let first_doc = (!path && !query.is_empty())
            .then(|| self.library.rows(&docs).first().map(|doc| doc.path.clone()))
            .flatten();
        egui::ScrollArea::vertical()
            .id_salt("launcher")
            .auto_shrink(false)
            .show(ui, |ui| {
                layout::column(ui, COLUMN, |ui, column| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.add_space((full.height() * 0.07).clamp(24.0, 64.0));
                    motif::rule_title(ui, "new tab", None);
                    ui.label(
                        egui::RichText::new("What do you want to read?")
                            .font(theme::semibold(22.0))
                            .color(tokens.text),
                    );
                    let field_id = ui.id().with("launcher-field");
                    let field = TextField::new(&mut self.query, "a path, a title or an author…")
                        .icon("folder-search")
                        .width(column)
                        .id(field_id)
                        .show(ui);
                    if !self.focused {
                        field.request_focus();
                        self.focused = true;
                    }
                    if field.changed() {
                        self.selected = 0;
                    }
                    if let Some(chosen) = self.keys(ui, &field, &matches, first_doc.as_deref()) {
                        action = Some(chosen);
                    }
                    hints(ui, &tokens);
                    if let Some(chosen) = links(ui, &context) {
                        action = Some(chosen);
                    }
                    ui.add_space(8.0);
                    if path {
                        motif::rule_title(ui, "in the folder", Some(&matches.len().to_string()));
                        if matches.is_empty() {
                            ui.label(
                                egui::RichText::new("nothing matches")
                                    .font(theme::mono(11.0))
                                    .color(tokens.faint),
                            );
                        }
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (index, found) in matches.iter().enumerate() {
                            let icon = if found.folder { "folder" } else { "file-text" };
                            let response = ListRow::new(&found.title)
                                .icon(icon)
                                .meta(found.meta.as_str())
                                .active(index == self.selected)
                                .show(ui);
                            if response.clicked() {
                                action = self.choose(found);
                            }
                        }
                        ui.spacing_mut().item_spacing.y = 12.0;
                    } else if query.is_empty() {
                        let recent: Vec<&Doc> =
                            docs.iter().filter(|doc| doc.opened.is_some()).collect();
                        if !recent.is_empty() {
                            let shown = home::covers_in(column).min(recent.len());
                            motif::rule_title(
                                ui,
                                "recent",
                                Some(&format!("{shown} of {}", recent.len())),
                            );
                            if let Some(HomeAction::Open(path)) =
                                home::cover_row(ui, context.covers, &recent[..shown], column)
                            {
                                action = Some(LauncherAction::Open(path));
                            }
                        }
                    }
                    if !path && !docs.is_empty() {
                        ui.add_space(8.0);
                        match self.library.ui(ui, &docs, column, Mode::Launcher, None) {
                            Some(LibraryAction::Open(path)) => {
                                action = Some(LauncherAction::Open(path));
                            }
                            Some(LibraryAction::Tab) => action = Some(LauncherAction::Library),
                            Some(LibraryAction::OpenDialog) => {
                                action = Some(LauncherAction::OpenDialog);
                            }
                            None => {}
                        }
                    }
                    ui.add_space(48.0);
                });
            });
        action
    }

    /// Up and down choose a folder entry, Tab completes a path to it, and
    /// Enter opens it, the path typed when it is a document, or the first
    /// document the words match.
    fn keys(
        &mut self,
        ui: &egui::Ui,
        field: &egui::Response,
        matches: &[Match],
        first_doc: Option<&Path>,
    ) -> Option<LauncherAction> {
        if !field.has_focus() && !field.lost_focus() {
            return None;
        }
        let (up, down, tab, enter) = ui.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Tab),
                input.key_pressed(egui::Key::Enter),
            )
        });
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        if down && self.selected + 1 < matches.len() {
            self.selected += 1;
        }
        if tab && let Some(found) = matches.get(self.selected) {
            self.complete(found);
            field.request_focus();
        }
        if enter {
            let typed = expand_home(self.query.trim());
            if typed.is_file() {
                return Some(LauncherAction::Open(typed));
            }
            if let Some(found) = matches.get(self.selected) {
                let found = found.clone();
                field.request_focus();
                return self.choose(&found);
            }
            if let Some(path) = first_doc {
                return Some(LauncherAction::Open(path.to_path_buf()));
            }
        }
        None
    }

    /// Puts `found`'s path in the field.
    fn complete(&mut self, found: &Match) {
        let mut path = found.path.display().to_string();
        if found.folder {
            path.push('/');
        }
        self.query = path;
        self.selected = 0;
    }

    /// Opens a document, or goes into a folder.
    fn choose(&mut self, found: &Match) -> Option<LauncherAction> {
        if found.folder {
            self.complete(found);
            None
        } else {
            Some(LauncherAction::Open(found.path.clone()))
        }
    }
}

/// `[ open file… ]` with its key, `[ library ]`, `[ reading activity ]`
/// and `[ home ]`.
fn links(ui: &mut egui::Ui, context: &LauncherContext<'_>) -> Option<LauncherAction> {
    let mut action = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if ui
            .add_enabled(!context.picking, Button::bracket("open file…"))
            .clicked()
        {
            action = Some(LauncherAction::OpenDialog);
        }
        if let Some(key) = &context.open_key {
            ui.add(Key(&glyphst_ui::key::display(key)));
        }
        for (label, chosen) in [
            ("library", LauncherAction::Library),
            ("reading activity", LauncherAction::Activity),
            ("home", LauncherAction::Home),
        ] {
            ui.add_space(10.0);
            if ui.add(Button::bracket_muted(label)).clicked() {
                action = Some(chosen);
            }
        }
    });
    action
}

/// `enter open · tab complete path · ↑ ↓ choose`.
fn hints(ui: &mut egui::Ui, tokens: &Tokens) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (key, label) in [
            ("enter", "open"),
            ("tab", "complete path"),
            ("↑ ↓", "choose"),
        ] {
            ui.add(Key(key));
            ui.label(
                egui::RichText::new(label)
                    .font(theme::mono(11.0))
                    .color(tokens.faint),
            );
            ui.add_space(10.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_words_are_told_apart() {
        assert!(is_path("/home/a.pdf"));
        assert!(is_path("~/papers"));
        assert!(is_path("./a"));
        assert!(!is_path("attention is all"));
    }

    #[test]
    fn a_folder_lists_its_folders_then_its_documents_by_prefix() {
        let dir = std::env::temp_dir().join(format!("glyphst-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("papers")).unwrap();
        for name in ["paper.pdf", "notes.txt", "Pages.epub", ".hidden.pdf"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let all = path_matches(&format!("{}/", dir.display()));
        let titles: Vec<&str> = all.iter().map(|found| found.title.as_str()).collect();
        assert_eq!(titles, ["papers/", "Pages.epub", "paper.pdf"]);
        let some = path_matches(&format!("{}/pap", dir.display()));
        assert_eq!(some.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
