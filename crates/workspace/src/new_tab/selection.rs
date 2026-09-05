//! Visible-order focus, pointer selection, and keyboard navigation.

use super::*;

impl NewTab {
    pub(super) fn item_count(&self) -> usize {
        self.filtered_entries.len()
    }

    pub(super) fn item_key(&self, item_ix: usize) -> Option<NewTabItemKey> {
        self.entry_for_item(item_ix)
            .map(|entry| NewTabItemKey::Document(entry.path.clone()))
    }

    pub(super) fn item_index(&self, key: &NewTabItemKey) -> Option<usize> {
        match key {
            NewTabItemKey::OpenFile | NewTabItemKey::Collection(_) => None,
            NewTabItemKey::Document(path) => self
                .filtered_entries
                .iter()
                .position(|entry_ix| self.entries[*entry_ix].path == *path),
        }
    }

    pub(super) fn entry_for_item(&self, item_ix: usize) -> Option<&NewTabEntry> {
        self.filtered_entries
            .get(item_ix)
            .and_then(|entry_ix| self.entries.get(*entry_ix))
    }

    pub(super) fn focused_index(&self) -> usize {
        self.item_index(&self.focused_item).unwrap_or(0)
    }

    pub(super) fn set_focused_index(&mut self, item_ix: usize) {
        if let Some(key) = self.item_key(item_ix.min(self.item_count().saturating_sub(1))) {
            self.focused_item = key;
        }
    }

    pub(super) fn row_for_item(&self, item_ix: usize, mode: NewTabViewMode) -> usize {
        match mode {
            NewTabViewMode::Grid => item_ix / self.cols.max(1),
            NewTabViewMode::Grouped | NewTabViewMode::Table => item_ix,
        }
    }

    pub(super) fn scroll_focused(&self, mode: NewTabViewMode) {
        self.scroll.scroll_to_item(
            self.row_for_item(self.focused_index(), mode),
            ScrollStrategy::Center,
        );
    }

    pub(super) fn row_height(mode: NewTabViewMode) -> f32 {
        match mode {
            NewTabViewMode::Grid => GRID_ROW_HEIGHT,
            NewTabViewMode::Grouped | NewTabViewMode::Table => LIST_ROW_HEIGHT,
        }
    }

    pub(super) fn scroll_by(&mut self, delta_y: Pixels, cx: &mut Context<Self>) {
        let max_offset = self.scroll.0.borrow().base_handle.max_offset().y;
        if max_offset <= px(0.) {
            return;
        }

        let current = self.scroll.offset();
        let new_y = (current.y - delta_y).clamp(-max_offset, px(0.));
        self.scroll.set_offset(point(current.x, new_y));
        cx.notify();
    }

    pub(super) fn enter_select(&mut self, cx: &mut Context<Self>) {
        if self.host != LibraryBrowserHost::Home {
            return;
        }
        self.selecting = true;
        if self.is_library_tree() {
            if let Some(active) = self.tree.active {
                self.tree.marked.insert(active);
                self.tree.anchor = Some(active);
            }
            cx.notify();
            return;
        }
        if self.focused_item == NewTabItemKey::OpenFile
            && let Some(first) = self.entry_for_item(0)
        {
            self.focused_item = NewTabItemKey::Document(first.path.clone());
        }
        cx.notify();
    }

    pub(super) fn cancel_select(&mut self, cx: &mut Context<Self>) {
        let changed = self.selecting || !self.selection.is_empty() || !self.tree.marked.is_empty();
        self.selecting = false;
        self.selection.clear();
        self.selection_anchor = None;
        self.tree.marked.clear();
        self.tree.anchor = self.tree.active;
        if changed {
            cx.notify();
        }
    }

    pub(super) fn toggle_selection(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selecting = true;
        self.focused_item = NewTabItemKey::Document(path.clone());
        self.selection_anchor = Some(path.clone());
        if !self.selection.insert(path.clone()) {
            self.selection.remove(&path);
        }
        cx.notify();
    }

