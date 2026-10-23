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

pub mod columns;
mod menus;
mod pages_view;
mod search_view;

use columns::{Column, Layout, Natural};
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
    Unfiled,
    Missing,
    Trash,
    Collection(LibraryCollectionId),
    Tag(LibraryTagId),
    /// Concept pages and topics for new ones (frame 04p).
    Pages,
    /// One concept page (04q).
    Page(u64),
    /// Files opened from outside the library (frame 04v).
    History,
}

/// A concept page's text read: the page, its file's time, the vault's notes.
type PageTextKey = (u64, Option<std::time::SystemTime>, usize);

/// What the tab asks the app to do.
pub enum LibraryTabAction {
    Open(PathBuf),
    /// Ask the assistant about an item: its conversation continues, with
    /// its document open when it has one.
    Ask {
        item: DocumentId,
        path: Option<PathBuf>,
    },
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
    /// Ask to write a concept page (the cost first, 04s).
    WritePage(crate::knowledge::PageAsk),
    /// Stop suggesting a page topic.
    DismissTopic(String),
    /// Keep a page as written, or undo its last write.
    KeepPage(u64),
    UndoPage(u64),
    /// Show a concept page.
    ShowPage(u64),
    /// Keep or reject a related item suggested from meaning; relate two
    /// items by hand, or unrelate them.
    AcceptRelated(DocumentId, DocumentId),
    RejectRelated(DocumentId, DocumentId),
    AddRelated(DocumentId, DocumentId),
    RemoveRelated(DocumentId, DocumentId),
    /// Tag an item (a tag of the user's); take a tag off (the AI's is then
    /// never added again).
    AddTag(DocumentId, String),
    RemoveTag(DocumentId, LibraryTagId),
    /// Rename a tag everywhere (into another of that name, the two are
    /// one), or delete it everywhere.
    RenameTag(LibraryTagId, String),
    DeleteTag(LibraryTagId),
    /// File items in a collection, or take one out.
    AddToCollection(Vec<DocumentId>, LibraryCollectionId),
    RemoveFromCollection(DocumentId, LibraryCollectionId),
    /// Make a collection (inside `parent`), filing `item` there.
    NewCollection {
        parent: Option<LibraryCollectionId>,
        name: String,
        item: Option<DocumentId>,
    },
    RenameCollection(LibraryCollectionId, String),
    /// Delete a collection (its items stay in the library).
    DeleteCollection(LibraryCollectionId),
    /// To the trash, back, or gone for good.
    Trash(DocumentId),
    Restore(DocumentId),
    DeleteForever(DocumentId),
    /// Show a file in the system's file manager.
    ShowFile(PathBuf),
    /// Forget a file opened from outside the library.
    ForgetOpened(PathBuf),
    /// The table's columns to leave out.
    HideColumns(Vec<Column>),
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
    /// The vault's notes and links, for backlinks.
    pub vault_index: Option<&'a glyphst_vault::index::VaultIndex>,
    /// Topics for new concept pages (04p).
    pub topics: &'a [crate::knowledge::TopicSuggestion],
    /// Pages being written: id and title.
    pub writing: &'a [(u64, String)],
    /// The table's columns the user turned off.
    pub hidden_columns: &'a [Column],
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
    /// The concept page shown, as Markdown to read, by page, file time and
    /// the notes its links could name.
    page_text: Option<(PageTextKey, String)>,
    markdown: glyphst_markdown::MarkdownCache,
    /// Relating an item by hand: the item and the words to find the other.
    relating: Option<(DocumentId, String)>,
    /// A right-click entry's prompt (a tag to add, a new name).
    prompt: Option<menus::Prompt>,
    /// The widest text of each column over the rows seen in this view and
    /// filter, so the columns hold still while scrolling.
    fit: Option<((View, String), Natural)>,
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
        let pages = matches!(self.view, View::Pages | View::Page(_));
        let item = self
            .selected
            .filter(|_| !pages && area.width() > LEFT + ITEM + 480.0)
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
        if let Some(chosen) = self.overlays(ui.ctx(), context.library) {
            action = Some(chosen);
        }
        action
    }

    /// What floats over the tab: a menu entry's prompt, and the paper
    /// being dragged onto a collection. The sidebar's item card calls it
    /// too.
    pub fn overlays(
        &mut self,
        ctx: &egui::Context,
        library: &LibraryStore,
    ) -> Option<LibraryTabAction> {
        let tokens = Tokens::get(ctx);
        let mut action = None;
        if let Some(prompt) = &mut self.prompt {
            let (chosen, closed) = menus::show_prompt(ctx, &tokens, library, prompt);
            action = chosen;
            if closed {
                self.prompt = None;
            }
        }
        if let Some(dragged) = egui::DragAndDrop::payload::<DocumentId>(ctx)
            && let Some(pointer) = ctx.pointer_interact_pos()
            && let Some(item) = library.library_item(*dragged)
        {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::unique("library-drag"),
            ));
            let galley =
                painter.layout_no_wrap(clip(&item.title, 48), theme::regular(12.0), tokens.text);
            let rect = Rect::from_min_size(
                pointer + vec2(12.0, 8.0),
                vec2(galley.size().x + 16.0, 22.0),
            );
            painter.rect_filled(rect, tokens.ctl_radius(), tokens.raised);
            painter.rect_stroke(
                rect,
                tokens.ctl_radius(),
                Stroke::new(1.0, tokens.accent),
                egui::StrokeKind::Inside,
            );
            let top = rect.center().y - galley.size().y / 2.0;
            painter.galley(pos2(rect.left() + 8.0, top), galley, tokens.text);
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
                let opened = history_outside(library).len();
                for (view, icon, label) in [
                    (View::All, "library", "all items"),
                    (View::Recent, "clock", "recently added"),
                    (View::Unfiled, "inbox", "unfiled"),
                    (View::Missing, "file-question", "missing"),
                    (View::Trash, "trash-2", "trash"),
                    (View::History, "rotate-ccw-clock", "history"),
                ] {
                    let count = match view {
                        View::History => opened,
                        _ => counts.get(&view).copied().unwrap_or(0),
                    };
                    let meta = count.to_string();
                    let response = ListRow::new(label)
                        .icon(icon)
                        .meta(meta.as_str())
                        .active(self.view == view)
                        .show(ui);
                    if response.clicked() {
                        self.choose(view);
                    }
                    // A paper dropped on the trash goes there.
                    if view == View::Trash
                        && let Some(dropped) = response.dnd_release_payload::<DocumentId>()
                    {
                        action = Some(LibraryTabAction::Trash(*dropped));
                    }
                }
                let pages = library.pages().count().to_string();
                if ListRow::new("pages")
                    .icon("file-text")
                    .meta(pages.as_str())
                    .active(matches!(self.view, View::Pages | View::Page(_)))
                    .show(ui)
                    .clicked()
                {
                    self.choose(View::Pages);
                }
                ui.add_space(10.0);
                let collections = library.library_collections().len();
                rule(ui, "collections", Some(&collections.to_string()));
                if let Some(chosen) = self.collection_tree(ui, tokens, library) {
                    action = Some(chosen);
                }
                ui.add_space(10.0);
                let tags = library.library_tags().len();
                rule(ui, "tags", Some(&tags.to_string()));
                if let Some(chosen) = self.tag_cloud(ui, tokens, library) {
                    action = Some(chosen);
                }
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

    /// The collections as a tree, each a place to drop papers on and with
    /// its menu, then `+ new collection`.
    fn collection_tree(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        library: &LibraryStore,
    ) -> Option<LibraryTabAction> {
        let mut action = None;
        let counts = collection_counts(library);
        let mut rows = Vec::new();
        collect_tree(library, None, 0, &self.folded, &mut rows);
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
            // A paper dragged over it: outlined, and filed there on release.
            if response.dnd_hover_payload::<DocumentId>().is_some() {
                ui.painter().rect_stroke(
                    response.rect.shrink(1.0),
                    tokens.ctl_radius(),
                    Stroke::new(1.0, tokens.accent),
                    egui::StrokeKind::Inside,
                );
            }
            if let Some(dropped) = response.dnd_release_payload::<DocumentId>() {
                action = Some(LibraryTabAction::AddToCollection(vec![*dropped], *id));
            }
            let mut choice = None;
            response.context_menu(|ui| choice = menus::collection_menu(ui, *id));
            if let Some(choice) = choice {
                action = self.take_choice(choice, library, ui).or(action.take());
            }
        }
        // A new collection, at the root.
        let response = ListRow::new("new collection").icon("plus").show(ui);
        if response.clicked() {
            let at = response.rect.left_bottom();
            self.prompt = Some(menus::Prompt::new(
                menus::PromptKind::NewCollection {
                    parent: None,
                    item: None,
                },
                library,
                at,
            ));
        }
        action
    }

    /// What a menu entry asks: an action for the app, a prompt here, or
    /// another view.
    fn take_choice(
        &mut self,
        choice: menus::Choice,
        library: &LibraryStore,
        ui: &Ui,
    ) -> Option<LibraryTabAction> {
        match choice {
            menus::Choice::Action(action) => Some(action),
            menus::Choice::Prompt(kind) => {
                let at = ui
                    .ctx()
                    .input(|input| input.pointer.latest_pos())
                    .unwrap_or_else(|| ui.max_rect().center());
                self.prompt = Some(menus::Prompt::new(kind, library, at));
                None
            }
            menus::Choice::Show(view) => {
                self.choose(view);
                None
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
                // Right-click: its colour, a new name, gone (04l, 04u).
                let mut choice = None;
                response.context_menu(|ui| {
                    choice = menus::cloud_tag_menu(ui, tokens, library, tag.id, count);
                });
                if let Some(choice) = choice {
                    action = self.take_choice(choice, library, ui).or(action.take());
                }
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
        if matches!(self.view, View::Pages | View::Page(_)) {
            return self.pages_part(ui, tokens, context, rect);
        }
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
        let mut counts = if self.view == View::History {
            format!(
                "{} you opened · not in the library",
                plural(history_outside(library).len(), "file")
            )
        } else {
            format!("{} items", ids.len())
        };
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
            if buttons.add(Button::bracket("ask the library")).clicked() {
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
                .add(Button::bracket_muted(&format!(
                    "summarise {unenriched} new"
                )))
                .on_hover_text("summarise, tag and file the papers that have no summary yet")
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
        if self.view == View::History {
            return self.history_list(ui, tokens, context, width).or(action);
        }

        // The table: columns as wide as their text in the rows seen so far.
        let key = (self.view, self.filter.clone());
        if self.fit.as_ref().is_none_or(|(seen, _)| *seen != key) {
            let natural = ids
                .iter()
                .take(60)
                .filter_map(|id| library.library_item(*id))
                .fold(Natural::default(), |natural, item| {
                    natural.max(natural_widths(ui, library, item))
                });
            self.fit = Some((key, natural));
        }
        let natural = self
            .fit
            .as_ref()
            .map(|(_, natural)| *natural)
            .unwrap_or_default();
        let columns = columns::layout(width, natural, context.hidden_columns);
        let (header, header_response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
        for &(column, x, _) in &columns.columns {
            ui.painter().text(
                pos2(header.left() + x + 8.0, header.center().y),
                Align2::LEFT_CENTER,
                column.label(),
                theme::mono(10.0),
                tokens.faint,
            );
        }
        ui.painter()
            .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
        // Its menu: the columns to show (04u).
        header_response.context_menu(|ui| {
            if let Some(hidden) = menus::column_menu(ui, context.hidden_columns) {
                action = Some(LibraryTabAction::HideColumns(hidden));
            }
        });
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
        let mut show_tag = None;
        let mut seen = natural;
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
                    // Dragged onto a collection in the pane.
                    let response = response.interact(Sense::click_and_drag());
                    let mut chips = Vec::new();
                    let duplicate = possible_duplicate(library, item);
                    if ui.is_rect_visible(row) {
                        seen = seen.max(natural_widths(ui, library, item));
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
                            None => {
                                chips = paint_item_row(
                                    ui, tokens, row, &columns, library, item, doc, selected,
                                );
                            }
                        }
                    }
                    if let Some(other) = duplicate {
                        let right = row.right() - 8.0;
                        let from = columns
                            .get(Column::Tags)
                            .or(columns.get(Column::Read))
                            .map_or(row.center().x, |(x, _)| row.left() + x);
                        let buttons =
                            Rect::from_min_max(pos2(from, row.top()), pos2(right, row.bottom()));
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
                    // A tag's chip: a click shows its items, a right-click
                    // its menu.
                    let mut on_chip = false;
                    for (index, (rect, tag)) in chips.iter().enumerate() {
                        let chip = ui.interact(
                            *rect,
                            ui.scope_id().with(("row-tag", id.0, index)),
                            Sense::click(),
                        );
                        on_chip |= chip.contains_pointer();
                        if chip.clicked() {
                            show_tag = Some(*tag);
                        }
                        let mut choice = None;
                        chip.context_menu(|ui| {
                            choice = menus::item_tag_menu(ui, tokens, library, *id, *tag);
                        });
                        if let Some(choice) = choice {
                            action = self.take_choice(choice, library, ui).or(action.take());
                        }
                    }
                    if !on_chip && (response.clicked() || response.secondary_clicked()) {
                        self.selected = Some(*id);
                    }
                    if response.double_clicked()
                        && let Some(attachment) = library.primary_attachment(*id)
                    {
                        action = Some(LibraryTabAction::Open(attachment.path.clone()));
                    }
                    // The row's menu (frame 04u).
                    if !on_chip {
                        let note = context
                            .vault
                            .zip(library.vault_note(*id))
                            .map(|(vault, note)| vault.join(&note.relative_path));
                        let mut choice = None;
                        response.context_menu(|ui| {
                            choice = menus::row_menu(ui, library, item, self.view, note);
                        });
                        if let Some(choice) = choice {
                            action = self.take_choice(choice, library, ui).or(action.take());
                        }
                    }
                    // Dragged onto a collection (or the trash).
                    response.dnd_set_drag_payload(*id);
                    if selected && response.has_focus() {
                        keyboard = Some(*id);
                    }
                }
            });
        // Wider text came into view: the columns follow from the next frame.
        if let Some((_, natural)) = &mut self.fit
            && seen != *natural
        {
            *natural = seen;
            ui.ctx().request_repaint();
        }
        if let Some(tag) = show_tag {
            self.choose(View::Tag(tag));
        }
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
            // Pages list no items; the views beside them are all items.
            View::All | View::Pages | View::Page(_) => {}
            // History lists files, not items (`history_list`).
            View::History => return Vec::new(),
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
        if self.view == View::Recent {
            let since = glyphst_library::current_timestamp().saturating_sub(30 * 86_400);
            ids.retain(|id| {
                library
                    .library_item(*id)
                    .is_some_and(|item| item.created >= since)
            });
        }
        ids
    }

    /// The files opened from outside the library (frame 04v): file,
    /// folder, when, how far, and `[ add to library ]`.
    fn history_list(
        &mut self,
        ui: &mut Ui,
        tokens: &Tokens,
        context: &mut TabContext<'_>,
        width: f32,
    ) -> Option<LibraryTabAction> {
        let library = context.library;
        let mut action = None;
        let entries = history_outside(library);
        let x = [0.0, width * 0.38, width * 0.68, width * 0.78];
        let (header, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
        for (name, x) in ["file", "folder", "opened", "read"].iter().zip(&x) {
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
        if entries.is_empty() {
            ui.add_space(16.0);
            empty_line(ui, "every file you opened is in the library");
            return None;
        }
        let progress = context
            .docs
            .iter()
            .map(|doc| (doc.path.clone(), doc))
            .collect::<HashMap<_, _>>();
        egui::ScrollArea::vertical()
            .id_salt("library-history")
            .auto_shrink(false)
            .show_rows(ui, ROW, entries.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for entry in &entries[range] {
                    let (row, response) = glyphst_ui::row::list_row(ui, ROW, false);
                    let y = row.center().y;
                    let name = entry
                        .path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let folder = entry.path.parent().map(home_relative).unwrap_or_default();
                    let cells = [
                        (name.as_str(), theme::regular(13.0), tokens.text_2),
                        (folder.as_str(), theme::mono(11.0), tokens.muted),
                    ];
                    for (index, (value, font, color)) in cells.into_iter().enumerate() {
                        text::row(
                            ui,
                            pos2(row.left() + x[index] + 8.0, y),
                            value,
                            font,
                            color,
                            x[index + 1] - x[index] - 12.0,
                        );
                    }
                    text::row(
                        ui,
                        pos2(row.left() + x[2] + 8.0, y),
                        crate::sidebar::when(entry.opened),
                        theme::mono(11.0),
                        tokens.muted,
                        x[3] - x[2] - 12.0,
                    );
                    match progress.get(&entry.path).and_then(|doc| doc.progress()) {
                        Some(done) if done >= 0.999 => {
                            text::row(
                                ui,
                                pos2(row.left() + x[3] + 8.0, y),
                                "done ✓",
                                theme::mono(11.0),
                                tokens.ok,
                                80.0,
                            );
                        }
                        Some(done) if done > 0.0 => {
                            let label = format!("{:.0}%", done * 100.0);
                            Meter::new(done).bar().width(40.0).label(&label).paint(
                                ui,
                                Rect::from_min_size(
                                    pos2(row.left() + x[3] + 8.0, y - 7.0),
                                    vec2(84.0, 14.0),
                                ),
                            );
                        }
                        _ => {}
                    }
                    let buttons = Rect::from_min_max(
                        pos2(row.left() + x[3] + 100.0, row.top()),
                        pos2(row.right() - 8.0, row.bottom()),
                    );
                    let mut buttons_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(buttons)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    );
                    if buttons_ui.add(Button::bracket("add to library")).clicked() {
                        action = Some(LibraryTabAction::AddOpened(vec![entry.path.clone()]));
                    }
                    if response.double_clicked() {
                        action = Some(LibraryTabAction::Open(entry.path.clone()));
                    }
                    response.context_menu(|ui| {
                        glyphst_ui::menu::style(ui);
                        let item = |ui: &mut Ui, icon: &str, label: &str| {
                            let clicked = glyphst_ui::menu::Item::new(label)
                                .icon(icon)
                                .width(220.0)
                                .show(ui)
                                .clicked();
                            if clicked {
                                ui.close();
                            }
                            clicked
                        };
                        if item(ui, "external-link", "open") {
                            action = Some(LibraryTabAction::Open(entry.path.clone()));
                        }
                        if item(ui, "library-big", "add to library") {
                            action = Some(LibraryTabAction::AddOpened(vec![entry.path.clone()]));
                        }
                        if item(ui, "folder-open", "show file") {
                            action = Some(LibraryTabAction::ShowFile(entry.path.clone()));
                        }
                        glyphst_ui::menu::separator(ui);
                        if item(ui, "x", "remove from history") {
                            action = Some(LibraryTabAction::ForgetOpened(entry.path.clone()));
                        }
                    });
                    ui.painter()
                        .hline(row.x_range(), row.bottom() - 0.5, tokens.hairline());
                }
            });
        let _ = &mut self.selected;
        action
    }

    /// `── library / …` and the heading.
    fn titles(&self, library: &LibraryStore) -> (String, String) {
        let named = |name: &str| (format!("library / {name}"), name.to_string());
        match self.view {
            View::All => ("library".to_string(), "all items".to_string()),
            View::Recent => named("recently added"),
            View::History => named("history"),
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
            View::Pages => named("pages"),
            View::Page(id) => (
                "library / pages".to_string(),
                library
                    .page(id)
                    .map(|page| page.title.clone())
                    .unwrap_or_default(),
            ),
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
        }
        let ask = if library.latest_item_session(id).is_some() {
            "continue"
        } else {
            "ask"
        };
        if with_open && foot_ui.add(Button::bracket_muted(ask)).clicked() {
            action = Some(LibraryTabAction::Ask {
                item: id,
                path: path.clone(),
            });
        }
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
            if summary_rule(ui, tokens, &date, short_model(&enrichment.model)) {
                action = Some(LibraryTabAction::Enrich(id));
            }
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

        // Tags: a click shows a tag's items, a right-click its menu.
        flush_rule(ui, "tags", None);
        let assignments = library.ai_assignments(id).to_vec();
        let tags = library.tags_for_item(id);
        if tags.is_empty() {
            mono_line(ui, "no tags yet", tokens.faint, 11.0);
        } else {
            let mut choice = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for tag in &tags {
                    let color = library.tag_color(tag.id).map(crate::annotations::color);
                    let response = tag_chip_button(ui, tokens, &tag.name, color);
                    if response.clicked() {
                        choice = Some(menus::Choice::Show(View::Tag(tag.id)));
                    }
                    response.context_menu(|ui| {
                        choice = menus::item_tag_menu(ui, tokens, library, id, tag.id);
                    });
                }
            });
            if let Some(choice) = choice {
                action = self.take_choice(choice, library, ui).or(action.take());
            }
        }
        if ui.add(Button::plain("+ tag").small()).clicked() {
            let at = ui.min_rect().left_bottom();
            self.prompt = Some(menus::Prompt::new(
                menus::PromptKind::AddTag(id),
                library,
                at,
            ));
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
                    && assignment.state == AssignmentState::Applied
            });
            let label = format!(
                "■ {}",
                library
                    .collection_path_label(collection.id)
                    .replace('/', " / ")
                    .to_lowercase()
            );
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(Button::plain("×").small())
                        .on_hover_text("take it out of this collection")
                        .clicked()
                    {
                        action = Some(if by_ai {
                            LibraryTabAction::Reject(
                                id,
                                AssignmentTarget::Collection(collection.id),
                            )
                        } else {
                            LibraryTabAction::RemoveFromCollection(id, collection.id)
                        });
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        mono_line(ui, &label, tokens.text_2, 11.0);
                    });
                });
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
                                                "suggested collection: {}",
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
        // Related items: kept ones (× unrelates), ones suggested from
        // meaning (✓ keeps, × never again), and relating one by hand.
        flush_rule(ui, "related", None);
        for other in library.related_items(id) {
            let Some(other) = library.library_item(other) else {
                continue;
            };
            let year = other.year.map(|year| year.to_string()).unwrap_or_default();
            let (row, marks) =
                marked_row(ui, tokens, "↔", &other.title, &year, &[("×", "unrelate")]);
            if marks[0] {
                action = Some(LibraryTabAction::RemoveRelated(id, other.id));
            } else if row.clicked() {
                action = Some(LibraryTabAction::ShowInLibrary(other.id));
            }
        }
        for (other, _) in library.related_suggestions(id).into_iter().take(5) {
            let Some(other) = library.library_item(other) else {
                continue;
            };
            let (row, marks) = marked_row(
                ui,
                tokens,
                "◇",
                &other.title,
                "close in meaning",
                &[("✓", "keep"), ("×", "not related")],
            );
            if marks[0] {
                action = Some(LibraryTabAction::AcceptRelated(id, other.id));
            } else if marks[1] {
                action = Some(LibraryTabAction::RejectRelated(id, other.id));
            } else if row.clicked() {
                action = Some(LibraryTabAction::ShowInLibrary(other.id));
            }
        }
        match &mut self.relating {
            Some((item, words)) if *item == id => {
                TextField::new(words, "find an item to relate")
                    .width(ui.available_width())
                    .show(ui);
                let words = words.trim().to_lowercase();
                let related = library.related_items(id);
                let found = library
                    .library_items()
                    .iter()
                    .filter(|other| {
                        other.id != id
                            && other.trashed_at.is_none()
                            && !related.contains(&other.id)
                            && !words.is_empty()
                            && other.title.to_lowercase().contains(&words)
                    })
                    .take(6)
                    .collect::<Vec<_>>();
                for other in found {
                    let year = other.year.map(|year| year.to_string()).unwrap_or_default();
                    if link_row(ui, tokens, "+", &other.title, &year, tokens.text_2).clicked() {
                        action = Some(LibraryTabAction::AddRelated(id, other.id));
                    }
                }
                if ui.add(Button::bracket_muted("done")).clicked() || action.is_some() {
                    self.relating = None;
                }
            }
            _ => {
                if ui.add(Button::bracket_muted("+ related")).clicked() {
                    self.relating = Some((id, String::new()));
                }
            }
        }
        ui.add_space(10.0);

        // Notes, pages and the user's own notes that link to this one.
        if let Some(index) = context.vault_index {
            let backlinks = index.backlinks(glyphst_vault::index::Owner::Item(id.0));
            if !backlinks.is_empty() {
                flush_rule(ui, "linked from", None);
                for note in backlinks {
                    if let Some(chosen) = pages_view::backlink_row(ui, tokens, note, context.vault)
                    {
                        action = Some(chosen);
                    }
                }
                ui.add_space(10.0);
            }
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

/// The widest each of an item's cells would be with nothing cut.
fn natural_widths(ui: &Ui, library: &LibraryStore, item: &LibraryItem) -> Natural {
    let mono = theme::mono(11.0);
    let tags = library.tags_for_item(item.id);
    let tags_width = tags
        .iter()
        .map(|tag| {
            let color = library.tag_color(tag.id).map(crate::annotations::color);
            tag_chip_width(ui, &tag.name, color) + 4.0
        })
        .sum::<f32>();
    Natural {
        title: text::width(ui, &item.title, &theme::regular(13.0)) + 12.0,
        authors: text::width(
            ui,
            &columns::authors_fit(&surnames(item), f32::INFINITY, |_| 0.0),
            &mono,
        ) + 12.0,
        venue: if item.publication.is_empty() {
            0.0
        } else {
            text::width(ui, &item.publication, &mono) + 12.0
        },
        tags: tags_width + 12.0,
    }
}

/// A tag's chip, `#name` filled with its colour as a dot, whoever added it.
fn tag_chip_width(ui: &Ui, name: &str, color: Option<egui::Color32>) -> f32 {
    let dot = if color.is_some() { 11.0 } else { 0.0 };
    dot + text::width(ui, &format!("#{name}"), &theme::mono(11.0)) + 12.0
}

/// Paints a tag's chip from `at` (its left centre); returns its rect.
fn paint_tag_chip(
    ui: &Ui,
    tokens: &Tokens,
    at: egui::Pos2,
    name: &str,
    color: Option<egui::Color32>,
    hovered: bool,
) -> Rect {
    let dot = if color.is_some() { 11.0 } else { 0.0 };
    let rect = Rect::from_min_size(
        pos2(at.x, at.y - 9.5),
        vec2(tag_chip_width(ui, name, color), 19.0),
    );
    let painter = ui.painter();
    let fill = if hovered {
        tokens.rule_strong
    } else {
        tokens.hover
    };
    painter.rect_filled(rect, tokens.ctl_radius(), fill);
    if let Some(color) = color {
        painter.circle_filled(pos2(rect.left() + 9.5, rect.center().y), 3.5, color);
    }
    painter.text(
        pos2(rect.left() + 6.0 + dot, rect.center().y),
        Align2::LEFT_CENTER,
        format!("#{name}"),
        theme::mono(11.0),
        tokens.text_2,
    );
    rect
}

/// An item's tags as chips from `left`, as many as fit before `right`,
/// then `+n`: each chip's rect and tag.
fn paint_tag_row(
    ui: &Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    id: glyphst_library::DocumentId,
    left: f32,
    right: f32,
    y: f32,
) -> Vec<(Rect, LibraryTagId)> {
    let tags = library.tags_for_item(id);
    let mut chips = Vec::new();
    let mut x = left;
    for (index, tag) in tags.iter().enumerate() {
        let color = library.tag_color(tag.id).map(crate::annotations::color);
        let width = tag_chip_width(ui, &tag.name, color);
        let left_over = tags.len() - index;
        // Room for this chip, and for `+n` after it when more follow.
        let more = if left_over > 1 { 28.0 } else { 0.0 };
        if x + width + more > right {
            ui.painter().text(
                pos2(x, y),
                Align2::LEFT_CENTER,
                format!("+{left_over}"),
                theme::mono(10.0),
                tokens.faint,
            );
            break;
        }
        let hovered =
            ui.rect_contains_pointer(Rect::from_min_size(pos2(x, y - 9.5), vec2(width, 19.0)));
        let rect = paint_tag_chip(ui, tokens, pos2(x, y), &tag.name, color, hovered);
        chips.push((rect, tag.id));
        x = rect.right() + 4.0;
    }
    chips
}

#[allow(clippy::too_many_arguments)]
fn paint_item_row(
    ui: &Ui,
    tokens: &Tokens,
    rect: Rect,
    columns: &Layout,
    library: &LibraryStore,
    item: &LibraryItem,
    doc: Option<&Doc>,
    selected: bool,
) -> Vec<(Rect, LibraryTagId)> {
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    let mono = theme::mono(11.0);
    let cell = |x: f32, width: f32, value: &str, font: egui::FontId, color: egui::Color32| {
        text::row(
            ui,
            pos2(rect.left() + x + 8.0, y),
            value,
            font,
            color,
            width - 4.0,
        );
    };
    let mut chips = Vec::new();
    for &(column, x, width) in &columns.columns {
        match column {
            Column::Title => cell(
                x,
                width,
                &item.title,
                theme::regular(13.0),
                if selected { tokens.text } else { tokens.text_2 },
            ),
            Column::Authors => {
                let names = columns::authors_fit(&surnames(item), width - 12.0, |line| {
                    text::width(ui, line, &mono)
                });
                cell(x, width, &names, mono.clone(), tokens.muted);
            }
            Column::Venue => cell(x, width, &item.publication, mono.clone(), tokens.muted),
            Column::Year => cell(
                x,
                width,
                &item.year.map(|year| year.to_string()).unwrap_or_default(),
                mono.clone(),
                tokens.muted,
            ),
            Column::Added => cell(
                x,
                width,
                &short_date(item.created),
                mono.clone(),
                tokens.faint,
            ),
            Column::Tags => {
                chips = paint_tag_row(
                    ui,
                    tokens,
                    library,
                    item.id,
                    rect.left() + x + 8.0,
                    rect.left() + x + width - 4.0,
                    y,
                );
            }
            Column::Read => match doc.and_then(Doc::progress) {
                Some(progress) if progress >= 0.999 => {
                    cell(x, width, "done ✓", mono.clone(), tokens.ok);
                }
                Some(progress) if progress > 0.0 => {
                    let label = format!("{:.0}%", progress * 100.0);
                    Meter::new(progress).bar().width(40.0).label(&label).paint(
                        ui,
                        Rect::from_min_size(
                            pos2(rect.left() + x + 8.0, y - 7.0),
                            vec2(width.min(84.0), 14.0),
                        ),
                    );
                }
                _ => cell(x, width, "—", mono.clone(), tokens.faint),
            },
        }
    }
    chips
}

/// A possible duplicate's row: its file and what it shares with the other
/// item across the title and authors, then its year; the buttons go on top.
fn paint_duplicate_row(
    ui: &Ui,
    tokens: &Tokens,
    rect: Rect,
    columns: &Layout,
    item: &LibraryItem,
    other: &LibraryItem,
) {
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, tokens.hairline());
    let y = rect.center().y;
    let left = rect.left() + 8.0;
    // Across the title, authors and venue, up to the year or the tags.
    let end = columns
        .get(Column::Year)
        .or(columns.get(Column::Tags))
        .or(columns.get(Column::Read))
        .map_or(rect.width(), |(x, _)| x);
    let width = end - 12.0;
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
    if let Some((x, year_width)) = columns.get(Column::Year) {
        text::row(
            ui,
            pos2(rect.left() + x + 8.0, y),
            item.year.map(|year| year.to_string()).unwrap_or_default(),
            theme::mono(11.0),
            tokens.muted,
            year_width - 4.0,
        );
    }
}

