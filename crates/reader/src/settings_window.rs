//! Settings, after pdfcraft's preferences dialog: an in-app window with a
//! grouped section list at the left and the section's settings at the
//! right, in titled groups of rows. Changes apply at once and are saved
//! with the preferences (or the settings file). Keyboard shortcuts have
//! their own tab (`keymap_page.rs`).

use egui::{CornerRadius, RichText, Sense, Stroke, vec2};
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_effects::EffectDef;

use crate::{
    icons,
    prefs::Prefs,
    settings_providers::ProviderPages,
    theme::{self, ThemeKind, ThemePreference, Tokens},
    user_settings::{Persistence, UserSettings},
    widgets,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Reading,
    Effects,
    Translation,
    Ocr,
    Ai,
}

/// The section list's groups, top to bottom.
const GROUPS: [(&str, &[Section]); 3] = [
    ("General", &[Section::Appearance, Section::Reading]),
    ("Look", &[Section::Effects]),
    (
        "Services",
        &[Section::Translation, Section::Ocr, Section::Ai],
    ),
];

const SIDEBAR_WIDTH: f32 = 212.0;

impl Section {
    pub const ALL: [Self; 6] = [
        Self::Appearance,
        Self::Reading,
        Self::Effects,
        Self::Translation,
        Self::Ocr,
        Self::Ai,
    ];

    /// The section a scripted run names: `appearance`, `ai`…
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
            Self::Reading => "book-open",
            Self::Effects => "wand-sparkles",
            Self::Translation => "languages",
            Self::Ocr => "scan-text",
            Self::Ai => "sparkles",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Reading => "Reading",
            Self::Effects => "Effects",
            Self::Translation => "Translation",
            Self::Ocr => "OCR",
            Self::Ai => "AI",
        }
    }

    /// The line under the section's title.
    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "The theme, and how much of the desktop shows through the window.",
            Self::Reading => "What opens at start, and what shows while you read.",
            Self::Effects => "Backgrounds, page transitions and tints for every book.",
            Self::Translation => "The service that translates selections and documents.",
            Self::Ocr => "Recognizing the text of scanned pages.",
            Self::Ai => "Models and providers for the assistant and AI translation.",
        }
    }

    /// Whether its settings live in the settings file (and keys in the
    /// keychain) rather than the app's preferences.
    fn in_settings_file(self) -> bool {
        matches!(
            self,
            Self::Appearance | Self::Translation | Self::Ocr | Self::Ai
        )
    }
}

#[derive(Default)]
pub struct SettingsWindow {
    pub open: bool,
    section: Option<Section>,
    providers: ProviderPages,
    /// What the Effects page asked for, for the app to do.
    pub effects_actions: Vec<crate::effects_page::Action>,
    /// The Keyboard shortcuts row was clicked: the app opens their tab.
    pub open_shortcuts: bool,
}

