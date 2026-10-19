//! The library tab (frames 04d and 04f): the collections pane (smart
//! views, the collection tree, tags and the library folder), the items of
//! what is chosen as a table, and the chosen item's pane (its metadata, the
//! AI's summary, its tags and collections with the AI's suggestions, and its
//! note).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, vec2};
use glyphst_library::{
    AssignmentState, AssignmentTarget, DocumentId, HighlightColor, LibraryCollectionId,
    LibraryFilter, LibraryItem, LibraryItemKind, LibrarySort, LibrarySortDirection,
    LibrarySortField, LibraryStore, LibraryTagId, LibraryTrashFilter, MetadataSource,
};
use glyphst_ui::{
    button::Button, field::TextField, menu, meter::Meter, motif, pane, row::ListRow, row::TreeRow,
    text,
};

mod search_view;

use search_view::{filter_words, search_words};

use crate::{
    covers::Covers,
    home::Doc,
    theme::{self, Tokens},
};

const LEFT: f32 = 240.0;
const ITEM: f32 = 380.0;
const ROW: f32 = 30.0;

/// What the library shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum View {
    #[default]
    All,
    Recent,
    NeedsReview,
    Unfiled,
    Missing,
    Trash,
    Collection(LibraryCollectionId),
    Tag(LibraryTagId),
}

/// What the tab asks the app to do.
pub enum LibraryTabAction {
    Open(PathBuf),
    /// Ask the assistant about the item's document.
    Ask(PathBuf),
    /// Run the AI pass on the item again.
    Enrich(DocumentId),
    /// Keep or reject one of the AI's tags or collections.
    Accept(DocumentId, AssignmentTarget),
    Reject(DocumentId, AssignmentTarget),
    /// The item's note, to open in the system's editor.
    OpenNote(PathBuf),
    /// Add the documents opened before the library kept them.
    AddOpened(Vec<PathBuf>),
    AddByIdentifier,
    ImportZotero,
    OpenDialog,
    /// Stop the import under way, or hide the last one's tally.
    CancelImport,
    /// Ask about running the AI pass over the items that have none.
    Backfill,
    /// Show the item in the library tab.
    ShowInLibrary(DocumentId),
    /// Keep, or reject, everything the AI suggested for these items.
    AcceptAll(Vec<DocumentId>),
    RejectAll(Vec<DocumentId>),
    /// The item and the one it may duplicate are different works.
    KeepBoth(DocumentId, DocumentId),
    /// Merge `absorb` into `keep` (frames 04l and 04m).
    Merge {
        keep: DocumentId,
        absorb: DocumentId,
    },
    /// Colour a tag, or take its colour off.
    TagColor(LibraryTagId, Option<HighlightColor>),
    /// Open a search hit's file at its page; `query` when it was opened
    /// from a search's results (kept to measure search by).
    OpenAt {
        path: PathBuf,
        page: usize,
        anchor: Option<glyphst_core::SemanticTextAnchor>,
        query: Option<String>,
    },
    /// Ask the assistant about the library, starting from a search.
    AskLibrary(String),
    /// Search by meaning's card: download, pause, …
    Meaning(crate::meaning::MeaningAction),
}

/// What the tab needs from the app this frame.
pub struct TabContext<'a> {
    pub library: &'a LibraryStore,
    pub covers: &'a mut Covers,
    /// Home's documents (for reading progress), by path.
    pub docs: &'a [Doc],
    /// The vault, when there is one.
    pub vault: Option<&'a Path>,
    /// The import under way, or the last one's tally.
    pub import: Option<&'a crate::import::ImportProgress>,
    /// The library's text search, when the index runs.
    pub search: Option<&'a mut crate::library_search::SearchService>,
    /// Search by meaning's card over the results, when it has something to
    /// say (frame 04o).
    pub meaning: Option<crate::meaning::MeaningCard>,
}

#[derive(Default)]
pub struct LibraryTab {
    pub view: View,
    pub filter: String,
    pub selected: Option<DocumentId>,
    sort: LibrarySort,
    /// Collections folded in the tree.
    folded: HashSet<LibraryCollectionId>,
    /// The chosen item's note text, read once per choice.
    note: Option<(DocumentId, String)>,
    /// The search's weaker matches are shown.
    weaker_open: bool,
}

