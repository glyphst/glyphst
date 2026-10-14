//! What agents changed in the library: each tool call's edits, kept so the
//! user can review them and undo them one by one or all together (like
//! Zed's agent review: changes apply at once and stay undoable).

use glyphst_ai::tools::{ToolContext, ToolOrigin};
use glyphst_library::{LibraryEdit, LibraryStore, is_edit_conflict};

use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeState {
    Applied,
    Reverted,
}

/// One edit an agent made.
#[derive(Clone, Debug)]
pub struct Change {
    pub id: u64,
    pub origin: ToolOrigin,
    /// The conversation it was made in.
    pub thread: Option<u64>,
    /// The user message it answered, by index in the conversation.
    pub turn: usize,
    pub edit: LibraryEdit,
    pub state: ChangeState,
}

impl Change {
    pub fn label(&self) -> String {
        match &self.origin {
            ToolOrigin::Builtin => self.edit.describe(),
            ToolOrigin::External(agent) => format!("{agent}: {}", self.edit.describe()),
        }
    }
}

/// What the review bar asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeAction {
    Undo(u64),
    Redo(u64),
    GoTo(u64),
    UndoAll,
    /// Accept the changes: they leave the review.
    Keep,
}

#[derive(Default)]
pub struct ChangeLog {
    changes: Vec<Change>,
    next_id: u64,
    show: bool,
}

impl ChangeLog {
    pub fn record(&mut self, context: &ToolContext, edits: Vec<LibraryEdit>) {
        for edit in edits {
            self.next_id += 1;
            self.changes.push(Change {
                id: self.next_id,
                origin: context.origin.clone(),
                thread: context.thread,
                turn: context.turn,
                edit,
                state: ChangeState::Applied,
            });
        }
    }

    #[cfg(test)]
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn get(&self, id: u64) -> Option<&Change> {
        self.changes.iter().find(|change| change.id == id)
    }

    fn applied(&self) -> usize {
        self.changes
            .iter()
            .filter(|change| change.state == ChangeState::Applied)
            .count()
    }

    /// Reverts one change; an error says why it could not be.
    pub fn undo(&mut self, id: u64, library: &mut LibraryStore) -> Result<(), String> {
        self.set(id, ChangeState::Reverted, library)
    }

    /// Makes a reverted change again.
    pub fn redo(&mut self, id: u64, library: &mut LibraryStore) -> Result<(), String> {
        self.set(id, ChangeState::Applied, library)
    }

    fn set(
        &mut self,
        id: u64,
        state: ChangeState,
        library: &mut LibraryStore,
    ) -> Result<(), String> {
        let change = self
            .changes
            .iter_mut()
            .find(|change| change.id == id)
            .ok_or("the change is gone")?;
        if change.state == state {
            return Ok(());
        }
        let result = match state {
            ChangeState::Reverted => library.revert_edit(&change.edit),
            ChangeState::Applied => library.reapply_edit(&change.edit),
        };
        match result {
            Ok(()) => {
                change.state = state;
                Ok(())
            }
            Err(error) if is_edit_conflict(&error) => Err(format!(
                "“{}” changed since; it was left as it is",
                change.label()
            )),
            Err(error) => Err(format!("{error:#}")),
        }
    }

