//! Right-click in the library (frame 04u): a row's menu, a tag's on an
//! item, a collection's and a tag's in the tag cloud, and the one-field
//! prompts some of their entries open at the click (a tag to add, a new
//! name, a new collection).

use glyphst_library::{AssignmentTarget, LibraryCollectionId, LibraryItem, LibraryTagId, TagOwner};
use glyphst_ui::menu as ui_menu;

use super::*;

/// What a menu entry asks for.
pub(super) enum Choice {
    Action(LibraryTabAction),
    Prompt(PromptKind),
    Show(View),
}

/// What a prompt is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptKind {
    AddTag(DocumentId),
    RenameTag(LibraryTagId),
    NewCollection {
        parent: Option<LibraryCollectionId>,
        /// The item to file in it once made.
        item: Option<DocumentId>,
    },
    RenameCollection(LibraryCollectionId),
}

/// A prompt open at the pointer.
pub(super) struct Prompt {
    pub(super) kind: PromptKind,
    pub(super) text: String,
    pub(super) at: egui::Pos2,
    focused: bool,
}

impl Prompt {
    pub(super) fn new(kind: PromptKind, library: &LibraryStore, at: egui::Pos2) -> Self {
        let text = match kind {
            PromptKind::RenameTag(tag) => library
                .library_tags()
                .iter()
                .find(|known| known.id == tag)
                .map(|tag| tag.name.clone())
                .unwrap_or_default(),
            PromptKind::RenameCollection(id) => library
                .library_collection(id)
                .map(|collection| collection.name.clone())
                .unwrap_or_default(),
            PromptKind::AddTag(_) | PromptKind::NewCollection { .. } => String::new(),
        };
        Self {
            kind,
            text,
            at,
            focused: false,
        }
    }
}

/// Draws `prompt`; returns what it gives, and whether it closed.
pub(super) fn show_prompt(
    ctx: &egui::Context,
    tokens: &Tokens,
    library: &LibraryStore,
    prompt: &mut Prompt,
) -> (Option<LibraryTabAction>, bool) {
    let (icon, hint) = match prompt.kind {
        PromptKind::AddTag(_) => ("tag", "a tag"),
        PromptKind::RenameTag(_) => ("pencil", "the tag's new name"),
        PromptKind::NewCollection { .. } => ("folder-plus", "the new collection"),
        PromptKind::RenameCollection(_) => ("pencil", "the collection's new name"),
    };
    let mut result = (None, false);
    let screen = ctx.content_rect();
    let at = pos2(
        prompt.at.x.min(screen.right() - 280.0),
        prompt.at.y.min(screen.bottom() - 180.0),
    );
    let area = egui::Area::new(egui::Id::unique("library-prompt"))
        .order(egui::Order::Foreground)
        .fixed_pos(at)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(tokens.raised)
                .stroke(Stroke::new(1.0, tokens.accent))
                .corner_radius(tokens.pop_radius())
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(244.0);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let field = TextField::new(&mut prompt.text, hint)
                        .icon(icon)
                        .id(egui::Id::unique("library-prompt-field"))
                        .width(244.0)
                        .show(ui);
                    if !prompt.focused {
                        field.request_focus();
                        prompt.focused = true;
                    }
                    let entered =
                        field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let text = prompt.text.trim().to_string();
                    // Tags to add: those that match, then a new one.
                    if let PromptKind::AddTag(item) = prompt.kind {
                        let on_item = library
                            .tags_for_item(item)
                            .iter()
                            .map(|tag| tag.id)
                            .collect::<Vec<_>>();
                        let query = text.to_lowercase();
                        let matches = library
                            .library_tags()
                            .iter()
                            .filter(|tag| !on_item.contains(&tag.id))
                            .filter(|tag| query.is_empty() || tag.normalized_name.contains(&query))
                            .take(5)
                            .collect::<Vec<_>>();
                        for tag in &matches {
                            if ui_menu::Item::new(&format!("#{}", tag.name))
                                .width(244.0)
                                .show(ui)
                                .clicked()
                            {
                                result =
                                    (Some(LibraryTabAction::AddTag(item, tag.name.clone())), true);
                            }
                        }
                        let exact = matches.iter().any(|tag| tag.normalized_name == query);
                        if !text.is_empty() && !exact {
                            let label = format!("new tag “{text}”");
                            if ui_menu::Item::new(&label).width(244.0).show(ui).clicked() {
                                result = (Some(LibraryTabAction::AddTag(item, text.clone())), true);
                            }
                        }
                        if entered && !text.is_empty() {
                            // An existing tag by its own spelling.
                            let name = matches
                                .iter()
                                .find(|tag| tag.normalized_name == query)
                                .map(|tag| tag.name.clone())
                                .unwrap_or_else(|| text.clone());
                            result = (Some(LibraryTabAction::AddTag(item, name)), true);
                        }
                    } else if entered && !text.is_empty() {
                        let action = match prompt.kind {
                            PromptKind::RenameTag(tag) => {
                                LibraryTabAction::RenameTag(tag, text.clone())
                            }
                            PromptKind::NewCollection { parent, item } => {
                                LibraryTabAction::NewCollection {
                                    parent,
                                    name: text.clone(),
                                    item,
                                }
                            }
                            PromptKind::RenameCollection(id) => {
                                LibraryTabAction::RenameCollection(id, text.clone())
                            }
                            PromptKind::AddTag(_) => unreachable!("handled above"),
                        };
                        result = (Some(action), true);
                    }
                    ui.label(
                        egui::RichText::new("enter · esc closes")
                            .font(theme::mono(10.0))
                            .color(tokens.faint),
                    );
                });
        });
    let escape = ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    let clicked_away = ctx.input(|input| {
        input.pointer.any_pressed()
            && input
                .pointer
                .interact_pos()
                .is_some_and(|pointer| !area.response.rect.contains(pointer))
    });
    if escape || clicked_away {
        result.1 = true;
    }
    result
}