/// What the window needs from the app.
pub struct SettingsContext<'a> {
    pub prefs: &'a mut Prefs,
    pub effects: &'a [EffectDef],
    /// Effects that failed to load, and the user's effects folder.
    pub effect_errors: &'a [String],
    pub effects_folder: Option<&'a std::path::Path>,
    /// Seconds, for previews.
    pub time: f32,
    pub user: &'a mut UserSettings,
    pub chatgpt: &'a ChatGptSubscription,
    /// The theme the desktop last asked for, if it ever did.
    pub system_theme: Option<ThemeKind>,
    /// Whether the compositor can blur behind the window; `None` until
    /// known.
    pub blur_supported: Option<bool>,
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

    /// Draws the window.
    pub fn show(&mut self, ctx: &egui::Context, app: SettingsContext<'_>) {
        if !self.open {
            return;
        }
        self.providers.poll();
        let tokens = Tokens::get(ctx);
        let screen = ctx.content_rect();
        let size = vec2(
            (screen.width() - 48.0).clamp(360.0, 960.0),
            (screen.height() - 80.0).clamp(280.0, 680.0),
        );
        let response = egui::Modal::new(egui::Id::new("settings"))
            .frame(widgets::card_frame(ctx).inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                let (full, _) = ui.allocate_exact_size(size, Sense::hover());
                let sidebar = egui::Rect::from_min_size(full.min, vec2(SIDEBAR_WIDTH, size.y));
                let content =
                    egui::Rect::from_min_max(egui::pos2(sidebar.right(), full.top()), full.max);
                ui.painter().rect_filled(
                    sidebar,
                    CornerRadius {
                        nw: 10,
                        sw: 10,
                        ne: 0,
                        se: 0,
                    },
                    tokens.hover,
                );
                ui.painter().vline(
                    sidebar.right(),
                    sidebar.y_range(),
                    Stroke::new(1.0, tokens.rule),
                );
                let current = self.section.unwrap_or(Section::Appearance);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(sidebar.shrink2(vec2(10.0, 16.0))),
                    |ui| self.sidebar(ui, current, &tokens),
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(content.shrink2(vec2(28.0, 20.0))),
                    |ui| self.content(ui, current, app, &tokens),
                );
            });
        if response.should_close() {
            self.open = false;
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, current: Section, tokens: &Tokens) {
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Settings").font(theme::semibold(16.0)));
        });
        ui.add_space(6.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        for (title, sections) in GROUPS {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                widgets::section_title(ui, title);
            });
            for &section in sections {
                let selected = current == section;
                let (rect, response) = widgets::list_row(ui, 32.0, selected);
                icons::paint(
                    ui,
                    egui::Rect::from_center_size(
                        rect.left_center() + vec2(18.0, 0.0),
                        vec2(16.0, 16.0),
                    ),
                    section.icon(),
                    16.0,
                    if selected {
                        tokens.accent_text
                    } else {
                        tokens.text_2
                    },
                );
                widgets::row_text(
                    ui,
                    rect.left_center() + vec2(36.0, 0.0),
                    section.label(),
                    if selected {
                        theme::medium(13.0)
                    } else {
                        theme::regular(13.0)
                    },
                    if selected { tokens.text } else { tokens.muted },
                    rect.width() - 40.0,
                );
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.section = Some(section);
                }
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            if widgets::link_row(ui, "keyboard", "Keyboard shortcuts", "arrow-right")
                .on_hover_text("Open the keyboard shortcuts in a tab")
                .clicked()
            {
                self.open_shortcuts = true;
                self.open = false;
            }
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 9.0), Sense::hover());
            ui.painter().hline(
                rect.x_range(),
                rect.center().y,
                Stroke::new(1.0, tokens.rule),
            );
        });
    }

    fn content(
        &mut self,
        ui: &mut egui::Ui,
        current: Section,
        app: SettingsContext<'_>,
        tokens: &Tokens,
    ) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 40.0).max(0.0));
                ui.label(RichText::new(current.label()).font(theme::semibold(20.0)));
                let mut description = current.description().to_string();
                if current.in_settings_file() {
                    description.push(' ');
                    description.push_str(match app.user.persistence() {
                        Persistence::User => {
                            "Saved in the settings file; keys in the system keychain."
                        }
                        Persistence::Memory => "Scripted run: changes are not saved.",
                    });
                }
                ui.add(egui::Label::new(RichText::new(description).color(tokens.muted)).wrap());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if icons::button(ui, "x", 28.0, false, "Close").clicked() {
                    self.open = false;
                }
            });
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt(("settings-page", current.label()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                widgets::form_style(ui);
                ui.set_width(ui.available_width() - 4.0);
                match current {
                    Section::Appearance => {
                        if appearance(
                            ui,
                            app.prefs,
                            app.user,
                            app.system_theme,
                            app.blur_supported,
                        ) {
                            self.section = Some(Section::Effects);
                        }
                    }
                    Section::Reading => reading(ui, app.prefs),
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
                    Section::Translation => self.providers.translation(ui, app.user),
                    Section::Ocr => self.providers.ocr(ui, app.user),
                    Section::Ai => self.providers.ai(ui, app.user, app.chatgpt),
                }
                ui.add_space(16.0);
            });
    }
}