impl LibraryTab {
    /// The tab in `ui`'s whole rect.
    pub fn ui(&mut self, ui: &mut Ui, mut context: TabContext<'_>) -> Option<LibraryTabAction> {
        let tokens = Tokens::get(ui.ctx());
        let area = ui.max_rect();
        ui.painter().rect_filled(area, 0, tokens.bg);
        // A merged item's selection moves to the item it went into.
        self.selected = self
            .selected
            .map(|id| context.library.resolve_item_id(id))
            .filter(|id| context.library.library_item(*id).is_some());
        let left = Rect::from_min_size(area.min, vec2(LEFT, area.height()));
        let item = self
            .selected
            .filter(|_| area.width() > LEFT + ITEM + 480.0)
            .map(|_| Rect::from_min_max(pos2(area.right() - ITEM, area.top()), area.max));
        let center = Rect::from_min_max(
            pos2(left.right(), area.top()),
            pos2(item.map_or(area.right(), |item| item.left()), area.bottom()),
        );
        let mut action = None;
        let mut child = |rect: Rect, id: &str| {
            ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .id_salt(("library-tab", id))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            )
        };
        {
            let mut ui = child(left, "collections");
            if let Some(chosen) = self.collections_pane(&mut ui, &tokens, context.library) {
                action = Some(chosen);
            }
        }
        {
            let mut ui = child(center, "items");
            if let Some(chosen) = self.items(&mut ui, &tokens, &mut context, center) {
                action = Some(chosen);
            }
        }
        if let (Some(rect), Some(id)) = (item, self.selected) {
            let mut ui = child(rect, "item");
            if let Some(chosen) = self.item_pane(&mut ui, &tokens, &mut context, rect, id) {
                action = Some(chosen);
            }
        }
        action
    }

    // ── the collections pane ──────────────────────────────────────────

    fn collections_pane(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &LibraryStore,
    ) -> Option<LibraryTabAction> {
        let mut action = None;
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, tokens.surface);
        pane::vline(ui, rect.right() - 0.5, rect.y_range());
        let active = |item: &&LibraryItem| item.trashed_at.is_none();
        let items = library.library_items().iter().filter(active).count();
        pane::Header::new("library")
            .trailing(items.to_string().as_str())
            .show(ui);
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt("library-collections")
            .auto_shrink(false)
            .max_height(rect.height() - 32.0 - 64.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                let counts = smart_counts(library);
                for (view, icon, label) in [
                    (View::All, "library", "all items"),
                    (View::Recent, "clock", "recently added"),
                    (View::NeedsReview, "sparkles", "needs review"),
                    (View::Unfiled, "inbox", "unfiled"),
                    (View::Missing, "file-question", "missing"),
                    (View::Trash, "trash-2", "trash"),
                ] {
                    let count = counts.get(&view).copied().unwrap_or(0);
                    let meta = count.to_string();
                    let mut row = ListRow::new(label)
                        .icon(icon)
                        .meta(meta.as_str())
                        .active(self.view == view);
                    if view == View::NeedsReview && count > 0 && self.view != view {
                        row = row.title_color(tokens.text_2);
                    }
                    if row.show(ui).clicked() {
                        self.choose(view);
                    }
                }
                ui.add_space(10.0);
                let collections = library.library_collections().len();
                rule(ui, "collections", Some(&collections.to_string()));
                self.collection_tree(ui, library);
                ui.add_space(10.0);
                let tags = library.library_tags().len();
                rule(ui, "tags", Some(&tags.to_string()));
                action = self.tag_cloud(ui, tokens, library);
            });
        // The folder at the foot.
        let foot = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 64.0), rect.max);
        pane::hline(ui, foot.x_range(), foot.top() + 0.5);
        let folder = library
            .files_root()
            .map(home_relative)
            .unwrap_or_else(|| "in memory".to_string());
        let lines = [
            ("folder", tokens.faint, 10.0),
            (folder.as_str(), tokens.text_2, 11.0),
            (
                if library.files_root_chosen() {
                    "notes/ · the vault"
                } else {
                    "not chosen yet · settings › library"
                },
                tokens.muted,
                10.0,
            ),
        ];
        for (index, (line, color, size)) in lines.into_iter().enumerate() {
            text::row(
                ui,
                pos2(foot.left() + 12.0, foot.top() + 14.0 + index as f32 * 16.0),
                line,
                theme::mono(size),
                color,
                foot.width() - 24.0,
            );
        }
        action
    }

    fn choose(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.selected = None;
        }
    }

    fn collection_tree(&mut self, ui: &mut Ui, library: &LibraryStore) {
        let counts = collection_counts(library);
        let mut rows = Vec::new();
        collect_tree(library, None, 0, &self.folded, &mut rows);
        if rows.is_empty() {
            empty_line(ui, "drop a folder to make one");
            return;
        }
        let depths = rows.iter().map(|(_, depth)| *depth).collect::<Vec<_>>();
        let shapes = motif::tree_shape(&depths);
        for ((id, _), (ancestors_last, last)) in rows.iter().zip(shapes) {
            let Some(collection) = library.library_collection(*id) else {
                continue;
            };
            let count = counts.get(id).copied().unwrap_or(0).to_string();
            let response = TreeRow {
                title: &collection.name,
                page: Some(&count),
                ancestors_last: &ancestors_last,
                last,
                active: self.view == View::Collection(*id),
            }
            .show(ui);
            if response.clicked() {
                self.choose(View::Collection(*id));
            }
            if response.double_clicked() && !self.folded.remove(id) {
                self.folded.insert(*id);
            }
        }
    }

    fn tag_cloud(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &LibraryStore,
    ) -> Option<LibraryTabAction> {
        let mut counts = HashMap::<LibraryTagId, usize>::new();
        for relation in library.library_item_tags() {
            *counts.entry(relation.tag_id).or_default() += 1;
        }
        let mut tags = library.library_tags().iter().collect::<Vec<_>>();
        tags.sort_by_key(|tag| std::cmp::Reverse(counts.get(&tag.id).copied().unwrap_or(0)));
        if tags.is_empty() {
            empty_line(ui, "the ai tags items as they come in");
            return None;
        }
        ui.add_space(4.0);
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(12, 0))
            .show(ui, |ui| self.tag_chips(ui, tokens, library, tags, &counts))
            .inner
    }

    fn tag_chips(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &LibraryStore,
        tags: Vec<&glyphst_library::LibraryTag>,
        counts: &HashMap<LibraryTagId, usize>,
    ) -> Option<LibraryTabAction> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for tag in tags.into_iter().take(40) {
                let count = counts.get(&tag.id).copied().unwrap_or(0);
                let active = self.view == View::Tag(tag.id);
                let color = library.tag_color(tag.id);
                let label = format!("#{}", tag.name);
                let response = tag_chip(
                    ui,
                    tokens,
                    &label,
                    Some(count),
                    active,
                    color.map(crate::annotations::color),
                );
                if response.clicked() {
                    self.choose(View::Tag(tag.id));
                }
                // Right-click: the tag's colour (frame 04l).
                response.context_menu(|ui| {
                    if let Some(chosen) = tag_menu(ui, tokens, &label, color) {
                        action = Some(LibraryTabAction::TagColor(tag.id, chosen));
                        ui.close();
                    }
                });
            }
        });
        action
    }

    // ── the items ─────────────────────────────────────────────────────

    fn items(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        rect: Rect,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let mut action = None;
        let inner = rect.shrink2(vec2(28.0, 24.0));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let ui = &mut ui;
        let width = inner.width();
        let ids = self.query(library);
        let (mut kicker, mut heading) = self.titles(library);
        // Words that are not `by:`/`tag:`/`year:` search the text (04n).
        let searching = context
            .search
            .is_some()
            .then(|| search_words(&self.filter))
            .filter(|words| !words.is_empty());
        if let (Some(words), Some(service)) = (&searching, context.search.as_deref_mut()) {
            let scoped = LibraryTab {
                view: self.view,
                filter: filter_words(&self.filter),
                sort: self.sort,
                ..LibraryTab::default()
            };
            service.search(glyphst_search::SearchRequest {
                text: words.clone(),
                items: Some(scoped.query(library).iter().map(|id| id.0).collect()),
                typing: !self.filter.ends_with(char::is_whitespace),
                limit: 200,
                ..Default::default()
            });
            kicker = format!("{kicker} / search");
            heading = words.clone();
        }

        // Heading: `── library / machine learning ──`, the name, counts, and
        // the add buttons at the right.
        let (head, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
        motif::paint_rule_title(
            ui,
            Rect::from_min_size(head.min, vec2(width.min(420.0), 18.0)),
            &kicker,
            None,
            true,
        );
        ui.add_space(6.0);
        let (line, _) = ui.allocate_exact_size(vec2(width, 30.0), Sense::hover());
        let title = text::line(
            ui,
            pos2(line.left(), line.center().y),
            Align2::LEFT_CENTER,
            &heading,
            theme::mono(22.0),
            tokens.text,
            width * 0.5,
        );
        let review = ids
            .iter()
            .filter(|id| library.item_needs_review(**id))
            .count();
        let mut counts = format!("{} items", ids.len());
        if review > 0 {
            counts.push_str(&format!(" · {review} need review"));
        }
        if searching.is_some() {
            counts = context
                .search
                .as_deref()
                .and_then(|service| service.results())
                .map(|results| {
                    let passages = results
                        .items
                        .iter()
                        .map(|item| item.pages.len() + item.more_pages)
                        .sum::<usize>();
                    let mut line = format!("{} items · {passages} passages", results.items.len());
                    if results.reranked {
                        line.push_str(" · reranked");
                    }
                    line
                })
                .unwrap_or_default();
        }
        text::line(
            ui,
            pos2(title.right() + 12.0, line.center().y + 4.0),
            Align2::LEFT_CENTER,
            &counts,
            theme::mono(11.0),
            tokens.muted,
            (line.right() - title.right() - 280.0).max(0.0),
        );
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(line)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        buttons.spacing_mut().item_spacing.x = 8.0;
        if let Some(words) = &searching {
            if buttons.add(Button::bracket("◆ ask the library")).clicked() {
                action = Some(LibraryTabAction::AskLibrary(words.clone()));
            }
        } else if buttons
            .add(Button::bracket_muted("import zotero"))
            .clicked()
        {
            action = Some(LibraryTabAction::ImportZotero);
        }
        let unenriched = library
            .library_items()
            .iter()
            .filter(|item| item.trashed_at.is_none() && library.item_enrichment(item.id).is_none())
            .count();
        if unenriched > 0
            && searching.is_none()
            && buttons
                .add(Button::bracket_muted(&format!("◆ enrich {unenriched}")))
                .on_hover_text("summarise, tag and file the items without an ai pass")
                .clicked()
        {
            action = Some(LibraryTabAction::Backfill);
        }
        let add = if searching.is_none() {
            Some(buttons.add(Button::bracket("+ add")))
        } else {
            None
        };
        if let Some(add) = &add {
            egui::Popup::menu(add).show(|ui| {
                menu::style(ui);
                if menu::Item::new("files or folders…")
                    .icon("file-plus")
                    .show(ui)
                    .clicked()
                {
                    action = Some(LibraryTabAction::OpenDialog);
                }
                if menu::Item::new("by doi, arxiv or isbn…")
                    .icon("hash")
                    .show(ui)
                    .clicked()
                {
                    action = Some(LibraryTabAction::AddByIdentifier);
                }
            });
        }
        ui.add_space(14.0);
        // How much of the library's text is read yet.
        if searching.is_some()
            && let Some(service) = context.search.as_deref()
        {
            let status = service.status();
            let meaning_waits = status.meaning && status.embedded < status.chunks;
            if status.done < status.docs || meaning_waits {
                let mut line =
                    format!("◌ text of {} / {} documents read", status.done, status.docs);
                if meaning_waits {
                    line.push_str(&format!(
                        " · meaning of {} / {} passages",
                        status.embedded, status.chunks
                    ));
                }
                line.push_str(" · the rest are searched as they are read");
                mono_line(ui, &line, tokens.faint, 11.0);
                ui.add_space(10.0);
            }
        }

        // The filter and the order.
        let (controls, _) = ui.allocate_exact_size(vec2(width, 28.0), Sense::hover());
        let mut controls_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        controls_ui.spacing_mut().item_spacing.x = 12.0;
        let sort_text = if searching.is_some() {
            "best match".to_string()
        } else {
            format!("{} ▾", sort_label(self.sort))
        };
        let sort = controls_ui.add_enabled(searching.is_none(), Button::plain(&sort_text).small());
        egui::Popup::menu(&sort).show(|ui| {
            menu::style(ui);
            for field in [
                LibrarySortField::DateAdded,
                LibrarySortField::LastOpened,
                LibrarySortField::Title,
                LibrarySortField::Creator,
                LibrarySortField::Year,
            ] {
                let option = LibrarySort {
                    field,
                    direction: match field {
                        LibrarySortField::Title | LibrarySortField::Creator => {
                            LibrarySortDirection::Ascending
                        }
                        _ => LibrarySortDirection::Descending,
                    },
                };
                if menu::Item::new(sort_label(option))
                    .checked(self.sort.field == field)
                    .show(ui)
                    .clicked()
                {
                    self.sort = option;
                }
            }
        });
        TextField::search(
            &mut self.filter,
            "filter · by:vaswani tag:attention year:>2020",
        )
        .shortcut("/")
        .width(controls_ui.available_width())
        .id(egui::Id::unique("library-filter"))
        .show(&mut controls_ui);
        if ui.input(|i| i.key_pressed(egui::Key::Slash))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(egui::Id::unique("library-filter")));
        }
        ui.add_space(14.0);

        if let Some(progress) = context.import
            && import_strip(ui, tokens, progress, width)
        {
            action = Some(LibraryTabAction::CancelImport);
        }
        if library.library_items().is_empty() {
            return self.empty(ui, tokens, context, width).or(action);
        }
        if searching.is_some() {
            return self.search_part(ui, tokens, context, width).or(action);
        }
        if self.view == View::NeedsReview {
            return review_list(ui, tokens, library, &ids, width).or(action);
        }

        // The table.
        let columns = columns(width);
        let (header, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
        for (name, x) in ["title", "authors", "year", "tags", "ai", "read"]
            .iter()
            .zip(&columns)
        {
            ui.painter().text(
                pos2(header.left() + x + 8.0, header.center().y),
                Align2::LEFT_CENTER,
                *name,
                theme::mono(10.0),
                tokens.faint,
            );
        }
        ui.painter()
            .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
        if ids.is_empty() {
            ui.add_space(16.0);
            empty_line(ui, "nothing here");
            return action;
        }
        let progress = context
            .docs
            .iter()
            .map(|doc| (doc.path.clone(), doc))
            .collect::<HashMap<_, _>>();
        let mut keyboard = None;
        egui::ScrollArea::vertical()
            .id_salt("library-rows")
            .auto_shrink(false)
            .show_rows(ui, ROW, ids.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for id in &ids[range] {
                    let Some(item) = library.library_item(*id) else {
                        continue;
                    };
                    let selected = self.selected == Some(*id);
                    let (row, response) = glyphst_ui::row::list_row(ui, ROW, selected);
                    let duplicate = possible_duplicate(library, item);
                    if ui.is_rect_visible(row) {
                        let path = library
                            .primary_attachment(*id)
                            .map(|attachment| attachment.path.clone());
                        let doc = path.as_ref().and_then(|path| progress.get(path).copied());
                        match duplicate {
                            // The row of a possible duplicate (frame 04g).
                            Some(other) => {
                                if !selected {
                                    ui.painter().rect_filled(row, 0, tokens.accent_soft);
                                }
                                paint_duplicate_row(ui, tokens, row, &columns, item, other);
                            }
                            None => paint_item_row(
                                ui, tokens, row, &columns, library, item, doc, selected,
                            ),
                        }
                    }
                    if let Some(other) = duplicate {
                        let right = row.right() - 8.0;
                        let buttons = Rect::from_min_max(
                            pos2(row.left() + columns[3], row.top()),
                            pos2(right, row.bottom()),
                        );
                        let mut buttons_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(buttons)
                                .layout(egui::Layout::right_to_left(egui::Align::Center)),
                        );
                        buttons_ui.spacing_mut().item_spacing.x = 8.0;
                        if buttons_ui.add(Button::bracket("merge")).clicked() {
                            action = Some(merge_action(item, other));
                        }
                        if buttons_ui.add(Button::bracket_muted("keep both")).clicked() {
                            action = Some(LibraryTabAction::KeepBoth(item.id, other.id));
                        }
                    }
                    if response.clicked() {
                        self.selected = Some(*id);
                    }
                    if response.double_clicked()
                        && let Some(attachment) = library.primary_attachment(*id)
                    {
                        action = Some(LibraryTabAction::Open(attachment.path.clone()));
                    }
                    if selected && response.has_focus() {
                        keyboard = Some(*id);
                    }
                }
            });
        // Enter opens the chosen item; Escape lets it go.
        if ui.ctx().memory(|memory| memory.focused().is_none()) {
            if ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Some(id) = self.selected.or(keyboard)
                && let Some(attachment) = library.primary_attachment(id)
            {
                action = Some(LibraryTabAction::Open(attachment.path.clone()));
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected = None;
            }
        }
        action
    }

    /// No items yet: what to do about it.
    fn empty(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &TabContext<'_>,
        width: f32,
    ) -> Option<LibraryTabAction> {
        let mut action = None;
        let (rect, _) = ui.allocate_exact_size(vec2(width, 160.0), Sense::hover());
        motif::dot_field(ui, rect, 14.0, 1.0, |_, _| 0.0);
        let painter = ui.painter();
        painter.text(
            rect.center() - vec2(0.0, 30.0),
            Align2::CENTER_CENTER,
            "the library is empty",
            theme::mono(13.0),
            tokens.text_2,
        );
        painter.text(
            rect.center() - vec2(0.0, 8.0),
            Align2::CENTER_CENTER,
            "drop pdfs, epubs or folders here · they are recognised, summarised and filed",
            theme::mono(11.0),
            tokens.muted,
        );
        let opened = context
            .docs
            .iter()
            .filter(|doc| crate::import::is_library_document(&doc.path) && doc.path.exists())
            .map(|doc| doc.path.clone())
            .collect::<Vec<_>>();
        let mut buttons = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_center_size(
                    rect.center() + vec2(0.0, 30.0),
                    vec2(width, 28.0),
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center).with_main_justify(false)),
        );
        let label = format!("add the {} documents you opened", opened.len());
        let total = text::width(&buttons, &label, &theme::mono(12.0)) + 200.0;
        buttons.add_space(((width - total) / 2.0).max(0.0));
        if !opened.is_empty() && buttons.add(Button::bracket(&label)).clicked() {
            action = Some(LibraryTabAction::AddOpened(opened));
        }
        if buttons.add(Button::bracket_muted("open files…")).clicked() {
            action = Some(LibraryTabAction::OpenDialog);
        }
        action
    }

    /// The items the view and filter choose, in order.
    pub fn query(&self, library: &LibraryStore) -> Vec<DocumentId> {
        let mut filter = parse_filter(&self.filter, library);
        let mut sort = self.sort;
        match self.view {
            View::All | View::NeedsReview => {}
            View::Recent => {
                sort = LibrarySort {
                    field: LibrarySortField::DateAdded,
                    direction: LibrarySortDirection::Descending,
                };
            }
            View::Unfiled => filter.unfiled = true,
            View::Missing => filter.missing = Some(true),
            View::Trash => filter.trash = LibraryTrashFilter::Trashed,
            View::Collection(id) => filter.collection_id = Some(id),
            View::Tag(id) => filter.tag_ids.push(id),
        }
        let mut ids = library.query_library_items(&filter, sort);
        match self.view {
            View::NeedsReview => ids.retain(|id| library.item_needs_review(*id)),
            View::Recent => {
                let since = glyphst_library::current_timestamp().saturating_sub(30 * 86_400);
                ids.retain(|id| {
                    library
                        .library_item(*id)
                        .is_some_and(|item| item.created >= since)
                });
            }
            _ => {}
        }
        ids
    }

    /// `── library / …` and the heading.
    fn titles(&self, library: &LibraryStore) -> (String, String) {
        let named = |name: &str| (format!("library / {name}"), name.to_string());
        match self.view {
            View::All => ("library".to_string(), "all items".to_string()),
            View::Recent => named("recently added"),
            View::NeedsReview => named("needs review"),
            View::Unfiled => named("unfiled"),
            View::Missing => named("missing"),
            View::Trash => named("trash"),
            View::Collection(id) => {
                let path = library
                    .collection_ancestors(id)
                    .into_iter()
                    .map(|collection| collection.name.to_lowercase())
                    .collect::<Vec<_>>();
                (
                    format!("library / {}", path.join(" / ")),
                    path.last().cloned().unwrap_or_default(),
                )
            }
            View::Tag(id) => {
                let name = library
                    .library_tags()
                    .iter()
                    .find(|tag| tag.id == id)
                    .map(|tag| format!("#{}", tag.name))
                    .unwrap_or_default();
                named(&name)
            }
        }
    }

    // ── the item pane ─────────────────────────────────────────────────

    fn item_pane(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        rect: Rect,
        id: DocumentId,
    ) -> Option<LibraryTabAction> {
        context.library.library_item(id)?;
        ui.painter().rect_filled(rect, 0, tokens.surface);
        pane::vline(ui, rect.left() + 0.5, rect.y_range());
        let header = pane::Header::new("item")
            .action(pane::Action::new("x", "close"))
            .show(ui);
        if header.clicked == Some(0) {
            self.selected = None;
            return None;
        }
        let card = Rect::from_min_max(pos2(rect.left(), rect.top() + 32.0), rect.max);
        self.item_card(ui, context, card, id, true, 16.0)
    }

    /// An item's card in `rect`: its body, scrolling, and its actions at the
    /// foot (`open` among them when `with_open`). The sidebar's item tab
    /// shows it too (frame 03b).
    pub fn item_card(
        &mut self,
        ui: &mut Ui,
        context: &mut TabContext<'_>,
        rect: Rect,
        id: DocumentId,
        with_open: bool,
        margin: f32,
    ) -> Option<LibraryTabAction> {
        let tokens = Tokens::get(ui.ctx());
        let library = context.library;
        let item = library.library_item(id)?.clone();
        let mut action = None;
        let path = library
            .primary_attachment(id)
            .map(|attachment| attachment.path.clone());
        let note_path = context
            .vault
            .zip(library.vault_note(id))
            .map(|(vault, note)| vault.join(&note.relative_path));
        // The note's own text, read once per item chosen.
        if self.note.as_ref().is_none_or(|(noted, _)| *noted != id) {
            let text = note_path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|note| notes_section(&note))
                .unwrap_or_else(|| item.work_note.clone());
            self.note = Some((id, text));
        }
        let actions_height = 36.0;
        let body = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - actions_height));
        let mut body_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(body)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt(("library-item", with_open))
            .auto_shrink(false)
            .show(&mut body_ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(margin as i8, 14))
                    .show(ui, |ui| {
                        ui.set_width(body.width() - 2.0 * margin);
                        ui.spacing_mut().item_spacing.y = 4.0;
                        if let Some(chosen) = self.item_body(
                            ui,
                            &tokens,
                            context,
                            &item,
                            path.as_deref(),
                            note_path.as_deref(),
                        ) {
                            action = Some(chosen);
                        }
                    });
            });
        // The actions along the foot.
        let foot = Rect::from_min_max(pos2(rect.left(), rect.bottom() - actions_height), rect.max);
        pane::hline(ui, foot.x_range(), foot.top() + 0.5);
        let mut foot_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(foot.shrink2(vec2(margin.min(12.0), 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        foot_ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(path) = &path {
            if with_open && foot_ui.add(Button::bracket("open")).clicked() {
                action = Some(LibraryTabAction::Open(path.clone()));
            }
            if !with_open && foot_ui.add(Button::bracket("library")).clicked() {
                action = Some(LibraryTabAction::ShowInLibrary(id));
            }
            if let Some(note) = &note_path
                && foot_ui.add(Button::bracket_muted("open note")).clicked()
            {
                action = Some(LibraryTabAction::OpenNote(note.clone()));
            }
            if with_open && foot_ui.add(Button::bracket_muted("ask")).clicked() {
                action = Some(LibraryTabAction::Ask(path.clone()));
            }
        }
        foot_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(Button::bracket_muted("◆ re-run")).clicked() {
                action = Some(LibraryTabAction::Enrich(id));
            }
        });
        action
    }

    fn item_body(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        item: &LibraryItem,
        path: Option<&Path>,
        note_path: Option<&Path>,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let id = item.id;
        let mut action = None;

        // The cover, title, authors, venue and where the metadata came from.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let kind = kind_line(item, path);
            let page = path.and_then(|path| context.covers.get(path));
            let (cover, _) = ui.allocate_exact_size(vec2(72.0, 96.0), Sense::hover());
            paint_cover(ui, tokens, cover, page, &kind);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&item.title)
                            .font(theme::semibold(16.0))
                            .color(tokens.text),
                    )
                    .wrap(),
                );
                if !item.creators.is_empty() {
                    let authors = authors_line(item, 5);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(authors)
                                .font(theme::regular(12.0))
                                .color(tokens.text_2),
                        )
                        .wrap(),
                    );
                }
                let mut venue = Vec::new();
                if let Some(year) = item.year {
                    venue.push(year.to_string());
                }
                if !item.publication.is_empty() {
                    venue.push(item.publication.clone());
                }
                venue.push(kind_name(item.kind).to_string());
                mono_line(ui, &venue.join(" · "), tokens.muted, 11.0);
                let source = library
                    .metadata_record(id)
                    .map(|record| {
                        let mut line = format!("via {} ✓", record.source.label());
                        if record.source == MetadataSource::Filename {
                            line = "from the file name".to_string();
                        }
                        if let Some(identifier) = &record.matched_identifier {
                            line.push_str(&format!(" · {identifier}"));
                        }
                        line
                    })
                    .unwrap_or_else(|| "looking it up…".to_string());
                mono_line(ui, &source, tokens.faint, 10.0);
            });
        });
        ui.add_space(10.0);

        // The work it may duplicate, to merge or keep apart (frame 04l).
        if let Some(other) = possible_duplicate(library, item) {
            if let Some(chosen) = duplicate_box(ui, tokens, library, item, other) {
                action = Some(chosen);
            }
            ui.add_space(10.0);
        }

        // The AI's summary.
        if let Some(enrichment) = library.item_enrichment(id) {
            let date = chrono::DateTime::from_timestamp(enrichment.finished_at as i64, 0)
                .map(|time| time.format("%b %d").to_string().to_lowercase())
                .unwrap_or_default();
            accent_rule(
                ui,
                tokens,
                "◆ summary",
                &format!("{} · {date}", short_model(&enrichment.model)),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&enrichment.tldr)
                        .font(theme::regular(13.0))
                        .color(tokens.text),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            for point in &enrichment.key_points {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.label(
                        egui::RichText::new("·")
                            .font(theme::mono(12.0))
                            .color(tokens.accent),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(point)
                                .font(theme::regular(12.0))
                                .color(tokens.text_2),
                        )
                        .wrap(),
                    );
                });
            }
            ui.add_space(10.0);
        } else if !item.abstract_text.is_empty() {
            flush_rule(ui, "abstract", None);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(clip(&item.abstract_text, 600))
                        .font(theme::regular(12.0))
                        .color(tokens.text_2),
                )
                .wrap(),
            );
            ui.add_space(10.0);
        }

        // Tags: the user's filled, the AI's outlined with ✓ and ×.
        flush_rule(ui, "tags", None);
        let assignments = library.ai_assignments(id).to_vec();
        let ai_tag = |tag: LibraryTagId| {
            assignments.iter().find(|assignment| {
                assignment.target == AssignmentTarget::Tag(tag) && assignment.state.needs_review()
            })
        };
        let tags = library.tags_for_item(id);
        if tags.is_empty() {
            mono_line(ui, "no tags yet", tokens.faint, 11.0);
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for tag in &tags {
                    match ai_tag(tag.id) {
                        Some(assignment) => {
                            let (accept, reject) = ai_chip(ui, tokens, &tag.name);
                            if accept {
                                action =
                                    Some(LibraryTabAction::Accept(id, assignment.target.clone()));
                            }
                            if reject {
                                action =
                                    Some(LibraryTabAction::Reject(id, assignment.target.clone()));
                            }
                        }
                        None => {
                            let color = library.tag_color(tag.id).map(crate::annotations::color);
                            tag_chip(ui, tokens, &format!("#{}", tag.name), None, false, color);
                        }
                    }
                }
            });
            if tags.iter().any(|tag| ai_tag(tag.id).is_some()) {
                mono_line(ui, "◆ added by ai · ✓ keep · × reject", tokens.faint, 10.0);
            }
        }
        ui.add_space(10.0);

        // Collections, and a proposed one.
        flush_rule(ui, "collections", None);
        let collections = library.collections_for_item(id);
        if collections.is_empty() {
            mono_line(ui, "in no collection", tokens.faint, 11.0);
        }
        for collection in collections {
            let by_ai = assignments.iter().any(|assignment| {
                assignment.target == AssignmentTarget::Collection(collection.id)
                    && assignment.state.needs_review()
            });
            let label = format!(
                "■ {}",
                library
                    .collection_path_label(collection.id)
                    .replace('/', " / ")
                    .to_lowercase()
            );
            ui.horizontal(|ui| {
                mono_line(ui, &label, tokens.text_2, 11.0);
                if by_ai {
                    let target = AssignmentTarget::Collection(collection.id);
                    if ui
                        .add(Button::plain("◆ ✓").small())
                        .on_hover_text("keep (filed by ai)")
                        .clicked()
                    {
                        action = Some(LibraryTabAction::Accept(id, target.clone()));
                    }
                    if ui
                        .add(Button::plain("×").small())
                        .on_hover_text("take it out")
                        .clicked()
                    {
                        action = Some(LibraryTabAction::Reject(id, target));
                    }
                }
            });
        }
        for assignment in assignments.iter().filter(|assignment| {
            matches!(assignment.target, AssignmentTarget::NewCollection(_))
                && assignment.state == AssignmentState::Suggested
        }) {
            egui::Frame::new()
                .fill(tokens.accent_soft)
                .corner_radius(tokens.ctl_radius())
                .inner_margin(egui::Margin::symmetric(8, 4))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let mut create = false;
                        let mut dismiss = false;
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            dismiss = ui.add(Button::plain("×").small()).clicked();
                            create = ui.add(Button::bracket("create")).clicked();
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "◆ new: {}",
                                                assignment.label.to_lowercase()
                                            ))
                                            .font(theme::mono(11.0))
                                            .color(tokens.accent_text),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                        });
                        if create {
                            action = Some(LibraryTabAction::Accept(id, assignment.target.clone()));
                        }
                        if dismiss {
                            action = Some(LibraryTabAction::Reject(id, assignment.target.clone()));
                        }
                    });
                });
        }
        ui.add_space(10.0);

        // Its files, when a merge gave it more than one (frame 04m).
        let attachments = library
            .library_attachments()
            .iter()
            .filter(|attachment| attachment.item_id == id)
            .collect::<Vec<_>>();
        if attachments.len() > 1 {
            flush_rule(ui, "files", None);
            let merged_at = library
                .library_item_merges()
                .iter()
                .filter(|merge| merge.into == id)
                .map(|merge| merge.at)
                .max();
            for attachment in &attachments {
                let name = attachment
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let highlights = library.highlights_for(&attachment.path).len();
                let detail = match merged_at {
                    Some(at) if !attachment.primary => format!("merged · {}", short_date(at)),
                    _ if highlights > 0 => plural(highlights, "highlight"),
                    _ => String::new(),
                };
                let color = if attachment.primary {
                    tokens.text_2
                } else {
                    tokens.muted
                };
                if link_row(ui, tokens, "▤", &name, &detail, color).double_clicked() {
                    action = Some(LibraryTabAction::Open(attachment.path.clone()));
                }
            }
            ui.add_space(10.0);
        }

        // The items related to it, from Zotero or an assistant.
        let related = library.related_items(id);
        if !related.is_empty() {
            flush_rule(ui, "related", None);
            for other in related {
                let Some(other) = library.library_item(other) else {
                    continue;
                };
                let year = other.year.map(|year| year.to_string()).unwrap_or_default();
                if link_row(ui, tokens, "↔", &other.title, &year, tokens.text_2).clicked() {
                    action = Some(LibraryTabAction::ShowInLibrary(other.id));
                }
            }
            ui.add_space(10.0);
        }

        // The note.
        let note_name = note_path
            .and_then(|path| path.file_name())
            .map(|name| format!("notes/items/{}", name.to_string_lossy()));
        flush_rule(ui, "note", note_name.as_deref());
        let note = self
            .note
            .as_ref()
            .map(|(_, note)| note.trim().to_string())
            .unwrap_or_default();
        if note.is_empty() {
            mono_line(
                ui,
                if note_path.is_some() {
                    "write in the note; it stays yours"
                } else {
                    "no notes · choose a library folder for the vault"
                },
                tokens.faint,
                11.0,
            );
        } else {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(clip(&note, 800))
                        .font(theme::regular(12.0))
                        .color(tokens.muted),
                )
                .wrap(),
            );
        }
        action
    }
}

