//! The Keyboard shortcuts tab, after the GPUI Reader's keymap editor (and
//! Zed's and VS Code's): every command in a table with its keys, which
//! category it is in and whether its keys are the preset's or the user's.
//! Keys are recorded in place; a pause ends a sequence.

use egui::{Key, RichText, Sense, vec2};

use crate::{
    commands::{Category, Command},
    icons,
    keys::{self, Keymap, KeymapPreset, Keys, Overrides, Shortcut},
    prefs::Prefs,
    theme::{self, Tokens},
    widgets,
};

/// Keys recorded for `command` that other bindings have, start, or are
/// the start of.
#[derive(Clone, Debug, PartialEq)]
struct Conflict {
    command: Command,
    keys: Keys,
    others: Vec<(Keys, Command)>,
}

/// Keys being recorded for `command`: each press adds one, and a pause
/// ends the sequence.
#[derive(Clone, Debug, PartialEq)]
struct Recording {
    command: Command,
    strokes: Vec<Shortcut>,
    /// When the last key was pressed (egui's input time).
    last: f64,
}

impl Recording {
    fn new(command: Command) -> Self {
        Self {
            command,
            strokes: Vec::new(),
            last: 0.0,
        }
    }
}

/// Seconds without a key press that end a recorded sequence.
const RECORD_PAUSE: f64 = 1.0;

/// The column the table is ordered by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// By category, in the registry's order within each.
    #[default]
    Category,
    /// By the command's name.
    Command,
}

const ROW_HEIGHT: f32 = 36.0;
const HEADER_HEIGHT: f32 = 32.0;
const CATEGORY_WIDTH: f32 = 120.0;
const SOURCE_WIDTH: f32 = 84.0;
const ACTIONS_WIDTH: f32 = 36.0;
const MAX_WIDTH: f32 = 1080.0;
/// Room below the table: its card's margin and stroke, and a gap.
const BOTTOM_GAP: f32 = 34.0;

#[derive(Default)]
pub struct KeymapPage {
    filter: String,
    /// The keys being recorded for a command.
    recording: Option<Recording>,
    conflict: Option<Conflict>,
    sort: Sort,
}

