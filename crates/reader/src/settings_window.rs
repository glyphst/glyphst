//! Settings, after pdfcraft's preferences dialog: an in-app window with a
//! section list. Changes apply at once and are saved with the
//! preferences.

use egui::{Key, RichText};
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_effects::EffectDef;

use crate::{
    commands::{Category, Command},
    icons,
    keys::{self, Keymap, KeymapPreset, Keys, Overrides, Shortcut},
    prefs::Prefs,
    settings_providers::ProviderPages,
    theme::{self, ThemePreference, Tokens},
    user_settings::{Persistence, UserSettings},
    widgets,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Effects,
    Reading,
    Shortcuts,
    Translation,
    Ocr,
    Ai,
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Appearance,
        Self::Effects,
        Self::Reading,
        Self::Shortcuts,
        Self::Translation,
        Self::Ocr,
        Self::Ai,
    ];

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

    pub fn icon(self) -> &'static str {
        match self {
            Self::Appearance => "palette",
            Self::Effects => "wand-sparkles",
            Self::Reading => "book-open",
            Self::Shortcuts => "keyboard",
            Self::Translation => "languages",
            Self::Ocr => "scan-text",
            Self::Ai => "sparkles",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Effects => "Effects",
            Self::Reading => "Reading",
            Self::Shortcuts => "Keyboard shortcuts",
            Self::Translation => "Translation",
            Self::Ocr => "OCR",
            Self::Ai => "AI",
        }
    }
}

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

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    section: Option<Section>,
    filter: String,
    /// The keys being recorded for a command.
    recording: Option<Recording>,
    conflict: Option<Conflict>,
    providers: ProviderPages,
    /// What the Effects page asked for, for the app to do.
    pub effects_actions: Vec<crate::effects_page::Action>,
}

/// What the window needs from the app.
pub struct SettingsContext<'a> {
    pub prefs: &'a mut Prefs,
    pub keymap: &'a Keymap,
    pub effects: &'a [EffectDef],
    /// Effects that failed to load, and the user's effects folder.
    pub effect_errors: &'a [String],
    pub effects_folder: Option<&'a std::path::Path>,
    /// Seconds, for previews.
    pub time: f32,
    pub user: &'a mut UserSettings,
    pub chatgpt: &'a ChatGptSubscription,
}

impl SettingsWindow {
    pub fn show_section(&mut self, section: Section) {
        self.open = true;
        self.section = Some(section);
    }