// ── helpers ───────────────────────────────────────────────────────────────

/// The import strip over the table (frame 04g): `adding 12 / 40`, a meter,
/// what was skipped, and `[ cancel ]` (or `[ dismiss ]` once done).
/// Returns whether its button was clicked.
fn import_strip(
    ui: &mut Ui,
    tokens: &Tokens,
    progress: &crate::import::ImportProgress,
    width: f32,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 36.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, tokens.ctl_radius(), tokens.surface);
    ui.painter().rect_stroke(
        rect,
        tokens.ctl_radius(),
        Stroke::new(1.0, tokens.rule_strong),
        egui::StrokeKind::Inside,
    );
    let y = rect.center().y;
    let total = progress.total.max(progress.processed);
    let count = if progress.finished {
        format!("added {}", progress.added.len())
    } else {
        format!("adding {} / {total}", progress.processed)
    };
    let mut x = rect.left() + 12.0;
    let count_rect = text::line(
        ui,
        pos2(x, y),
        Align2::LEFT_CENTER,
        &count,
        theme::mono(12.0),
        tokens.text,
        200.0,
    );
    x = count_rect.right() + 14.0;
    if !progress.finished && total > 0 {
        Meter::new(progress.processed as f32 / total as f32)
            .bar()
            .width(140.0)
            .paint(ui, Rect::from_min_size(pos2(x, y - 7.0), vec2(140.0, 14.0)));
        x += 154.0;
    }
    let mut detail = Vec::new();
    if !progress.duplicates.is_empty() {
        detail.push(format!("{} duplicates", progress.duplicates.len()));
    }
    if !progress.failed.is_empty() {
        detail.push(format!("{} failed", progress.failed.len()));
    }
    detail.push(format!("from {}", progress.source));
    text::line(
        ui,
        pos2(x, y),
        Align2::LEFT_CENTER,
        detail.join(" · "),
        theme::mono(11.0),
        tokens.muted,
        (rect.right() - x - 120.0).max(0.0),
    );
    let label = if progress.finished {
        "dismiss"
    } else {
        "cancel"
    };
    let button = Rect::from_min_max(pos2(rect.right() - 110.0, rect.top()), rect.max);
    let mut button_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(button.shrink2(vec2(12.0, 0.0)))
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let clicked = button_ui.add(Button::bracket_muted(label)).clicked();
    ui.add_space(14.0);
    clicked
}