impl KeymapPage {
    /// Whether key presses are going to a recording (or a conflict waits
    /// for an answer), so shortcuts must not run.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some() || self.conflict.is_some()
    }

    /// The commands the table lists: those whose name or keys match the
    /// filter, in the chosen order.
    fn rows(&self, keymap: &Keymap) -> Vec<Command> {
        let filter = self.filter.trim().to_lowercase();
        let mut rows: Vec<Command> = Command::ALL
            .into_iter()
            .filter(|command| {
                filter.is_empty()
                    || command.label().to_lowercase().contains(&filter)
                    || command.category().label().to_lowercase().contains(&filter)
                    || keymap
                        .keys(*command)
                        .iter()
                        .any(|key| key.label().to_lowercase().contains(&filter))
            })
            .collect();
        match self.sort {
            Sort::Category => rows.sort_by_key(|command| {
                let category = Category::ALL
                    .iter()
                    .position(|category| *category == command.category());
                (category, *command)
            }),
            Sort::Command => rows.sort_by_key(|command| command.label().to_lowercase()),
        }
        rows
    }

    /// Draws the page; returns whether the shortcuts (or the preset)
    /// changed.
    pub fn show(&mut self, ui: &mut egui::Ui, prefs: &mut Prefs, keymap: &Keymap) -> bool {
        let tokens = Tokens::get(ui.ctx());
        let mut changed = self.take_recording(ui.ctx(), prefs, keymap);
        let area = ui.available_rect_before_wrap();
        let width = (area.width() - 48.0).clamp(320.0, MAX_WIDTH);
        let page = egui::Rect::from_min_size(
            egui::pos2(area.center().x - width / 2.0, area.top() + 24.0),
            vec2(width, (area.height() - 24.0).max(0.0)),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(page), |ui| {
            widgets::form_style(ui);
            ui.horizontal(|ui| {
                ui.add(icons::image("keyboard", 22.0, tokens.icon));
                ui.label(RichText::new("Keyboard shortcuts").font(theme::semibold(20.0)));
            });
            ui.label(
                RichText::new(
                    "Click + to record keys (a pause ends a sequence), \
                     a key to remove it, or double-click a row.",
                )
                .color(tokens.text_muted),
            );
            ui.add_space(12.0);
            changed |= self.toolbar(ui, prefs, keymap, &tokens);
            ui.add_space(4.0);
            ui.label(
                RichText::new(match prefs.keymap {
                    KeymapPreset::Standard => "Ctrl shortcuts, arrows and Page Up/Down.",
                    KeymapPreset::Vim => {
                        "hjkl, gg and G (42G goes to page 42), Ctrl+D/U, / n N, \
                         Ctrl+O/I, : for the palette, Ctrl+W v/s/w/q for panes. \
                         A number typed first repeats a move."
                    }
                })
                .small()
                .color(tokens.text_muted),
            );
            ui.add_space(8.0);
            if let Some(conflict) = self.conflict.clone() {
                changed |= self.conflict_banner(ui, prefs, keymap, &conflict, &tokens);
                ui.add_space(8.0);
            }
            changed |= self.table(ui, prefs, keymap, &tokens);
        });
        changed
    }

    /// Takes keys recorded once the user pauses; a free shortcut is
    /// bound, a taken one waits on the conflict banner.
    fn take_recording(&mut self, ctx: &egui::Context, prefs: &mut Prefs, keymap: &Keymap) -> bool {
        let Some((command, recorded)) = take_recorded(ctx, &mut self.recording) else {
            return false;
        };
        let others: Vec<(Keys, Command)> = keymap
            .conflicts(&recorded)
            .into_iter()
            .filter(|(_, owner)| *owner != command)
            .collect();
        if others.is_empty() {
            keys::assign(&mut prefs.shortcuts, keymap, command, recorded);
            true
        } else {
            self.conflict = Some(Conflict {
                command,
                keys: recorded,
                others,
            });
            false
        }
    }

    fn toolbar(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.allocate_ui(vec2(300.0, 30.0), |ui| {
                widgets::filter_field(ui, &mut self.filter, "Search commands, keys or categories");
            });
            ui.add_space(8.0);
            ui.label(RichText::new("Keymap").color(tokens.text_muted));
            let options = KeymapPreset::ALL.map(|preset| (preset, preset.label()));
            changed |= widgets::segmented(ui, &mut prefs.keymap, &options);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled_ui(!prefs.shortcuts.is_empty(), |ui| {
                        widgets::ghost_button(ui, "rotate-ccw", "Reset all")
                    })
                    .inner
                    .on_hover_text("Back to the preset's keys for every command")
                    .clicked()
                {
                    prefs.shortcuts = Overrides::new();
                    changed = true;
                }
                let custom = Command::ALL
                    .iter()
                    .filter(|command| prefs.shortcuts.contains_key(command.id()))
                    .count();
                let shown = self.rows(keymap).len();
                ui.label(
                    RichText::new(format!(
                        "{shown} of {} commands · {custom} customised",
                        Command::ALL.len()
                    ))
                    .small()
                    .color(tokens.text_faint),
                );
            });
        });
        changed
    }

    fn conflict_banner(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        conflict: &Conflict,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        egui::Frame::new()
            .fill(tokens.accent_soft)
            .corner_radius(8)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(icons::image("triangle-alert", 16.0, tokens.accent_text));
                    let others: Vec<String> = conflict
                        .others
                        .iter()
                        .map(|(keys, owner)| format!("{} (“{}”)", keys.label(), owner.label()))
                        .collect();
                    ui.label(format!(
                        "{} meets {}.",
                        conflict.keys.label(),
                        others.join(", ")
                    ));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Cancel").clicked() {
                            self.resolve_conflict(prefs, keymap, false);
                        }
                        if widgets::pill_button(
                            ui,
                            None,
                            &format!("Use it for “{}”", conflict.command.label()),
                            true,
                        )
                        .clicked()
                        {
                            changed |= self.resolve_conflict(prefs, keymap, true);
                        }
                    });
                });
            });
        changed
    }

    /// Moves the conflicting shortcut to the command recording it, or
    /// leaves it; returns whether the shortcuts changed.
    fn resolve_conflict(&mut self, prefs: &mut Prefs, keymap: &Keymap, accept: bool) -> bool {
        let Some(conflict) = self.conflict.take() else {
            return false;
        };
        if accept {
            keys::assign(
                &mut prefs.shortcuts,
                keymap,
                conflict.command,
                conflict.keys,
            );
        }
        accept
    }

    /// The header and the rows, on a card.
    fn table(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        let rows = self.rows(keymap);
        // The rows scroll inside the card, which ends above the window's
        // bottom edge.
        let rows_height =
            (ui.available_height() - HEADER_HEIGHT - BOTTOM_GAP).max(ROW_HEIGHT * 3.0);
        egui::Frame::new()
            .fill(tokens.card)
            .stroke(egui::Stroke::new(1.0, tokens.border))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let columns = Columns::new(ui.available_rect_before_wrap().x_range());
                self.header(ui, &columns, tokens);
                if rows.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("No command matches the search.")
                                .color(tokens.text_muted),
                        );
                    });
                    ui.add_space(24.0);
                    return;
                }
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(rows_height)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for command in rows {
                            changed |= self.row(ui, prefs, keymap, command, tokens);
                        }
                    });
            });
        changed
    }

    fn header(&mut self, ui: &mut egui::Ui, columns: &Columns, tokens: &Tokens) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
        let font = theme::semibold(11.5);
        let mut cell = |x: egui::Rangef, label: &str, sort: Option<Sort>| {
            let cell = egui::Rect::from_x_y_ranges(x, rect.y_range());
            let response = sort.map(|sort| {
                ui.interact(cell, ui.id().with(("keymap-sort", label)), Sense::click())
                    .on_hover_text(format!("Order by {}", label.to_lowercase()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    .then_some(sort)
            });
            let active = sort.is_some() && sort == Some(self.sort);
            let color = if active {
                tokens.text
            } else {
                tokens.text_muted
            };
            let galley = ui
                .painter()
                .layout_no_wrap(label.to_uppercase(), font.clone(), color);
            let width = galley.size().x;
            let pos = egui::pos2(cell.left() + 10.0, cell.center().y - galley.size().y / 2.0);
            ui.painter().galley(pos, galley, color);
            if active {
                icons::paint(
                    ui,
                    egui::Rect::from_center_size(
                        egui::pos2(pos.x + width + 10.0, cell.center().y),
                        vec2(12.0, 12.0),
                    ),
                    "chevron-down",
                    12.0,
                    color,
                );
            }
            if let Some(Some(sort)) = response {
                self.sort = sort;
            }
        };
        cell(columns.command, "Command", Some(Sort::Command));
        cell(columns.keys, "Keys", None);
        cell(columns.category, "Category", Some(Sort::Category));
        cell(columns.source, "Source", None);
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, tokens.divider),
        );
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        command: Command,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        let recording_here = self
            .recording
            .as_ref()
            .is_some_and(|recording| recording.command == command);
        let (rect, response) = widgets::list_row(ui, ROW_HEIGHT, recording_here);
        let columns = Columns::new(rect.x_range());
        let y = rect.center().y;
        icons::paint(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(columns.command.min + 18.0, y),
                vec2(16.0, 16.0),
            ),
            command.icon(),
            15.0,
            tokens.icon,
        );
        widgets::row_text(
            ui,
            egui::pos2(columns.command.min + 34.0, y),
            command.label(),
            theme::regular(13.0),
            tokens.text,
            columns.command.span() - 40.0,
        );
        widgets::row_text(
            ui,
            egui::pos2(columns.category.min + 10.0, y),
            command.category().label(),
            theme::regular(12.5),
            tokens.text_muted,
            columns.category.span() - 14.0,
        );
        let custom = prefs.shortcuts.contains_key(command.id());
        if custom {
            let label = "Custom";
            let size = egui::vec2(
                ui.painter()
                    .layout_no_wrap(label.into(), theme::medium(11.0), tokens.accent_text)
                    .size()
                    .x
                    + 14.0,
                20.0,
            );
            let badge = egui::Rect::from_min_size(
                egui::pos2(columns.source.min + 8.0, y - size.y / 2.0),
                size,
            );
            ui.painter().rect_filled(badge, 10.0, tokens.accent_soft);
            ui.painter().text(
                badge.center(),
                egui::Align2::CENTER_CENTER,
                label,
                theme::medium(11.0),
                tokens.accent_text,
            );
        } else {
            widgets::row_text(
                ui,
                egui::pos2(columns.source.min + 10.0, y),
                "Default",
                theme::regular(12.5),
                tokens.text_faint,
                columns.source.span() - 14.0,
            );
        }
        // The keys, drawn over the row so their buttons take clicks.
        let keys_rect =
            egui::Rect::from_x_y_ranges(columns.keys, rect.y_range()).shrink2(vec2(8.0, 0.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(keys_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(keys_rect.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(recording) = self.recording.as_ref().filter(|_| recording_here) {
                    let text = match Keys::new(recording.strokes.clone()) {
                        Some(so_far) => format!("{} …", so_far.label()),
                        None => "Press keys… (Esc cancels)".to_string(),
                    };
                    ui.label(RichText::new(text).color(tokens.accent_text));
                    return;
                }
                for bound in keymap.keys(command) {
                    if ui
                        .push_id(("key", command.id(), bound.label()), |ui| {
                            shortcut_chip(ui, &bound.label(), tokens)
                        })
                        .inner
                        .on_hover_text("Remove")
                        .clicked()
                    {
                        keys::unbind(&mut prefs.shortcuts, keymap, command, bound);
                        changed = true;
                    }
                }
                if ui
                    .push_id(("add", command.id()), |ui| {
                        icons::button(ui, "plus", 24.0, false, "Add a shortcut")
                    })
                    .inner
                    .clicked()
                {
                    self.recording = Some(Recording::new(command));
                    self.conflict = None;
                }
            },
        );
        let actions = egui::Rect::from_x_y_ranges(columns.actions, rect.y_range());
        if custom
            && ui
                .scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(actions)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.push_id(("reset", command.id()), |ui| {
                            icons::button(ui, "rotate-ccw", 24.0, false, "Back to the default keys")
                        })
                        .inner
                    },
                )
                .inner
                .clicked()
        {
            keys::reset(&mut prefs.shortcuts, command);
            changed = true;
        }
        if response.double_clicked() && !recording_here {
            self.recording = Some(Recording::new(command));
            self.conflict = None;
        }
        changed
    }
}