/// The item's authors' surnames.
fn surnames(item: &LibraryItem) -> Vec<String> {
    item.creators
        .iter()
        .map(|creator| surname(&creator.name))
        .collect()
}

fn surname(name: &str) -> String {
    match name.split_once(',') {
        Some((last, _)) => last.trim().to_string(),
        None => name.split_whitespace().last().unwrap_or(name).to_string(),
    }
}

fn authors_short(item: &LibraryItem) -> String {
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

/// The summary's rule: `summary`, its date (the model on hover) and
/// `[ summarise again ]`, which reports a click.
fn summary_rule(ui: &mut Ui, tokens: &Tokens, date: &str, model: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let y = rect.center().y;
    let title = text::line(
        ui,
        pos2(rect.left(), y),
        Align2::LEFT_CENTER,
        "summary",
        theme::mono(11.0),
        tokens.accent_text,
        rect.width() / 3.0,
    );
    let mut again_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    again_ui.spacing_mut().item_spacing.x = 8.0;
    let again = again_ui
        .add(Button::bracket_muted("summarise again").small())
        .on_hover_text(
            "ask the library model for a new summary, tags and collection; your own tags stay",
        );
    let when = again_ui
        .add(egui::Label::new(
            egui::RichText::new(date)
                .font(theme::mono(10.0))
                .color(tokens.faint),
        ))
        .on_hover_text(format!("by {model}"));
    if tokens.ornaments && when.rect.left() > title.right() + 16.0 {
        ui.painter().hline(
            title.right() + 8.0..=when.rect.left() - 8.0,
            y,
            Stroke::new(1.0, tokens.rule_strong),
        );
    }
    again.clicked()
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

/// A tag's colours in its menu: the five (the chosen one ringed) and
/// `none`. Returns the colour picked.
fn colour_swatches(
    ui: &mut Ui,
    tokens: &Tokens,
    current: Option<HighlightColor>,
) -> Option<Option<HighlightColor>> {
    let mut chosen = None;
    ui.set_min_width(212.0);
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
/// `link_row` with marks at its right (✓, ×), each with its tip; returns
/// the row's response and which marks were clicked.
fn marked_row(
    ui: &mut Ui,
    tokens: &Tokens,
    glyph: &str,
    name: &str,
    detail: &str,
    marks: &[(&str, &str)],
) -> (egui::Response, Vec<bool>) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click());
    let y = rect.center().y;
    let mut right = rect.right();
    let mut clicked = vec![false; marks.len()];
    for (index, (mark, tip)) in marks.iter().enumerate().rev() {
        let at = Rect::from_center_size(pos2(right - 7.0, y), vec2(14.0, 17.0));
        let hit = ui
            .interact(at, response.id.with(index), Sense::click())
            .on_hover_text(*tip)
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let color = match *mark {
            "✓" => tokens.ok,
            _ if hit.hovered() => tokens.text,
            _ => tokens.muted,
        };
        ui.painter().text(
            at.center(),
            Align2::CENTER_CENTER,
            *mark,
            theme::mono(11.0),
            color,
        );
        clicked[index] = hit.clicked();
        right -= 16.0;
    }
    let detail_rect = text::line(
        ui,
        pos2(right - 6.0, y),
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
        if glyph == "◇" {
            tokens.accent_text
        } else {
            tokens.faint
        },
    );
    text::row(
        ui,
        pos2(rect.left() + 18.0, y),
        name,
        theme::mono(11.0),
        if response.hovered() {
            tokens.text
        } else {
            tokens.text_2
        },
        (detail_rect.left() - rect.left() - 26.0).max(0.0),
    );
    (
        response.on_hover_cursor(egui::CursorIcon::PointingHand),
        clicked,
    )
}

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

/// A tag on an item: a click shows its items, a right-click its menu.
fn tag_chip_button(
    ui: &mut Ui,
    tokens: &Tokens,
    name: &str,
    color: Option<egui::Color32>,
) -> egui::Response {
    let width = tag_chip_width(ui, name, color);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 19.0), Sense::click());
    paint_tag_chip(
        ui,
        tokens,
        rect.left_center(),
        name,
        color,
        response.hovered(),
    );
    response
        .on_hover_text("show its papers · right-click for more")
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The files opened from outside the library, newest first.
pub fn history_outside(library: &LibraryStore) -> Vec<&glyphst_library::HistoryEntry> {
    let inside = library
        .library_attachments()
        .iter()
        .map(|attachment| attachment.path.as_path())
        .collect::<HashSet<_>>();
    let mut entries = library
        .history_entries()
        .iter()
        .filter(|entry| !inside.contains(entry.path.as_path()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.opened));
    entries
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
    fn history_lists_only_files_outside_the_library() {
        let dir = std::env::temp_dir().join(format!("glyphst-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut library = LibraryStore::open_for_test(dir.join("library.redb")).unwrap();
        let inside = dir.join("inside.pdf");
        let outside = dir.join("outside.pdf");
        for path in [&inside, &outside] {
            std::fs::write(path, format!("%PDF-1.4 {}", path.display())).unwrap();
        }
        library
            .link_item(&inside, LibraryItemMetadata::for_path(&inside), None)
            .unwrap();
        let entry = |path: &Path, opened| glyphst_library::HistoryEntry {
            path: glyphst_library::normalize_path(path),
            opened,
            position: glyphst_library::DocumentPosition::fixed(0.0, 0.0, 1.0, 1),
        };
        library
            .replace_history_entries(vec![entry(&inside, 2), entry(&outside, 1)])
            .unwrap();
        let listed = history_outside(&library)
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(listed, vec![glyphst_library::normalize_path(&outside)]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pages_notes_are_its_notes_section() {
        assert_eq!(notes_section("# T\n\n## Notes\n\nmine\n"), "mine");
    }
}