/// The AI's suggestions, item by item (frame 04i), with accept and reject
/// for each and for all.
fn review_list(
    ui: &mut Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    ids: &[DocumentId],
    width: f32,
) -> Option<LibraryTabAction> {
    let mut action = None;
    if ids.is_empty() {
        empty_line(ui, "nothing to review · the ai's changes are all looked at");
        return None;
    }
    let (bar, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let mut bar_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(bar)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    bar_ui.spacing_mut().item_spacing.x = 8.0;
    if bar_ui.add(Button::bracket_muted("× reject all")).clicked() {
        action = Some(LibraryTabAction::RejectAll(ids.to_vec()));
    }
    if bar_ui.add(Button::bracket("✓ accept all")).clicked() {
        action = Some(LibraryTabAction::AcceptAll(ids.to_vec()));
    }
    ui.add_space(6.0);
    egui::ScrollArea::vertical()
        .id_salt("library-review")
        .auto_shrink(false)
        .show(ui, |ui| {
            for id in ids {
                let Some(item) = library.library_item(*id) else {
                    continue;
                };
                let pending = library
                    .ai_assignments(*id)
                    .iter()
                    .filter(|assignment| assignment.state.needs_review())
                    .cloned()
                    .collect::<Vec<_>>();
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(&item.title)
                            .font(theme::regular(14.0))
                            .color(tokens.text),
                    );
                    let mut meta = Vec::new();
                    if let Some(year) = item.year {
                        meta.push(year.to_string());
                    }
                    let authors = authors_short(item);
                    if !authors.is_empty() {
                        meta.push(authors);
                    }
                    if let Some(enrichment) = library.item_enrichment(*id) {
                        meta.push(short_model(&enrichment.model).to_string());
                    }
                    ui.label(
                        egui::RichText::new(meta.join(" · "))
                            .font(theme::mono(11.0))
                            .color(tokens.faint),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(path) = library.primary_attachment(*id).map(|a| a.path.clone())
                            && ui.add(Button::bracket_muted("open")).clicked()
                        {
                            action = Some(LibraryTabAction::Open(path));
                        }
                        let label = format!("✓ accept {}", pending.len());
                        if ui.add(Button::bracket(&label)).clicked() {
                            action = Some(LibraryTabAction::AcceptAll(vec![*id]));
                        }
                    });
                });
                ui.add_space(6.0);
                for (kind, targets) in [
                    (
                        "tags",
                        pending
                            .iter()
                            .filter(|a| matches!(a.target, AssignmentTarget::Tag(_)))
                            .collect::<Vec<_>>(),
                    ),
                    (
                        "collection",
                        pending
                            .iter()
                            .filter(|a| !matches!(a.target, AssignmentTarget::Tag(_)))
                            .collect::<Vec<_>>(),
                    ),
                ] {
                    if targets.is_empty() {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        let (label, _) = ui.allocate_exact_size(vec2(90.0, 19.0), Sense::hover());
                        ui.painter().text(
                            label.left_center(),
                            Align2::LEFT_CENTER,
                            kind,
                            theme::mono(11.0),
                            tokens.muted,
                        );
                        ui.spacing_mut().item_spacing.x = 8.0;
                        for assignment in targets {
                            let name = match &assignment.target {
                                AssignmentTarget::NewCollection(_) => {
                                    format!("new: {}", assignment.label.to_lowercase())
                                }
                                _ => assignment.label.to_lowercase(),
                            };
                            let (accept, reject) = ai_chip(ui, tokens, &name);
                            if accept {
                                action =
                                    Some(LibraryTabAction::Accept(*id, assignment.target.clone()));
                            }
                            if reject {
                                action =
                                    Some(LibraryTabAction::Reject(*id, assignment.target.clone()));
                            }
                        }
                    });
                    ui.add_space(4.0);
                }
                ui.add_space(8.0);
                let bottom = ui.cursor().top();
                ui.painter()
                    .hline(ui.max_rect().x_range(), bottom - 0.5, tokens.hairline());
            }
        });
    action
}