/// The table's columns across `x`.
struct Columns {
    command: egui::Rangef,
    keys: egui::Rangef,
    category: egui::Rangef,
    source: egui::Rangef,
    actions: egui::Rangef,
}

impl Columns {
    fn new(x: egui::Rangef) -> Self {
        let flexible = (x.span() - CATEGORY_WIDTH - SOURCE_WIDTH - ACTIONS_WIDTH).max(200.0);
        let command_width = flexible * 0.48;
        let mut left = x.min;
        let mut next = |width: f32| {
            let range = egui::Rangef::new(left, left + width);
            left += width;
            range
        };
        Self {
            command: next(command_width),
            keys: next(flexible - command_width),
            category: next(CATEGORY_WIDTH),
            source: next(SOURCE_WIDTH),
            actions: next(ACTIONS_WIDTH),
        }
    }
}

/// The keys recorded once the user pauses (or presses the most a
/// sequence has), taking the presses from egui's input. Escape ends the
/// recording with nothing.
fn take_recorded(
    ctx: &egui::Context,
    recording: &mut Option<Recording>,
) -> Option<(Command, Keys)> {
    let now = ctx.input(|input| input.time);
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            let Some(active) = recording.as_mut() else {
                return true;
            };
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            if active.strokes.len() >= keys::MAX_KEYS {
                return true;
            }
            if *key == Key::Escape && modifiers.is_none() {
                *recording = None;
                return false;
            }
            match Shortcut::from_key(*key, *modifiers) {
                Some(shortcut) => {
                    active.strokes.push(shortcut);
                    active.last = now;
                    false
                }
                None => true,
            }
        });
    });
    let active = recording.as_ref()?;
    if active.strokes.is_empty() {
        return None;
    }
    let waited = now - active.last;
    if active.strokes.len() < keys::MAX_KEYS && waited < RECORD_PAUSE {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(RECORD_PAUSE - waited));
        return None;
    }
    let done = recording.take()?;
    Some((done.command, Keys::new(done.strokes)?))
}