    pub(super) fn selection_range(
        entries: &[NewTabEntry],
        filtered_entries: &[usize],
        anchor: &Path,
        target_item_ix: usize,
    ) -> BTreeSet<PathBuf> {
        if filtered_entries.is_empty() {
            return BTreeSet::new();
        }
        let Some(anchor_item_ix) = filtered_entries
            .iter()
            .position(|entry_ix| entries[*entry_ix].path == anchor)
        else {
            return BTreeSet::new();
        };
        let target_item_ix = target_item_ix.min(filtered_entries.len().saturating_sub(1));
        let (start, end) = if anchor_item_ix <= target_item_ix {
            (anchor_item_ix, target_item_ix)
        } else {
            (target_item_ix, anchor_item_ix)
        };
        filtered_entries[start..=end]
            .iter()
            .map(|entry_ix| entries[*entry_ix].path.clone())
            .collect()
    }

    pub(super) fn extend_selection_to(&mut self, target_item_ix: usize, cx: &mut Context<Self>) {
        if self.filtered_entries.is_empty() {
            return;
        }

        let anchor = self.selection_anchor.clone().or_else(|| {
            self.entry_for_item(self.focused_index())
                .map(|entry| entry.path.clone())
        });
        let target_item_ix = target_item_ix.min(self.item_count() - 1);
        let Some(anchor) = anchor.or_else(|| {
            self.entry_for_item(target_item_ix)
                .map(|entry| entry.path.clone())
        }) else {
            return;
        };

        self.selecting = true;
        self.selection_anchor = Some(anchor.clone());
        self.selection = Self::selection_range(
            &self.entries,
            &self.filtered_entries,
            &anchor,
            target_item_ix,
        );
        self.set_focused_index(target_item_ix);
        cx.notify();
    }

    pub(super) fn handle_document_pointer(
        &mut self,
        path: PathBuf,
        modifiers: Modifiers,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !modifiers.shift
            && !modifiers.secondary()
            && !self.selecting
            && click_count > 0
        {
            self.open_document(path, cx);
            return;
        }

        let ordered = self
            .filtered_entries
            .iter()
            .map(|entry_ix| self.entries[*entry_ix].path.clone())
            .collect::<Vec<_>>();

        if modifiers.shift || modifiers.secondary() || self.selecting {
            self.selecting = true;
            self.focused_item = NewTabItemKey::Document(path.clone());
            apply_entry_click(
                &mut self.selection,
                &mut self.selection_anchor,
                &ordered,
                path,
                modifiers,
            );
            cx.notify();
        } else {
            self.focused_item = NewTabItemKey::Document(path.clone());
            apply_entry_click(
                &mut self.selection,
                &mut self.selection_anchor,
                &ordered,
                path.clone(),
                modifiers,
            );
            if self.host == LibraryBrowserHost::Home {
                self.inspector_open = true;
            }
            if click_count >= 2 {
                self.open_document(path, cx);
            }
            cx.notify();
        }
    }

    pub(super) fn tree_node_index(&self, node: LibraryNodeId) -> Option<usize> {
        self.tree
            .rows
            .iter()
            .position(|row| row.node_id(&self.entries) == Some(node))
    }

