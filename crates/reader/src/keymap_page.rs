//! The Keyboard shortcuts tab, after the GPUI Reader's keymap editor (and
//! Zed's and VS Code's): every command in a table with its keys, which
//! category it is in and whether its keys are the preset's or the user's.
//! Keys are recorded in place; a pause ends a sequence. Its gamepad side
//! (frame 07 · shortcuts · gamepad) lists the pad's buttons the same way,
//! recorded from the next press.

use egui::{Key, RichText, Sense, vec2};

use glyphst_ui::Tip;

use crate::{
    commands::{Category, Command},
    icons,
    keys::{self, Keymap, KeymapPreset, Keys, Overrides, Shortcut},
    pad::{PadButton, PadEvent},
    pad_keys::{self, PadChord, PadKeymap},
    prefs::Prefs,
    theme::{self, Tokens},
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

/// Pad buttons being recorded for `command`: what is held and since when
/// (egui's input time); a release ends it.
#[derive(Clone, Debug, PartialEq)]
struct PadRecording {
    command: Command,
    held: Vec<(PadButton, f64)>,
    /// Select started a chord, so its own release records nothing.
    chorded: bool,
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
    /// The gamepad's buttons instead of the keys.
    pub gamepad: bool,
    pad_recording: Option<PadRecording>,
}

impl KeymapPage {
    /// Whether key presses are going to a recording (or a conflict waits
    /// for an answer), so shortcuts must not run.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some() || self.conflict.is_some() || self.pad_recording.is_some()
    }

    /// The commands the table lists: those whose name or keys match the
    /// filter, in the chosen order.
    fn rows(&self, keymap: &Keymap, pad: &PadKeymap) -> Vec<Command> {
        let filter = self.filter.trim().to_lowercase();
        let mut rows: Vec<Command> = Command::ALL
            .into_iter()
            .filter(|command| {
                filter.is_empty()
                    || command.label().to_lowercase().contains(&filter)
                    || command.category().label().to_lowercase().contains(&filter)
                    || if self.gamepad {
                        pad.chords(*command)
                            .iter()
                            .any(|chord| chord.notation().contains(&filter))
                    } else {
                        keymap
                            .keys(*command)
                            .iter()
                            .any(|key| key.label().to_lowercase().contains(&filter))
                    }
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
        // The pad has few buttons: the commands with some come first.
        if self.gamepad {
            rows.sort_by_key(|command| pad.chords(*command).is_empty());
        }
        rows
    }

    /// Draws the page; returns whether the shortcuts (or the preset)
    /// changed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        pad: &PadKeymap,
    ) -> bool {
        let tokens = Tokens::get(ui.ctx());
        let mut changed = self.take_recording(ui.ctx(), prefs, keymap);
        if let Some((command, chord)) = take_pad_recorded(ui.ctx(), &mut self.pad_recording) {
            pad_keys::assign(&mut prefs.pad_shortcuts, pad, command, chord);
            changed = true;
        }
        let area = ui.available_rect_before_wrap();
        let width = (area.width() - 48.0).clamp(320.0, MAX_WIDTH);
        let page = egui::Rect::from_min_size(
            egui::pos2(area.center().x - width / 2.0, area.top() + 24.0),
            vec2(width, (area.height() - 24.0).max(0.0)),
        );
        // An opaque card under the page, so a transparent window doesn't
        // show the desktop through the text; it ends 16pt under the table.
        let card = egui::Rect::from_min_max(
            page.min - vec2(16.0, 16.0),
            egui::pos2(page.right() + 16.0, area.bottom() - BOTTOM_GAP + 16.0),
        );
        ui.painter()
            .rect_filled(card, tokens.pop_radius(), tokens.surface.to_opaque());
        ui.painter().rect_stroke(
            card,
            tokens.pop_radius(),
            tokens.hairline(),
            egui::StrokeKind::Inside,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(page), |ui| {
            glyphst_ui::settings::form_style(ui);
            if self.gamepad {
                glyphst_ui::settings::heading(
                    ui,
                    "Gamepad bindings",
                    "Click + and press buttons on the controller: hold one, or select and \
                     another, to bind a hold or a chord. The left stick scrolls, the right \
                     one opens the ring, the triggers zoom and B is Escape.",
                );
            } else {
                glyphst_ui::settings::heading(
                    ui,
                    "Keyboard shortcuts",
                    "Click + to record keys (a pause ends a sequence), a key to remove it, or double-click a row.",
                );
            }
            changed |= self.toolbar(ui, prefs, keymap, pad, &tokens);
            ui.add_space(4.0);
            let note = match (self.gamepad, prefs.keymap) {
                (true, _) => "Buttons by position: south is A on an Xbox pad, ✕ on a PlayStation one.",
                (false, KeymapPreset::Standard) => "Ctrl shortcuts, arrows and Page Up/Down.",
                (false, KeymapPreset::Vim) => {
                    "hjkl, gg and G (42G goes to page 42), Ctrl+D/U, / n N, \
                     Ctrl+O/I, : for the palette, Ctrl+W v/s/w/q for panes. \
                     A number typed first repeats a move."
                }
            };
            ui.label(RichText::new(note).font(theme::mono(10.0)).color(tokens.faint));
            ui.add_space(8.0);
            if let Some(conflict) = self.conflict.clone() {
                changed |= self.conflict_banner(ui, prefs, keymap, &conflict, &tokens);
                ui.add_space(8.0);
            }
            changed |= self.table(ui, prefs, keymap, pad, &tokens);
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
            .conflicts(&recorded, command)
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
        pad: &PadKeymap,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.allocate_ui(vec2(300.0, 30.0), |ui| {
                glyphst_ui::field::TextField::search(
                    &mut self.filter,
                    "Search commands, keys or categories",
                )
                .show(ui);
            });
            ui.add_space(8.0);
            let devices = [(false, "keyboard"), (true, "gamepad")];
            if glyphst_ui::segmented::labels(ui, &mut self.gamepad, &devices) {
                self.recording = None;
                self.pad_recording = None;
                self.conflict = None;
            }
            if !self.gamepad {
                ui.add_space(8.0);
                ui.label(RichText::new("Keymap").color(tokens.muted));
                let options = KeymapPreset::ALL.map(|preset| (preset, preset.label()));
                changed |= glyphst_ui::segmented::labels(ui, &mut prefs.keymap, &options);
            }
            let gamepad = self.gamepad;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let overrides = if gamepad {
                    &mut prefs.pad_shortcuts
                } else {
                    &mut prefs.shortcuts
                };
                if ui
                    .add_enabled_ui(!overrides.is_empty(), |ui| {
                        ui.add(glyphst_ui::button::Button::bracket_muted("reset all"))
                    })
                    .inner
                    .tip("back to the default for every command")
                    .clicked()
                {
                    *overrides = Overrides::new();
                    changed = true;
                }
                let custom = Command::ALL
                    .iter()
                    .filter(|command| overrides.contains_key(command.id()))
                    .count();
                let shown = self.rows(keymap, pad).len();
                ui.label(
                    RichText::new(format!(
                        "{shown} of {} commands · {custom} changed",
                        Command::ALL.len()
                    ))
                    .font(theme::mono(10.0))
                    .color(tokens.faint),
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
            .stroke(egui::Stroke::new(1.0, Tokens::fade(tokens.accent, 0.45)))
            .corner_radius(tokens.ctl_radius())
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
                        if ui
                            .add(glyphst_ui::button::Button::ghost("cancel"))
                            .clicked()
                        {
                            self.resolve_conflict(prefs, keymap, false);
                        }
                        if ui
                            .add(glyphst_ui::button::Button::primary(&format!(
                                "use it for “{}”",
                                conflict.command.label().to_lowercase()
                            )))
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
        pad: &PadKeymap,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        let rows = self.rows(keymap, pad);
        // The rows scroll inside the card, which ends above the window's
        // bottom edge.
        let rows_height =
            (ui.available_height() - HEADER_HEIGHT - BOTTOM_GAP).max(ROW_HEIGHT * 3.0);
        egui::Frame::new()
            .stroke(tokens.hairline())
            .inner_margin(egui::Margin::symmetric(0, 0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let columns = Columns::new(ui.available_rect_before_wrap().x_range());
                self.header(ui, &columns, tokens);
                if rows.is_empty() {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("no command matches the search")
                                .font(theme::mono(11.0))
                                .color(tokens.muted),
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
                            changed |= if self.gamepad {
                                self.pad_row(ui, prefs, pad, command, tokens)
                            } else {
                                self.row(ui, prefs, keymap, command, tokens)
                            };
                        }
                    });
            });
        changed
    }

    fn header(&mut self, ui: &mut egui::Ui, columns: &Columns, tokens: &Tokens) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), HEADER_HEIGHT), Sense::hover());
        let font = theme::mono(10.0);
        let mut cell = |x: egui::Rangef, label: &str, sort: Option<Sort>| {
            let cell = egui::Rect::from_x_y_ranges(x, rect.y_range());
            let response = sort.map(|sort| {
                ui.interact(cell, ui.id().with(("keymap-sort", label)), Sense::click())
                    .tip(&format!("order by {}", label.to_lowercase()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                    .then_some(sort)
            });
            let active = sort.is_some() && sort == Some(self.sort);
            let color = if active { tokens.text_2 } else { tokens.faint };
            let galley = ui
                .painter()
                .layout_no_wrap(label.to_lowercase(), font.clone(), color);
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
        cell(
            columns.keys,
            if self.gamepad { "Buttons" } else { "Keys" },
            None,
        );
        cell(columns.category, "Category", Some(Sort::Category));
        cell(columns.source, "Source", None);
        ui.painter()
            .hline(rect.x_range(), rect.bottom(), tokens.hairline());
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
        let (rect, response) = glyphst_ui::row::list_row(ui, ROW_HEIGHT, recording_here);
        let columns = Columns::new(rect.x_range());
        let y = rect.center().y;
        icons::paint(
            ui,
            egui::Rect::from_center_size(
                egui::pos2(columns.command.min + 18.0, y),
                vec2(16.0, 16.0),
            ),
            command.icon(),
            13.0,
            tokens.faint,
        );
        glyphst_ui::text::row(
            ui,
            egui::pos2(columns.command.min + 34.0, y),
            command.label().to_lowercase(),
            theme::mono(12.0),
            if recording_here {
                tokens.accent_text
            } else {
                tokens.text_2
            },
            columns.command.span() - 40.0,
        );
        glyphst_ui::text::row(
            ui,
            egui::pos2(columns.category.min + 10.0, y),
            command.category().label().to_lowercase(),
            theme::mono(11.0),
            tokens.faint,
            columns.category.span() - 14.0,
        );
        let custom = prefs.shortcuts.contains_key(command.id());
        glyphst_ui::text::row(
            ui,
            egui::pos2(columns.source.min + 10.0, y),
            if custom { "changed" } else { "default" },
            theme::mono(11.0),
            if custom {
                tokens.accent_text
            } else {
                tokens.faint
            },
            columns.source.span() - 14.0,
        );
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
                        None => "press keys… (esc cancels)".to_string(),
                    };
                    ui.label(
                        RichText::new(text.to_lowercase())
                            .font(theme::mono(11.0))
                            .color(tokens.accent_text),
                    );
                    return;
                }
                for bound in keymap.keys(command) {
                    if ui
                        .push_id(("key", command.id(), bound.label()), |ui| {
                            shortcut_chip(ui, &bound.label(), tokens)
                        })
                        .inner
                        .tip("remove")
                        .clicked()
                    {
                        keys::unbind(&mut prefs.shortcuts, keymap, command, bound);
                        changed = true;
                    }
                }
                if ui
                    .push_id(("add", command.id()), |ui| {
                        icons::button(ui, "plus", 22.0, false, "add a shortcut")
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
                            icons::button(ui, "rotate-ccw", 22.0, false, "back to the default keys")
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

impl KeymapPage {
    /// A command's row on the gamepad side: its buttons as glyphs.
    fn pad_row(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        pad: &PadKeymap,
        command: Command,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        let recording_here = self
            .pad_recording
            .as_ref()
            .is_some_and(|recording| recording.command == command);
        let (rect, response) = glyphst_ui::row::list_row(ui, ROW_HEIGHT, recording_here);
        let columns = Columns::new(rect.x_range());
        let y = rect.center().y;
        label_cells(ui, &columns, rect, command, recording_here, tokens);
        let custom = prefs.pad_shortcuts.contains_key(command.id());
        source_cell(ui, &columns, y, custom, tokens);
        let keys_rect =
            egui::Rect::from_x_y_ranges(columns.keys, rect.y_range()).shrink2(vec2(8.0, 0.0));
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(keys_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(keys_rect.intersect(ui.clip_rect()));
                ui.spacing_mut().item_spacing.x = 6.0;
                if recording_here {
                    ui.label(
                        RichText::new("press buttons… (b cancels)")
                            .font(theme::mono(11.0))
                            .color(tokens.accent_text),
                    );
                    return;
                }
                for &chord in pad.chords(command) {
                    if ui
                        .push_id(("chord", command.id(), chord.notation()), |ui| {
                            chord_chip(ui, chord, tokens)
                        })
                        .inner
                        .tip("remove")
                        .clicked()
                    {
                        pad_keys::unbind(&mut prefs.pad_shortcuts, pad, command, chord);
                        changed = true;
                    }
                }
                if ui
                    .push_id(("add-pad", command.id()), |ui| {
                        icons::button(ui, "plus", 22.0, false, "add a button")
                    })
                    .inner
                    .clicked()
                {
                    self.pad_recording = Some(PadRecording::new(command));
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
                        ui.push_id(("reset-pad", command.id()), |ui| {
                            icons::button(
                                ui,
                                "rotate-ccw",
                                22.0,
                                false,
                                "back to the default buttons",
                            )
                        })
                        .inner
                    },
                )
                .inner
                .clicked()
        {
            prefs.pad_shortcuts.remove(command.id());
            changed = true;
        }
        if response.double_clicked() && !recording_here {
            self.pad_recording = Some(PadRecording::new(command));
        }
        changed
    }
}

impl PadRecording {
    fn new(command: Command) -> Self {
        Self {
            command,
            held: Vec::new(),
            chorded: false,
        }
    }
}

/// The command's icon and name, and its category.
fn label_cells(
    ui: &egui::Ui,
    columns: &Columns,
    rect: egui::Rect,
    command: Command,
    on: bool,
    tokens: &Tokens,
) {
    let y = rect.center().y;
    icons::paint(
        ui,
        egui::Rect::from_center_size(egui::pos2(columns.command.min + 18.0, y), vec2(16.0, 16.0)),
        command.icon(),
        13.0,
        tokens.faint,
    );
    glyphst_ui::text::row(
        ui,
        egui::pos2(columns.command.min + 34.0, y),
        command.label().to_lowercase(),
        theme::mono(12.0),
        if on {
            tokens.accent_text
        } else {
            tokens.text_2
        },
        columns.command.span() - 40.0,
    );
    glyphst_ui::text::row(
        ui,
        egui::pos2(columns.category.min + 10.0, y),
        command.category().label().to_lowercase(),
        theme::mono(11.0),
        tokens.faint,
        columns.category.span() - 14.0,
    );
}

fn source_cell(ui: &egui::Ui, columns: &Columns, y: f32, custom: bool, tokens: &Tokens) {
    glyphst_ui::text::row(
        ui,
        egui::pos2(columns.source.min + 10.0, y),
        if custom { "changed" } else { "default" },
        theme::mono(11.0),
        if custom {
            tokens.accent_text
        } else {
            tokens.faint
        },
        columns.source.span() - 14.0,
    );
}

/// The buttons recorded: a release ends a recording, as a hold when the
/// button was held [`pad_keys::HOLD`], as a chord when select was held.
/// B (or Escape) cancels it.
fn take_pad_recorded(
    ctx: &egui::Context,
    recording: &mut Option<PadRecording>,
) -> Option<(Command, PadChord)> {
    let active = recording.as_mut()?;
    let now = ctx.input(|input| input.time);
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, Key::Escape)) {
        *recording = None;
        return None;
    }
    for event in crate::pad::frame_events(ctx) {
        match event {
            PadEvent::Pressed(PadButton::East) => {
                *recording = None;
                return None;
            }
            PadEvent::Pressed(button) => {
                if button != PadButton::Select
                    && active
                        .held
                        .iter()
                        .any(|(held, _)| *held == PadButton::Select)
                {
                    active.chorded = true;
                }
                active.held.push((button, now));
            }
            PadEvent::Released(button) => {
                let Some(index) = active.held.iter().position(|(held, _)| *held == button) else {
                    continue;
                };
                let (_, since) = active.held.remove(index);
                let modifier = button != PadButton::Select
                    && active
                        .held
                        .iter()
                        .any(|(held, _)| *held == PadButton::Select);
                if button == PadButton::Select && active.chorded {
                    continue;
                }
                let held = now - since >= pad_keys::HOLD.as_secs_f64();
                let notation = format!(
                    "{}{}{}",
                    if held && !modifier { "hold " } else { "" },
                    if modifier { "select+" } else { "" },
                    button.name()
                );
                if let Some(chord) = PadChord::parse(&notation) {
                    let command = active.command;
                    *recording = None;
                    return Some((command, chord));
                }
            }
            PadEvent::Disconnected(_) => {
                *recording = None;
                return None;
            }
            PadEvent::Connected { .. } => {}
        }
    }
    None
}

/// A button chip that removes its binding when clicked (an × while
/// hovered).
fn chord_chip(ui: &mut egui::Ui, chord: PadChord, tokens: &Tokens) -> egui::Response {
    use glyphst_ui::pad_glyph::Hint;
    let glyphs = pad_keys::glyphs(chord);
    let hint = Hint::new(&glyphs, if chord.hold { "hold" } else { "" }).color(tokens.muted);
    let size = hint.size(ui) + vec2(10.0, 6.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.hovered() {
        ui.painter().rect(
            rect,
            tokens.ctl_radius(),
            tokens.err_soft,
            egui::Stroke::new(1.0, tokens.err),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            theme::mono(10.0),
            tokens.err,
        );
    } else {
        hint.paint(ui, rect.left() + 5.0, rect.center().y);
    }
    response
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

/// A key chip that removes its key when clicked (it shows an × while
/// hovered).
fn shortcut_chip(ui: &mut egui::Ui, label: &str, tokens: &Tokens) -> egui::Response {
    let label = glyphst_ui::key::display(label);
    let size = glyphst_ui::key::size(ui, &label) + egui::vec2(4.0, 3.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.hovered() {
        ui.painter().rect(
            rect,
            tokens.ctl_radius(),
            tokens.err_soft,
            egui::Stroke::new(1.0, tokens.err),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            theme::mono(10.0),
            tokens.err,
        );
    } else {
        glyphst_ui::key::paint(ui, rect, &label, None);
    }
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
            changed = page.show(ui, prefs, keymap, &PadKeymap::default());
        });
        output.textures_delta.clear();
        changed
    }

    fn keys(text: &str) -> Keys {
        Keys::parse(text).expect("parses")
    }

    #[test]
    fn pad_buttons_record_as_chords_and_holds() {
        let ctx = context();
        let mut page = KeymapPage {
            gamepad: true,
            ..KeymapPage::default()
        };
        let mut prefs = Prefs::default();
        let keymap = Keymap::default();
        let pad_frame =
            |page: &mut KeymapPage, prefs: &mut Prefs, events: Vec<PadEvent>, time: f64| {
                crate::pad::set_frame_events(&ctx, &events);
                frame(&ctx, page, prefs, &keymap, None, time)
            };
        page.pad_recording = Some(PadRecording::new(Command::ZoomIn));
        assert!(page.is_recording(), "the pad does nothing else meanwhile");
        let press = PadEvent::Pressed;
        let release = PadEvent::Released;
        pad_frame(&mut page, &mut prefs, vec![press(PadButton::Select)], 1.0);
        pad_frame(
            &mut page,
            &mut prefs,
            vec![press(PadButton::RightBumper)],
            1.1,
        );
        assert!(pad_frame(
            &mut page,
            &mut prefs,
            vec![release(PadButton::RightBumper), release(PadButton::Select)],
            1.2
        ));
        assert_eq!(prefs.pad_shortcuts["view.zoom_in"], ["select+rb"]);
        assert!(page.pad_recording.is_none());
        page.pad_recording = Some(PadRecording::new(Command::ZoomOut));
        pad_frame(
            &mut page,
            &mut prefs,
            vec![press(PadButton::LeftStick)],
            2.0,
        );
        assert!(pad_frame(
            &mut page,
            &mut prefs,
            vec![release(PadButton::LeftStick)],
            2.5
        ));
        assert_eq!(prefs.pad_shortcuts["view.zoom_out"], ["hold ls"]);
        page.pad_recording = Some(PadRecording::new(Command::ZoomOut));
        pad_frame(&mut page, &mut prefs, vec![press(PadButton::East)], 3.0);
        assert!(page.pad_recording.is_none(), "B cancels");
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
        assert_eq!(
            page.rows(&keymap, &PadKeymap::default()).len(),
            Command::ALL.len()
        );

        // By category: the categories in order, the registry's order in each.
        let rows = page.rows(&keymap, &PadKeymap::default());
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
            .rows(&keymap, &PadKeymap::default())
            .iter()
            .map(|command| command.label().to_lowercase())
            .collect();
        assert!(labels.is_sorted());

        page.filter = "ctrl+f".into();
        assert!(
            page.rows(&keymap, &PadKeymap::default())
                .contains(&Command::Find)
        );
        page.filter = "keyboard".into();
        assert_eq!(
            page.rows(&keymap, &PadKeymap::default()),
            [Command::KeyboardShortcuts]
        );
        page.filter = "translation".into();
        assert!(
            page.rows(&keymap, &PadKeymap::default())
                .iter()
                .all(|command| command.category() == Category::Translation
                    || command.label().to_lowercase().contains("translat"))
        );
        page.filter = "no such command".into();
        assert!(page.rows(&keymap, &PadKeymap::default()).is_empty());
    }

    /// The page draws in both themes, with a recording and a conflict.
    #[test]
    fn the_page_draws_while_recording_and_with_a_conflict() {
        let ctx = context();
        for kind in [theme::ThemeKind::Light, theme::ThemeKind::Dark] {
            theme::apply(
                &ctx,
                &theme::Appearance {
                    kind,
                    opacity: 0.8,
                    ..Default::default()
                },
            );
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
