//! Library queries, catalog editing, and cancellable managed imports.

use super::rendering::format_file_size;
use super::*;

impl NewTab {
    pub(super) fn filter_for_destination(
        store: &LibraryStore,
        destination: &ReaderNewTabDestination,
    ) -> LibraryFilter {
        let mut filter = LibraryFilter::default();
        match destination {
            ReaderNewTabDestination::Favorites => filter.favorite = Some(true),
            ReaderNewTabDestination::Trash => filter.trash = LibraryTrashFilter::Trashed,
            ReaderNewTabDestination::Tag(tag_id) => filter.tag_ids.push(*tag_id),
            ReaderNewTabDestination::SavedView(view_id) => {
                if let Some(view) = store
                    .library_saved_views()
                    .iter()
                    .find(|view| view.id == *view_id)
                {
                    filter = view.filter.clone();
                }
            }
            ReaderNewTabDestination::Home
            | ReaderNewTabDestination::Library
            | ReaderNewTabDestination::History
            | ReaderNewTabDestination::Tags => {}
        }
        filter
    }

    pub(super) fn entries_for_session(
        store: &LibraryStore,
        session: &LibraryBrowserSession,
        kind_filter: Option<LibraryItemKind>,
        status_filter: Option<ReadingStatus>,
    ) -> Vec<NewTabEntry> {
        if session.destination == ReaderNewTabDestination::History {
            let mut entries = store
                .history_entries()
                .iter()
                .map(|history| {
                    let item = store.library_item_for_path(&history.path);
                    NewTabEntry::from_history_and_item(history, item)
                })
                .collect::<Vec<_>>();
            Self::sort_entries(&mut entries, session.sort);
            return entries;
        }
        if matches!(session.destination, ReaderNewTabDestination::Tags) {
            return Vec::new();
        }

        let mut filter = Self::filter_for_destination(store, &session.destination);
        if let Some(kind) = kind_filter {
            filter.kinds = vec![kind];
        }
        if let Some(status) = status_filter {
            filter.statuses = vec![status];
        }
        let mut ids = store.query_library_items(&filter, session.sort);
        if session.destination == ReaderNewTabDestination::Home {
            ids.sort_by_key(|id| {
                let item = store.library_item(*id).expect("queried item must exist");
                let rank = if item.status == ReadingStatus::Reading {
                    0
                } else if item.favorite {
                    1
                } else {
                    2
                };
                (rank, std::cmp::Reverse(item.updated))
            });
            ids.truncate(24);
        }
        let mut entries = ids
            .into_iter()
            .filter_map(|id| NewTabEntry::from_library(store, id))
            .collect::<Vec<_>>();
        if session.destination == ReaderNewTabDestination::Home {
            entries.extend(
                store
                    .history_entries()
                    .iter()
                    .filter(|history| store.library_item_for_path(&history.path).is_none())
                    .take(12)
                    .map(NewTabEntry::from_history),
            );
        }
        entries
    }