/// A key chip with an × that removes it.
fn shortcut_chip(ui: &mut egui::Ui, label: &str, tokens: &Tokens) -> egui::Response {
    let size = widgets::kbd_size(ui, label) + egui::vec2(18.0, 4.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.hovered() {
        tokens.pressed
    } else {
        tokens.hover
    };
    ui.painter().rect(
        rect,
        6.0,
        fill,
        egui::Stroke::new(1.0, tokens.border),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        theme::medium(11.5),
        tokens.text,
    );
    icons::paint(
        ui,
        egui::Rect::from_center_size(
            rect.right_center() - egui::vec2(11.0, 0.0),
            egui::vec2(12.0, 12.0),
        ),
        "x",
        11.0,
        tokens.text_muted,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at `time` seconds, pressing `key` if there is one.
    fn frame(
        ctx: &egui::Context,
        page: &mut KeymapPage,
        prefs: &mut Prefs,
        keymap: &Keymap,
        key: Option<(Key, egui::Modifiers)>,
        time: f64,
    ) -> bool {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            time: Some(time),
            events: key
                .into_iter()
                .map(|(key, modifiers)| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                })
                .collect(),
            ..Default::default()
        };
        let mut changed = false;
        let mut output = ctx.run_ui(input, |ui| {
            changed = page.show(ui, prefs, keymap);
        });
        output.textures_delta.clear();
        changed
    }

    fn keys(text: &str) -> Keys {
        Keys::parse(text).expect("parses")
    }

    /// Records `key` for `command` and lets the pause pass; returns whether
    /// the shortcuts changed.
    fn record(
        ctx: &egui::Context,
        page: &mut KeymapPage,
        prefs: &mut Prefs,
        keymap: &Keymap,
        command: Command,
        key: Key,
        clock: &mut f64,
    ) -> bool {
        page.recording = Some(Recording::new(command));
        let none = egui::Modifiers::NONE;
        *clock += 1.0;
        let mut changed = frame(ctx, page, prefs, keymap, Some((key, none)), *clock);
        *clock += RECORD_PAUSE;
        changed |= frame(ctx, page, prefs, keymap, None, *clock);
        changed
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        ctx
    }

    #[test]
    fn a_recorded_shortcut_another_command_has_moves_only_when_confirmed() {
        let ctx = context();
        let mut prefs = Prefs::default();
        let mut page = KeymapPage::default();
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        let mut clock = 0.0;

        assert!(!record(
            &ctx,
            &mut page,
            &mut prefs,
            &keymap,
            Command::Assistant,
            Key::P,
            &mut clock
        ));
        let p = keys("P");
        assert_eq!(
            page.conflict,
            Some(Conflict {
                command: Command::Assistant,
                keys: p.clone(),
                others: vec![(p.clone(), Command::Present)],
            })
        );
        assert!(page.is_recording(), "shortcuts wait for the answer");
        assert!(!page.resolve_conflict(&mut prefs, &keymap, false));
        assert!(prefs.shortcuts.is_empty());

        record(
            &ctx,
            &mut page,
            &mut prefs,
            &keymap,
            Command::Assistant,
            Key::P,
            &mut clock,
        );
        assert!(page.resolve_conflict(&mut prefs, &keymap, true));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&p), Some(Command::Assistant));
        assert!(keymap.keys(Command::Present).is_empty());

        // A free key is taken after the pause; Escape ends a recording
        // with nothing.
        page.recording = Some(Recording::new(Command::Present));
        clock += 1.0;
        let f7 = Some((Key::F7, egui::Modifiers::NONE));
        assert!(!frame(&ctx, &mut page, &mut prefs, &keymap, f7, clock));
        assert!(page.is_recording(), "a sequence may follow");
        clock += RECORD_PAUSE;
        assert!(frame(&ctx, &mut page, &mut prefs, &keymap, None, clock));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.keys(Command::Present), &[keys("F7")]);
        page.recording = Some(Recording::new(Command::Present));
        let escape = Some((Key::Escape, egui::Modifiers::NONE));
        assert!(!frame(&ctx, &mut page, &mut prefs, &keymap, escape, clock));
        assert_eq!(page.recording, None);
        assert!(!page.is_recording());
    }

    #[test]
    fn sequences_are_recorded_and_prefixes_conflict() {
        let ctx = context();
        let mut prefs = Prefs {
            keymap: KeymapPreset::Vim,
            ..Prefs::default()
        };
        let mut page = KeymapPage::default();
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);

        page.recording = Some(Recording::new(Command::ToggleStats));
        let ctrl_w = Some((Key::W, egui::Modifiers::COMMAND));
        let x = Some((Key::X, egui::Modifiers::NONE));
        assert!(!frame(&ctx, &mut page, &mut prefs, &keymap, ctrl_w, 1.0));
        assert!(!frame(&ctx, &mut page, &mut prefs, &keymap, x, 1.5));
        assert!(frame(&ctx, &mut page, &mut prefs, &keymap, None, 3.0));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&keys("Ctrl+W X")), Some(Command::ToggleStats));

        // G starts First page's and the tabs' sequences.
        let mut clock = 3.0;
        record(
            &ctx,
            &mut page,
            &mut prefs,
            &keymap,
            Command::Zen,
            Key::G,
            &mut clock,
        );
        let conflict = page.conflict.clone().expect("G conflicts");
        assert_eq!(conflict.others.len(), 3);
        assert!(page.resolve_conflict(&mut prefs, &keymap, true));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&keys("G")), Some(Command::Zen));
        assert_eq!(keymap.keys(Command::FirstPage), &[keys("Home")]);
    }

    #[test]
    fn rows_filter_by_name_key_or_category_and_sort_by_either_column() {
        let prefs = Prefs::default();
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        let mut page = KeymapPage::default();
        assert_eq!(page.rows(&keymap).len(), Command::ALL.len());

        // By category: the categories in order, the registry's order in each.
        let rows = page.rows(&keymap);
        let positions: Vec<usize> = rows
            .iter()
            .map(|command| {
                Category::ALL
                    .iter()
                    .position(|category| *category == command.category())
                    .unwrap()
            })
            .collect();
        assert!(positions.is_sorted());
        assert_eq!(rows[0], Command::Open);

        page.sort = Sort::Command;
        let labels: Vec<String> = page
            .rows(&keymap)
            .iter()
            .map(|command| command.label().to_lowercase())
            .collect();
        assert!(labels.is_sorted());

        page.filter = "ctrl+f".into();
        assert!(page.rows(&keymap).contains(&Command::Find));
        page.filter = "keyboard".into();
        assert_eq!(page.rows(&keymap), [Command::KeyboardShortcuts]);
        page.filter = "translation".into();
        assert!(
            page.rows(&keymap)
                .iter()
                .all(|command| command.category() == Category::Translation
                    || command.label().to_lowercase().contains("translat"))
        );
        page.filter = "no such command".into();
        assert!(page.rows(&keymap).is_empty());
    }

    /// The page draws in both themes, with a recording and a conflict.
    #[test]
    fn the_page_draws_while_recording_and_with_a_conflict() {
        let ctx = context();
        for kind in [theme::ThemeKind::Light, theme::ThemeKind::Dark] {
            theme::apply(&ctx, kind, 0.8);
            let mut prefs = Prefs::default();
            let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
            let mut page = KeymapPage {
                recording: Some(Recording::new(Command::Zen)),
                ..KeymapPage::default()
            };
            for time in [0.0, 0.1] {
                frame(&ctx, &mut page, &mut prefs, &keymap, None, time);
            }
            page.recording = None;
            page.conflict = Some(Conflict {
                command: Command::Assistant,
                keys: keys("P"),
                others: vec![(keys("P"), Command::Present)],
            });
            frame(&ctx, &mut page, &mut prefs, &keymap, None, 0.2);
            assert!(page.is_recording());
        }
    }
}