/// The Appearance page; returns whether the Effects link was clicked.
fn appearance(
    ui: &mut egui::Ui,
    prefs: &mut Prefs,
    user: &mut UserSettings,
    system: Option<ThemeKind>,
    blur_supported: Option<bool>,
) -> bool {
    widgets::settings_group(ui, "Theme", |group| {
        let caption = match (prefs.theme, system) {
            (ThemePreference::System, Some(ThemeKind::Dark)) => {
                Some("Following the desktop, which asks for dark.")
            }
            (ThemePreference::System, Some(ThemeKind::Light)) => {
                Some("Following the desktop, which asks for light.")
            }
            (ThemePreference::System, None) => {
                Some("The desktop states no preference, so the theme is light.")
            }
            _ => None,
        };
        group.row("Colour scheme", caption, |ui| {
            widgets::segmented_with_icons(
                ui,
                &mut prefs.theme,
                &[
                    (ThemePreference::System, Some("monitor"), "System"),
                    (ThemePreference::Light, Some("sun"), "Light"),
                    (ThemePreference::Dark, Some("moon"), "Dark"),
                ],
            );
        });
    });
    widgets::settings_group(ui, "Window", |group| {
        let Some(mut window) = user.draft().map(|settings| settings.window) else {
            group.note("Reading your settings…");
            return;
        };
        let before = window;
        let mut transparent = window.transparent();
        group.row(
            "Transparent window",
            Some(
                "Shows the desktop through the title bar and the background around the \
                 pages; panels, menus, dialogs and the palette stay opaque.",
            ),
            |ui| {
                if widgets::toggle(ui, &mut transparent).changed() {
                    window.set_transparent(transparent);
                }
            },
        );
        group.row(
            "Opacity",
            Some("How much of the window stays opaque."),
            |ui| {
                ui.add_enabled_ui(transparent, |ui| {
                    let mut percent = (window.opacity * 100.0).round();
                    let min = (glyphst_settings::WindowSettings::MIN_OPACITY * 100.0).round();
                    let response = widgets::slider(
                        ui,
                        egui::Slider::new(&mut percent, min..=100.0)
                            .step_by(1.0)
                            .suffix(" %"),
                    );
                    if response.changed() {
                        window.opacity = percent / 100.0;
                    }
                });
            },
        );
        let mut blur = window.blur();
        let blur_description = match blur_supported {
            Some(false) => "Your compositor doesn't offer blur (ext-background-effect).",
            _ => "Blurs the desktop behind the window, where the compositor can.",
        };
        group.row("Blur behind the window", Some(blur_description), |ui| {
            ui.add_enabled_ui(transparent && blur_supported != Some(false), |ui| {
                if widgets::toggle(ui, &mut blur).changed() {
                    window.set_blur(blur);
                }
            });
        });
        if window != before {
            user.edit(|settings| settings.window = window);
        }
    });
    widgets::settings_group(ui, "", |group| {
        group.link("wand-sparkles", "Backgrounds, page transitions and tints")
    })
}