/// The items under each smart view.
fn smart_counts(library: &LibraryStore) -> HashMap<View, usize> {
    let since = glyphst_library::current_timestamp().saturating_sub(30 * 86_400);
    let mut counts = HashMap::new();
    for item in library.library_items() {
        if item.trashed_at.is_some() {
            *counts.entry(View::Trash).or_default() += 1;
            continue;
        }
        *counts.entry(View::All).or_default() += 1;
        if item.created >= since {
            *counts.entry(View::Recent).or_default() += 1;
        }
        if library.item_needs_review(item.id) {
            *counts.entry(View::NeedsReview).or_default() += 1;
        }
        if library.collections_for_item(item.id).is_empty() {
            *counts.entry(View::Unfiled).or_default() += 1;
        }
        if library
            .primary_attachment(item.id)
            .is_some_and(|attachment| attachment.is_missing())
        {
            *counts.entry(View::Missing).or_default() += 1;
        }
    }
    counts
}

/// Each collection's items, its subcollections' included.
fn collection_counts(library: &LibraryStore) -> HashMap<LibraryCollectionId, usize> {
    let active = library
        .library_items()
        .iter()
        .filter(|item| item.trashed_at.is_none())
        .map(|item| item.id)
        .collect::<HashSet<_>>();
    let mut members = HashMap::<LibraryCollectionId, HashSet<DocumentId>>::new();
    for membership in library.library_item_collections() {
        if !active.contains(&membership.item_id) {
            continue;
        }
        for ancestor in library.collection_ancestors(membership.collection_id) {
            members
                .entry(ancestor.id)
                .or_default()
                .insert(membership.item_id);
        }
    }
    members
        .into_iter()
        .map(|(id, items)| (id, items.len()))
        .collect()
}