    /// Reverts every applied change matching `filter`, newest first; the
    /// ones that changed since are left and reported.
    pub fn undo_where(
        &mut self,
        library: &mut LibraryStore,
        filter: impl Fn(&Change) -> bool,
    ) -> Vec<String> {
        let ids = self
            .changes
            .iter()
            .rev()
            .filter(|change| change.state == ChangeState::Applied && filter(change))
            .map(|change| change.id)
            .collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| self.undo(id, library).err())
            .collect()
    }

    /// Accepts the changes: they leave the review.
    pub fn keep(&mut self) {
        self.changes.clear();
        self.show = false;
    }

    /// The bar above the assistant's composer: how many changes agents
    /// made, with review, keep and undo all.
    pub fn ui(&mut self, ui: &mut egui::Ui, tokens: &Tokens) -> Option<ChangeAction> {
        if self.changes.is_empty() {
            return None;
        }
        let mut action = None;
        egui::Frame::new()
            .fill(tokens.chrome)
            .stroke(egui::Stroke::new(1.0, tokens.border))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let applied = self.applied();
                    let text = match applied {
                        1 => "1 change by the agent".to_string(),
                        count => format!("{count} changes by the agent"),
                    };
                    ui.label(egui::RichText::new(text).small().color(tokens.text_muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(applied > 0, egui::Button::new("Undo all").small())
                            .clicked()
                        {
                            action = Some(ChangeAction::UndoAll);
                        }
                        if ui.small_button("Keep").clicked() {
                            action = Some(ChangeAction::Keep);
                        }
                        let review = if self.show { "Hide" } else { "Review" };
                        if ui.small_button(review).clicked() {
                            self.show = !self.show;
                        }
                    });
                });
                if !self.show {
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .id_salt("agent-changes")
                    .show(ui, |ui| {
                        for change in self.changes.iter().rev() {
                            ui.horizontal(|ui| {
                                let reverted = change.state == ChangeState::Reverted;
                                let mut label = egui::RichText::new(change.label()).small();
                                label = if reverted {
                                    label.strikethrough().color(tokens.text_faint)
                                } else {
                                    label.color(tokens.text)
                                };
                                ui.label(label);
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if reverted {
                                            if ui.small_button("Redo").clicked() {
                                                action = Some(ChangeAction::Redo(change.id));
                                            }
                                        } else if ui.small_button("Undo").clicked() {
                                            action = Some(ChangeAction::Undo(change.id));
                                        }
                                        if change.edit.path().is_some()
                                            && ui.small_button("Show").clicked()
                                        {
                                            action = Some(ChangeAction::GoTo(change.id));
                                        }
                                    },
                                );
                            });
                        }
                    });
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use glyphst_library::{HighlightColor, HighlightRect};

    use super::*;

    #[test]
    fn changes_undo_redo_and_undo_all_skips_conflicts() {
        let directory =
            std::env::temp_dir().join(format!("glyphst-reader-changes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let mut library = LibraryStore::open_for_test(directory.join("library.redb")).unwrap();
        let document = Path::new("/tmp/changes.pdf");
        let rect = HighlightRect {
            page_idx: 0,
            x0: 0.0,
            y0: 0.0,
            x1: 1.0,
            y1: 1.0,
        };
        let mut log = ChangeLog::default();
        let mut context = ToolContext {
            thread: Some(7),
            ..ToolContext::default()
        };
        let mut ids = Vec::new();
        for quote in ["one", "two", "three"] {
            context.turn += 2;
            let (highlight, edits) = library.record_edits(|library| {
                library
                    .add_highlight(document, HighlightColor::Yellow, vec![rect], quote, "")
                    .unwrap()
            });
            ids.push(highlight.id);
            log.record(&context, edits);
        }
        assert_eq!(log.applied(), 3);

        let first = log.changes()[0].id;
        log.undo(first, &mut library).unwrap();
        assert_eq!(library.highlights_for(document).len(), 2);
        log.redo(first, &mut library).unwrap();
        assert_eq!(library.highlights_for(document).len(), 3);

        // Restoring to the second question undoes what came after it.
        let failures = log.undo_where(&mut library, |change| {
            change.thread == Some(7) && change.turn >= 6
        });
        assert!(failures.is_empty());
        assert_eq!(library.highlights_for(document).len(), 2);
        let last = log.changes()[2].id;
        log.redo(last, &mut library).unwrap();

        // The user recolors "two" before undoing everything.
        library
            .set_highlight_color(ids[1], HighlightColor::Blue)
            .unwrap();
        let failures = log.undo_where(&mut library, |_| true);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("“two”"));
        let left = library.highlights_for(document);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].text, "two");
        assert_eq!(log.applied(), 1);

        log.keep();
        assert!(log.changes().is_empty());
        drop(library);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