    pub(super) fn sort_entries(entries: &mut [NewTabEntry], sort: LibrarySort) {
        entries.sort_by(|left, right| {
            let primary = match sort.field {
                LibrarySortField::Title => left.title_sort.cmp(&right.title_sort),
                LibrarySortField::Creator => left.creator_sort.cmp(&right.creator_sort),
                LibrarySortField::Year => left.year.cmp(&right.year),
                LibrarySortField::DateAdded => left.date_added.cmp(&right.date_added),
                LibrarySortField::LastOpened => {
                    left.last_opened_timestamp.cmp(&right.last_opened_timestamp)
                }
            };
            let ordering = primary
                .then_with(|| left.title_sort.cmp(&right.title_sort))
                .then_with(|| left.path.cmp(&right.path));
            match sort.direction {
                LibrarySortDirection::Ascending => ordering,
                LibrarySortDirection::Descending => ordering.reverse(),
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn append_collection_tree_rows(
        store: &LibraryStore,
        parent_id: Option<LibraryCollectionId>,
        depth: usize,
        entries_by_id: &HashMap<DocumentId, usize>,
        visible_items: &HashSet<DocumentId>,
        included_collections: Option<&HashSet<LibraryCollectionId>>,
        expanded: &BTreeSet<LibraryCollectionId>,
        rows: &mut Vec<LibraryTreeRow>,
    ) {
        for collection in store.child_collections(parent_id) {
            if included_collections.is_some_and(|included| !included.contains(&collection.id)) {
                continue;
            }
            rows.push(LibraryTreeRow::Collection {
                id: collection.id,
                depth,
            });
            if included_collections.is_some() || expanded.contains(&collection.id) {
                Self::append_collection_tree_rows(
                    store,
                    Some(collection.id),
                    depth + 1,
                    entries_by_id,
                    visible_items,
                    included_collections,
                    expanded,
                    rows,
                );
            }
        }
        let mut entry_indices = store
            .direct_collection_items(parent_id)
            .into_iter()
            .filter(|item_id| visible_items.contains(item_id))
            .filter_map(|item_id| entries_by_id.get(&item_id).copied())
            .collect::<Vec<_>>();
        entry_indices.sort_unstable();
        for entry_ix in entry_indices {
            rows.push(LibraryTreeRow::Document { entry_ix, depth });
        }
    }

    pub(super) fn update_filter(&mut self, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().trim().to_lowercase();
        let mut filtered_entries = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                query.is_empty()
                    || entry.file_name.to_lowercase().contains(&query)
                    || entry.directory.to_lowercase().contains(&query)
                    || entry.search_text.to_lowercase().contains(&query)
            })
            .map(|(entry_ix, _)| entry_ix)
            .collect::<Vec<_>>();

        self.tree.rows.clear();
        if self.session.destination == ReaderNewTabDestination::Library {
            let store = cx.global::<LibraryStore>();
            if self
                .tree
                .current_collection
                .is_some_and(|id| store.library_collection(id).is_none())
            {
                self.tree.current_collection = None;
            }
            match self.session.view_mode {
                LibraryViewMode::Grid | LibraryViewMode::Grouped => {
                    let direct_ids = store
                        .direct_collection_items(self.tree.current_collection)
                        .into_iter()
                        .collect::<HashSet<_>>();
                    filtered_entries.retain(|entry_ix| {
                        self.entries[*entry_ix]
                            .item_id
                            .is_some_and(|id| direct_ids.contains(&id))
                    });
                }
                LibraryViewMode::Table => {
                    let entries_by_id = self
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(entry_ix, entry)| entry.item_id.map(|id| (id, entry_ix)))
                        .collect::<HashMap<_, _>>();
                    let visible_items = filtered_entries
                        .iter()
                        .filter_map(|entry_ix| self.entries[*entry_ix].item_id)
                        .collect::<HashSet<_>>();
                    let included_collections = (!query.is_empty()).then(|| {
                        let mut included = HashSet::new();
                        for item_id in &visible_items {
                            if let Some(parent_id) = store.item_parent(*item_id) {
                                included.extend(
                                    store
                                        .collection_ancestors(parent_id)
                                        .into_iter()
                                        .map(|collection| collection.id),
                                );
                            }
                        }
                        for collection in store.library_collections() {
                            let path = store.collection_path_label(collection.id).to_lowercase();
                            if collection.normalized_name.contains(&query) || path.contains(&query)
                            {
                                included.extend(
                                    store
                                        .collection_ancestors(collection.id)
                                        .into_iter()
                                        .map(|ancestor| ancestor.id),
                                );
                            }
                        }
                        included
                    });
                    Self::append_collection_tree_rows(
                        store,
                        None,
                        0,
                        &entries_by_id,
                        &visible_items,
                        included_collections.as_ref(),
                        &self.tree.expanded_collections,
                        &mut self.tree.rows,
                    );
                    filtered_entries = self
                        .tree
                        .rows
                        .iter()
                        .filter_map(|row| match row {
                            LibraryTreeRow::Document { entry_ix, .. } => Some(*entry_ix),
                            LibraryTreeRow::Collection { .. } => None,
                        })
                        .collect();
                }
            }
        }
        self.filtered_entries = filtered_entries;

        if self.session.destination == ReaderNewTabDestination::Library
            && self.session.view_mode == LibraryViewMode::Table
        {
            let visible_nodes = self
                .tree
                .rows
                .iter()
                .filter_map(|row| row.node_id(&self.entries))
                .collect::<HashSet<_>>();
            self.tree.marked.retain(|node| visible_nodes.contains(node));
            self.tree.anchor = self.tree.anchor.filter(|node| visible_nodes.contains(node));
            let focused_node = match &self.focused_item {
                NewTabItemKey::OpenFile => None,
                NewTabItemKey::Collection(id) => Some(LibraryNodeId::Collection(*id)),
                NewTabItemKey::Document(path) => self
                    .entries
                    .iter()
                    .find(|entry| entry.path == *path)
                    .and_then(|entry| entry.item_id)
                    .map(LibraryNodeId::Item),
            }
            .filter(|node| visible_nodes.contains(node));
            self.tree.active = focused_node
                .or_else(|| self.tree.active.filter(|node| visible_nodes.contains(node)))
                .or_else(|| {
                    self.tree
                        .rows
                        .first()
                        .and_then(|row| row.node_id(&self.entries))
                });
            if let Some(node) = self.tree.active {
                self.focused_item = self.new_tab_key_for_library_node(node);
            } else {
                self.focused_item = NewTabItemKey::OpenFile;
            }
        } else if self.item_index(&self.focused_item).is_none() {
            self.focused_item = self
                .filtered_entries
                .first()
                .and_then(|entry_ix| self.entries.get(*entry_ix))
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                .unwrap_or(NewTabItemKey::OpenFile);
        }
        self.selection_anchor = self.selection_anchor.take().filter(|path| {
            self.item_index(&NewTabItemKey::Document(path.clone()))
                .is_some()
        });
        if !self.filtered_entries.is_empty() {
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
    }

    pub(super) fn on_search_input_event(
        &mut self,
        _: &Entity<InputState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.update_filter(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. }
                if self.session.destination != ReaderNewTabDestination::Tags =>
            {
                self.activate_focused(cx)
            }
            InputEvent::PressEnter { .. } => {}
            InputEvent::Focus | InputEvent::Blur => {}
        }
    }

    pub(super) fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        if self.is_library_tree() {
            let nodes = cx
                .global::<LibraryStore>()
                .disjoint_library_nodes(&self.effective_tree_selection());
            if nodes.is_empty() {
                return;
            }
            if nodes
                .iter()
                .any(|node| matches!(node, LibraryNodeId::Collection(_)))
            {
                if self.editors.folder_delete_prompt_pending {
                    return;
                }
                let confirmation = window.prompt(
                    PromptLevel::Warning,
                    "Move selected entries to Trash?",
                    Some(
                        "Selected folders and their subfolders will be removed. Papers inside them will move to Trash.",
                    ),
                    &[
                        PromptButton::ok("Move to Trash"),
                        PromptButton::cancel("Cancel"),
                    ],
                    cx,
                );
                self.editors.folder_delete_prompt_pending = true;
                cx.spawn_in(window, async move |this, cx| {
                    let confirmed = confirmation.await == Ok(0);
                    _ = this.update_in(cx, |this, _, cx| {
                        this.editors.folder_delete_prompt_pending = false;
                        if confirmed {
                            this.delete_tree_nodes_now(nodes, cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
                .detach();
            } else {
                self.delete_tree_nodes_now(nodes, cx);
            }
            return;
        }
        if self.selection.is_empty() {
            return;
        }

        if matches!(self.session.destination, ReaderNewTabDestination::Trash) {
            if self.editors.permanent_delete_prompt_pending {
                return;
            }
            let count = self.selection.len();
            let detail = format!(
                "This permanently removes {count} catalog {} and its managed Library file. The original imported source is unaffected.",
                if count == 1 { "record" } else { "records" }
            );
            let confirmation = window.prompt(
                PromptLevel::Warning,
                "Delete permanently?",
                Some(&detail),
                &[
                    PromptButton::ok("Delete Permanently"),
                    PromptButton::cancel("Cancel"),
                ],
                cx,
            );
            self.editors.permanent_delete_prompt_pending = true;
            cx.spawn_in(window, async move |this, cx| {
                let confirmed = confirmation.await == Ok(0);
                _ = this.update_in(cx, |this, _, cx| {
                    this.editors.permanent_delete_prompt_pending = false;
                    if confirmed {
                        this.delete_selected_now(cx);
                    } else {
                        cx.notify();
                    }
                });
            })
            .detach();
            return;
        }

        self.delete_selected_now(cx);
    }

    pub(super) fn delete_tree_nodes_now(
        &mut self,
        nodes: Vec<LibraryNodeId>,
        cx: &mut Context<Self>,
    ) {
        let nodes = cx.global::<LibraryStore>().disjoint_library_nodes(&nodes);
        let collections = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Collection(id) => Some(*id),
                LibraryNodeId::Item(_) => None,
            })
            .collect::<Vec<_>>();
        let items = nodes
            .iter()
            .filter_map(|node| match node {
                LibraryNodeId::Item(id) => Some(*id),
                LibraryNodeId::Collection(_) => None,
            })
            .collect::<Vec<_>>();

        let store = cx.global_mut::<LibraryStore>();
        let result = collections
            .into_iter()
            .try_for_each(|id| store.delete_library_collection(id).map(|_| ()))
            .and_then(|_| store.trash_library_items(&items).map(|_| ()));
        if let Err(error) = result {
            self.import_message =
                Some(format!("Could not delete Library entries: {error:#}").into());
        }
        self.tree.active = None;
        self.tree.marked.clear();
        self.tree.anchor = None;
        self.selection.clear();
        self.selection_anchor = None;
        self.selecting = false;
        self.inspector_open = false;
        self.focused_item = NewTabItemKey::OpenFile;
        self.tree.expanded_collections.retain(|collection_id| {
            cx.global::<LibraryStore>()
                .library_collection(*collection_id)
                .is_some()
        });
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn delete_selected_now(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() {
            return;
        }

        let paths = self.selection.iter().cloned().collect::<Vec<_>>();
        let item_ids = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path))
            .filter_map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        let history_only_paths = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path) && entry.item_id.is_none())
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let focused_ix = self.focused_index();

        {
            let store = cx.global_mut::<LibraryStore>();
            let result = match &self.session.destination {
                ReaderNewTabDestination::History => {
                    store.remove_history_entries(&paths).map(|_| ())
                }
                ReaderNewTabDestination::Trash => store
                    .permanently_delete_library_items(&item_ids)
                    .map(|_| ()),
                _ => store.trash_library_items(&item_ids).and_then(|_| {
                    store
                        .remove_history_entries(&history_only_paths)
                        .map(|_| ())
                }),
            };
            if let Err(error) = result {
                eprintln!("failed to remove New Tab items: {error:#}");
            }
        }

        for path in &paths {
            self.thumbnails.remove(path);
        }

        self.refresh_entries(cx);
        if self.item_index(&self.focused_item).is_none() {
            self.set_focused_index(focused_ix.min(self.item_count().saturating_sub(1)));
        }
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        cx.notify();
    }

    pub(super) fn refresh_entries(&mut self, cx: &mut Context<Self>) {
        let store = cx.global::<LibraryStore>();
        self.entries =
            Self::entries_for_session(store, &self.session, self.kind_filter, self.status_filter);
        self.observed_catalog_revision = store.catalog_revision();
        self.update_filter(cx);
        let existing = self
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.selection.retain(|path| existing.contains(path));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !existing.contains(path))
        {
            self.selection_anchor = None;
        }
    }