fn collect_tree(
    library: &LibraryStore,
    parent: Option<LibraryCollectionId>,
    depth: usize,
    folded: &HashSet<LibraryCollectionId>,
    rows: &mut Vec<(LibraryCollectionId, usize)>,
) {
    for collection in library.child_collections(parent) {
        rows.push((collection.id, depth));
        if !folded.contains(&collection.id) {
            collect_tree(library, Some(collection.id), depth + 1, folded, rows);
        }
    }
}

/// The filter's typed words: `by:name`, `tag:name`, `year:2017`,
/// `year:>2020`, `year:<2000`, `year:2018-2020`; the rest is free text.
pub fn parse_filter(query: &str, library: &LibraryStore) -> LibraryFilter {
    let mut filter = LibraryFilter::default();
    let mut words = Vec::new();
    for word in query.split_whitespace() {
        let lower = word.to_lowercase();
        if let Some(name) = lower
            .strip_prefix("tag:")
            .or_else(|| lower.strip_prefix('#'))
        {
            let name = name.trim_matches('"');
            match library.library_tags().iter().find(|tag| {
                tag.normalized_name == name
                    || glyphst_vault::vault_tag(&tag.normalized_name) == name
            }) {
                Some(tag) => filter.tag_ids.push(tag.id),
                // An unknown tag matches nothing.
                None => filter.tag_ids.push(LibraryTagId(0)),
            }
        } else if let Some(year) = lower.strip_prefix("year:") {
            let parse = |text: &str| text.trim().parse::<i32>().ok();
            if let Some(after) = year.strip_prefix('>') {
                filter.year_min = parse(after).map(|year| year + 1);
            } else if let Some(before) = year.strip_prefix('<') {
                filter.year_max = parse(before).map(|year| year - 1);
            } else if let Some((from, to)) = year.split_once('-') {
                filter.year_min = parse(from);
                filter.year_max = parse(to);
            } else {
                filter.year_min = parse(year);
                filter.year_max = parse(year);
            }
        } else if let Some(name) = lower.strip_prefix("by:") {
            words.push(name.to_string());
        } else {
            words.push(lower);
        }
    }
    filter.text = words.join(" ");
    filter
}

fn sort_label(sort: LibrarySort) -> &'static str {
    match sort.field {
        LibrarySortField::DateAdded => "added",
        LibrarySortField::LastOpened => "opened",
        LibrarySortField::Title => "title",
        LibrarySortField::Creator => "author",
        LibrarySortField::Year => "year",
    }
}

/// The columns' left edges in a table `width` wide.
fn columns(width: f32) -> [f32; 6] {
    let fixed = [130.0, 44.0, 150.0, 28.0, 84.0];
    let title = (width - fixed.iter().sum::<f32>() - 12.0 * 5.0).max(120.0);
    let mut x = [0.0; 6];
    let mut at = title + 12.0;
    for (index, width) in fixed.iter().enumerate() {
        x[index + 1] = at;
        at += width + 12.0;
    }
    x
}

#[allow(clippy::too_many_arguments)]
fn paint_item_row(
    ui: &Ui,
    tokens: &Tokens,
    rect: Rect,
    columns: &[f32; 6],
    library: &LibraryStore,
    item: &LibraryItem,
    doc: Option<&Doc>,
    selected: bool,
) {
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    let cell = |column: usize, value: &str, font: egui::FontId, color: egui::Color32| {
        let right = columns.get(column + 1).copied().unwrap_or(rect.width());
        text::row(
            ui,
            pos2(rect.left() + columns[column] + 8.0, y),
            value,
            font,
            color,
            right - columns[column] - 12.0,
        );
    };
    cell(
        0,
        &item.title,
        theme::regular(13.0),
        if selected { tokens.text } else { tokens.text_2 },
    );
    cell(1, &authors_short(item), theme::mono(11.0), tokens.muted);
    cell(
        2,
        &item.year.map(|year| year.to_string()).unwrap_or_default(),
        theme::mono(11.0),
        tokens.muted,
    );
    let assignments = library.ai_assignments(item.id);
    let tags = library
        .tags_for_item(item.id)
        .into_iter()
        .map(|tag| {
            let by_ai = assignments.iter().any(|assignment| {
                assignment.target == AssignmentTarget::Tag(tag.id)
                    && assignment.state.needs_review()
            });
            format!("{}{}", if by_ai { "◆" } else { "#" }, tag.name)
        })
        .collect::<Vec<_>>()
        .join(" ");
    cell(3, &tags, theme::mono(11.0), tokens.muted);
    let (glyph, color) = if library.item_needs_review(item.id) {
        ("?", tokens.accent_text)
    } else if library.item_enrichment(item.id).is_some() {
        ("◆", tokens.accent)
    } else if library
        .library_jobs()
        .iter()
        .any(|job| job.item_id == item.id)
    {
        ("…", tokens.accent_text)
    } else {
        ("·", tokens.faint)
    };
    cell(4, glyph, theme::mono(11.0), color);
    let read_x = rect.left() + columns[5] + 8.0;
    match doc.and_then(Doc::progress) {
        Some(progress) if progress >= 0.999 => {
            cell(5, "done ✓", theme::mono(11.0), tokens.ok);
        }
        Some(progress) if progress > 0.0 => {
            let label = format!("{:.0}%", progress * 100.0);
            Meter::new(progress).bar().width(40.0).label(&label).paint(
                ui,
                Rect::from_min_size(pos2(read_x, y - 7.0), vec2(84.0, 14.0)),
            );
        }
        _ => cell(5, "—", theme::mono(11.0), tokens.faint),
    }
}

