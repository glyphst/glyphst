//! Settings, after pdfcraft's preferences dialog: an in-app window with a
//! grouped section list at the left and the section's settings at the
//! right, in titled groups of rows. Changes apply at once and are saved
//! with the preferences (or the settings file). Keyboard shortcuts have
//! their own tab (`keymap_page.rs`).

use egui::{RichText, Sense, vec2};
use glyphst_ai::chatgpt_subscription::ChatGptSubscription;
use glyphst_effects::EffectDef;
use glyphst_ui::{
    Tip,
    button::Button,
    field::TextField,
    motif,
    row::ListRow,
    settings as ui_settings,
    slider::Slider,
    theme::{Accent, Density},
    toggle::Switch,
};

use crate::{
    icons,
    prefs::Prefs,
    settings_providers::ProviderPages,
    theme::{self, ThemeKind, ThemePreference, Tokens},
    user_settings::{Persistence, UserSettings},
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
    ("general", &[Section::Appearance, Section::Reading]),
    ("look", &[Section::Effects]),
    (
        "services",
        &[Section::Translation, Section::Ocr, Section::Ai],
    ),
];

const SIDEBAR_WIDTH: f32 = 220.0;
const HEADER_HEIGHT: f32 = 40.0;

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
                .any(|word| word.eq_ignore_ascii_case(name))
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
            Self::Ai => "AI & agents",
        }
    }

    /// Words its settings answer to, for the settings search.
    fn keywords(self) -> &'static str {
        match self {
            Self::Appearance => {
                "theme dark light system accent colour font chrome prose ui scale size density \
                 transparency opacity blur glyph ornaments"
            }
            Self::Reading => "session reopen sidebar drawer vim keys motion",
            Self::Effects => "background transition tint wallpaper animation folder",
            Self::Translation => "translate deepl google microsoft language engine glossary",
            Self::Ocr => "ocr scan recognize text",
            Self::Ai => "ai model provider key agent claude codex mcp assistant",
        }
    }

    /// The line under the section's title.
    fn description(self) -> &'static str {
        match self {
            Self::Appearance => "How Glyphst looks. Changes apply to every window immediately.",
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
    /// The settings search.
    query: String,
    /// Installed families for the font pickers (chrome, prose), read once.
    families: Option<(Vec<String>, Vec<String>)>,
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

    /// Draws the window (frame 07): a header with the section and the
    /// settings search, the sections grouped at the left with the file's
    /// state at their foot, and the section's settings at the right.
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
            .backdrop_color(tokens.scrim)
            .frame(glyphst_ui::pane::popover(ctx).inner_margin(egui::Margin::ZERO))
            .show(ctx, |ui| {
                let (full, _) = ui.allocate_exact_size(size, Sense::hover());
                let header = egui::Rect::from_min_size(full.min, vec2(size.x, HEADER_HEIGHT));
                let sidebar = egui::Rect::from_min_max(
                    egui::pos2(full.left(), header.bottom()),
                    egui::pos2(full.left() + SIDEBAR_WIDTH, full.bottom()),
                );
                let content = egui::Rect::from_min_max(
                    egui::pos2(sidebar.right(), header.bottom()),
                    full.max,
                );
                ui.painter()
                    .hline(header.x_range(), header.bottom() - 0.5, tokens.hairline());
                ui.painter()
                    .vline(sidebar.right() - 0.5, sidebar.y_range(), tokens.hairline());
                ui.painter().rect_filled(
                    sidebar
                        .shrink2(vec2(0.5, 0.0))
                        .with_min_x(sidebar.left() + 1.0),
                    0,
                    tokens.surface.to_opaque(),
                );
                let current = self.section.unwrap_or(Section::Appearance);
                ui.scope_builder(egui::UiBuilder::new().max_rect(header), |ui| {
                    self.header(ui, current, &tokens)
                });
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(sidebar.shrink2(vec2(0.0, 10.0))),
                    |ui| self.sidebar(ui, current, app.user, &tokens),
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

    /// `settings — section`, the search and the close button.
    fn header(&mut self, ui: &mut egui::Ui, current: Section, tokens: &Tokens) {
        let rect = ui.max_rect();
        let y = rect.center().y;
        let title = ui.painter().text(
            egui::pos2(rect.left() + 16.0, y),
            egui::Align2::LEFT_CENTER,
            "settings",
            theme::mono_strong(13.0),
            tokens.text,
        );
        let x = title.right() + 10.0;
        ui.painter()
            .hline(x..=x + 12.0, y, egui::Stroke::new(1.0, tokens.rule_strong));
        ui.painter().text(
            egui::pos2(x + 20.0, y),
            egui::Align2::LEFT_CENTER,
            current.label().to_lowercase(),
            theme::mono(12.0),
            tokens.faint,
        );
        let close =
            egui::Rect::from_center_size(egui::pos2(rect.right() - 22.0, y), vec2(24.0, 24.0));
        let mut close_ui = ui.new_child(egui::UiBuilder::new().max_rect(close));
        if icons::button(&mut close_ui, "x", 24.0, false, "close (esc)").clicked() {
            self.open = false;
        }
        let field = egui::Rect::from_min_size(
            egui::pos2(close.left() - 12.0 - 220.0, y - 14.0),
            vec2(220.0, 28.0),
        );
        let mut field_ui = ui.new_child(egui::UiBuilder::new().max_rect(field));
        TextField::search(&mut self.query, "search settings")
            .shortcut("/")
            .width(220.0)
            .id(egui::Id::new("settings-search"))
            .show(&mut field_ui);
        if ui.input(|i| i.key_pressed(egui::Key::Slash))
            && ui.ctx().memory(|memory| memory.focused().is_none())
        {
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(egui::Id::new("settings-search")));
        }
    }

    fn sidebar(
        &mut self,
        ui: &mut egui::Ui,
        current: Section,
        user: &UserSettings,
        tokens: &Tokens,
    ) {
        ui.spacing_mut().item_spacing.y = 0.0;
        let query = self.query.trim().to_lowercase();
        let matches = |section: Section| {
            query.is_empty()
                || section.label().to_lowercase().contains(&query)
                || section.description().to_lowercase().contains(&query)
                || section.keywords().contains(&query)
        };
        for (title, sections) in GROUPS {
            let shown: Vec<Section> = sections
                .iter()
                .copied()
                .filter(|&section| matches(section))
                .collect();
            if shown.is_empty() {
                continue;
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.set_width(SIDEBAR_WIDTH - 28.0);
                motif::rule_title(ui, title, None);
            });
            for section in shown {
                let label = section.label().to_lowercase();
                let response = ListRow::new(&label)
                    .icon(section.icon())
                    .active(current == section)
                    .indent(14.0)
                    .show(ui);
                if response
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .clicked()
                {
                    self.section = Some(section);
                }
            }
        }
        if query.is_empty() || "keys keyboard shortcuts".contains(&query) {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.set_width(SIDEBAR_WIDTH - 28.0);
                motif::rule_title(ui, "keys", None);
            });
            if ListRow::new("shortcuts")
                .icon("keyboard")
                .indent(14.0)
                .show(ui)
                .tip("open the keyboard shortcuts in a tab")
                .clicked()
            {
                self.open_shortcuts = true;
                self.open = false;
            }
        }
        // The settings file's state at the foot.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let file = glyphst_settings::settings_file();
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                if ui.add(Button::bracket_muted("open file")).clicked() {
                    let _ = std::process::Command::new("xdg-open").arg(&file).spawn();
                }
            });
            let shown = match dirs::home_dir() {
                Some(home) => file
                    .strip_prefix(&home)
                    .map(|rest| format!("~/{}", rest.display()))
                    .unwrap_or_else(|_| file.display().to_string()),
                None => file.display().to_string(),
            };
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(shown)
                            .font(theme::mono(10.0))
                            .color(tokens.faint),
                    )
                    .truncate(),
                );
            });
            let (state, color) = match user.persistence() {
                Persistence::User => ("● saved · applies live", tokens.ok),
                Persistence::Memory => ("● not saved · scripted run", tokens.warn),
            };
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new(state).font(theme::mono(10.0)).color(color));
            });
        });
    }

    fn content(
        &mut self,
        ui: &mut egui::Ui,
        current: Section,
        app: SettingsContext<'_>,
        _tokens: &Tokens,
    ) {
        let mut description = current.description().to_string();
        if current.in_settings_file() && app.user.persistence() == Persistence::Memory {
            description.push_str(" Scripted run: changes are not saved.");
        }
        let title = match current {
            Section::Ai => "AI & agents",
            other => other.label(),
        };
        ui_settings::heading(ui, title, &description);
        let families = self.families.get_or_insert_with(|| {
            (
                glyphst_ui::fonts::installed_families(true),
                glyphst_ui::fonts::installed_families(false),
            )
        });
        egui::ScrollArea::vertical()
            .id_salt(("settings-page", current.label()))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                glyphst_ui::settings::form_style(ui);
                ui.set_width(ui.available_width() - 4.0);
                match current {
                    Section::Appearance => {
                        if appearance(
                            ui,
                            app.prefs,
                            app.user,
                            app.system_theme,
                            app.blur_supported,
                            families,
                        ) {
                            self.section = Some(Section::Effects);
                        }
                    }
                    Section::Reading => reading(ui, app.prefs, app.user),
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

/// The Appearance page (frame 07a); returns whether the Effects link was
/// clicked.
fn appearance(
    ui: &mut egui::Ui,
    prefs: &mut Prefs,
    user: &mut UserSettings,
    system: Option<ThemeKind>,
    blur_supported: Option<bool>,
    families: &(Vec<String>, Vec<String>),
) -> bool {
    let tokens = Tokens::get(ui.ctx());
    ui_settings::group(ui, "", |group| {
        let caption = match (prefs.theme, system) {
            (ThemePreference::System, Some(ThemeKind::Dark)) => {
                "follows the desktop, which asks for dark"
            }
            (ThemePreference::System, Some(ThemeKind::Light)) => {
                "follows the desktop, which asks for light"
            }
            (ThemePreference::System, None) => "the desktop states no preference: light",
            _ => "follows the desktop when set to system",
        };
        group.row("theme", Some(caption), |ui| {
            glyphst_ui::segmented::labels(
                ui,
                &mut prefs.theme,
                &[
                    (ThemePreference::System, "system"),
                    (ThemePreference::Light, "light"),
                    (ThemePreference::Dark, "dark"),
                ],
            );
        });
        group.row(
            "accent",
            Some("used for focus, selection and progress"),
            |ui| {
                ui.label(
                    RichText::new(prefs.accent.label())
                        .font(theme::mono(11.0))
                        .color(tokens.muted),
                );
                for accent in Accent::ALL.into_iter().rev() {
                    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                    let chosen = prefs.accent == accent;
                    ui.painter().rect(
                        rect.shrink(2.0),
                        tokens.ctl_radius(),
                        accent.swatch(),
                        if chosen {
                            egui::Stroke::new(1.5, tokens.text)
                        } else {
                            tokens.edge()
                        },
                        egui::StrokeKind::Outside,
                    );
                    if response.tip(accent.label()).clicked() {
                        prefs.accent = accent;
                    }
                }
            },
        );
        group.row(
            "chrome font",
            Some("menus, tabs, labels and numbers"),
            |ui| {
                font_choice(ui, "chrome-font", &mut prefs.fonts.chrome, &families.0);
            },
        );
        group.row(
            "prose font",
            Some("assistant answers and help text"),
            |ui| {
                font_choice(ui, "prose-font", &mut prefs.fonts.prose, &families.1);
            },
        );
        group.row("ui scale", None, |ui| {
            let (low, high) = (
                *glyphst_ui::theme::UI_SCALE_RANGE.start(),
                *glyphst_ui::theme::UI_SCALE_RANGE.end(),
            );
            ui.add(
                Slider::new(&mut prefs.ui_scale, low..=high)
                    .step(1.0)
                    .width(196.0)
                    .label("ui scale")
                    .format(Some(Box::new(|value| format!("{value:.0} px")))),
            );
        });
        group.row("density", Some("row height and padding"), |ui| {
            glyphst_ui::segmented::labels(
                ui,
                &mut prefs.density,
                &[
                    (Density::Compact, "compact"),
                    (Density::Default, "default"),
                    (Density::Airy, "airy"),
                ],
            );
        });
        // The window's transparency, in the settings file.
        match user.draft().map(|settings| settings.window) {
            None => group.note("reading your settings…"),
            Some(mut window) => {
                let before = window;
                let mut transparent = window.transparent();
                let blur_hint = match blur_supported {
                    Some(false) => "show the desktop through panes (no blur from this compositor)",
                    _ => "show the desktop through panes (wayland blur when available)",
                };
                group.row("window transparency", Some(blur_hint), |ui| {
                    if ui.add(Switch(&mut transparent)).changed() {
                        window.set_transparent(transparent);
                    }
                    ui.add_enabled_ui(transparent, |ui| {
                        let mut opacity = window.opacity;
                        let min = glyphst_settings::WindowSettings::MIN_OPACITY;
                        if ui
                            .add(
                                Slider::new(&mut opacity, min..=1.0)
                                    .width(120.0)
                                    .label("opacity"),
                            )
                            .changed()
                        {
                            window.opacity = opacity;
                        }
                    });
                });
                let mut blur = window.blur();
                group.row("blur behind the window", None, |ui| {
                    ui.add_enabled_ui(transparent && blur_supported != Some(false), |ui| {
                        if ui.add(Switch(&mut blur)).changed() {
                            window.set_blur(blur);
                        }
                    });
                });
                if window != before {
                    user.edit(|settings| settings.window = window);
                }
            }
        }
        group.row(
            "glyph ornaments",
            Some("rule titles, tree lines and dot fields; off = plain labels"),
            |ui| {
                ui.add(Switch(&mut prefs.glyph_ornaments));
            },
        );
    });
    ui.add_space(8.0);
    ui.add(Button::bracket("backgrounds, page transitions and tints →"))
        .clicked()
}

/// A family from `families` in a combo box.
fn font_choice(ui: &mut egui::Ui, id: &str, chosen: &mut String, families: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(RichText::new(chosen.as_str()).font(theme::mono(12.0)))
        .width(200.0)
        .height(320.0)
        .show_ui(ui, |ui| {
            glyphst_ui::menu::style(ui);
            for family in families {
                if glyphst_ui::menu::Item::new(family)
                    .checked(family == chosen)
                    .show(ui)
                    .clicked()
                {
                    *chosen = family.clone();
                }
            }
        });
}

fn reading(ui: &mut egui::Ui, prefs: &mut Prefs, user: &mut UserSettings) {
    ui_settings::group(ui, "", |group| {
        group.row(
            "reopen the last session",
            Some("the documents, pages and panes you had when the app closed"),
            |ui| {
                ui.add(Switch(&mut prefs.reopen_last_session));
            },
        );
        group.row(
            "remember zoom",
            Some("reopen each document at its zoom; off opens at the default zoom, on its page"),
            |ui| {
                ui.add(Switch(&mut prefs.remember_zoom));
            },
        );
        group.row(
            "sidebar",
            Some("the outline, pages, bookmarks, notes and search beside the pages"),
            |ui| {
                ui.add(Switch(&mut prefs.show_sidebar));
            },
        );
        group.row(
            "vim keys",
            Some("j / k scroll, gg / G for pages; pending keys show in the corner"),
            |ui| {
                let mut vim = prefs.keymap == crate::keys::KeymapPreset::Vim;
                if ui.add(Switch(&mut vim)).changed() {
                    prefs.keymap = if vim {
                        crate::keys::KeymapPreset::Vim
                    } else {
                        crate::keys::KeymapPreset::Standard
                    };
                }
            },
        );
        if let Some(mut reduce) = user.draft().map(|settings| settings.behavior.reduce_motion) {
            group.row(
                "reduce motion",
                Some("replace page transitions and generation with short fades"),
                |ui| {
                    if ui.add(Switch(&mut reduce)).changed() {
                        user.edit(|settings| settings.behavior.reduce_motion = reduce);
                    }
                },
            );
        }
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
