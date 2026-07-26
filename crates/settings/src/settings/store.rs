use super::persistence::*;
use super::*;

pub fn setup_ui_font(window: &mut Window, cx: &mut App) -> Font {
    let font_settings = cx.global::<SettingsStore>().settings.font.clone();
    window.set_rem_size(gpui::px(font_settings.ui_font_size));
    font_settings.ui_font()
}

pub struct SettingsStore {
    pub settings: Settings,
    pub theme_catalog: ThemeCatalog,
    pub resolved_theme: Theme,
    pub system_appearance: ThemeAppearance,
    pub user_managed: bool,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn memory() -> Self {
        let settings = Settings::default();
        let theme_catalog = ThemeCatalog::bundled();
        let system_appearance = ThemeAppearance::Light;
        let resolved_theme =
            theme_catalog.resolve(settings.theme.name(system_appearance), settings.overrides);
        Self {
            settings,
            theme_catalog,
            resolved_theme,
            system_appearance,
            user_managed: false,
            config_path: PathBuf::new(),
            state_path: PathBuf::new(),
        }
    }

    pub fn load() -> Self {
        let config_path = config_path();
        let state_path = state_path();
        let themes_path = user_themes_dir();
        if let Err(error) = fs::create_dir_all(&themes_path) {
            eprintln!(
                "failed to create user theme directory {}: {error}",
                themes_path.display()
            );
        }
        let theme_catalog = ThemeCatalog::load(&themes_path);
        let user_managed = config_path.exists();
        let load_path = if user_managed {
            Some(&config_path)
        } else if state_path.exists() {
            Some(&state_path)
        } else {
            None
        };
        let mut settings = load_path
            .map(|path| match load_settings(path) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("failed to load settings from {}: {error:#}", path.display());
                    Settings::default()
                }
            })
            .unwrap_or_default();
        let system_appearance = ThemeAppearance::Light;
        validate_theme_selection(&mut settings, &theme_catalog);
        let resolved_theme =
            theme_catalog.resolve(settings.theme.name(system_appearance), settings.overrides);

        Self {
            settings,
            theme_catalog,
            resolved_theme,
            system_appearance,
            user_managed,
            config_path,
            state_path,
        }
    }

    pub fn install_theme_catalog(&self, cx: &mut App) -> Result<()> {
        self.theme_catalog.install_into_component_registry(cx)
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        let mut settings = settings;
        settings.sanitize();
        validate_theme_selection(&mut settings, &self.theme_catalog);
        let resolved_theme = self.theme_catalog.resolve(
            settings.theme.name(self.system_appearance),
            settings.overrides,
        );
        let path = if self.user_managed {
            &self.config_path
        } else {
            &self.state_path
        };

        write_kdl(path, settings.to_kdl_string())?;
        self.settings = settings;
        self.resolved_theme = resolved_theme;
        Ok(())
    }

    pub fn set_settings_in_memory(&mut self, settings: Settings) {
        let mut settings = settings;
        settings.sanitize();
        validate_theme_selection(&mut settings, &self.theme_catalog);
        let resolved_theme = self.theme_catalog.resolve(
            settings.theme.name(self.system_appearance),
            settings.overrides,
        );
        self.settings = settings;
        self.resolved_theme = resolved_theme;
    }

    pub fn set_system_appearance(&mut self, appearance: ThemeAppearance) {
        if self.system_appearance == appearance {
            return;
        }
        self.system_appearance = appearance;
        self.resolved_theme = self.theme_catalog.resolve(
            self.settings.theme.name(appearance),
            self.settings.overrides,
        );
    }

    pub fn save(&mut self) -> Result<()> {
        let (path, contents) = self.persistence_snapshot();
        Self::write_snapshot(&path, contents)
    }

    pub fn persistence_snapshot(&self) -> (PathBuf, String) {
        let path = if self.user_managed {
            self.config_path.clone()
        } else {
            self.state_path.clone()
        };
        (path, self.export())
    }

    pub fn write_snapshot(path: &Path, contents: String) -> Result<()> {
        write_kdl(path, contents)
    }

    pub fn override_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.settings.sanitize();
        validate_theme_selection(&mut self.settings, &self.theme_catalog);
        self.resolved_theme = self.theme_catalog.resolve(
            self.settings.theme.name(self.system_appearance),
            self.settings.overrides,
        );
    }

    pub fn export(&self) -> String {
        self.settings.to_kdl_string()
    }

    pub fn export_to(&self, path: &Path) -> Result<()> {
        write_kdl(path, self.export())
    }
}

/// Keep gpui-components and Glyphst's document/workspace adapter on the same
/// selected theme. Call this after a theme, appearance, override, or UI-font
/// setting changes.
pub fn sync_component_theme(cx: &mut App) {
    let (light, dark, active, overrides, font) = {
        let store = cx.global::<SettingsStore>();
        let light = store
            .theme_catalog
            .config(&store.settings.theme.light)
            .or_else(|| store.theme_catalog.config(DEFAULT_LIGHT_THEME_ID))
            .expect("bundled default light theme is missing");
        let dark = store
            .theme_catalog
            .config(&store.settings.theme.dark)
            .or_else(|| store.theme_catalog.config(DEFAULT_DARK_THEME_ID))
            .expect("bundled default dark theme is missing");
        let active = match store
            .settings
            .theme
            .effective_appearance(store.system_appearance)
        {
            ThemeAppearance::Light => light.clone(),
            ThemeAppearance::Dark => dark.clone(),
        };
        (
            light,
            dark,
            active,
            store.settings.overrides,
            store.settings.font.clone(),
        )
    };

    let theme = gpui_component::Theme::global_mut(cx);
    theme.light_theme = light;
    theme.dark_theme = dark;
    theme.apply_config(&active);
    apply_component_overrides(theme, overrides);
    theme.font_family = font.ui_font_family.into();
    theme.font_size = gpui::px(font.ui_font_size);
    cx.refresh_windows();
}

fn validate_theme_selection(settings: &mut Settings, catalog: &ThemeCatalog) {
    if !catalog.contains_for_appearance(&settings.theme.light, ThemeAppearance::Light) {
        settings.theme.light = DEFAULT_LIGHT_THEME_ID.to_string();
    }
    if !catalog.contains_for_appearance(&settings.theme.dark, ThemeAppearance::Dark) {
        settings.theme.dark = DEFAULT_DARK_THEME_ID.to_string();
    }
}