fn entry(ui: &mut Ui, icon: &str, label: &str) -> bool {
    let clicked = ui_menu::Item::new(label)
        .icon(icon)
        .width(240.0)
        .show(ui)
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

fn danger(ui: &mut Ui, icon: &str, label: &str) -> bool {
    let clicked = ui_menu::Item::new(label)
        .icon(icon)
        .danger(true)
        .width(240.0)
        .show(ui)
        .clicked();
    if clicked {
        ui.close();
    }
    clicked
}

/// A row's menu: open it, its note and file; tag it, file it; the AI; the
/// trash. `note` is the item's note when it has one.
pub(super) fn row_menu(
    ui: &mut Ui,
    library: &LibraryStore,
    item: &LibraryItem,
    view: View,
    note: Option<PathBuf>,
) -> Option<Choice> {
    ui_menu::style(ui);
    let id = item.id;
    let path = library
        .primary_attachment(id)
        .map(|attachment| attachment.path.clone());
    let mut choice = None;
    if view == View::Trash {
        if entry(ui, "rotate-ccw", "restore") {
            choice = Some(Choice::Action(LibraryTabAction::Restore(id)));
        }
        ui_menu::separator(ui);
        if danger(ui, "trash-2", "delete forever") {
            choice = Some(Choice::Action(LibraryTabAction::DeleteForever(id)));
        }
        return choice;
    }
    if let Some(path) = &path
        && entry(ui, "external-link", "open")
    {
        choice = Some(Choice::Action(LibraryTabAction::Open(path.clone())));
    }
    if let Some(note) = note
        && entry(ui, "file-text", "open note")
    {
        choice = Some(Choice::Action(LibraryTabAction::OpenNote(note)));
    }
    if let Some(path) = &path
        && entry(ui, "folder-open", "show file")
    {
        choice = Some(Choice::Action(LibraryTabAction::ShowFile(path.clone())));
    }
    ui_menu::separator(ui);
    if entry(ui, "tag", "add tag…") {
        choice = Some(Choice::Prompt(PromptKind::AddTag(id)));
    }
    let member = library
        .collections_for_item(id)
        .iter()
        .map(|collection| collection.id)
        .collect::<Vec<_>>();
    ui.menu_button("     add to collection", |ui| {
        ui_menu::style(ui);
        let mut collections = library
            .library_collections()
            .iter()
            .map(|collection| (collection.id, library.collection_path_label(collection.id)))
            .collect::<Vec<_>>();
        collections.sort_by_key(|(_, label)| label.to_lowercase());
        for (collection, label) in collections {
            let inside = member.contains(&collection);
            if ui_menu::Item::new(&label.to_lowercase())
                .icon("folder")
                .checked(inside)
                .width(220.0)
                .show(ui)
                .clicked()
            {
                choice = Some(Choice::Action(if inside {
                    LibraryTabAction::RemoveFromCollection(id, collection)
                } else {
                    LibraryTabAction::AddToCollection(vec![id], collection)
                }));
                ui.close();
            }
        }
        ui_menu::separator(ui);
        if entry(ui, "plus", "new collection…") {
            choice = Some(Choice::Prompt(PromptKind::NewCollection {
                parent: None,
                item: Some(id),
            }));
        }
    });
    if let View::Collection(collection) = view
        && member.contains(&collection)
        && let Some(shown) = library.library_collection(collection)
        && entry(ui, "folder-minus", &format!("remove from “{}”", shown.name))
    {
        choice = Some(Choice::Action(LibraryTabAction::RemoveFromCollection(
            id, collection,
        )));
    }
    ui_menu::separator(ui);
    if entry(ui, "sparkles", "◆ run the ai again") {
        choice = Some(Choice::Action(LibraryTabAction::Enrich(id)));
    }
    ui_menu::separator(ui);
    if danger(ui, "trash-2", "move to trash") {
        choice = Some(Choice::Action(LibraryTabAction::Trash(id)));
    }
    choice
}

/// A tag's menu on an item: the AI's can be kept as the user's; either can
/// be taken off; its colour; its items.
pub(super) fn item_tag_menu(
    ui: &mut Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    id: DocumentId,
    tag: LibraryTagId,
) -> Option<Choice> {
    ui_menu::style(ui);
    let known = library
        .library_tags()
        .iter()
        .find(|known| known.id == tag)?;
    let by_ai = library.tag_owner(id, tag) == TagOwner::Ai;
    let title = if by_ai {
        format!("◆ {} · added by the ai", known.name)
    } else {
        format!("#{} · yours", known.name)
    };
    ui_menu::group(ui, &title);
    let mut choice = None;
    if by_ai && entry(ui, "check", "keep as mine") {
        choice = Some(Choice::Action(LibraryTabAction::Accept(
            id,
            AssignmentTarget::Tag(tag),
        )));
    }
    if entry(ui, "x", "remove") {
        choice = Some(Choice::Action(LibraryTabAction::RemoveTag(id, tag)));
    }
    ui_menu::separator(ui);
    if let Some(color) = colour_swatches(ui, tokens, library.tag_color(tag)) {
        choice = Some(Choice::Action(LibraryTabAction::TagColor(tag, color)));
        ui.close();
    }
    if entry(ui, "list-filter", "show its items") {
        choice = Some(Choice::Show(View::Tag(tag)));
    }
    choice
}

/// A collection's menu in the tree.
pub(super) fn collection_menu(ui: &mut Ui, collection: LibraryCollectionId) -> Option<Choice> {
    ui_menu::style(ui);
    let mut choice = None;
    if entry(ui, "folder-plus", "new inside…") {
        choice = Some(Choice::Prompt(PromptKind::NewCollection {
            parent: Some(collection),
            item: None,
        }));
    }
    if entry(ui, "pencil", "rename…") {
        choice = Some(Choice::Prompt(PromptKind::RenameCollection(collection)));
    }
    ui_menu::separator(ui);
    if danger(ui, "trash-2", "delete") {
        choice = Some(Choice::Action(LibraryTabAction::DeleteCollection(
            collection,
        )));
    }
    choice
}

/// A tag's menu in the tag cloud: its colour, a new name, gone everywhere.
pub(super) fn cloud_tag_menu(
    ui: &mut Ui,
    tokens: &Tokens,
    library: &LibraryStore,
    tag: LibraryTagId,
    count: usize,
) -> Option<Choice> {
    ui_menu::style(ui);
    let name = library
        .library_tags()
        .iter()
        .find(|known| known.id == tag)
        .map(|tag| tag.name.clone())
        .unwrap_or_default();
    ui_menu::group(ui, &format!("#{name} · {}", plural(count, "item")));
    let mut choice = None;
    if let Some(color) = colour_swatches(ui, tokens, library.tag_color(tag)) {
        choice = Some(Choice::Action(LibraryTabAction::TagColor(tag, color)));
        ui.close();
    }
    if entry(ui, "pencil", "rename…") {
        choice = Some(Choice::Prompt(PromptKind::RenameTag(tag)));
    }
    ui_menu::separator(ui);
    if danger(ui, "trash-2", "delete everywhere") {
        choice = Some(Choice::Action(LibraryTabAction::DeleteTag(tag)));
    }
    choice
}
