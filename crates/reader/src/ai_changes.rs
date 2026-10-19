//! What agents changed: each tool call's library edits and each external
//! agent turn's files, kept so the user can review them and undo them one
//! by one or all together (like Zed's agent review: changes apply at once
//! and stay undoable).

use glyphst_agent_host::scratch::{FileChanges, Scratch};
use glyphst_ai::tools::{ToolContext, ToolOrigin};
use glyphst_library::{LibraryEdit, LibraryStore, is_edit_conflict};

use crate::{theme::Tokens, vault_blocks::VaultBlockEdit};

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
    pub target: ChangeTarget,
    pub state: ChangeState,
}

/// What a change changed.
#[derive(Clone, Debug)]
pub enum ChangeTarget {
    Library(LibraryEdit),
    /// Files an external agent wrote in a document's working folder.
    Files {
        scratch: Scratch,
        changes: FileChanges,
    },
    /// A block an agent wrote in a vault note.
    VaultBlock(VaultBlockEdit),
}

impl Change {
    pub fn label(&self) -> String {
        let what = match &self.target {
            ChangeTarget::Library(edit) => edit.describe(),
            ChangeTarget::Files { changes, .. } => match changes.files.as_slice() {
                [file] => format!("Changed {file}"),
                files => format!("Changed {} files: {}", files.len(), files.join(", ")),
            },
            ChangeTarget::VaultBlock(edit) => edit.describe(),
        };
        match &self.origin {
            ToolOrigin::Builtin => what,
            ToolOrigin::External(agent) => format!("{agent}: {what}"),
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
            self.push(context, ChangeTarget::Library(edit));
        }
    }

    /// Records the blocks a call wrote in vault notes.
    pub fn record_vault(&mut self, context: &ToolContext, edits: Vec<VaultBlockEdit>) {
        for edit in edits {
            self.push(context, ChangeTarget::VaultBlock(edit));
        }
    }

    /// Records an external agent turn's files.
    pub fn record_files(&mut self, context: &ToolContext, scratch: Scratch, changes: FileChanges) {
        if !changes.files.is_empty() {
            self.push(context, ChangeTarget::Files { scratch, changes });
        }
    }

    fn push(&mut self, context: &ToolContext, target: ChangeTarget) {
        self.next_id += 1;
        self.changes.push(Change {
            id: self.next_id,
            origin: context.origin.clone(),
            thread: context.thread,
            turn: context.turn,
            target,
            state: ChangeState::Applied,
        });
    }

    /// How many applied changes match `filter`.
    pub fn count_where(&self, filter: impl Fn(&Change) -> bool) -> usize {
        self.changes
            .iter()
            .filter(|change| change.state == ChangeState::Applied && filter(change))
            .count()
    }

    /// Whether there are changes to review.
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
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
        let result = match (&change.target, state) {
            (ChangeTarget::Library(edit), ChangeState::Reverted) => library.revert_edit(edit),
            (ChangeTarget::Library(edit), ChangeState::Applied) => library.reapply_edit(edit),
            (ChangeTarget::Files { scratch, changes }, state) => {
                let (expected, target) = match state {
                    ChangeState::Reverted => (&changes.after, &changes.before),
                    ChangeState::Applied => (&changes.before, &changes.after),
                };
                match scratch.matches(expected) {
                    Ok(true) => scratch.restore(target),
                    Ok(false) => {
                        return Err(format!(
                            "“{}”: the files changed since; they were left as they are",
                            change.label()
                        ));
                    }
                    Err(error) => Err(error),
                }
            }
            (ChangeTarget::VaultBlock(edit), state) => {
                let (expected, target) = match state {
                    ChangeState::Reverted => (&edit.after, &edit.before),
                    ChangeState::Applied => (&edit.before, &edit.after),
                };
                // Refused when the block changed since: say so as for the
                // others.
                return match crate::vault_blocks::write_block(
                    library,
                    edit.item,
                    &edit.path,
                    &edit.name,
                    expected.as_deref(),
                    target.as_deref(),
                ) {
                    Ok(()) => {
                        change.state = state;
                        Ok(())
                    }
                    Err(error) => Err(format!("“{}”: {error}", change.label())),
                };
            }
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
    /// The agent's changes (frame 05): `agent changes ── 4  [ undo all ]
    /// [ keep ]` over one line per change, each with go-to and undo or
    /// redo.
    pub fn ui(&mut self, ui: &mut egui::Ui, tokens: &Tokens) -> Option<ChangeAction> {
        use glyphst_ui::{Tip, button::Button};
        if self.changes.is_empty() {
            return None;
        }
        let mut action = None;
        let applied = self.applied();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(
                egui::RichText::new("agent changes")
                    .font(crate::theme::mono(11.0))
                    .color(tokens.text_2),
            );
            ui.label(
                egui::RichText::new(format!("── {}", self.changes.len()))
                    .font(crate::theme::mono(11.0))
                    .color(tokens.faint),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(Button::bracket("keep")).clicked() {
                    action = Some(ChangeAction::Keep);
                }
                if ui
                    .add_enabled(applied > 0, Button::bracket_muted("undo all"))
                    .clicked()
                {
                    action = Some(ChangeAction::UndoAll);
                }
            });
        });
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .id_salt("agent-changes")
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for change in self.changes.iter().rev() {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 22.0),
                        egui::Sense::click(),
                    );
                    if response.hovered() {
                        ui.painter().rect_filled(rect, 0, tokens.hover);
                    }
                    let reverted = change.state == ChangeState::Reverted;
                    let y = rect.center().y;
                    ui.painter().text(
                        egui::pos2(rect.left() + 2.0, y),
                        egui::Align2::LEFT_CENTER,
                        if reverted { "−" } else { "+" },
                        crate::theme::mono(11.0),
                        if reverted { tokens.faint } else { tokens.ok },
                    );
                    let undo = egui::Rect::from_center_size(
                        egui::pos2(rect.right() - 10.0, y),
                        egui::vec2(18.0, 18.0),
                    );
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        change.label().to_lowercase(),
                        crate::theme::mono(11.0),
                        if reverted {
                            tokens.faint
                        } else {
                            tokens.text_2
                        },
                    );
                    if reverted && let Some(section) = job.sections.first_mut() {
                        section.format.strikethrough = egui::Stroke::new(1.0, tokens.faint);
                    }
                    job.wrap = egui::text::TextWrapping::truncate_at_width(
                        undo.left() - rect.left() - 20.0,
                    );
                    let galley = ui.painter().layout_job(job);
                    ui.painter().galley(
                        egui::pos2(rect.left() + 16.0, y - galley.size().y / 2.0),
                        galley,
                        tokens.text_2,
                    );
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(undo));
                    let (icon, tip) = if reverted {
                        ("redo-2", "redo")
                    } else {
                        ("undo-2", "undo")
                    };
                    if crate::icons::button(&mut child, icon, 18.0, false, tip).clicked() {
                        action = Some(if reverted {
                            ChangeAction::Redo(change.id)
                        } else {
                            ChangeAction::Undo(change.id)
                        });
                    } else if response.tip("show it").clicked() {
                        action = Some(ChangeAction::GoTo(change.id));
                    }
                }
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