fn reading(ui: &mut egui::Ui, prefs: &mut Prefs) {
    widgets::settings_group(ui, "Session", |group| {
        group.row(
            "Reopen the last session",
            Some("Open the documents, pages and panes you had when the app closed."),
            |ui| widgets::toggle(ui, &mut prefs.reopen_last_session),
        );
    });
    widgets::settings_group(ui, "While reading", |group| {
        group.row(
            "Side drawer",
            Some("The outline, pages, bookmarks, notes and search beside the pages."),
            |ui| widgets::toggle(ui, &mut prefs.show_sidebar),
        );
        group.row(
            "Hide the reading controls",
            Some("After a few seconds without moving the pointer."),
            |ui| widgets::toggle(ui, &mut prefs.autohide_controls),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(ctx: &egui::Context) -> UserSettings {
        UserSettings::in_memory(
            ctx,
            glyphst_settings::Settings::default(),
            Default::default(),
        )
    }

    fn chatgpt() -> ChatGptSubscription {
        ChatGptSubscription::start(
            std::sync::Arc::new(glyphst_settings::MemoryCredentials::default()),
            || {},
        )
    }

    /// Runs `frames` frames of the window and returns the last output.
    fn run(
        ctx: &egui::Context,
        window: &mut SettingsWindow,
        prefs: &mut Prefs,
        user: &mut UserSettings,
        effects: &[EffectDef],
        frames: usize,
    ) -> egui::FullOutput {
        let chatgpt = chatgpt();
        let errors = ["wave.wgsl: line 3".to_string()];
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 900.0),
            )),
            ..Default::default()
        };
        let mut output = egui::FullOutput::default();
        for _ in 0..frames {
            output = ctx.run_ui(input.clone(), |ui| {
                window.show(
                    ui.ctx(),
                    SettingsContext {
                        prefs: &mut *prefs,
                        effects,
                        effect_errors: &errors,
                        effects_folder: Some(std::path::Path::new("/config/glyphst/effects")),
                        time: 1.0,
                        user: &mut *user,
                        chatgpt: &chatgpt,
                        system_theme: Some(ThemeKind::Dark),
                        blur_supported: Some(false),
                    },
                );
            });
            output.textures_delta.clear();
        }
        output
    }

    #[test]
    fn sections_are_named_by_their_last_word() {
        assert_eq!(Section::parse("ai"), Some(Section::Ai));
        assert_eq!(Section::parse("Appearance"), Some(Section::Appearance));
        // Shortcuts have their own tab now.
        assert_eq!(Section::parse("shortcuts"), None);
        assert_eq!(Section::parse("nope"), None);
        let grouped: Vec<Section> = GROUPS
            .iter()
            .flat_map(|(_, sections)| sections.iter().copied())
            .collect();
        assert_eq!(grouped, Section::ALL, "every section is listed once");
    }

    #[test]
    fn every_section_draws_in_both_themes_and_transparent() {
        let ctx = egui::Context::default();
        ctx.set_fonts(crate::fonts::embedded());
        for (kind, opacity) in [(ThemeKind::Light, 1.0), (ThemeKind::Dark, 0.7)] {
            theme::apply(
                &ctx,
                &theme::Appearance {
                    kind,
                    opacity,
                    ..Default::default()
                },
            );
            let mut prefs = Prefs::default();
            let mut user = user(&ctx);
            for section in Section::ALL {
                let mut window = SettingsWindow::default();
                window.show_section(section);
                run(&ctx, &mut window, &mut prefs, &mut user, &[], 3);
                assert!(window.open, "{section:?} stays open");
            }
        }
    }

    #[test]
    fn transparency_is_off_until_turned_on_and_saved_in_the_settings() {
        let ctx = egui::Context::default();
        let mut user = user(&ctx);
        let mut window = user.draft().unwrap().window;
        assert!(!window.transparent());
        // What the Appearance toggle does.
        window.set_transparent(true);
        user.edit(|settings| settings.window = window);
        let settings = user.get().unwrap();
        assert!(settings.window.transparent());
        assert!(settings.window.effective_opacity(true) < 1.0);
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
        // The page itself, tall enough that every gallery is in view
        // (in the window, the tints are scrolled to).
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 2000.0),
            )),
            ..Default::default()
        };
        let errors = ["wave.wgsl: line 3".to_string()];
        let mut actions = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            crate::effects_page::settings_page(
                ui,
                crate::effects_page::SettingsPage {
                    prefs: &mut prefs,
                    effects: &effects,
                    errors: &errors,
                    folder: Some(std::path::Path::new("/config/glyphst/effects")),
                    time: 1.0,
                    actions: &mut actions,
                },
            );
        });
        output.textures_delta.clear();
        // Cards draw live previews: one per effect, none for "None".
        let previews = output
            .shapes
            .iter()
            .filter(|shape| matches!(shape.shape, egui::Shape::Callback(_)))
            .count();
        assert_eq!(previews, effects.len());
        assert_eq!(actions, []);
    }
}