/// A possible duplicate's row: its file and what it shares with the other
/// item across the title and authors, then its year; the buttons go on top.
fn paint_duplicate_row(
    ui: &Ui,
    tokens: &Tokens,
    rect: Rect,
    columns: &[f32; 6],
    item: &LibraryItem,
    other: &LibraryItem,
) {
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    let left = rect.left() + columns[0] + 8.0;
    let width = columns[2] - columns[0] - 12.0;
    let title = text::line(
        ui,
        pos2(left, y),
        Align2::LEFT_CENTER,
        &item.title,
        theme::regular(13.0),
        tokens.text,
        width * 0.45,
    );
    text::row(
        ui,
        pos2(title.right() + 12.0, y),
        duplicate_line(item, other),
        theme::mono(11.0),
        tokens.accent_text,
        (left + width - title.right() - 12.0).max(0.0),
    );
    text::row(
        ui,
        pos2(rect.left() + columns[2] + 8.0, y),
        item.year.map(|year| year.to_string()).unwrap_or_default(),
        theme::mono(11.0),
        tokens.muted,
        columns[3] - columns[2] - 12.0,
    );
}

fn authors_short(item: &LibraryItem) -> String {
    let surname = |name: &str| match name.split_once(',') {
        Some((last, _)) => last.trim().to_string(),
        None => name.split_whitespace().last().unwrap_or(name).to_string(),
    };
    match item.creators.as_slice() {
        [] => String::new(),
        [one] => surname(&one.name),
        [one, two] => format!("{}, {}", surname(&one.name), surname(&two.name)),
        [first, ..] => format!("{} et al.", surname(&first.name)),
    }
}

fn authors_line(item: &LibraryItem, shown: usize) -> String {
    let mut names = item
        .creators
        .iter()
        .take(shown)
        .map(|creator| creator.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    if item.creators.len() > shown {
        names.push_str(&format!(", +{}", item.creators.len() - shown));
    }
    names
}

fn kind_name(kind: LibraryItemKind) -> &'static str {
    match kind {
        LibraryItemKind::Paper => "paper",
        LibraryItemKind::Book => "book",
        LibraryItemKind::Other => "other",
    }
}

fn kind_line(item: &LibraryItem, path: Option<&Path>) -> String {
    path.and_then(|path| path.extension())
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| kind_name(item.kind).to_string())
}

/// A model id as the summary's rule shows it (`claude-haiku-5-5` →
/// `haiku-5-5`).
fn short_model(model: &str) -> &str {
    model.strip_prefix("claude-").unwrap_or(model)
}

fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let mut clipped = text.chars().take(chars).collect::<String>();
    clipped.push('…');
    clipped
}

/// The user's part of a note: what follows `## Notes`.
fn notes_section(note: &str) -> String {
    note.split_once("\n## Notes")
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_default()
}

fn home_relative(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// A pane's rule title, inset like its rows.
fn rule(ui: &mut Ui, title: &str, trailing: Option<&str>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    motif::paint_rule_title(ui, rect.shrink2(vec2(12.0, 0.0)), title, trailing, false);
}

/// A rule title flush with the content around it.
fn flush_rule(ui: &mut Ui, title: &str, trailing: Option<&str>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    motif::paint_rule_title(ui, rect, title, trailing, false);
}

/// The first page in `rect`, or a blank page with the file's kind.
fn paint_cover(
    ui: &Ui,
    tokens: &Tokens,
    rect: Rect,
    page: Option<crate::covers::CoverTexture>,
    kind: &str,
) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0, tokens.page);
    match page {
        Some((texture, aspect)) => {
            let shown = (rect.height() * aspect.max(0.1) / rect.width()).min(1.0);
            painter.image(
                texture,
                rect,
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, shown)),
                egui::Color32::WHITE,
            );
        }
        None => {
            let ink = Stroke::new(2.0, theme::mix(tokens.page, tokens.page_ink, 0.18));
            for line in 0..7 {
                let y = rect.top() + 14.0 + line as f32 * 9.0;
                let right = if line == 0 {
                    rect.right() - 30.0
                } else {
                    rect.right() - 10.0
                };
                painter.hline(rect.left() + 10.0..=right, y, ink);
            }
            painter.text(
                rect.left_bottom() + vec2(8.0, -8.0),
                Align2::LEFT_BOTTOM,
                kind,
                theme::mono(9.0),
                theme::mix(tokens.page, tokens.page_ink, 0.5),
            );
        }
    }
    painter.rect_stroke(rect, 0, tokens.hairline(), egui::StrokeKind::Inside);
}

fn accent_rule(ui: &mut Ui, tokens: &Tokens, title: &str, trailing: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let y = rect.center().y;
    let title_rect = text::line(
        ui,
        pos2(rect.left(), y),
        Align2::LEFT_CENTER,
        title,
        theme::mono(11.0),
        tokens.accent_text,
        rect.width() / 2.0,
    );
    let trailing_rect = text::line(
        ui,
        pos2(rect.right(), y),
        Align2::RIGHT_CENTER,
        trailing,
        theme::mono(10.0),
        tokens.faint,
        rect.width() / 2.0,
    );
    if tokens.ornaments && trailing_rect.left() > title_rect.right() + 16.0 {
        ui.painter().hline(
            title_rect.right() + 8.0..=trailing_rect.left() - 8.0,
            y,
            Stroke::new(1.0, tokens.rule_strong),
        );
    }
}

fn mono_line(ui: &mut Ui, line: &str, color: egui::Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size + 5.0), Sense::hover());
    text::row(
        ui,
        pos2(rect.left(), rect.center().y),
        line,
        theme::mono(size),
        color,
        rect.width(),
    );
}

fn empty_line(ui: &mut Ui, line: &str) {
    let tokens = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    text::row(
        ui,
        pos2(rect.left() + 12.0, rect.center().y),
        line,
        theme::mono(11.0),
        tokens.faint,
        rect.width() - 24.0,
    );
}

