//! Settings, after pdfcraft's preferences dialog: an in-app window with a
//! section list. Changes apply at once and are saved with the
//! preferences.

use egui::{Key, RichText};
use glyphst_effects::{EffectDef, EffectKind};

use crate::{
    commands::{Category, Command},
    keys::{self, Keymap, Overrides, Shortcut},
    prefs::Prefs,
    theme::{ThemePreference, Tokens},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Reading,
    Shortcuts,
}

impl Section {
    pub const ALL: [Self; 3] = [Self::Appearance, Self::Reading, Self::Shortcuts];

    /// The section a scripted run names: `appearance`, `shortcuts`…
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| {
            section
                .label()
                .split_whitespace()
                .last()
                .is_some_and(|word| word.eq_ignore_ascii_case(name))
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Reading => "Reading",
            Self::Shortcuts => "Keyboard shortcuts",
        }
    }
}

/// A shortcut recorded for `command` that `owner` has.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Conflict {
    command: Command,
    shortcut: Shortcut,
    owner: Command,
}

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    section: Option<Section>,
    filter: String,
    /// The command whose next key press is being recorded.
    recording: Option<Command>,
    conflict: Option<Conflict>,
}

/// What the window needs from the app.
pub struct SettingsContext<'a> {
    pub prefs: &'a mut Prefs,
    pub keymap: &'a Keymap,
    pub effects: &'a [EffectDef],
}

impl SettingsWindow {
    pub fn show_section(&mut self, section: Section) {
        self.open = true;
        self.section = Some(section);
    }

    /// Whether key presses are going to a recording, so shortcuts must not
    /// run.
    pub fn is_recording(&self) -> bool {
        self.open && (self.recording.is_some() || self.conflict.is_some())
    }

    /// Draws the window; returns whether the shortcuts changed.
    pub fn show(&mut self, ctx: &egui::Context, app: SettingsContext<'_>) -> bool {
        if !self.open {
            return false;
        }
        let mut changed = false;
        let tokens = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let size = egui::vec2(
            (screen.width() - 48.0).clamp(320.0, 760.0),
            (screen.height() - 96.0).clamp(240.0, 560.0),
        );
        let response = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Settings").size(18.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        self.open = false;
                    }
                });
            });
            ui.separator();
            let current = self.section.unwrap_or(Section::Appearance);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for section in Section::ALL {
                        if ui
                            .selectable_label(current == section, section.label())
                            .clicked()
                        {
                            self.section = Some(section);
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match current {
                            Section::Appearance => appearance(ui, app.prefs, app.effects),
                            Section::Reading => reading(ui, app.prefs),
                            Section::Shortcuts => {
                                changed = self.shortcuts(ui, app.prefs, app.keymap, &tokens);
                            }
                        });
                });
            });
        });
        if response.should_close() && !self.is_recording() {
            self.open = false;
        }
        if !self.open {
            self.recording = None;
            self.conflict = None;
        }
        changed
    }

    fn shortcuts(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        if let Some(command) = self.recording
            && let Some(shortcut) = take_recorded(ui.ctx(), &mut self.recording)
        {
            match keymap.owner(shortcut) {
                Some(owner) if owner != command => {
                    self.conflict = Some(Conflict {
                        command,
                        shortcut,
                        owner,
                    });
                }
                _ => {
                    keys::assign(&mut prefs.shortcuts, keymap, command, shortcut);
                    changed = true;
                }
            }
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Search commands or keys")
                    .desired_width(220.0),
            );
            if ui
                .add_enabled(!prefs.shortcuts.is_empty(), egui::Button::new("Reset all"))
                .clicked()
            {
                prefs.shortcuts = Overrides::new();
                changed = true;
            }
        });
        if let Some(conflict) = self.conflict {
            egui::Frame::new()
                .fill(tokens.selected)
                .corner_radius(tokens.radius)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.label(format!(
                        "{} is used by “{}”.",
                        conflict.shortcut.label(),
                        conflict.owner.label()
                    ));
                    ui.horizontal(|ui| {
                        if ui
                            .button(format!("Use it for “{}”", conflict.command.label()))
                            .clicked()
                        {
                            changed |= self.resolve_conflict(prefs, keymap, true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.resolve_conflict(prefs, keymap, false);
                        }
                    });
                });
        }
        let filter = self.filter.trim().to_lowercase();
        for category in Category::ALL {
            let commands: Vec<Command> = Command::ALL
                .into_iter()
                .filter(|command| command.category() == category)
                .filter(|command| {
                    filter.is_empty()
                        || command.label().to_lowercase().contains(&filter)
                        || keymap
                            .keys(*command)
                            .iter()
                            .any(|key| key.label().to_lowercase().contains(&filter))
                })
                .collect();
            if commands.is_empty() {
                continue;
            }
            ui.add_space(6.0);
            ui.label(RichText::new(category.label()).strong());
            for command in commands {
                changed |= self.shortcut_row(ui, prefs, keymap, command, tokens);
            }
        }
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
                conflict.shortcut,
            );
        }
        accept
    }

    fn shortcut_row(
        &mut self,
        ui: &mut egui::Ui,
        prefs: &mut Prefs,
        keymap: &Keymap,
        command: Command,
        tokens: &Tokens,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(220.0, 22.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(220.0);
                    ui.add(egui::Label::new(command.label()).truncate());
                },
            );
            for &shortcut in keymap.keys(command) {
                let chip = egui::Button::new(format!("{}  ×", shortcut.label()))
                    .fill(tokens.hover)
                    .stroke(egui::Stroke::new(1.0, tokens.border))
                    .small();
                if ui.add(chip).on_hover_text("Remove").clicked() {
                    keys::unbind(&mut prefs.shortcuts, keymap, command, shortcut);
                    changed = true;
                }
            }
            if self.recording == Some(command) {
                ui.label(RichText::new("Press a key… (Esc cancels)").color(tokens.accent));
            } else if ui
                .small_button("+")
                .on_hover_text("Add a shortcut")
                .clicked()
            {
                self.recording = Some(command);
                self.conflict = None;
            }
            if prefs.shortcuts.contains_key(command.id())
                && ui
                    .small_button("Reset")
                    .on_hover_text("Back to the default keys")
                    .clicked()
            {
                keys::reset(&mut prefs.shortcuts, command);
                changed = true;
            }
        });
        changed
    }
}