    pub(super) fn handle_tree_pointer(
        &mut self,
        node: LibraryNodeId,
        modifiers: Modifiers,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        if self.host == LibraryBrowserHost::QuickOpen
            && !modifiers.shift
            && !modifiers.secondary()
            && !self.selecting
            && let LibraryNodeId::Item(id) = node
            && let Some(path) = self
                .entry_for_library_item(id)
                .map(|entry| entry.path.clone())
        {
            self.open_document(path, cx);
            return;
        }

        let selection_click = modifiers.shift || modifiers.secondary() || self.selecting;
        if selection_click {
            self.selecting = true;
            let ordered = self
                .tree
                .rows
                .iter()
                .filter_map(|row| row.node_id(&self.entries))
                .collect::<Vec<_>>();
            if self.tree.anchor.is_none() {
                self.tree.anchor = self.tree.active;
            }
            apply_entry_click(
                &mut self.tree.marked,
                &mut self.tree.anchor,
                &ordered,
                node,
                modifiers,
            );
            self.tree.active = Some(node);
        } else {
            self.selecting = false;
            self.tree.active = Some(node);
            self.tree.anchor = Some(node);
            self.tree.marked.clear();
        }

        self.focused_item = self.new_tab_key_for_library_node(node);
        match node {
            LibraryNodeId::Collection(id) => {
                self.inspector_open = false;
                if !selection_click {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
            }
            LibraryNodeId::Item(id) => {
                if let Some(entry) = self.entry_for_library_item(id) {
                    let path = entry.path.clone();
                    self.selection.clear();
                    self.selection.insert(path.clone());
                    self.selection_anchor = Some(path.clone());
                    if self.host == LibraryBrowserHost::Home {
                        self.inspector_open = true;
                    }
                    if click_count >= 2 && !selection_click {
                        self.open_document(path, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    pub(super) fn dragged_library_selection(
        &self,
        active: LibraryNodeId,
        label: SharedString,
    ) -> DraggedLibrarySelection {
        DraggedLibrarySelection {
            active,
            marked: self.tree.marked.iter().copied().collect::<Vec<_>>().into(),
            label,
        }
    }

    pub(super) fn target_index(&self, movement: CursorMove, mode: NewTabViewMode) -> usize {
        let current = self.focused_index();
        let last = self.item_count().saturating_sub(1);
        match movement {
            CursorMove::First => 0,
            CursorMove::Last => last,
            CursorMove::Left | CursorMove::Up if mode != NewTabViewMode::Grid => {
                current.saturating_sub(1)
            }
            CursorMove::Right | CursorMove::Down if mode != NewTabViewMode::Grid => {
                current.saturating_add(1).min(last)
            }
            CursorMove::Left => current.saturating_sub(1),
            CursorMove::Right => current.saturating_add(1).min(last),
            CursorMove::Up => current.saturating_sub(self.cols.max(1)),
            CursorMove::Down => current.saturating_add(self.cols.max(1)).min(last),
        }
    }

    pub(super) fn is_library_tree(&self) -> bool {
        self.session.destination == ReaderNewTabDestination::Library
            && self.session.view_mode == LibraryViewMode::Table
    }

    pub(super) fn effective_tree_selection(&self) -> Vec<LibraryNodeId> {
        let Some(active) = self.tree.active else {
            return Vec::new();
        };
        if self.tree.marked.contains(&active) {
            self.tree.marked.iter().copied().collect()
        } else {
            vec![active]
        }
    }

    pub(super) fn move_tree_cursor(
        &mut self,
        movement: CursorMove,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if self.tree.rows.is_empty() {
            return;
        }
        if self.tree.active.is_none()
            && matches!(
                movement,
                CursorMove::Up | CursorMove::Down | CursorMove::Left | CursorMove::Right
            )
        {
            let Some(node) = self.tree.rows[0].node_id(&self.entries) else {
                return;
            };
            self.tree.active = Some(node);
            self.tree.anchor = Some(node);
            self.focused_item = self.new_tab_key_for_library_node(node);
            if extend {
                self.selecting = true;
                self.tree.marked.insert(node);
            }
            self.scroll.scroll_to_item(0, ScrollStrategy::Center);
            cx.notify();
            return;
        }
        let current = self
            .tree
            .active
            .and_then(|node| self.tree_node_index(node))
            .unwrap_or(0);
        let current_row = self.tree.rows[current].clone();
        if let LibraryTreeRow::Collection { id, .. } = current_row {
            match movement {
                CursorMove::Left if self.tree.expanded_collections.contains(&id) => {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
                CursorMove::Right if !self.tree.expanded_collections.contains(&id) => {
                    self.toggle_collection_expanded(id, cx);
                    return;
                }
                _ => {}
            }
        }

        let last = self.tree.rows.len().saturating_sub(1);
        let target = match movement {
            CursorMove::First => 0,
            CursorMove::Last => last,
            CursorMove::Up => current.saturating_sub(1),
            CursorMove::Down => current.saturating_add(1).min(last),
            CursorMove::Left => {
                let depth = current_row.depth();
                if depth == 0 {
                    current
                } else {
                    (0..current)
                        .rev()
                        .find(|index| self.tree.rows[*index].depth() + 1 == depth)
                        .unwrap_or(current)
                }
            }
            CursorMove::Right => {
                let depth = current_row.depth();
                if current < last && self.tree.rows[current + 1].depth() > depth {
                    current + 1
                } else {
                    current
                }
            }
        };
        let Some(node) = self.tree.rows[target].node_id(&self.entries) else {
            return;
        };
        if extend {
            self.selecting = true;
            let anchor = self.tree.anchor.or(self.tree.active).unwrap_or(node);
            self.tree.marked.clear();
            if let Some(anchor_ix) = self.tree_node_index(anchor) {
                for row in &self.tree.rows[anchor_ix.min(target)..=anchor_ix.max(target)] {
                    if let Some(node) = row.node_id(&self.entries) {
                        self.tree.marked.insert(node);
                    }
                }
            }
        } else {
            self.tree.anchor = Some(node);
        }
        self.tree.active = Some(node);
        self.focused_item = self.new_tab_key_for_library_node(node);
        self.scroll.scroll_to_item(target, ScrollStrategy::Center);
        cx.notify();
    }

    pub(super) fn move_cursor(
        &mut self,
        movement: CursorMove,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if self.is_library_tree() {
            self.move_tree_cursor(movement, extend, cx);
            return;
        }
        let mode = self.view_mode(cx);
        if self.item_index(&self.focused_item).is_none()
            && matches!(
                movement,
                CursorMove::Up | CursorMove::Down | CursorMove::Left | CursorMove::Right
            )
        {
            if self.item_count() == 0 {
                return;
            }
            if extend {
                let Some(path) = self.entry_for_item(0).map(|entry| entry.path.clone()) else {
                    return;
                };
                self.selection_anchor = Some(path.clone());
                self.selection.clear();
                self.selection.insert(path);
                self.selecting = true;
            }
            self.set_focused_index(0);
            self.scroll_focused(mode);
            cx.notify();
            return;
        }
        let target = self.target_index(movement, mode);
        if extend {
            if self.selection_anchor.is_none() {
                self.selection_anchor = self
                    .entry_for_item(self.focused_index())
                    .map(|entry| entry.path.clone())
                    .or_else(|| self.entry_for_item(target).map(|entry| entry.path.clone()));
            }
            self.extend_selection_to(target, cx);
        } else {
            self.set_focused_index(target);
            cx.notify();
        }
        self.scroll_focused(mode);
    }

    pub(super) fn move_linear_cursor(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            self.move_tree_cursor(
                if forward {
                    CursorMove::Down
                } else {
                    CursorMove::Up
                },
                false,
                cx,
            );
            return;
        }
        if self.item_count() == 0 {
            return;
        }

        let target = match self.item_index(&self.focused_item) {
            Some(current) if forward => current.saturating_add(1).min(self.item_count() - 1),
            Some(current) => current.saturating_sub(1),
            None => 0,
        };
        self.set_focused_index(target);
        self.scroll_focused(self.view_mode(cx));
        cx.notify();
    }

    pub(super) fn activate_focused(&mut self, cx: &mut Context<Self>) {
        match self.focused_item.clone() {
            NewTabItemKey::OpenFile if self.host == LibraryBrowserHost::QuickOpen => {}
            NewTabItemKey::OpenFile => self.open_file(cx),
            NewTabItemKey::Document(path) => self.open_document(path, cx),
            NewTabItemKey::Collection(id) => self.toggle_collection_expanded(id, cx),
        }
    }

    pub(super) fn toggle_focused(&mut self, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            let Some(node) = self.tree.active else {
                return;
            };
            self.selecting = true;
            if !self.tree.marked.remove(&node) {
                self.tree.marked.insert(node);
            }
            self.tree.anchor = Some(node);
            cx.notify();
            return;
        }
        let path = match self.focused_item.clone() {
            NewTabItemKey::Document(path) => path,
            NewTabItemKey::Collection(id) => {
                let node = LibraryNodeId::Collection(id);
                self.tree.active = Some(node);
                if !self.tree.marked.remove(&node) {
                    self.tree.marked.insert(node);
                }
                cx.notify();
                return;
            }
            NewTabItemKey::OpenFile => {
                let Some(first) = self.entry_for_item(0) else {
                    self.enter_select(cx);
                    return;
                };
                first.path.clone()
            }
        };
        self.toggle_selection(path, cx);
    }

    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.is_library_tree() {
            self.selecting = true;
            self.tree.marked = self
                .tree
                .rows
                .iter()
                .filter_map(|row| row.node_id(&self.entries))
                .collect();
            if self.tree.active.is_none() {
                self.tree.active = self.tree.marked.iter().next().copied();
            }
            cx.notify();
            return;
        }
        if self.filtered_entries.is_empty() {
            return;
        }
        self.selecting = true;
        self.selection = self
            .filtered_entries
            .iter()
            .map(|entry_ix| self.entries[*entry_ix].path.clone())
            .collect();
        if self.selection_anchor.is_none() {
            self.selection_anchor = self.entry_for_item(0).map(|entry| entry.path.clone());
        }
        if self.focused_item == NewTabItemKey::OpenFile
            && let Some(first) = self.entry_for_item(0)
        {
            self.focused_item = NewTabItemKey::Document(first.path.clone());
        }
        cx.notify();
    }
}