    pub(super) fn set_destination(
        &mut self,
        destination: ReaderNewTabDestination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !matches!(
                destination,
                ReaderNewTabDestination::History | ReaderNewTabDestination::Library
            )
        {
            return;
        }
        if self.session.destination == destination {
            self.nav_menu_open = false;
            cx.notify();
            return;
        }
        self.persist_current_presentation(cx);
        let presentation = match self.host {
            LibraryBrowserHost::QuickOpen => {
                let source = Self::quick_source(&destination);
                let session = self.quick_session.get_or_insert_with(Default::default);
                session.source = source;
                session.presentation(source)
            }
            LibraryBrowserHost::Home => {
                let home_destination = Self::home_destination(&destination);
                cx.global::<LibraryStore>()
                    .home_preferences()
                    .presentation(home_destination)
            }
        };
        self.session.sort = presentation.sort;
        self.session.view_mode = presentation.view_mode;
        if let ReaderNewTabDestination::SavedView(view_id) = destination
            && let Some(view) = cx
                .global::<LibraryStore>()
                .library_saved_views()
                .iter()
                .find(|view| view.id == view_id)
        {
            self.session.sort = view.sort;
            self.session.view_mode = view.view_mode;
        }
        self.session.destination = destination;
        self.persist_current_presentation(cx);
        self.nav_menu_open = false;
        self.inspector_open = false;
        self.editors.note_expanded = false;
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        self.focused_item = NewTabItemKey::OpenFile;
        self.tree.active = None;
        self.tree.marked.clear();
        self.tree.anchor = None;
        let search_placeholder = Self::search_placeholder(&self.session.destination);
        self.search_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(search_placeholder, window, cx);
        });
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn sort_field_label(field: LibrarySortField) -> &'static str {
        match field {
            LibrarySortField::Title => "Title",
            LibrarySortField::Creator => "Creator",
            LibrarySortField::Year => "Year",
            LibrarySortField::DateAdded => "Added",
            LibrarySortField::LastOpened => "Opened",
        }
    }

    pub(super) fn cycle_sort_field(&mut self, cx: &mut Context<Self>) {
        self.session.sort.field = match self.session.sort.field {
            LibrarySortField::Title => LibrarySortField::Creator,
            LibrarySortField::Creator => LibrarySortField::Year,
            LibrarySortField::Year => LibrarySortField::DateAdded,
            LibrarySortField::DateAdded => LibrarySortField::LastOpened,
            LibrarySortField::LastOpened => LibrarySortField::Title,
        };
        self.persist_current_presentation(cx);
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn toggle_sort_direction(&mut self, cx: &mut Context<Self>) {
        self.session.sort.direction = match self.session.sort.direction {
            LibrarySortDirection::Ascending => LibrarySortDirection::Descending,
            LibrarySortDirection::Descending => LibrarySortDirection::Ascending,
        };
        self.persist_current_presentation(cx);
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn cycle_kind_filter(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.kind_filter = match self.kind_filter {
            None => Some(LibraryItemKind::Paper),
            Some(LibraryItemKind::Paper) => Some(LibraryItemKind::Book),
            Some(LibraryItemKind::Book) => Some(LibraryItemKind::Other),
            Some(LibraryItemKind::Other) => None,
        };
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn cycle_status_filter(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.status_filter = match self.status_filter {
            None => Some(ReadingStatus::ToRead),
            Some(ReadingStatus::ToRead) => Some(ReadingStatus::Reading),
            Some(ReadingStatus::Reading) => Some(ReadingStatus::Read),
            Some(ReadingStatus::Read) => None,
        };
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn selected_item_id(&self) -> Option<DocumentId> {
        let path = match &self.focused_item {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => return None,
        };
        self.entries
            .iter()
            .find(|entry| entry.path == *path)
            .and_then(|entry| entry.item_id)
    }

    pub(super) fn selected_entry(&self) -> Option<&NewTabEntry> {
        let path = match &self.focused_item {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => return None,
        };
        self.entries.iter().find(|entry| entry.path == *path)
    }

    pub(super) fn entry_for_library_item(&self, id: DocumentId) -> Option<&NewTabEntry> {
        self.entries.iter().find(|entry| entry.item_id == Some(id))
    }

    pub(super) fn new_tab_key_for_library_node(&self, node: LibraryNodeId) -> NewTabItemKey {
        match node {
            LibraryNodeId::Collection(id) => NewTabItemKey::Collection(id),
            LibraryNodeId::Item(id) => self
                .entry_for_library_item(id)
                .map(|entry| NewTabItemKey::Document(entry.path.clone()))
                .unwrap_or(NewTabItemKey::OpenFile),
        }
    }

    pub(super) fn sync_work_note_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item_id = self.selected_item_id();
        if item_id == self.editors.note_item_id {
            return;
        }
        self.editors.note_item_id = item_id;
        let note = item_id
            .and_then(|id| cx.global::<LibraryStore>().library_item(id))
            .map(|item| item.work_note.clone())
            .unwrap_or_default();
        self.editors
            .work_note_input
            .update(cx, |input, cx| input.set_value(note, window, cx));
    }

    pub(super) fn on_work_note_input_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }
        let Some(item_id) = self.editors.note_item_id else {
            return;
        };
        let note = input.read(cx).value().to_string();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_note(item_id, note)
        {
            eprintln!("failed to save library work note: {error:#}");
        }
        self.observed_catalog_revision = cx.global::<LibraryStore>().catalog_revision();
        cx.notify();
    }

    pub(super) fn enter_collection(&mut self, id: LibraryCollectionId, cx: &mut Context<Self>) {
        if cx.global::<LibraryStore>().library_collection(id).is_none() {
            return;
        }
        self.tree.current_collection = Some(id);
        self.selection.clear();
        self.selection_anchor = None;
        self.inspector_open = false;
        self.update_filter(cx);
        cx.notify();
    }

    pub(super) fn enter_collection_parent(
        &mut self,
        id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        self.tree.current_collection = id.filter(|id| {
            cx.global::<LibraryStore>()
                .library_collection(*id)
                .is_some()
        });
        self.selection.clear();
        self.selection_anchor = None;
        self.inspector_open = false;
        self.update_filter(cx);
        cx.notify();
    }

    pub(super) fn toggle_collection_expanded(
        &mut self,
        id: LibraryCollectionId,
        cx: &mut Context<Self>,
    ) {
        if !self.tree.expanded_collections.insert(id) {
            self.tree.expanded_collections.remove(&id);
        }
        self.update_filter(cx);
        cx.notify();
    }

    pub(super) fn open_new_folder_editor(
        &mut self,
        parent_id: Option<LibraryCollectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.editors
            .folder_name_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.editors.folder = Some(FolderEditor {
            collection_id: None,
            parent_id,
        });
        let input = self.editors.folder_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn open_rename_folder_editor(
        &mut self,
        id: LibraryCollectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return;
        };
        let name = collection.name.clone();
        let parent_id = collection.parent_id;
        self.editors
            .folder_name_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.editors.folder = Some(FolderEditor {
            collection_id: Some(id),
            parent_id,
        });
        let input = self.editors.folder_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn save_folder_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editors.folder else {
            return;
        };
        let name = self.editors.folder_name_input.read(cx).value().to_string();
        let result = if let Some(id) = editor.collection_id {
            cx.global_mut::<LibraryStore>()
                .rename_library_collection(id, &name)
                .map(|_| id)
        } else {
            cx.global_mut::<LibraryStore>()
                .create_library_collection(editor.parent_id, &name)
        };
        match result {
            Ok(id) => {
                self.editors.folder = None;
                if let Some(parent_id) = editor.parent_id {
                    self.tree.expanded_collections.insert(parent_id);
                }
                if editor.collection_id.is_none() {
                    self.tree.expanded_collections.insert(id);
                }
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to save library folder: {error:#}"),
        }
        cx.notify();
    }

    pub(super) fn request_delete_folder(
        &mut self,
        id: LibraryCollectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home || self.editors.folder_delete_prompt_pending {
            return;
        }
        let Some(collection) = cx.global::<LibraryStore>().library_collection(id) else {
            return;
        };
        let name = collection.name.clone();
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete folder?",
            Some(&format!(
                "Delete {name:?} and its subfolders? Papers inside will move to Trash; managed files remain until Trash is emptied."
            )),
            &[
                PromptButton::ok("Delete Folder"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        self.editors.folder_delete_prompt_pending = true;
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = confirmation.await == Ok(0);
            _ = this.update_in(cx, |this, _, cx| {
                this.editors.folder_delete_prompt_pending = false;
                if confirmed {
                    match cx
                        .global_mut::<LibraryStore>()
                        .delete_library_collection(id)
                    {
                        Ok(_) => {
                            if this.tree.current_collection == Some(id)
                                || this.tree.current_collection.is_some_and(|current| {
                                    cx.global::<LibraryStore>()
                                        .library_collection(current)
                                        .is_none()
                                })
                            {
                                this.tree.current_collection = None;
                            }
                            this.tree.expanded_collections.retain(|collection_id| {
                                cx.global::<LibraryStore>()
                                    .library_collection(*collection_id)
                                    .is_some()
                            });
                            this.refresh_entries(cx);
                        }
                        Err(error) => eprintln!("failed to delete library folder: {error:#}"),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn move_library_nodes(
        &mut self,
        nodes: &[LibraryNodeId],
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        match cx
            .global_mut::<LibraryStore>()
            .move_library_nodes(nodes, parent_id)
        {
            Ok(_) => self.import_message = None,
            Err(error) => {
                self.import_message =
                    Some(format!("Could not move Library entries: {error:#}").into());
            }
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn drop_library_selection(
        &mut self,
        dragged: &DraggedLibrarySelection,
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        self.clear_library_drag_state(cx);
        self.move_library_nodes(&dragged.items(), parent_id, cx);
    }

    pub(super) fn begin_library_drag(
        &mut self,
        dragged: &DraggedLibrarySelection,
        cx: &mut Context<Self>,
    ) {
        self.tree.dragging_nodes = dragged.items().into_iter().collect();
        cx.notify();
    }

    pub(super) fn clear_library_drag_state(&mut self, cx: &mut Context<Self>) {
        let changed = self.tree.drop_target.take().is_some()
            | self.tree.hover_expand_task.take().is_some()
            | self.tree.hover_scroll_task.take().is_some()
            | !self.tree.dragging_nodes.is_empty();
        self.tree.dragging_nodes.clear();
        if changed {
            cx.notify();
        }
    }

    pub(super) fn clear_library_drop_feedback(&mut self, cx: &mut Context<Self>) {
        let changed = self.tree.drop_target.take().is_some()
            | self.tree.hover_expand_task.take().is_some()
            | self.tree.hover_scroll_task.take().is_some();
        if changed {
            cx.notify();
        }
    }

    pub(super) fn set_library_drag_target(
        &mut self,
        node: LibraryNodeId,
        dragged: &DraggedLibrarySelection,
        bounds_contains_pointer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !bounds_contains_pointer {
            if matches!(
                self.tree.drop_target,
                Some(LibraryDropTarget::Entry { hovered, .. }) if hovered == node
            ) {
                self.tree.drop_target = None;
                self.tree.hover_expand_task.take();
                cx.notify();
            }
            return;
        }
        let parent = match node {
            LibraryNodeId::Collection(id) => Some(id),
            LibraryNodeId::Item(id) => cx.global::<LibraryStore>().item_parent(id),
        };
        let target = LibraryDropTarget::Entry {
            hovered: node,
            parent,
        };
        if self.tree.drop_target == Some(target) {
            return;
        }
        let valid = cx
            .global::<LibraryStore>()
            .validate_library_move(&dragged.items(), parent)
            .unwrap_or(false);
        if !valid {
            self.tree.drop_target = None;
            self.tree.hover_expand_task.take();
            cx.notify();
            return;
        }
        self.tree.drop_target = Some(target);
        self.tree.hover_expand_task.take();
        if let LibraryNodeId::Collection(id) = node
            && !self.tree.expanded_collections.contains(&id)
        {
            self.tree.hover_expand_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                _ = this.update(cx, |this, cx| {
                    this.tree.hover_expand_task.take();
                    if matches!(
                        this.tree.drop_target,
                        Some(LibraryDropTarget::Entry { hovered, .. })
                            if hovered == LibraryNodeId::Collection(id)
                    ) {
                        this.tree.expanded_collections.insert(id);
                        this.update_filter(cx);
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    pub(super) fn library_node_is_drop_highlighted(
        &self,
        node: LibraryNodeId,
        cx: &Context<Self>,
    ) -> bool {
        let parent = match self.tree.drop_target {
            Some(LibraryDropTarget::Root) => return true,
            Some(LibraryDropTarget::Entry { parent, .. }) => parent,
            None => return false,
        };
        let Some(parent_id) = parent else {
            return true;
        };
        match node {
            LibraryNodeId::Collection(id) => cx
                .global::<LibraryStore>()
                .collection_ancestors(id)
                .iter()
                .any(|ancestor| ancestor.id == parent_id),
            LibraryNodeId::Item(id) => {
                cx.global::<LibraryStore>()
                    .item_parent(id)
                    .is_some_and(|id| {
                        cx.global::<LibraryStore>()
                            .collection_ancestors(id)
                            .iter()
                            .any(|ancestor| ancestor.id == parent_id)
                    })
            }
        }
    }

    pub(super) fn set_library_root_drag_target(
        &mut self,
        dragged: &DraggedLibrarySelection,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if !active {
            if self.tree.drop_target == Some(LibraryDropTarget::Root) {
                self.tree.drop_target = None;
                cx.notify();
            }
            return;
        }
        if self.tree.drop_target == Some(LibraryDropTarget::Root) {
            return;
        }
        if cx
            .global::<LibraryStore>()
            .validate_library_move(&dragged.items(), None)
            .unwrap_or(false)
        {
            self.tree.drop_target = Some(LibraryDropTarget::Root);
        } else {
            self.tree.drop_target = None;
        }
        self.tree.hover_expand_task.take();
        cx.notify();
    }

    pub(super) fn set_external_library_drag_target(
        &mut self,
        node: LibraryNodeId,
        active: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !active {
            if matches!(
                self.tree.drop_target,
                Some(LibraryDropTarget::Entry { hovered, .. }) if hovered == node
            ) {
                self.tree.drop_target = None;
                self.tree.hover_expand_task.take();
                cx.notify();
            }
            return;
        }
        let parent = match node {
            LibraryNodeId::Collection(id) => Some(id),
            LibraryNodeId::Item(id) => cx.global::<LibraryStore>().item_parent(id),
        };
        let target = LibraryDropTarget::Entry {
            hovered: node,
            parent,
        };
        if self.tree.drop_target == Some(target) {
            return;
        }
        self.tree.drop_target = Some(target);
        self.tree.hover_expand_task.take();
        if let LibraryNodeId::Collection(id) = node
            && !self.tree.expanded_collections.contains(&id)
        {
            self.tree.hover_expand_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                _ = this.update(cx, |this, cx| {
                    this.tree.hover_expand_task.take();
                    if matches!(
                        this.tree.drop_target,
                        Some(LibraryDropTarget::Entry { hovered, .. })
                            if hovered == LibraryNodeId::Collection(id)
                    ) {
                        this.tree.expanded_collections.insert(id);
                        this.update_filter(cx);
                    }
                    cx.notify();
                });
            }));
        }
        cx.notify();
    }

    pub(super) fn set_external_library_root_drag_target(
        &mut self,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        if active {
            self.tree.drop_target = Some(LibraryDropTarget::Root);
            self.tree.hover_expand_task.take();
        } else if self.tree.drop_target == Some(LibraryDropTarget::Root) {
            self.tree.drop_target = None;
        }
        cx.notify();
    }

    pub(super) fn drop_external_library_paths(
        &mut self,
        paths: &ExternalPaths,
        parent_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        let paths = paths.paths().to_vec();
        self.clear_library_drag_state(cx);
        self.start_library_import(paths, parent_id, cx);
    }

    pub(super) fn track_external_document_drag(
        &mut self,
        event: &DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let feedback = event
            .bounds
            .contains(&event.event.position)
            .then(|| classify_external_document_paths(event.drag(cx).paths()).feedback())
            .flatten();
        if self.external_document_drop_feedback != feedback {
            self.external_document_drop_feedback = feedback;
            cx.notify();
        }
    }

    pub(super) fn handle_library_drag_move<T: 'static>(
        &mut self,
        event: &DragMoveEvent<T>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.bounds.contains(&event.event.position) {
            self.clear_library_drop_feedback(cx);
            return;
        }
        self.tree.hover_scroll_task.take();
        let height = event.bounds.size.height;
        if height <= px(0.) {
            return;
        }
        let fraction = (event.event.position.y - event.bounds.origin.y) / height;
        let delta = if fraction <= 0.05 {
            8.0
        } else if fraction <= 0.15 {
            5.0
        } else if fraction >= 0.95 {
            -8.0
        } else if fraction >= 0.85 {
            -5.0
        } else {
            return;
        };
        self.tree.hover_scroll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let continuing = this
                    .update(cx, |this, cx| {
                        if this.tree.hover_scroll_task.is_none() {
                            return false;
                        }
                        let handle = this.scroll.0.borrow_mut();
                        let offset = handle.base_handle.offset();
                        handle
                            .base_handle
                            .set_offset(offset + point(px(0.0), px(delta)));
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !continuing {
                    break;
                }
            }
        }));
    }

    pub(super) fn library_import_target(&self, cx: &Context<Self>) -> Option<LibraryCollectionId> {
        if self.is_library_tree() {
            return match self.tree.active {
                Some(LibraryNodeId::Collection(id)) => Some(id),
                Some(LibraryNodeId::Item(id)) => cx.global::<LibraryStore>().item_parent(id),
                None => None,
            };
        }
        self.tree.current_collection
    }

    pub(super) fn cancel_library_import(&mut self, cx: &mut Context<Self>) {
        let Some(cancel) = &self.import_cancel else {
            return;
        };
        cancel.store(true, AtomicOrdering::Release);
        if let Some(progress) = &mut self.import_progress {
            progress.cancelled = true;
        }
        self.import_message = Some("Cancelling import…".into());
        cx.notify();
    }

    pub(super) fn start_library_import(
        &mut self,
        paths: Vec<PathBuf>,
        target_collection: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home || paths.is_empty() {
            return;
        }
        if self.import_cancel.is_some() {
            self.import_message = Some("A Library import is already running.".into());
            cx.notify();
            return;
        }
        let Some(managed_root) = cx
            .global::<LibraryStore>()
            .managed_library_root()
            .map(Path::to_path_buf)
        else {
            self.import_message = Some("Managed Library storage is unavailable.".into());
            cx.notify();
            return;
        };

        let cancel = Arc::new(AtomicBool::new(false));
        self.import_cancel = Some(cancel.clone());
        self.import_progress = Some(LibraryImportProgress::default());
        self.import_message = Some("Scanning files…".into());
        cx.notify();

        cx.spawn(async move |this, cx| {
            let scan = cx
                .background_spawn(async move { scan_library_import_paths(paths) })
                .await;
            let folders = scan.folders.clone();
            let candidates = scan.candidates;
            let skipped = scan.skipped;
            let scan_failed = scan.failed;
            let total = candidates.len();

            if !cancel.load(AtomicOrdering::Acquire)
                && this
                    .update(cx, |this, cx| {
                        if let Some(progress) = &mut this.import_progress {
                            progress.total = total;
                            progress.skipped = skipped;
                            progress.failed = scan_failed;
                        }
                        for folder in &folders {
                            if let Err(error) = cx
                                .global_mut::<LibraryStore>()
                                .ensure_library_collection_path(target_collection, folder)
                            {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.failed = progress.failed.saturating_add(1);
                                }
                                this.import_message = Some(
                                    format!("Could not create imported folder: {error:#}").into(),
                                );
                            }
                        }
                        this.refresh_entries(cx);
                        cx.notify();
                    })
                    .is_err()
            {
                return;
            }

            for candidate in candidates {
                if cancel.load(AtomicOrdering::Acquire) {
                    break;
                }
                let folder_path = candidate.folder_path.clone();
                let parent_result = this.update(cx, |_, cx| {
                    cx.global_mut::<LibraryStore>()
                        .ensure_library_collection_path(target_collection, &folder_path)
                });
                let parent_id = match parent_result {
                    Ok(Ok(parent_id)) => parent_id,
                    Ok(Err(error)) => {
                        _ = this.update(cx, |this, cx| {
                            if let Some(progress) = &mut this.import_progress {
                                progress.completed = progress.completed.saturating_add(1);
                                progress.failed = progress.failed.saturating_add(1);
                            }
                            this.import_message =
                                Some(format!("Could not create import folder: {error:#}").into());
                            cx.notify();
                        });
                        continue;
                    }
                    Err(_) => return,
                };

                let source_path = candidate.path;
                let work_path = source_path.clone();
                let work_root = managed_root.clone();
                let work_cancel = cancel.clone();
                let prepared = cx
                    .background_spawn(async move {
                        let metadata = extract_library_metadata(&work_path)?;
                        let prepared =
                            prepare_managed_import(&work_root, &work_path, &work_cancel)?;
                        Ok::<_, anyhow::Error>((prepared, metadata))
                    })
                    .await;
                let was_cancelled = cancel.load(AtomicOrdering::Acquire);
                if this
                    .update(cx, |this, cx| {
                        if let Some(progress) = &mut this.import_progress {
                            progress.completed = progress.completed.saturating_add(1);
                        }
                        match prepared {
                            Ok((prepared, metadata)) => match cx
                                .global_mut::<LibraryStore>()
                                .finalize_managed_import(prepared, metadata, parent_id)
                            {
                                Ok(LibraryImportOutcome::Added(_)) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.added = progress.added.saturating_add(1);
                                    }
                                }
                                Ok(
                                    LibraryImportOutcome::Duplicate(_)
                                    | LibraryImportOutcome::DuplicateInTrash(_),
                                ) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.duplicates = progress.duplicates.saturating_add(1);
                                    }
                                }
                                Err(error) => {
                                    if let Some(progress) = &mut this.import_progress {
                                        progress.failed = progress.failed.saturating_add(1);
                                    }
                                    this.import_message = Some(
                                        format!(
                                            "Could not import {}: {error:#}",
                                            source_path.display()
                                        )
                                        .into(),
                                    );
                                }
                            },
                            Err(_error) if was_cancelled => {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.cancelled = true;
                                }
                                this.import_message = Some("Cancelling import…".into());
                            }
                            Err(error) => {
                                if let Some(progress) = &mut this.import_progress {
                                    progress.failed = progress.failed.saturating_add(1);
                                }
                                this.import_message = Some(
                                    format!(
                                        "Could not import {}: {error:#}",
                                        source_path.display()
                                    )
                                    .into(),
                                );
                            }
                        }
                        this.refresh_entries(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }

            _ = this.update(cx, |this, cx| {
                let progress = this.import_progress.take().unwrap_or_default();
                let cancelled = cancel.load(AtomicOrdering::Acquire) || progress.cancelled;
                this.import_cancel = None;
                this.import_message = Some(
                    format!(
                        "{} {}. {} already present, {} skipped, {} failed{}.",
                        if cancelled {
                            "Import stopped after"
                        } else {
                            "Imported"
                        },
                        progress.added,
                        progress.duplicates,
                        progress.skipped,
                        progress.failed,
                        if cancelled { " before completion" } else { "" }
                    )
                    .into(),
                );
                this.refresh_entries(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn add_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target_collection = self.library_import_target(cx);
        self.add_files_to(target_collection, window, cx);
    }

    pub(super) fn add_files_to(
        &mut self,
        target_collection: Option<LibraryCollectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        let directory = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let dialog = prompt_for_paths(
            window,
            cx,
            OpenPathOptions::files(directory, "Add books and papers")
                .multiple(true)
                .filter(PathFilter::new(
                    crate::fl!("external-drop-supported-hint"),
                    ["pdf", "epub"],
                ))
                .allow_all_files(true),
        );
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = dialog.await else {
                return;
            };
            _ = this.update(cx, |this, cx| {
                this.start_library_import(paths, target_collection, cx);
            });
        })
        .detach();
    }

    pub(super) fn add_selected_history_item(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        if entry.item_id.is_some() {
            return;
        }
        self.start_library_import(vec![entry.path], None, cx);
    }

    pub(super) fn toggle_selected_favorite(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_item_id() else {
            return;
        };
        let favorite = cx
            .global::<LibraryStore>()
            .library_item(id)
            .is_some_and(|item| !item.favorite);
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_favorite(id, favorite)
        {
            eprintln!("failed to update favorite: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn cycle_selected_status(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected_item_id() else {
            return;
        };
        let status = match cx
            .global::<LibraryStore>()
            .library_item(id)
            .map(|item| item.status)
        {
            Some(ReadingStatus::ToRead) => ReadingStatus::Reading,
            Some(ReadingStatus::Reading) => ReadingStatus::Read,
            Some(ReadingStatus::Read) | None => ReadingStatus::ToRead,
        };
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_library_item_status(id, status)
        {
            eprintln!("failed to update reading status: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn restore_selected_items(&mut self, cx: &mut Context<Self>) {
        let ids = self
            .entries
            .iter()
            .filter(|entry| self.selection.contains(&entry.path))
            .filter_map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        if let Err(error) = cx.global_mut::<LibraryStore>().restore_library_items(&ids) {
            eprintln!("failed to restore library items: {error:#}");
        }
        self.refresh_entries(cx);
        self.selection.clear();
        cx.notify();
    }

    pub(super) fn add_tag_to_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let name = self.editors.tag_input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .add_tag_to_items(&[item_id], &name)
        {
            eprintln!("failed to add library tag: {error:#}");
            return;
        }
        self.editors
            .tag_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn remove_tag_from_selected(
        &mut self,
        tag_id: LibraryTagId,
        cx: &mut Context<Self>,
    ) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let retained = cx
            .global::<LibraryStore>()
            .tags_for_item(item_id)
            .into_iter()
            .map(|tag| tag.id)
            .filter(|candidate| *candidate != tag_id)
            .collect::<Vec<_>>();
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .set_item_tags(item_id, &retained)
        {
            eprintln!("failed to remove library tag: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn set_selected_collection(
        &mut self,
        collection_id: Option<LibraryCollectionId>,
        cx: &mut Context<Self>,
    ) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let collection_ids = collection_id.into_iter().collect::<Vec<_>>();
        let result = cx
            .global_mut::<LibraryStore>()
            .set_item_collections(item_id, &collection_ids);
        if let Err(error) = result {
            eprintln!("failed to move Library item: {error:#}");
        }
        self.refresh_entries(cx);
        cx.notify();
    }

    pub(super) fn open_saved_view_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let suggested = match &self.session.destination {
            ReaderNewTabDestination::Favorites => "Favorite works",
            ReaderNewTabDestination::Tag(_) => "Tagged works",
            ReaderNewTabDestination::SavedView(_) => "Copy of saved view",
            _ => "Research view",
        };
        self.editors
            .saved_view_name_input
            .update(cx, |input, cx| input.set_value(suggested, window, cx));
        self.editors.saved_view_open = true;
        let input = self.editors.saved_view_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn save_current_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .editors
            .saved_view_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        let mut filter =
            Self::filter_for_destination(cx.global::<LibraryStore>(), &self.session.destination);
        let search = self.search_input.read(cx).value().trim().to_string();
        if !search.is_empty() {
            filter.text = search;
        }
        let result = cx.global_mut::<LibraryStore>().save_library_view(
            &name,
            filter,
            self.session.sort,
            self.session.view_mode,
        );
        match result {
            Ok(view_id) => {
                self.editors.saved_view_open = false;
                self.set_destination(ReaderNewTabDestination::SavedView(view_id), window, cx);
            }
            Err(error) => eprintln!("failed to save library view: {error:#}"),
        }
        cx.notify();
    }

    pub(super) fn delete_current_saved_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ReaderNewTabDestination::SavedView(view_id) = self.session.destination else {
            return;
        };
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete saved view?",
            Some("The books and papers matched by this view remain in your library."),
            &[
                PromptButton::ok("Delete View"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = cx.global_mut::<LibraryStore>().delete_library_view(view_id) {
                    eprintln!("failed to delete saved library view: {error:#}");
                    return;
                }
                this.set_destination(ReaderNewTabDestination::Library, window, cx);
            });
        })
        .detach();
    }

    pub(super) fn open_tag_editor(
        &mut self,
        tag_id: LibraryTagId,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editors.tag_id = Some(tag_id);
        self.editors
            .tag_name_input
            .update(cx, |input, cx| input.set_value(name, window, cx));
        let input = self.editors.tag_name_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn save_tag_editor(&mut self, cx: &mut Context<Self>) {
        let Some(tag_id) = self.editors.tag_id else {
            return;
        };
        let name = self
            .editors
            .tag_name_input
            .read(cx)
            .value()
            .trim()
            .to_string();
        if name.is_empty() {
            return;
        }
        match cx
            .global_mut::<LibraryStore>()
            .rename_library_tag(tag_id, &name)
        {
            Ok(result_id) => {
                if self.session.destination == ReaderNewTabDestination::Tag(tag_id) {
                    self.session.destination = ReaderNewTabDestination::Tag(result_id);
                }
                self.editors.tag_id = None;
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to rename library tag: {error:#}"),
        }
        cx.notify();
    }

    pub(super) fn delete_edited_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tag_id) = self.editors.tag_id else {
            return;
        };
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "Delete tag?",
            Some("This removes the tag from every work. It does not delete any library item."),
            &[
                PromptButton::ok("Delete Tag"),
                PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            _ = this.update_in(cx, |this, window, cx| {
                if let Err(error) = cx.global_mut::<LibraryStore>().delete_library_tag(tag_id) {
                    eprintln!("failed to delete library tag: {error:#}");
                    return;
                }
                this.editors.tag_id = None;
                if this.session.destination == ReaderNewTabDestination::Tag(tag_id) {
                    this.set_destination(ReaderNewTabDestination::Tags, window, cx);
                } else {
                    this.refresh_entries(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn open_metadata_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let Some(item) = cx.global::<LibraryStore>().library_item(item_id).cloned() else {
            return;
        };
        let creators = item
            .creators
            .iter()
            .map(|creator| creator.name.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        let mut set = |input: &Entity<InputState>, value: String| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        self.editors.metadata_kind = item.kind;
        set(&self.editors.metadata_title_input, item.title);
        set(&self.editors.metadata_creators_input, creators);
        set(
            &self.editors.metadata_year_input,
            item.year.map(|year| year.to_string()).unwrap_or_default(),
        );
        set(&self.editors.metadata_publication_input, item.publication);
        set(&self.editors.metadata_publisher_input, item.publisher);
        set(
            &self.editors.metadata_doi_input,
            item.identifiers.doi.unwrap_or_default(),
        );
        set(
            &self.editors.metadata_isbn_input,
            item.identifiers.isbn.unwrap_or_default(),
        );
        set(
            &self.editors.metadata_arxiv_input,
            item.identifiers.arxiv.unwrap_or_default(),
        );
        set(&self.editors.metadata_subject_input, item.subject);
        set(&self.editors.metadata_language_input, item.language);
        self.editors
            .metadata_abstract_input
            .update(cx, |input, cx| {
                input.set_value(item.abstract_text, window, cx)
            });
        self.editors.metadata_open = true;
        let input = self.editors.metadata_title_input.clone();
        window.defer(cx, move |window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
        });
        cx.notify();
    }

    pub(super) fn save_metadata_editor(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let Some(mut item) = cx.global::<LibraryStore>().library_item(item_id).cloned() else {
            return;
        };
        item.kind = self.editors.metadata_kind;
        item.title = self
            .editors
            .metadata_title_input
            .read(cx)
            .value()
            .to_string();
        item.creators = self
            .editors
            .metadata_creators_input
            .read(cx)
            .value()
            .split(';')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| LibraryCreator {
                name: name.to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
        item.year = self
            .editors
            .metadata_year_input
            .read(cx)
            .value()
            .trim()
            .parse::<i32>()
            .ok();
        item.publication = self
            .editors
            .metadata_publication_input
            .read(cx)
            .value()
            .to_string();
        item.publisher = self
            .editors
            .metadata_publisher_input
            .read(cx)
            .value()
            .to_string();
        let optional_value = |input: &Entity<InputState>, cx: &App| {
            let value = input.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        item.identifiers.doi = optional_value(&self.editors.metadata_doi_input, cx);
        item.identifiers.isbn = optional_value(&self.editors.metadata_isbn_input, cx);
        item.identifiers.arxiv = optional_value(&self.editors.metadata_arxiv_input, cx);
        item.subject = self
            .editors
            .metadata_subject_input
            .read(cx)
            .value()
            .to_string();
        item.language = self
            .editors
            .metadata_language_input
            .read(cx)
            .value()
            .to_string();
        item.abstract_text = self
            .editors
            .metadata_abstract_input
            .read(cx)
            .value()
            .to_string();
        match cx.global_mut::<LibraryStore>().update_library_item(item) {
            Ok(()) => {
                self.editors.metadata_open = false;
                self.refresh_entries(cx);
            }
            Err(error) => eprintln!("failed to update library metadata: {error:#}"),
        }
        cx.notify();
    }

    pub(super) fn locate_selected_attachment(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        let directory = self
            .selected_entry()
            .and_then(|entry| entry.path.parent())
            .map(Path::to_path_buf)
            .or_else(dirs::document_dir)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let Some(managed_root) = cx
            .global::<LibraryStore>()
            .managed_library_root()
            .map(Path::to_path_buf)
        else {
            self.import_message = Some("Managed Library storage is unavailable.".into());
            cx.notify();
            return;
        };
        let dialog = prompt_for_paths(
            window,
            cx,
            OpenPathOptions::files(directory, "Locate linked book or paper")
                .filter(PathFilter::new(
                    crate::fl!("external-drop-supported-hint"),
                    ["pdf", "epub"],
                ))
                .allow_all_files(true),
        );
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = dialog.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let prepared = cx
                .background_spawn(async move {
                    prepare_managed_import(&managed_root, &path, &AtomicBool::new(false))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                let result = prepared.and_then(|prepared| {
                    cx.global_mut::<LibraryStore>()
                        .finalize_managed_replacement(item_id, prepared)
                });
                match result {
                    Ok(()) => {
                        this.import_message = Some("Managed Library file replaced.".into());
                        this.refresh_entries(cx);
                    }
                    Err(error) => {
                        this.import_message =
                            Some(format!("Could not replace Library file: {error:#}").into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn trash_selected_item(&mut self, cx: &mut Context<Self>) {
        let Some(item_id) = self.selected_item_id() else {
            return;
        };
        if let Err(error) = cx
            .global_mut::<LibraryStore>()
            .trash_library_items(&[item_id])
        {
            eprintln!("failed to move library item to Trash: {error:#}");
        }
        self.refresh_entries(cx);
        self.selection.clear();
        cx.notify();
    }
}

impl NewTabEntry {
    pub(super) fn from_history(entry: &HistoryEntry) -> Self {
        let file_name: SharedString = entry
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::fl!("untitled"))
            .into();
        let directory = entry
            .path
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default()
            .into();
        let format = entry
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .and_then(DocumentFormat::from_extension);
        let format_label = match format {
            Some(DocumentFormat::Pdf) => "PDF",
            Some(DocumentFormat::Epub) => "EPUB",
            None => "—",
        };
        let file_size = fs::metadata(&entry.path)
            .ok()
            .map(|metadata| format_file_size(metadata.len()))
            .unwrap_or_else(|| "—".to_string());
        let format_size = format!("{format_label} · {file_size}").into();
        let last_opened = i64::try_from(entry.opened)
            .ok()
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0))
            .map(|timestamp| {
                timestamp
                    .with_timezone(&Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "—".to_string())
            .into();
        let (last_read, progress) = entry
            .position
            .semantic
            .as_ref()
            .map(|position| {
                let progress =
                    format_user_float((position.progress.clamp(0.0, 1.0) * 100.0) as f64);
                (
                    crate::fl!("new-tab-last-read-progress", progress = progress.clone()).into(),
                    format!("{progress}%").into(),
                )
            })
            .unwrap_or_else(|| {
                (
                    crate::fl!("new-tab-last-read", page = entry.position.page as i64).into(),
                    crate::fl!("new-tab-progress-page", page = entry.position.page as i64).into(),
                )
            });

        Self {
            item_id: None,
            path: entry.path.clone(),
            file_name: file_name.clone(),
            directory,
            last_read,
            progress,
            format_size,
            last_opened,
            format,
            favorite: false,
            status: None,
            missing: !entry.path.exists(),
            title_sort: file_name.to_lowercase().into(),
            creator_sort: "".into(),
            year: None,
            date_added: None,
            last_opened_timestamp: Some(entry.opened),
            search_text: String::new().into(),
        }
    }

    pub(super) fn from_history_and_item(entry: &HistoryEntry, item: Option<&LibraryItem>) -> Self {
        let mut result = Self::from_history(entry);
        if let Some(item) = item {
            result.apply_library_item(item, &[]);
        }
        result
    }

    pub(super) fn from_library(store: &LibraryStore, item_id: DocumentId) -> Option<Self> {
        let item = store.library_item(item_id)?;
        let attachment = store.primary_attachment(item_id)?;
        let history = store
            .history_entries()
            .iter()
            .find(|history| history.path == attachment.path);
        let mut result = if let Some(history) = history {
            Self::from_history(history)
        } else {
            let format_label = match attachment.format {
                DocumentFormat::Pdf => "PDF",
                DocumentFormat::Epub => "EPUB",
            };
            Self {
                item_id: None,
                path: attachment.path.clone(),
                file_name: item.title.clone().into(),
                directory: attachment
                    .path
                    .parent()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
                    .into(),
                last_read: "Not opened yet".into(),
                progress: "—".into(),
                format_size: format!(
                    "{format_label} · {}",
                    format_file_size(attachment.file_size)
                )
                .into(),
                last_opened: "—".into(),
                format: Some(attachment.format),
                favorite: false,
                status: None,
                missing: attachment.is_missing(),
                title_sort: item.title.to_lowercase().into(),
                creator_sort: "".into(),
                year: item.year,
                date_added: Some(item.created),
                last_opened_timestamp: None,
                search_text: String::new().into(),
            }
        };
        let tags = store
            .tags_for_item(item_id)
            .into_iter()
            .map(|tag| tag.name.clone())
            .collect::<Vec<_>>();
        result.apply_library_item(item, &tags);
        let collection_paths = store
            .collections_for_item(item_id)
            .into_iter()
            .map(|collection| store.collection_path_label(collection.id))
            .collect::<Vec<_>>();
        if !collection_paths.is_empty() {
            let collection_paths = collection_paths.join(" · ");
            let secondary = result.directory.to_string();
            result.directory = if secondary.is_empty() {
                collection_paths.clone().into()
            } else {
                format!("{secondary} · {collection_paths}").into()
            };
            result.search_text = format!("{} {collection_paths}", result.search_text).into();
        }
        Some(result)
    }

    pub(super) fn apply_library_item(&mut self, item: &LibraryItem, tags: &[String]) {
        self.item_id = Some(item.id);
        self.file_name = item.title.clone().into();
        self.title_sort = item.title.to_lowercase().into();
        let creators = item
            .creators
            .iter()
            .map(|creator| creator.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        if !creators.is_empty() {
            self.directory = creators.clone().into();
        }
        self.creator_sort = creators.to_lowercase().into();
        self.year = item.year;
        self.date_added = Some(item.created);
        self.favorite = item.favorite;
        self.status = Some(item.status);
        self.last_read = match item.status {
            ReadingStatus::ToRead => "To Read".into(),
            ReadingStatus::Reading => "Reading".into(),
            ReadingStatus::Read => "Read".into(),
        };
        self.search_text = [
            creators,
            item.year.map(|year| year.to_string()).unwrap_or_default(),
            item.publication.clone(),
            item.publisher.clone(),
            item.abstract_text.clone(),
            item.subject.clone(),
            item.language.clone(),
            item.identifiers.doi.clone().unwrap_or_default(),
            item.identifiers.isbn.clone().unwrap_or_default(),
            item.identifiers.arxiv.clone().unwrap_or_default(),
            item.embedded_keywords.join(" "),
            tags.join(" "),
        ]
        .join(" ")
        .into();
    }
}

pub(super) fn extract_library_metadata(path: &Path) -> Result<LibraryItemMetadata> {
    let mut metadata = LibraryItemMetadata::for_path(path);
    let document = match mupdf::Document::open(path) {
        Ok(document) => document,
        Err(error) => {
            eprintln!(
                "failed to extract embedded metadata from {}; using the filename instead: {error:#}",
                path.display()
            );
            return Ok(metadata);
        }
    };
    let read = |name| {
        document
            .metadata(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(title) = read(mupdf::MetadataName::Title) {
        metadata.title = title;
    }
    if let Some(authors) = read(mupdf::MetadataName::Author) {
        metadata.creators = authors
            .split([';', '\n'])
            .map(str::trim)
            .filter(|author| !author.is_empty())
            .map(|name| LibraryCreator {
                name: name.to_string(),
                role: LibraryCreatorRole::Author,
            })
            .collect();
    }
    metadata.subject = read(mupdf::MetadataName::Subject).unwrap_or_default();
    if let Some(keywords) = read(mupdf::MetadataName::Keywords) {
        metadata.embedded_keywords = keywords
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|keyword| !keyword.is_empty())
            .map(str::to_string)
            .collect();
    }
    if let Some(date) = read(mupdf::MetadataName::CreationDate) {
        metadata.year = date
            .as_bytes()
            .windows(4)
            .filter_map(|digits| std::str::from_utf8(digits).ok())
            .filter_map(|digits| digits.parse::<i32>().ok())
            .find(|year| (1000..=3000).contains(year));
    }
    Ok(metadata)
}

pub(super) fn scan_library_import_paths(paths: Vec<PathBuf>) -> LibraryImportScan {
    fn visit(path: PathBuf, logical_path: Vec<String>, scan: &mut LibraryImportScan) {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            scan.failed += 1;
            return;
        };
        if metadata.file_type().is_symlink() {
            scan.skipped += 1;
            return;
        }
        if metadata.is_file() {
            let supported = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(DocumentFormat::from_extension)
                .is_some();
            if supported {
                scan.candidates.push(LibraryImportCandidate {
                    path,
                    folder_path: logical_path,
                });
            } else {
                scan.skipped += 1;
            }
            return;
        }
        if !metadata.is_dir() {
            scan.skipped += 1;
            return;
        }

        scan.folders.push(logical_path.clone());
        let Ok(entries) = fs::read_dir(&path) else {
            scan.failed += 1;
            return;
        };
        let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
        for entry in entries {
            let child_path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                scan.failed += 1;
                continue;
            };
            if file_type.is_symlink() {
                scan.skipped += 1;
                continue;
            }
            if file_type.is_dir() {
                let mut child_logical_path = logical_path.clone();
                child_logical_path.push(entry.file_name().to_string_lossy().into_owned());
                visit(child_path, child_logical_path, scan);
            } else {
                visit(child_path, logical_path.clone(), scan);
            }
        }
    }

    let mut scan = LibraryImportScan::default();
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            scan.failed += 1;
            continue;
        };
        if metadata.file_type().is_symlink() {
            scan.skipped += 1;
        } else if metadata.is_dir() {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Imported".to_string());
            visit(path, vec![name], &mut scan);
        } else {
            visit(path, Vec::new(), &mut scan);
        }
    }
    scan.folders
        .sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    scan.folders.dedup();
    scan
}