/// The shortcut of the key pressed for a recording, taking the press from
/// egui's input. Escape ends the recording with nothing.
fn take_recorded(ctx: &egui::Context, recording: &mut Option<Command>) -> Option<Shortcut> {
    let mut recorded = None;
    ctx.input_mut(|input| {
        input.events.retain(|event| {
            if recording.is_none() || recorded.is_some() {
                return true;
            }
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = event
            else {
                return true;
            };
            if *key == Key::Escape && modifiers.is_none() {
                *recording = None;
                return false;
            }
            match Shortcut::from_key(*key, *modifiers) {
                Some(shortcut) => {
                    recorded = Some(shortcut);
                    *recording = None;
                    false
                }
                None => true,
            }
        });
    });
    recorded
}

fn appearance(ui: &mut egui::Ui, prefs: &mut Prefs, effects: &[EffectDef]) {
    ui.label(RichText::new("Theme").strong());
    ui.horizontal(|ui| {
        for preference in ThemePreference::ALL {
            ui.radio_value(&mut prefs.theme, preference, preference.label());
        }
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Page transition").strong());
    ui.radio_value(&mut prefs.transition, None, "None");
    for def in effects
        .iter()
        .filter(|def| def.kind == EffectKind::Transition)
    {
        ui.radio_value(&mut prefs.transition, Some(def.id.clone()), &def.name);
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Background").strong());
    for def in effects
        .iter()
        .filter(|def| def.kind == EffectKind::Background)
    {
        let label = if def.animated {
            format!("{} (animated)", def.name)
        } else {
            def.name.clone()
        };
        ui.radio_value(&mut prefs.background, def.id.clone(), label);
    }
    ui.add(
        egui::Slider::new(&mut prefs.background_fps, 1..=120)
            .text("frames per second for animated backgrounds"),
    );
}

fn reading(ui: &mut egui::Ui, prefs: &mut Prefs) {
    ui.checkbox(
        &mut prefs.reopen_last_session,
        "Reopen the last session's documents",
    );
    ui.checkbox(&mut prefs.show_sidebar, "Show the sidebar");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        window: &mut SettingsWindow,
        prefs: &mut Prefs,
        keymap: &Keymap,
        key: Key,
    ) -> bool {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            events: vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut changed = false;
        let mut output = ctx.run_ui(input, |ui| {
            changed = window.show(
                ui.ctx(),
                SettingsContext {
                    prefs,
                    keymap,
                    effects: &[],
                },
            );
        });
        output.textures_delta.clear();
        changed
    }

    #[test]
    fn a_recorded_shortcut_another_command_has_moves_only_when_confirmed() {
        let ctx = egui::Context::default();
        let mut prefs = Prefs::default();
        let mut window = SettingsWindow::default();
        window.show_section(Section::Shortcuts);
        let keymap = Keymap::new(&prefs.shortcuts);

        window.recording = Some(Command::Assistant);
        assert!(!frame(&ctx, &mut window, &mut prefs, &keymap, Key::P));
        let p = Shortcut::parse("P").expect("parses");
        assert_eq!(
            window.conflict,
            Some(Conflict {
                command: Command::Assistant,
                shortcut: p,
                owner: Command::Present,
            })
        );
        assert!(window.is_recording(), "shortcuts wait for the answer");
        assert!(!window.resolve_conflict(&mut prefs, &keymap, false));
        assert!(prefs.shortcuts.is_empty());

        window.recording = Some(Command::Assistant);
        frame(&ctx, &mut window, &mut prefs, &keymap, Key::P);
        assert!(window.resolve_conflict(&mut prefs, &keymap, true));
        let keymap = Keymap::new(&prefs.shortcuts);
        assert_eq!(keymap.owner(p), Some(Command::Assistant));
        assert!(keymap.keys(Command::Present).is_empty());

        // A free key is taken at once; Escape ends a recording with
        // nothing, and the window stays open.
        window.recording = Some(Command::Present);
        assert!(frame(&ctx, &mut window, &mut prefs, &keymap, Key::F7));
        let keymap = Keymap::new(&prefs.shortcuts);
        assert_eq!(
            keymap.keys(Command::Present),
            &[Shortcut::parse("F7").expect("parses")]
        );
        window.recording = Some(Command::Present);
        assert!(!frame(&ctx, &mut window, &mut prefs, &keymap, Key::Escape));
        assert_eq!(window.recording, None);
        assert!(window.open);
    }

    #[test]
    fn sections_are_named_by_their_last_word() {
        assert_eq!(Section::parse("shortcuts"), Some(Section::Shortcuts));
        assert_eq!(Section::parse("Appearance"), Some(Section::Appearance));
        assert_eq!(Section::parse("nope"), None);
    }
}