/// A tag (`#name 12`) on the hover fill, on `accent-soft` when chosen,
/// with its colour as a dot before the name.
fn tag_chip(
    ui: &mut Ui,
    tokens: &Tokens,
    label: &str,
    count: Option<usize>,
    active: bool,
    color: Option<egui::Color32>,
) -> egui::Response {
    let count = count.map(|count| count.to_string());
    let label_width = text::width(ui, label, &theme::mono(11.0));
    let count_width = count.as_ref().map_or(0.0, |count| {
        text::width(ui, count, &theme::mono(10.0)) + 4.0
    });
    let dot = if color.is_some() { 11.0 } else { 0.0 };
    let (rect, response) = ui.allocate_exact_size(
        vec2(dot + label_width + count_width + 12.0, 19.0),
        Sense::click(),
    );
    let fill = if active {
        tokens.accent_soft
    } else if response.hovered() {
        theme::mix(tokens.hover, tokens.text, 0.06)
    } else {
        tokens.hover
    };
    ui.painter().rect_filled(rect, tokens.ctl_radius(), fill);
    if let Some(color) = color {
        ui.painter()
            .circle_filled(pos2(rect.left() + 9.5, rect.center().y), 3.5, color);
    }
    ui.painter().text(
        pos2(rect.left() + 6.0 + dot, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::mono(11.0),
        if active {
            tokens.accent_text
        } else {
            tokens.text_2
        },
    );
    if let Some(count) = count {
        ui.painter().text(
            pos2(rect.left() + 6.0 + dot + label_width + 4.0, rect.center().y),
            Align2::LEFT_CENTER,
            count,
            theme::mono(10.0),
            tokens.faint,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The tag's menu: its name, then the five colours (the chosen one
/// ringed) and `none`. Returns the colour picked.
fn tag_menu(
    ui: &mut Ui,
    tokens: &Tokens,
    label: &str,
    current: Option<HighlightColor>,
) -> Option<Option<HighlightColor>> {
    let mut chosen = None;
    ui.set_min_width(212.0);
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| mono_line(ui, label, tokens.muted, 11.0));
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                for color in HighlightColor::ALL {
                    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                    let hovered = response.hovered();
                    ui.painter().circle_filled(
                        rect.center(),
                        if hovered { 6.5 } else { 6.0 },
                        crate::annotations::color(color),
                    );
                    if current == Some(color) {
                        ui.painter().circle_stroke(
                            rect.center(),
                            8.5,
                            Stroke::new(1.0, tokens.text),
                        );
                    }
                    if response
                        .on_hover_text(crate::annotations::color_name(color))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        chosen = Some(Some(color));
                    }
                }
                let none = ui.add(Button::plain("none").small());
                if none.clicked() {
                    chosen = Some(None);
                }
            });
        });
    chosen
}

/// The item that `item` may duplicate, unless it is gone.
fn possible_duplicate<'a>(
    library: &'a LibraryStore,
    item: &LibraryItem,
) -> Option<&'a LibraryItem> {
    library
        .metadata_record(item.id)
        .and_then(|record| record.possible_duplicate_of)
        .and_then(|other| library.library_item(other))
}

/// `same doi as “Title”`: what the two share, by Zotero's rules.
fn duplicate_line(item: &LibraryItem, other: &LibraryItem) -> String {
    let reason = match glyphst_metadata::find_duplicate(item, [other]) {
        Some((_, glyphst_metadata::DuplicateReason::Doi)) => "same doi as",
        Some((_, glyphst_metadata::DuplicateReason::Isbn)) => "same isbn as",
        Some((_, glyphst_metadata::DuplicateReason::Arxiv)) => "same arxiv id as",
        Some((_, glyphst_metadata::DuplicateReason::Title)) => "same title as",
        None => "maybe the same as",
    };
    format!("≡ {reason} “{}”", other.title)
}

/// The older item stays; a trashed one never does.
fn merge_action(item: &LibraryItem, other: &LibraryItem) -> LibraryTabAction {
    if other.trashed_at.is_some() {
        LibraryTabAction::Merge {
            keep: item.id,
            absorb: other.id,
        }
    } else {
        LibraryTabAction::Merge {
            keep: other.id,
            absorb: item.id,
        }
    }
}

/// The `accent-soft` box under an item's source (frame 04l): what it
/// shares with the other, how long that one has been here, and
/// `[ keep both ] [ merge ]`.
fn duplicate_box(
    ui: &mut Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    item: &LibraryItem,
    other: &LibraryItem,
) -> Option<LibraryTabAction> {
    let mut action = None;
    egui::Frame::new()
        .fill(tokens.accent_soft)
        .corner_radius(tokens.ctl_radius())
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add(
                egui::Label::new(
                    egui::RichText::new(duplicate_line(item, other))
                        .font(theme::mono(11.0))
                        .color(tokens.accent_text),
                )
                .wrap(),
            );
            let highlights = library
                .primary_attachment(other.id)
                .map_or(0, |attachment| {
                    library.highlights_for(&attachment.path).len()
                });
            let mut since = format!("in the library since {}", month_year(other.created));
            if highlights > 0 {
                since.push_str(&format!(" · {}", plural(highlights, "highlight")));
            }
            mono_line(ui, &since, tokens.muted, 10.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if ui.add(Button::bracket_muted("keep both")).clicked() {
                    action = Some(LibraryTabAction::KeepBoth(item.id, other.id));
                }
                if ui.add(Button::bracket("merge")).clicked() {
                    action = Some(merge_action(item, other));
                }
            });
        });
    action
}

/// A row of a list in the item pane: a glyph, a name and a faint detail
/// on the right.
fn link_row(
    ui: &mut Ui,
    tokens: &Tokens,
    glyph: &str,
    name: &str,
    detail: &str,
    color: egui::Color32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
    let y = rect.center().y;
    let detail_rect = text::line(
        ui,
        pos2(rect.right(), y),
        Align2::RIGHT_CENTER,
        detail,
        theme::mono(10.0),
        tokens.faint,
        rect.width() / 2.0,
    );
    ui.painter().text(
        pos2(rect.left(), y),
        Align2::LEFT_CENTER,
        glyph,
        theme::mono(11.0),
        tokens.faint,
    );
    text::row(
        ui,
        pos2(rect.left() + 18.0, y),
        name,
        theme::mono(11.0),
        if response.hovered() {
            tokens.text
        } else {
            color
        },
        (detail_rect.left() - rect.left() - 26.0).max(0.0),
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// `oct 19`.
fn short_date(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|time| time.format("%b %d").to_string().to_lowercase())
        .unwrap_or_default()
}

/// `mar 2024`.
fn month_year(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(seconds as i64, 0)
        .map(|time| time.format("%b %Y").to_string().to_lowercase())
        .unwrap_or_default()
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// An AI tag (`◆ name ✓ ×`), outlined in the accent; returns whether ✓ or
/// × was clicked.
fn ai_chip(ui: &mut Ui, tokens: &Tokens, name: &str) -> (bool, bool) {
    let label = format!("◆ {name}");
    let label_width = text::width(ui, &label, &theme::mono(11.0));
    let (rect, response) = ui.allocate_exact_size(
        vec2(label_width + 6.0 + 12.0 + 14.0 + 14.0, 19.0),
        Sense::hover(),
    );
    ui.painter().rect_stroke(
        rect,
        tokens.ctl_radius(),
        Stroke::new(1.0, tokens.accent),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        pos2(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::mono(11.0),
        tokens.accent_text,
    );
    let mark = |offset: f32, glyph: &str, color: egui::Color32, tip: &str| {
        let at = Rect::from_center_size(
            pos2(rect.left() + 6.0 + label_width + offset, rect.center().y),
            vec2(14.0, 17.0),
        );
        let clicked = ui
            .interact(at, response.id.with(glyph), Sense::click())
            .on_hover_text(tip)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked();
        ui.painter().text(
            at.center(),
            Align2::CENTER_CENTER,
            glyph,
            theme::mono(11.0),
            color,
        );
        clicked
    };
    let accept = mark(11.0, "✓", tokens.ok, "keep");
    let reject = mark(25.0, "×", tokens.muted, "reject");
    (accept, reject)
}

#[cfg(test)]
mod tests {
    use glyphst_library::{LibraryImportOutcome, LibraryItemMetadata};

    use super::*;

    #[test]
    fn typed_filters_become_tags_years_and_words() {
        let mut library = LibraryStore::memory();
        let path =
            std::env::temp_dir().join(format!("glyphst-tab-filter-{}.pdf", std::process::id()));
        std::fs::write(&path, b"%PDF-1.4 filter").unwrap();
        let LibraryImportOutcome::Added(id) = library
            .link_item(&path, LibraryItemMetadata::for_path(&path), None)
            .unwrap()
        else {
            panic!("expected item");
        };
        library
            .set_item_tag_names(id, &["to read".to_string()])
            .unwrap();
        let filter = parse_filter("by:Vaswani tag:to-read year:>2016 attention", &library);
        assert_eq!(filter.text, "vaswani attention");
        assert_eq!(filter.tag_ids.len(), 1);
        assert_eq!(filter.year_min, Some(2017));
        let filter = parse_filter("year:2018-2020 #unknown", &library);
        assert_eq!((filter.year_min, filter.year_max), (Some(2018), Some(2020)));
        assert_eq!(filter.tag_ids, vec![LibraryTagId(0)]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_title_column_takes_what_the_others_leave() {
        let x = columns(1000.0);
        assert!(x.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(x[5] + 84.0 <= 1000.0);
        assert_eq!(notes_section("# T\n\n## Notes\n\nmine\n"), "mine");
    }
}