    /// Lists a provider's models, as its Fetch models button does.
    pub fn fetch_models(
        &mut self,
        settings: &glyphst_settings::Settings,
        provider: glyphst_settings::AiProvider,
        ctx: &egui::Context,
    ) {
        self.providers.fetch_models(settings, provider, ctx);
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
        self.providers.poll();
        let tokens = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let size = egui::vec2(
            (screen.width() - 48.0).clamp(320.0, 760.0),
            (screen.height() - 96.0).clamp(240.0, 560.0),
        );
        let response = egui::Modal::new(egui::Id::new("settings"))
            .frame(widgets::card_frame(ctx).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.set_min_size(size);
                ui.set_max_size(size);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Settings").font(theme::semibold(17.0)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icons::button(ui, "x", 28.0, false, "Close").clicked() {
                            self.open = false;
                        }
                    });
                });
                ui.add_space(6.0);
                let current = self.section.unwrap_or(Section::Appearance);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for section in Section::ALL {
                            let selected = current == section;
                            let (rect, response) = widgets::list_row(ui, 32.0, selected);
                            let color = if selected {
                                tokens.accent_text
                            } else {
                                tokens.icon
                            };
                            icons::paint(
                                ui,
                                egui::Rect::from_center_size(
                                    rect.left_center() + egui::vec2(18.0, 0.0),
                                    egui::vec2(16.0, 16.0),
                                ),
                                section.icon(),
                                16.0,
                                color,
                            );
                            widgets::row_text(
                                ui,
                                rect.left_center() + egui::vec2(36.0, 0.0),
                                section.label(),
                                if selected {
                                    theme::medium(13.0)
                                } else {
                                    theme::regular(13.0)
                                },
                                if selected {
                                    tokens.text
                                } else {
                                    tokens.text_muted
                                },
                                rect.width() - 40.0,
                            );
                            if response.clicked() {
                                self.section = Some(section);
                            }
                        }
                        if matches!(current, Section::Translation | Section::Ocr | Section::Ai) {
                            ui.add_space(12.0);
                            let note = match app.user.persistence() {
                                Persistence::User => {
                                    "Saved in the settings file and the system keychain."
                                }
                                Persistence::Memory => "Scripted run: changes are not saved.",
                            };
                            ui.label(RichText::new(note).small().color(tokens.text_faint));
                        }
                    });
                    let line = ui.available_rect_before_wrap();
                    ui.painter().vline(
                        line.left(),
                        line.y_range(),
                        egui::Stroke::new(1.0, tokens.divider),
                    );
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| match current {
                                Section::Appearance => appearance(ui, app.prefs),
                                Section::Effects => {
                                    // The previews are animated.
                                    ui.ctx().request_repaint();
                                    crate::effects_page::settings_page(
                                        ui,
                                        crate::effects_page::SettingsPage {
                                            prefs: app.prefs,
                                            effects: app.effects,
                                            errors: app.effect_errors,
                                            folder: app.effects_folder,
                                            time: app.time,
                                            actions: &mut self.effects_actions,
                                        },
                                    );
                                }
                                Section::Reading => reading(ui, app.prefs),
                                Section::Shortcuts => {
                                    changed = self.shortcuts(ui, app.prefs, app.keymap, &tokens);
                                }
                                Section::Translation => self.providers.translation(ui, app.user),
                                Section::Ocr => self.providers.ocr(ui, app.user),
                                Section::Ai => self.providers.ai(ui, app.user, app.chatgpt),
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
        if let Some((command, recorded)) = take_recorded(ui.ctx(), &mut self.recording) {
            let others: Vec<(Keys, Command)> = keymap
                .conflicts(&recorded)
                .into_iter()
                .filter(|(_, owner)| *owner != command)
                .collect();
            if others.is_empty() {
                keys::assign(&mut prefs.shortcuts, keymap, command, recorded);
                changed = true;
            } else {
                self.conflict = Some(Conflict {
                    command,
                    keys: recorded,
                    others,
                });
            }
        }
        ui.horizontal(|ui| {
            ui.label("Keys");
            let options = KeymapPreset::ALL.map(|preset| (preset, preset.label()));
            changed |= widgets::segmented(ui, &mut prefs.keymap, &options);
        });
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
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Search commands or keys")
                    .desired_width(220.0),
            );
            if ui
                .add_enabled_ui(!prefs.shortcuts.is_empty(), |ui| {
                    widgets::ghost_button(ui, "rotate-ccw", "Reset all")
                })
                .inner
                .clicked()
            {
                prefs.shortcuts = Overrides::new();
                changed = true;
            }
        });
        if let Some(conflict) = self.conflict.clone() {
            egui::Frame::new()
                .fill(tokens.accent_soft)
                .corner_radius(tokens.radius)
                .inner_margin(8)
                .show(ui, |ui| {
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
            widgets::section_title(ui, category.label());
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
                conflict.keys,
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
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.allocate_ui_with_layout(
                egui::vec2(220.0, 28.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(220.0);
                    ui.add(egui::Label::new(command.label()).truncate());
                },
            );
            for bound in keymap.keys(command) {
                if shortcut_chip(ui, &bound.label(), tokens)
                    .on_hover_text("Remove")
                    .clicked()
                {
                    keys::unbind(&mut prefs.shortcuts, keymap, command, bound);
                    changed = true;
                }
            }
            if let Some(recording) = self
                .recording
                .as_ref()
                .filter(|recording| recording.command == command)
            {
                let text = match Keys::new(recording.strokes.clone()) {
                    Some(so_far) => format!("{} …", so_far.label()),
                    None => "Press keys… (Esc cancels)".to_string(),
                };
                ui.label(RichText::new(text).color(tokens.accent_text));
            } else if ui
                .push_id(("add", command.id()), |ui| {
                    icons::button(ui, "plus", 24.0, false, "Add a shortcut")
                })
                .inner
                .clicked()
            {
                self.recording = Some(Recording::new(command));
                self.conflict = None;
            }
            if prefs.shortcuts.contains_key(command.id())
                && ui
                    .push_id(("reset", command.id()), |ui| {
                        icons::button(ui, "rotate-ccw", 24.0, false, "Back to the default keys")
                    })
                    .inner
                    .clicked()
            {
                keys::reset(&mut prefs.shortcuts, command);
                changed = true;
            }
        });
        changed
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

fn appearance(ui: &mut egui::Ui, prefs: &mut Prefs) {
    widgets::section_title(ui, "Theme");
    let options: Vec<(ThemePreference, &str)> = ThemePreference::ALL
        .into_iter()
        .map(|preference| (preference, preference.label()))
        .collect();
    widgets::segmented(ui, &mut prefs.theme, &options);
    ui.label(
        RichText::new("Backgrounds, page transitions and tints are under Effects.")
            .small()
            .color(Tokens::get(ui.ctx()).text_faint),
    );
}

fn reading(ui: &mut egui::Ui, prefs: &mut Prefs) {
    widgets::section_title(ui, "Session");
    ui.checkbox(
        &mut prefs.reopen_last_session,
        "Reopen the last session's documents",
    );
    widgets::section_title(ui, "While reading");
    ui.checkbox(&mut prefs.show_sidebar, "Show the side drawer");
    ui.checkbox(
        &mut prefs.autohide_controls,
        "Hide the reading controls after a few seconds without moving the pointer",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at `time` seconds, pressing `key` if there is one.
    fn frame(
        ctx: &egui::Context,
        window: &mut SettingsWindow,
        prefs: &mut Prefs,
        keymap: &Keymap,
        key: Option<(Key, egui::Modifiers)>,
        time: f64,
    ) -> bool {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
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
        let mut user = UserSettings::in_memory(
            ctx,
            glyphst_settings::Settings::default(),
            Default::default(),
        );
        let chatgpt = ChatGptSubscription::start(
            std::sync::Arc::new(glyphst_settings::MemoryCredentials::default()),
            || {},
        );
        let mut output = ctx.run_ui(input, |ui| {
            changed = window.show(
                ui.ctx(),
                SettingsContext {
                    prefs,
                    keymap,
                    effects: &[],
                    effect_errors: &[],
                    effects_folder: None,
                    time: 0.0,
                    user: &mut user,
                    chatgpt: &chatgpt,
                },
            );
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
        window: &mut SettingsWindow,
        prefs: &mut Prefs,
        keymap: &Keymap,
        command: Command,
        key: Key,
        clock: &mut f64,
    ) -> bool {
        window.recording = Some(Recording::new(command));
        let none = egui::Modifiers::NONE;
        *clock += 1.0;
        let mut changed = frame(ctx, window, prefs, keymap, Some((key, none)), *clock);
        *clock += RECORD_PAUSE;
        changed |= frame(ctx, window, prefs, keymap, None, *clock);
        changed
    }

    #[test]
    fn a_recorded_shortcut_another_command_has_moves_only_when_confirmed() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut prefs = Prefs::default();
        let mut window = SettingsWindow::default();
        window.show_section(Section::Shortcuts);
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        let mut clock = 0.0;

        assert!(!record(
            &ctx,
            &mut window,
            &mut prefs,
            &keymap,
            Command::Assistant,
            Key::P,
            &mut clock
        ));
        let p = keys("P");
        assert_eq!(
            window.conflict,
            Some(Conflict {
                command: Command::Assistant,
                keys: p.clone(),
                others: vec![(p.clone(), Command::Present)],
            })
        );
        assert!(window.is_recording(), "shortcuts wait for the answer");
        assert!(!window.resolve_conflict(&mut prefs, &keymap, false));
        assert!(prefs.shortcuts.is_empty());

        record(
            &ctx,
            &mut window,
            &mut prefs,
            &keymap,
            Command::Assistant,
            Key::P,
            &mut clock,
        );
        assert!(window.resolve_conflict(&mut prefs, &keymap, true));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&p), Some(Command::Assistant));
        assert!(keymap.keys(Command::Present).is_empty());

        // A free key is taken after the pause; Escape ends a recording
        // with nothing, and the window stays open.
        window.recording = Some(Recording::new(Command::Present));
        clock += 1.0;
        let f7 = Some((Key::F7, egui::Modifiers::NONE));
        assert!(!frame(&ctx, &mut window, &mut prefs, &keymap, f7, clock));
        assert!(window.is_recording(), "a sequence may follow");
        clock += RECORD_PAUSE;
        assert!(frame(&ctx, &mut window, &mut prefs, &keymap, None, clock));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.keys(Command::Present), &[keys("F7")]);
        window.recording = Some(Recording::new(Command::Present));
        let escape = Some((Key::Escape, egui::Modifiers::NONE));
        assert!(!frame(
            &ctx,
            &mut window,
            &mut prefs,
            &keymap,
            escape,
            clock
        ));
        assert_eq!(window.recording, None);
        assert!(window.open);
    }

    #[test]
    fn sequences_are_recorded_and_prefixes_conflict() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let mut prefs = Prefs {
            keymap: KeymapPreset::Vim,
            ..Prefs::default()
        };
        let mut window = SettingsWindow::default();
        window.show_section(Section::Shortcuts);
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);

        window.recording = Some(Recording::new(Command::ToggleStats));
        let ctrl_w = Some((Key::W, egui::Modifiers::COMMAND));
        let x = Some((Key::X, egui::Modifiers::NONE));
        assert!(!frame(&ctx, &mut window, &mut prefs, &keymap, ctrl_w, 1.0));
        assert!(!frame(&ctx, &mut window, &mut prefs, &keymap, x, 1.5));
        assert!(frame(&ctx, &mut window, &mut prefs, &keymap, None, 3.0));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&keys("Ctrl+W X")), Some(Command::ToggleStats));

        // G starts First page's and the tabs' sequences.
        let mut clock = 3.0;
        record(
            &ctx,
            &mut window,
            &mut prefs,
            &keymap,
            Command::Zen,
            Key::G,
            &mut clock,
        );
        let conflict = window.conflict.clone().expect("G conflicts");
        assert_eq!(conflict.others.len(), 3);
        assert!(window.resolve_conflict(&mut prefs, &keymap, true));
        let keymap = Keymap::new(prefs.keymap, &prefs.shortcuts);
        assert_eq!(keymap.owner(&keys("G")), Some(Command::Zen));
        assert_eq!(keymap.keys(Command::FirstPage), &[keys("Home")]);
    }

    #[test]
    fn sections_are_named_by_their_last_word() {
        assert_eq!(Section::parse("shortcuts"), Some(Section::Shortcuts));
        assert_eq!(Section::parse("Appearance"), Some(Section::Appearance));
        assert_eq!(Section::parse("nope"), None);
    }

    #[test]
    fn the_effects_page_shows_a_live_card_per_effect() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        let def = |id: &str, kind, user| EffectDef {
            id: id.into(),
            name: id.into(),
            kind,
            animated: false,
            duration_ms: 300,
            image: false,
            hidden: false,
            params: vec![glyphst_effects::ParamDef {
                id: "amount".into(),
                label: "Amount".into(),
                kind: glyphst_effects::ParamKind::Float { min: 0.0, max: 1.0 },
                default: 0.5,
            }],
            source: format!("{id}.wgsl"),
            user,
        };
        use glyphst_effects::EffectKind::{Background, Overlay, Transition};
        let effects = [
            def("solid", Background, false),
            def("wave", Background, true),
            def("crossfade", Transition, false),
            def("night", Overlay, false),
        ];
        let mut prefs = Prefs {
            background: "wave".into(),
            overlay: Some("night".into()),
            ..Prefs::default()
        };
        let keymap = Keymap::default();
        let mut window = SettingsWindow::default();
        window.show_section(Section::Effects);
        let mut user = UserSettings::in_memory(
            &ctx,
            glyphst_settings::Settings::default(),
            Default::default(),
        );
        let chatgpt = ChatGptSubscription::start(
            std::sync::Arc::new(glyphst_settings::MemoryCredentials::default()),
            || {},
        );
        let errors = ["wave.wgsl: line 3".to_string()];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 900.0),
            )),
            ..Default::default()
        };
        // A window's first frame only measures it.
        let mut output = Default::default();
        for _ in 0..3 {
            output = ctx.run_ui(input.clone(), |ui| {
                window.show(
                    ui.ctx(),
                    SettingsContext {
                        prefs: &mut prefs,
                        keymap: &keymap,
                        effects: &effects,
                        effect_errors: &errors,
                        effects_folder: Some(std::path::Path::new("/config/glyphst/effects")),
                        time: 1.0,
                        user: &mut user,
                        chatgpt: &chatgpt,
                    },
                );
            });
            output.textures_delta.clear();
        }
        let output: egui::FullOutput = output;
        // Cards draw live previews: one per effect, none for "None".
        let previews = output
            .shapes
            .iter()
            .filter(|shape| matches!(shape.shape, egui::Shape::Callback(_)))
            .count();
        assert_eq!(previews, effects.len());
        assert_eq!(window.effects_actions, []);
    }
}
