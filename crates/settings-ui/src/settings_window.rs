use glyphst_actions::settings_panel::PanelClose;
use glyphst_ai::ai_client::{AiModelInfo, list_ai_models, test_ai_connection};
use glyphst_keymap::keybindings;
use glyphst_settings::{
    AiModel, AiProvider, AiProviderConfig, AiThinkingLevel, AppLanguage, FontSettings,
    KeymapPreset, ModelRef, ResolvedAiConfig, Settings, SettingsStore, TabBarVisibility,
    TranslationBoxCorner, TranslationProvider, TranslationProviderConfig, WindowBackgroundMode,
    localize, setup_ui_font,
    theme::{
        PartialTheme, Theme, ThemeAppearance, ThemeAppearanceMode, ThemeMetadata, ThemeSelection,
        format_rgb_hex, format_rgba_hex, parse_hex_color,
    },
    ui_rems,
};
use glyphst_ui::{
    scrollbar::{WithScrollbar as _, uniform_list_scrollbar_for},
    searchable_dropdown::{SearchableDropdown, SearchableDropdownEvent, SearchableItem},
    settings_input::{SettingsInputEvent, SettingsTextInput},
};
use gpui::{
    Anchor, AnyElement, App, AppContext, Bounds, Context, ElementId, Entity, FocusHandle,
    Focusable, IntoElement, MouseButton, MouseDownEvent, Pixels, Point as GpuiPoint, Render,
    ScrollHandle, SharedString, Task, TitlebarOptions, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions, anchored, deferred, div, point, prelude::*, px, rgb, rgba, size,
    uniform_list,
};
use std::{
    ops::Range,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

// ---- settings panel ----

const NUMBER_STEP_SMALL: f32 = 0.05;
const OPACITY_STEP: f32 = 0.05;
const PAGE_GAP_STEP: f32 = 4.0;
const FONT_SIZE_STEP: f32 = 1.0;
const FONT_WEIGHT_STEP: f32 = 100.0;
const COLOR_SWATCHES: [u32; 10] = [
    0x101010, 0x181818, 0x202020, 0x282c34, 0x1e1e2e, 0x313244, 0x61afef, 0x89b4fa, 0xf5f5f5,
    0xffffff,
];
const HIGHLIGHT_SWATCHES: [u32; 6] = [
    0x61afef66, 0x89b4fa66, 0xc678dd66, 0xe5c07b66, 0x98c37966, 0xe06c7566,
];
const ALPHA_SWATCHES: [u32; 6] = [
    0x00000033, 0x00000066, 0xffffff66, 0x61afef55, 0x3a3f4bcc, 0x45475a99,
];

#[derive(Clone, Copy)]
struct ColorRowConfig {
    label: &'static str,
    current: u32,
    override_value: Option<u32>,
    swatches: &'static [u32],
    with_alpha: bool,
}

#[derive(Clone, Copy)]
struct ActiveColorPicker {
    label: &'static str,
    anchor: GpuiPoint<Pixels>,
    current: u32,
    swatches: &'static [u32],
    with_alpha: bool,
    update: fn(&mut PartialTheme, u32),
}

fn format_color_hex(color: u32, with_alpha: bool) -> String {
    if with_alpha {
        format_rgba_hex(color)
    } else {
        format_rgb_hex(color)
    }
}

fn parse_color_input(value: &str, with_alpha: bool) -> Option<u32> {
    let hex = value.trim().strip_prefix('#').unwrap_or(value.trim());
    match (with_alpha, hex.len()) {
        (false, 6) => parse_hex_color(hex, false),
        (true, 6) => parse_hex_color(hex, false).map(|color| (color << 8) | 0xff),
        (true, 8) => parse_hex_color(hex, true),
        _ => None,
    }
}

pub fn open_settings_window(open_keymap: fn(&mut App), cx: &mut App) {
    open_settings_window_with_section(None, open_keymap, cx);
}

fn open_settings_window_with_section(
    section: Option<SettingsSection>,
    open_keymap: fn(&mut App),
    cx: &mut App,
) {
    if let Some(existing_window) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>())
    {
        if let Err(error) = existing_window.update(cx, move |settings, window, cx| {
            if let Some(section) = section {
                settings.select_section(section);
                cx.notify();
            }
            window.activate_window();
        }) {
            eprintln!("failed to activate settings window: {error:#}");
        }
        return;
    }

    let window_settings = cx.global::<SettingsStore>().settings.window;
    let transparency_enabled = window_settings.transparency_enabled_by_default();
    let bounds = Bounds::centered(None, size(px(900.0), px(760.0)), cx);

    if let Err(error) = cx.open_window(
        WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(crate::fl!("settings-window-title").into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_background: window_settings.background_appearance(transparency_enabled),
            app_id: Some("glyphst".to_string()),
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("glyphst");
            cx.new(|cx| {
                let mut settings = SettingsWindow::new(open_keymap, cx);
                if let Some(section) = section {
                    settings.select_section(section);
                }
                settings
            })
        },
    ) {
        eprintln!("failed to open settings window: {error:#}");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    General,
    Keymap,
    Appearance,
    AiGeneral,
    AiProviders,
    Translation,
}

impl SettingsTab {
    fn label(self) -> String {
        match self {
            Self::General => crate::fl!("settings-tab-general"),
            Self::Keymap => crate::fl!("settings-tab-keymap"),
            Self::Appearance => crate::fl!("settings-tab-appearance"),
            Self::AiGeneral => crate::fl!("settings-tab-general"),
            Self::AiProviders => crate::fl!("settings-tab-model-providers"),
            Self::Translation => crate::fl!("settings-tab-translation"),
        }
    }

    fn first_section(self) -> SettingsSection {
        SettingsSection::for_tab(self)[0]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsSection {
    GeneralLanguage,
    GeneralZoom,
    GeneralScroll,
    GeneralKeymap,
    GeneralWindow,
    GeneralTabs,
    AppearanceTheme,
    AppearanceFonts,
    AppearanceOverrides,
    AiGeneral,
    AiProviders,
    TranslationGeneral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsNavEntry {
    Root(SettingsTab),
    AiParent,
    AiChild(SettingsSection),
}

impl SettingsNavEntry {
    fn element_id(self) -> &'static str {
        match self {
            Self::Root(SettingsTab::General) => "settings-nav-general",
            Self::Root(SettingsTab::Keymap) => "settings-nav-keymap",
            Self::Root(SettingsTab::Appearance) => "settings-nav-appearance",
            Self::Root(SettingsTab::AiGeneral) => "settings-nav-ai-general-root",
            Self::Root(SettingsTab::AiProviders) => "settings-nav-ai-providers-root",
            Self::Root(SettingsTab::Translation) => "settings-nav-translation",
            Self::AiParent => "settings-nav-ai",
            Self::AiChild(SettingsSection::AiGeneral) => "settings-nav-ai-general",
            Self::AiChild(SettingsSection::AiProviders) => "settings-nav-ai-providers",
            Self::AiChild(_) => "settings-nav-child",
        }
    }
}

const SETTINGS_NAV_ENTRIES: [SettingsNavEntry; 7] = [
    SettingsNavEntry::Root(SettingsTab::General),
    SettingsNavEntry::Root(SettingsTab::Keymap),
    SettingsNavEntry::Root(SettingsTab::Appearance),
    SettingsNavEntry::AiParent,
    SettingsNavEntry::AiChild(SettingsSection::AiGeneral),
    SettingsNavEntry::AiChild(SettingsSection::AiProviders),
    SettingsNavEntry::Root(SettingsTab::Translation),
];

fn visible_settings_nav_entries(ai_expanded: bool) -> impl Iterator<Item = SettingsNavEntry> {
    let mut show_children = true;
    SETTINGS_NAV_ENTRIES
        .into_iter()
        .filter(move |entry| match entry {
            SettingsNavEntry::Root(_) => {
                show_children = true;
                true
            }
            SettingsNavEntry::AiParent => {
                show_children = ai_expanded;
                true
            }
            SettingsNavEntry::AiChild(_) => show_children,
        })
}

const GENERAL_SECTIONS: [SettingsSection; 5] = [
    SettingsSection::GeneralLanguage,
    SettingsSection::GeneralZoom,
    SettingsSection::GeneralScroll,
    SettingsSection::GeneralWindow,
    SettingsSection::GeneralTabs,
];
const KEYMAP_SECTIONS: [SettingsSection; 1] = [SettingsSection::GeneralKeymap];
const APPEARANCE_SECTIONS: [SettingsSection; 3] = [
    SettingsSection::AppearanceTheme,
    SettingsSection::AppearanceFonts,
    SettingsSection::AppearanceOverrides,
];
const AI_GENERAL_SECTIONS: [SettingsSection; 1] = [SettingsSection::AiGeneral];
const AI_PROVIDER_SECTIONS: [SettingsSection; 1] = [SettingsSection::AiProviders];
const TRANSLATION_SECTIONS: [SettingsSection; 1] = [SettingsSection::TranslationGeneral];

impl SettingsSection {
    fn tab(self) -> SettingsTab {
        match self {
            Self::GeneralLanguage
            | Self::GeneralZoom
            | Self::GeneralScroll
            | Self::GeneralWindow
            | Self::GeneralTabs => SettingsTab::General,
            Self::GeneralKeymap => SettingsTab::Keymap,
            Self::AppearanceTheme | Self::AppearanceFonts | Self::AppearanceOverrides => {
                SettingsTab::Appearance
            }
            Self::AiGeneral => SettingsTab::AiGeneral,
            Self::AiProviders => SettingsTab::AiProviders,
            Self::TranslationGeneral => SettingsTab::Translation,
        }
    }

    fn label(self) -> String {
        match self {
            Self::GeneralLanguage => crate::fl!("settings-section-language"),
            Self::GeneralZoom => crate::fl!("settings-section-zoom"),
            Self::GeneralScroll => crate::fl!("settings-section-scroll"),
            Self::GeneralKeymap => crate::fl!("settings-section-keymap"),
            Self::GeneralWindow => crate::fl!("settings-section-window"),
            Self::GeneralTabs => crate::fl!("settings-section-tabs"),
            Self::AppearanceTheme => crate::fl!("settings-section-theme"),
            Self::AppearanceFonts => crate::fl!("settings-section-fonts"),
            Self::AppearanceOverrides => crate::fl!("settings-section-overrides"),
            Self::AiGeneral => crate::fl!("settings-section-general"),
            Self::AiProviders => crate::fl!("settings-section-model-providers"),
            Self::TranslationGeneral => crate::fl!("settings-tab-translation"),
        }
    }

    fn for_tab(tab: SettingsTab) -> &'static [Self] {
        match tab {
            SettingsTab::General => &GENERAL_SECTIONS,
            SettingsTab::Keymap => &KEYMAP_SECTIONS,
            SettingsTab::Appearance => &APPEARANCE_SECTIONS,
            SettingsTab::AiGeneral => &AI_GENERAL_SECTIONS,
            SettingsTab::AiProviders => &AI_PROVIDER_SECTIONS,
            SettingsTab::Translation => &TRANSLATION_SECTIONS,
        }
    }

    fn index_in_tab(self) -> usize {
        Self::for_tab(self.tab())
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProviderDropdownTarget {
    Main,
    Title,
}

#[derive(Clone, PartialEq, Eq)]
struct ModelRequestSignature {
    provider: AiProvider,
    base_url: String,
    has_api_key: bool,
}

struct ModelPickerState {
    open: bool,
    query: SharedString,
    models: Vec<AiModelInfo>,
    loading: bool,
    status: Option<SharedString>,
    request: Option<ModelRequestSignature>,
    task: Option<Task<()>>,
    anchor: Option<GpuiPoint<Pixels>>,
    scroll_handle: ScrollHandle,
    fetch_scroll_handle: UniformListScrollHandle,
}

impl Default for ModelPickerState {
    fn default() -> Self {
        Self {
            open: false,
            query: SharedString::default(),
            models: Vec::new(),
            loading: false,
            status: None,
            request: None,
            task: None,
            anchor: None,
            scroll_handle: ScrollHandle::new(),
            fetch_scroll_handle: UniformListScrollHandle::new(),
        }
    }
}

struct FontPickerState {
    open: bool,
    anchor: Option<GpuiPoint<Pixels>>,
    fonts: Vec<String>,
    scroll_handle: ScrollHandle,
}

impl FontPickerState {
    fn new(cx: &App, font_settings: &FontSettings) -> Self {
        let mut fonts = cx.text_system().all_font_names();
        fonts.extend(
            [".ZedSans", ".ZedMono", ".SystemUIFont"]
                .into_iter()
                .map(str::to_string),
        );
        fonts.push(font_settings.ui_font_family.clone());
        fonts.extend(font_settings.ui_font_fallbacks.clone());
        fonts.extend(FontSettings::default().ui_font_fallbacks);
        fonts.sort_by_cached_key(|font| font.to_ascii_lowercase());
        fonts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));

        Self {
            open: false,
            anchor: None,
            fonts,
            scroll_handle: ScrollHandle::new(),
        }
    }
}

#[derive(Clone)]
enum SettingsModelMenuEntry {
    Header(AiProvider),
    Model(AiProvider, AiModel),
}

pub struct SettingsWindow {
    focus_handle: FocusHandle,
    open_keymap: fn(&mut App),
    active_tab: SettingsTab,
    active_section: SettingsSection,
    ai_expanded: bool,
    content_scroll: ScrollHandle,
    provider_list_scroll: ScrollHandle,
    status: Option<SharedString>,
    save_error: Option<SharedString>,
    selected_ai_provider: AiProvider,
    selected_translation_provider: TranslationProvider,
    main_model_picker: ModelPickerState,
    title_model_picker: ModelPickerState,
    provider_model_picker: ModelPickerState,
    provider_model_selection: Vec<String>,
    font_picker: FontPickerState,
    active_color_picker: Option<ActiveColorPicker>,
    color_picker_input: Entity<SettingsTextInput>,
    font_search_input: Entity<SettingsTextInput>,
    ui_font_fallbacks_input: Entity<SettingsTextInput>,
    ai_model_input: Entity<SettingsTextInput>,
    ai_key_input: Entity<SettingsTextInput>,
    ai_base_url_input: Entity<SettingsTextInput>,
    translation_target_input: Entity<SettingsTextInput>,
    translation_key_input: Entity<SettingsTextInput>,
    translation_base_url_input: Entity<SettingsTextInput>,
    translation_region_input: Entity<SettingsTextInput>,
    translation_settings_save_generation: Arc<AtomicU64>,
    main_model_search: Entity<SettingsTextInput>,
    title_model_search: Entity<SettingsTextInput>,
    fetch_model_search: Entity<SettingsTextInput>,
    static_theme_dropdown: Entity<SearchableDropdown>,
    light_theme_dropdown: Entity<SearchableDropdown>,
    dark_theme_dropdown: Entity<SearchableDropdown>,
}

impl SettingsWindow {
    fn select_section(&mut self, section: SettingsSection) {
        self.active_tab = section.tab();
        self.active_section = section;
        if matches!(
            section,
            SettingsSection::AiGeneral | SettingsSection::AiProviders
        ) {
            self.ai_expanded = true;
        }
    }

    fn font_fallbacks_input_value(font: &FontSettings) -> SharedString {
        font.ui_font_fallbacks.join(", ").into()
    }

    fn parse_font_fallbacks_input(value: &str) -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|fallback| !fallback.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn new(open_keymap: fn(&mut App), cx: &mut Context<Self>) -> Self {
        let (
            provider,
            config,
            font_settings,
            translation_provider,
            translation_config,
            target_language,
        ) = {
            let settings = &cx.global::<SettingsStore>().settings;
            let ai = &settings.ai;
            let provider = ai
                .main_model
                .as_ref()
                .map(|model_ref| model_ref.provider)
                .unwrap_or(AiProvider::OpenAi);
            (
                provider,
                ai.provider_config(provider),
                settings.font.clone(),
                settings.translation.active_provider,
                settings
                    .translation
                    .provider_config(settings.translation.active_provider),
                settings.translation.target_language.clone(),
            )
        };

        let font_picker = FontPickerState::new(cx, &font_settings);
        let color_picker_input =
            cx.new(|cx| SettingsTextInput::new("#000000".into(), "#rrggbb".into(), false, cx));
        cx.subscribe(&color_picker_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let Some(mut picker) = this.active_color_picker else {
                return;
            };
            let Some(color) = parse_color_input(value.as_ref(), picker.with_alpha) else {
                return;
            };
            picker.current = color;
            this.active_color_picker = Some(picker);
            this.persist_theme_color_override(picker.update, color, cx);
        })
        .detach();
        let font_search_input = cx.new(|cx| {
            SettingsTextInput::new(
                "".into(),
                crate::fl!("settings-search-fonts-placeholder").into(),
                false,
                cx,
            )
        });
        cx.subscribe(&font_search_input, |_, _, _, cx| cx.notify())
            .detach();

        let ui_font_fallbacks_input = cx.new(|cx| {
            SettingsTextInput::new(
                Self::font_fallbacks_input_value(&font_settings),
                "Noto Sans CJK SC, Source Han Sans SC".into(),
                false,
                cx,
            )
        });
        cx.subscribe(&ui_font_fallbacks_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let fallbacks = Self::parse_font_fallbacks_input(value.as_ref());
            this.update_font_settings(move |font| font.ui_font_fallbacks = fallbacks, cx);
        })
        .detach();

        let ai_model_input = cx.new(|cx| {
            SettingsTextInput::new(
                "".into(),
                crate::fl!("settings-model-id-placeholder").into(),
                false,
                cx,
            )
        });
        let ai_key_input = cx.new(|cx| {
            SettingsTextInput::new(
                config.api_key.clone().into(),
                crate::fl!("settings-api-key-placeholder").into(),
                true,
                cx,
            )
        });
        let ai_base_url_input = cx.new(|cx| {
            SettingsTextInput::new(
                config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| provider.default_base_url().to_string())
                    .into(),
                provider.default_base_url().into(),
                false,
                cx,
            )
        });
        cx.subscribe(&ai_key_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let provider = this.selected_ai_provider;
            this.update_provider_config(provider, move |_, config| config.api_key = value, cx);
        })
        .detach();

        let main_model_search = cx.new(|cx| {
            SettingsTextInput::new(
                "".into(),
                crate::fl!("settings-search-model-placeholder").into(),
                false,
                cx,
            )
        });
        let title_model_search = cx.new(|cx| {
            SettingsTextInput::new(
                "".into(),
                crate::fl!("settings-search-model-placeholder").into(),
                false,
                cx,
            )
        });
        cx.subscribe(&main_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.main_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();
        cx.subscribe(&title_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.title_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();

        let fetch_model_search = cx.new(|cx| {
            SettingsTextInput::new(
                "".into(),
                crate::fl!("settings-search-model-placeholder").into(),
                false,
                cx,
            )
        });
        cx.subscribe(&fetch_model_search, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            this.provider_model_picker.query = value.clone();
            cx.notify();
        })
        .detach();

        cx.subscribe(&ai_base_url_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let provider = this.selected_ai_provider;
            this.update_provider_config(
                provider,
                move |provider, config| {
                    let trimmed = value.trim().trim_end_matches('/').to_string();
                    config.base_url =
                        if trimmed.is_empty() || trimmed == provider.default_base_url() {
                            None
                        } else {
                            Some(trimmed)
                        };
                },
                cx,
            );
        })
        .detach();

        let translation_target_input = cx.new(|cx| {
            SettingsTextInput::new(
                target_language.into(),
                "Target language, for example en or zh-Hans".into(),
                false,
                cx,
            )
        });
        let translation_key_input = cx.new(|cx| {
            SettingsTextInput::new(
                translation_config.api_key.clone().into(),
                crate::fl!("settings-api-key-placeholder").into(),
                true,
                cx,
            )
        });
        let translation_base_url_input = cx.new(|cx| {
            SettingsTextInput::new(
                translation_config
                    .base_url
                    .clone()
                    .or_else(|| translation_provider.default_base_url().map(str::to_string))
                    .unwrap_or_default()
                    .into(),
                "Provider API endpoint".into(),
                false,
                cx,
            )
        });
        let translation_region_input = cx.new(|cx| {
            SettingsTextInput::new(
                translation_config.region.clone().unwrap_or_default().into(),
                "Azure region, if required".into(),
                false,
                cx,
            )
        });
        cx.subscribe(&translation_target_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.translation.target_language = value;
            this.persist_translation_settings(settings, cx);
        })
        .detach();
        cx.subscribe(&translation_key_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            this.update_translation_provider_config(
                this.selected_translation_provider,
                move |config| config.api_key = value,
                cx,
            );
        })
        .detach();
        cx.subscribe(&translation_base_url_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            let provider = this.selected_translation_provider;
            this.update_translation_provider_config(
                provider,
                move |config| {
                    let value = value.trim().trim_end_matches('/').to_string();
                    config.base_url = if value.is_empty()
                        || Some(value.as_str()) == provider.default_base_url()
                    {
                        None
                    } else {
                        Some(value)
                    };
                },
                cx,
            );
        })
        .detach();
        cx.subscribe(&translation_region_input, |this, _, event, cx| {
            let SettingsInputEvent::Changed(value) = event;
            let value = value.to_string();
            this.update_translation_provider_config(
                this.selected_translation_provider,
                move |config| config.region = (!value.trim().is_empty()).then_some(value),
                cx,
            );
        })
        .detach();

        // Theme dropdowns for the appearance tab
        let (
            static_selected,
            light_selected,
            dark_selected,
            all_theme_items,
            light_theme_items,
            dark_theme_items,
        ) = {
            let store = cx.global::<SettingsStore>();
            let system_appearance = store.system_appearance;
            let (static_sel, light_sel, dark_sel) = match &store.settings.theme {
                ThemeSelection::Static(name) => (name.clone(), name.clone(), name.clone()),
                ThemeSelection::Dynamic { light, dark, .. } => (
                    store.settings.theme.name(system_appearance).to_string(),
                    light.clone(),
                    dark.clone(),
                ),
            };
            let all_items: Vec<SearchableItem> = store
                .registry
                .metadata()
                .into_iter()
                .map(|m| SearchableItem {
                    value: m.id,
                    label: m.name,
                })
                .collect();
            let light_items: Vec<SearchableItem> = store
                .registry
                .metadata()
                .into_iter()
                .filter(|m| m.appearance == ThemeAppearance::Light)
                .map(|m| SearchableItem {
                    value: m.id,
                    label: m.name,
                })
                .collect();
            let dark_items: Vec<SearchableItem> = store
                .registry
                .metadata()
                .into_iter()
                .filter(|m| m.appearance == ThemeAppearance::Dark)
                .map(|m| SearchableItem {
                    value: m.id,
                    label: m.name,
                })
                .collect();
            (
                static_sel,
                light_sel,
                dark_sel,
                all_items,
                light_items,
                dark_items,
            )
        };
        let static_theme_dropdown = cx.new(|cx| {
            SearchableDropdown::new(
                all_theme_items,
                static_selected,
                &crate::fl!("settings-search-themes-placeholder"),
                cx,
            )
        });
        let light_theme_dropdown = cx.new(|cx| {
            SearchableDropdown::new(
                light_theme_items,
                light_selected,
                &crate::fl!("settings-search-light-themes-placeholder"),
                cx,
            )
        });
        let dark_theme_dropdown = cx.new(|cx| {
            SearchableDropdown::new(
                dark_theme_items,
                dark_selected,
                &crate::fl!("settings-search-dark-themes-placeholder"),
                cx,
            )
        });
        cx.subscribe(&static_theme_dropdown, |this, _, event, cx| {
            let SearchableDropdownEvent::Selected(value) = event;
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            settings.theme = ThemeSelection::Static(value.clone());
            this.persist_settings(settings, cx);
        })
        .detach();
        cx.subscribe(&light_theme_dropdown, |this, _, event, cx| {
            let SearchableDropdownEvent::Selected(value) = event;
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            if let ThemeSelection::Dynamic { light, .. } = &mut settings.theme {
                *light = value.clone();
                this.persist_settings(settings, cx);
            }
        })
        .detach();
        cx.subscribe(&dark_theme_dropdown, |this, _, event, cx| {
            let SearchableDropdownEvent::Selected(value) = event;
            let mut settings = cx.global::<SettingsStore>().settings.clone();
            if let ThemeSelection::Dynamic { dark, .. } = &mut settings.theme {
                *dark = value.clone();
                this.persist_settings(settings, cx);
            }
        })
        .detach();

        cx.observe_global::<SettingsStore>(|this, cx| {
            let (theme, system_appearance) = {
                let store = cx.global::<SettingsStore>();
                (store.settings.theme.clone(), store.system_appearance)
            };
            match &theme {
                ThemeSelection::Static(name) => {
                    this.static_theme_dropdown.update(cx, |d, cx| {
                        d.set_selected_value(name.clone());
                        cx.notify();
                    });
                }
                ThemeSelection::Dynamic { light, dark, .. } => {
                    this.light_theme_dropdown.update(cx, |d, cx| {
                        d.set_selected_value(light.clone());
                        cx.notify();
                    });
                    this.dark_theme_dropdown.update(cx, |d, cx| {
                        d.set_selected_value(dark.clone());
                        cx.notify();
                    });
                    this.static_theme_dropdown.update(cx, |d, cx| {
                        d.set_selected_value(theme.name(system_appearance).to_string());
                        cx.notify();
                    });
                }
            }
            cx.notify();
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            open_keymap,
            active_tab: SettingsTab::General,
            active_section: SettingsSection::GeneralLanguage,
            ai_expanded: true,
            content_scroll: ScrollHandle::new(),
            provider_list_scroll: ScrollHandle::new(),
            status: None,
            save_error: None,
            selected_ai_provider: provider,
            selected_translation_provider: translation_provider,
            main_model_picker: ModelPickerState::default(),
            title_model_picker: ModelPickerState::default(),
            provider_model_picker: ModelPickerState::default(),
            provider_model_selection: Vec::new(),
            font_picker,
            active_color_picker: None,
            color_picker_input,
            font_search_input,
            ui_font_fallbacks_input,
            ai_model_input,
            ai_key_input,
            ai_base_url_input,
            translation_target_input,
            translation_key_input,
            translation_base_url_input,
            translation_region_input,
            translation_settings_save_generation: Arc::new(AtomicU64::new(0)),
            main_model_search,
            title_model_search,
            fetch_model_search,
            static_theme_dropdown,
            light_theme_dropdown,
            dark_theme_dropdown,
        }
    }

    fn persist_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let result = cx.global_mut::<SettingsStore>().set_settings(settings);
        let (keymap_preset, keybinding_overrides) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.keymap_preset,
                store.settings.keybinding_overrides.clone(),
            )
        };
        keybindings::apply(cx, keymap_preset, &keybinding_overrides);
        localize::select_language(cx.global::<SettingsStore>().settings.language);
        cx.refresh_windows();
        self.status = None;
        self.save_error = result.err().map(|e| format!("{e:#}").into());
        cx.notify();
    }

    fn persist_translation_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        cx.global_mut::<SettingsStore>()
            .set_settings_in_memory(settings);
        self.status = None;
        self.save_error = None;
        let snapshot = cx.global::<SettingsStore>().persistence_snapshot();
        let generation = self
            .translation_settings_save_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let save_generation = self.translation_settings_save_generation.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            if save_generation.load(Ordering::Relaxed) != generation {
                return;
            }
            let (path, contents) = snapshot;
            let result = cx
                .background_spawn(async move {
                    SettingsStore::write_snapshot(&path, contents)
                        .map_err(|error| format!("{error:#}"))
                })
                .await;
            if save_generation.load(Ordering::Relaxed) != generation {
                return;
            }
            _ = this.update(cx, |this, cx| {
                this.save_error = result.err().map(Into::into);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn base_url_input_content(provider: AiProvider, config: &AiProviderConfig) -> SharedString {
        config
            .base_url
            .clone()
            .unwrap_or_else(|| provider.default_base_url().to_string())
            .into()
    }

    fn update_provider_config(
        &mut self,
        provider: AiProvider,
        update: impl FnOnce(AiProvider, &mut AiProviderConfig),
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(provider, settings.ai.provider_config_mut(provider));
        self.persist_settings(settings, cx);
    }

    fn update_translation_provider_config(
        &mut self,
        provider: TranslationProvider,
        update: impl FnOnce(&mut TranslationProviderConfig),
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(settings.translation.provider_config_mut(provider));
        self.persist_translation_settings(settings, cx);
    }

    fn select_translation_provider(
        &mut self,
        provider: TranslationProvider,
        cx: &mut Context<Self>,
    ) {
        self.selected_translation_provider = provider;
        let config = cx
            .global::<SettingsStore>()
            .settings
            .translation
            .provider_config(provider);
        self.translation_key_input
            .update(cx, |input, cx| input.set_content(config.api_key.into(), cx));
        self.translation_base_url_input.update(cx, |input, cx| {
            input.set_content(
                config
                    .base_url
                    .or_else(|| provider.default_base_url().map(str::to_string))
                    .unwrap_or_default()
                    .into(),
                cx,
            );
        });
        self.translation_region_input.update(cx, |input, cx| {
            input.set_content(config.region.unwrap_or_default().into(), cx);
        });
        cx.notify();
    }

    fn update_font_settings(
        &mut self,
        update: impl FnOnce(&mut FontSettings),
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings.font);
        self.persist_settings(settings, cx);
    }

    fn model_picker(&self, target: ProviderDropdownTarget) -> &ModelPickerState {
        match target {
            ProviderDropdownTarget::Main => &self.main_model_picker,
            ProviderDropdownTarget::Title => &self.title_model_picker,
        }
    }

    fn model_picker_mut(&mut self, target: ProviderDropdownTarget) -> &mut ModelPickerState {
        match target {
            ProviderDropdownTarget::Main => &mut self.main_model_picker,
            ProviderDropdownTarget::Title => &mut self.title_model_picker,
        }
    }

    fn model_search_input(&self, target: ProviderDropdownTarget) -> &Entity<SettingsTextInput> {
        match target {
            ProviderDropdownTarget::Main => &self.main_model_search,
            ProviderDropdownTarget::Title => &self.title_model_search,
        }
    }

    fn resolved_ai_config_for_provider(
        settings: &Settings,
        provider: AiProvider,
    ) -> ResolvedAiConfig {
        let config = settings.ai.provider_config(provider);
        let model = config
            .models
            .first()
            .map(|model| model.id.clone())
            .unwrap_or_else(|| provider.default_model().to_string());
        let base_url_overridden = config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            model,
            api_key: config.api_key,
            base_url: config
                .base_url
                .unwrap_or_else(|| provider.default_base_url().to_string()),
            base_url_overridden,
            thinking_level: AiThinkingLevel::Off,
        }
    }

    fn model_request_signature(config: &ResolvedAiConfig) -> ModelRequestSignature {
        ModelRequestSignature {
            provider: config.provider,
            base_url: config.base_url.clone(),
            has_api_key: !config.api_key.is_empty(),
        }
    }

    fn current_provider_request_signature(
        &self,
        provider: AiProvider,
        cx: &mut Context<Self>,
    ) -> ModelRequestSignature {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        Self::model_request_signature(&config)
    }

    fn select_model(
        &mut self,
        target: ProviderDropdownTarget,
        provider: AiProvider,
        model: String,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let model_ref = Some(ModelRef::new(provider, model));
        match target {
            ProviderDropdownTarget::Main => {
                settings.ai.main_model = model_ref;
                self.main_model_picker.open = false;
                self.main_model_picker.anchor = None;
                self.main_model_picker.query = SharedString::default();
            }
            ProviderDropdownTarget::Title => {
                settings.ai.title_model = model_ref;
                self.title_model_picker.open = false;
                self.title_model_picker.anchor = None;
                self.title_model_picker.query = SharedString::default();
            }
        }
        self.model_search_input(target).update(cx, |input, cx| {
            input.set_content(SharedString::default(), cx);
        });
        self.persist_settings(settings, cx);
    }

    fn fetch_provider_models(
        &mut self,
        provider: AiProvider,
        anchor: Option<GpuiPoint<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        let signature = Self::model_request_signature(&config);
        {
            let picker = &mut self.provider_model_picker;
            picker.loading = true;
            picker.status = Some(crate::fl!("settings-fetching-models").into());
            picker.request = Some(signature.clone());
            picker.open = false;
            picker.anchor = anchor;
            picker.query = SharedString::default();
        }
        self.provider_model_selection.clear();
        self.fetch_model_search.update(cx, |input, cx| {
            input.set_content(SharedString::default(), cx);
        });

        self.provider_model_picker.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    list_ai_models(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.current_provider_request_signature(provider, cx) != signature {
                    return;
                }
                let picker = &mut this.provider_model_picker;
                picker.loading = false;
                match result {
                    Ok(models) => {
                        let count = models.len();
                        picker.models = models;
                        picker.status = Some(
                            crate::fl!("settings-fetched-models", count = count as i64).into(),
                        );
                        picker.open = true;
                    }
                    Err(error) => {
                        picker.status = Some(
                            crate::fl!("settings-fetch-models-failed", error = error.as_str())
                                .into(),
                        );
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn test_connection(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let settings = cx.global::<SettingsStore>().settings.clone();
        let config = Self::resolved_ai_config_for_provider(&settings, provider);
        let signature = Self::model_request_signature(&config);
        {
            let picker = &mut self.provider_model_picker;
            picker.loading = true;
            picker.status = Some(crate::fl!("settings-testing-connection").into());
            picker.request = Some(signature.clone());
        }

        self.provider_model_picker.task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    test_ai_connection(config).map_err(|error| format!("{error:#}"))
                })
                .await;
            _ = this.update(cx, |this, cx| {
                if this.current_provider_request_signature(provider, cx) != signature {
                    return;
                }
                let picker = &mut this.provider_model_picker;
                picker.loading = false;
                picker.status = Some(match result {
                    Ok(()) => crate::fl!("settings-connection-succeeded").into(),
                    Err(error) => {
                        crate::fl!("settings-connection-failed", error = error.as_str()).into()
                    }
                });
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn sync_provider_inputs(
        &mut self,
        provider: AiProvider,
        config: AiProviderConfig,
        cx: &mut Context<Self>,
    ) {
        self.ai_model_input.update(cx, |input, cx| {
            input.placeholder = crate::fl!("settings-model-id-placeholder").into();
            input.set_content("".into(), cx);
        });
        self.ai_key_input.update(cx, |input, cx| {
            input.placeholder = crate::fl!("settings-api-key-placeholder").into();
            input.set_content(config.api_key.clone().into(), cx);
        });
        self.ai_base_url_input.update(cx, |input, cx| {
            input.placeholder = provider.default_base_url().into();
            input.set_content(Self::base_url_input_content(provider, &config), cx);
        });
    }

    fn select_ai_provider(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        self.selected_ai_provider = provider;
        let settings = cx.global::<SettingsStore>().settings.clone();
        self.sync_provider_inputs(provider, settings.ai.provider_config(provider), cx);
        self.provider_model_picker.open = false;
        self.provider_model_picker.anchor = None;
        self.provider_model_selection.clear();
        cx.notify();
    }

    fn dismiss(&mut self, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    fn close(&mut self, _: &PanelClose, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss(window, cx);
    }

    fn change_settings(
        &mut self,
        update: impl FnOnce(&mut Settings),
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings);
        self.persist_settings(settings, cx);
    }

    fn persist_theme_color_override(
        &mut self,
        update: fn(&mut PartialTheme, u32),
        color: u32,
        cx: &mut Context<Self>,
    ) {
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        update(&mut settings.overrides, color);
        self.persist_settings(settings, cx);
    }

    fn open_color_picker(
        &mut self,
        config: ColorRowConfig,
        update: fn(&mut PartialTheme, u32),
        anchor: GpuiPoint<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_color_picker = Some(ActiveColorPicker {
            label: config.label,
            anchor,
            current: config.current,
            swatches: config.swatches,
            with_alpha: config.with_alpha,
            update,
        });
        let value = format_color_hex(config.current, config.with_alpha);
        self.color_picker_input
            .update(cx, |input, cx| input.set_content(value.into(), cx));
        self.color_picker_input
            .read(cx)
            .focus_handle
            .clone()
            .focus(window, cx);
        cx.notify();
    }

    fn set_active_color_picker_color(
        &mut self,
        color: u32,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut picker) = self.active_color_picker else {
            return;
        };
        picker.current = color;
        self.active_color_picker = Some(picker);
        let value = format_color_hex(color, picker.with_alpha);
        self.color_picker_input
            .update(cx, |input, cx| input.set_content(value.into(), cx));
        self.persist_theme_color_override(picker.update, color, cx);
    }

    fn export(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let config_path = cx.global::<SettingsStore>().config_path.clone();
        let directory = config_path
            .parent()
            .map(Path::to_path_buf)
            .filter(|path| path.exists())
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let dialog = cx.prompt_for_new_path(&directory, Some("config.kdl"));

        cx.spawn(async move |this, cx| {
            let result = dialog.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(Some(path))) => match cx.global::<SettingsStore>().export_to(&path) {
                        Ok(()) => {
                            this.save_error = None;
                            this.status = Some(
                                crate::fl!(
                                    "settings-exported-to",
                                    path = path.display().to_string()
                                )
                                .into(),
                            );
                        }
                        Err(error) => {
                            this.status = None;
                            this.save_error = Some(format!("{error:#}").into());
                        }
                    },
                    Ok(Err(error)) => {
                        this.status = None;
                        this.save_error = Some(format!("{error:#}").into());
                    }
                    Ok(Ok(None)) | Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn settings_nav(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = visible_settings_nav_entries(self.ai_expanded)
            .map(|entry| self.nav_entry(entry, theme, cx))
            .collect::<Vec<_>>();

        div()
            .w(px(220.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .border_r(px(1.0))
            .border_color(rgb(theme.border))
            .bg(rgb(theme.panel_background))
            .children(entries)
    }

    fn nav_entry(
        &self,
        entry: SettingsNavEntry,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = match entry {
            SettingsNavEntry::Root(tab) => self.active_tab == tab,
            SettingsNavEntry::AiParent => false,
            SettingsNavEntry::AiChild(section) => self.active_section == section,
        };
        let row = div()
            .id(entry.element_id())
            .w_full()
            .h(px(28.0))
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(if selected {
                rgb(theme.border_variant)
            } else {
                rgba(0x00000000)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer();

        match entry {
            SettingsNavEntry::Root(tab) => row
                .px_2()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.active_tab = tab;
                        this.active_section = tab.first_section();
                        this.content_scroll.scroll_to_top_of_item(0);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(tab.label())
                .into_any_element(),
            SettingsNavEntry::AiParent => {
                let expanded = self.ai_expanded;
                row.px_1()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.ai_expanded = !this.ai_expanded;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(px(16.0))
                            .flex_none()
                            .text_color(rgb(theme.text_muted))
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                    .child(crate::fl!("ai-title"))
                    .into_any_element()
            }
            SettingsNavEntry::AiChild(section) => {
                let section_index = section.index_in_tab();
                row.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.select_section(section);
                        this.content_scroll.scroll_to_top_of_item(section_index);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(28.0))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(div().w(px(1.0)).h_full().bg(rgb(theme.border_variant))),
                )
                .child(div().px_1().child(section.label()))
                .into_any_element()
            }
        }
    }

    fn number_row(
        &self,
        label: &'static str,
        value: f32,
        decrement: impl FnOnce(&mut Settings) + Clone + 'static,
        increment: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(theme.text)).child(label))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(self.small_button(
                        format!("number-row-{label}-decrement"),
                        "−",
                        decrement,
                        theme,
                        cx,
                    ))
                    .child(
                        div()
                            .w(px(72.0))
                            .text_center()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{value:.2}")),
                    )
                    .child(self.small_button(
                        format!("number-row-{label}-increment"),
                        "+",
                        increment,
                        theme,
                        cx,
                    )),
            )
    }

    fn toggle_row(
        &self,
        label: &'static str,
        enabled: bool,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .child(div().text_color(rgb(theme.text)).child(label))
            .child(
                div()
                    .id(format!("settings-toggle-{label}"))
                    .h(px(28.0))
                    .min_w(px(56.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .border_color(rgb(theme.border_variant))
                    .bg(if enabled {
                        rgb(theme.element_selected)
                    } else {
                        rgb(theme.surface_background)
                    })
                    .text_color(rgb(if enabled {
                        theme.accent
                    } else {
                        theme.text_muted
                    }))
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.change_settings(update.clone(), window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(if enabled { "On" } else { "Off" }),
            )
    }

    /// Compact On/Off toggle switch used in the provider list rows.
    fn compact_toggle(
        &self,
        id: impl Into<gpui::ElementId>,
        enabled: bool,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(20.0))
            .min_w(px(38.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(if enabled {
                rgb(theme.element_selected)
            } else {
                rgb(theme.surface_background)
            })
            .text_color(rgb(if enabled {
                theme.accent
            } else {
                theme.text_muted
            }))
            .text_size(ui_rems(11.0))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(update.clone(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(if enabled { "On" } else { "Off" })
    }

    fn text_input_row(
        &self,
        label: &'static str,
        input: Entity<SettingsTextInput>,
        theme: Theme,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(132.0))
                    .text_color(rgb(theme.text))
                    .child(label),
            )
            .child(input)
    }

    fn small_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        update: impl FnOnce(&mut Settings) + Clone + 'static,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(update.clone(), window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn reset_font_settings_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-reset-fonts")
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let defaults = FontSettings::default();
                    this.font_picker.open = false;
                    this.font_picker.anchor = None;
                    this.font_search_input
                        .update(cx, |input, cx| input.set_content("".into(), cx));
                    this.ui_font_fallbacks_input.update(cx, |input, cx| {
                        input.set_content(Self::font_fallbacks_input_value(&defaults), cx);
                    });
                    this.change_settings(move |settings| settings.font = defaults, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(crate::fl!("settings-reset-fonts"))
    }

    fn color_row(
        &self,
        config: ColorRowConfig,
        update: fn(&mut PartialTheme, u32),
        reset: fn(&mut PartialTheme),
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().flex().flex_col().py_2().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .child(div().text_color(rgb(theme.text)).child(config.label))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .when(config.override_value.is_some(), |this| {
                            this.child(self.small_button(
                                format!("color-row-{}-reset", config.label),
                                "Reset",
                                move |settings| reset(&mut settings.overrides),
                                theme,
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id(format!("color-row-{}-pick", config.label))
                                .h(px(28.0))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .border_1()
                                .border_color(rgb(theme.border_variant))
                                .bg(rgb(theme.surface_background))
                                .hover(move |style| style.bg(rgb(theme.element_hover)))
                                .cursor_pointer()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        let anchor = point(
                                            event.position.x - px(120.0),
                                            event.position.y + px(18.0),
                                        );
                                        this.open_color_picker(config, update, anchor, window, cx);
                                        cx.stop_propagation();
                                    }),
                                )
                                .child(
                                    div()
                                        .w(px(16.0))
                                        .h(px(16.0))
                                        .border_1()
                                        .border_color(rgb(theme.border_variant))
                                        .bg(if config.with_alpha {
                                            rgba(config.current)
                                        } else {
                                            rgb(config.current)
                                        }),
                                )
                                .child("Pick"),
                        ),
                ),
        )
    }

    fn color_picker_overlay(&self, theme: Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.active_color_picker?;
        let input = self.color_picker_input.clone();
        let value = input.read(cx).content.to_string();
        let parsed = parse_color_input(&value, picker.with_alpha);
        let preview = parsed.unwrap_or(picker.current);
        let hint = if picker.with_alpha {
            "Use #rrggbb or #rrggbbaa"
        } else {
            "Use #rrggbb"
        };

        Some(
            deferred(
                anchored()
                    .position(picker.anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .w(px(320.0))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_3()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(rgb(theme.border_variant))
                            .bg(rgb(theme.surface_background))
                            .shadow_sm()
                            .overflow_hidden()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.active_color_picker = None;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_color(rgb(theme.text))
                                            .child(format!("Pick {}", picker.label)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(theme.text_muted))
                                            .child(format_color_hex(preview, picker.with_alpha)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .w(px(48.0))
                                            .h(px(48.0))
                                            .flex_none()
                                            .border_1()
                                            .border_color(rgb(theme.border_variant))
                                            .bg(if picker.with_alpha {
                                                rgba(preview)
                                            } else {
                                                rgb(preview)
                                            }),
                                    )
                                    .child(div().flex_1().flex().child(input)),
                            )
                            .child(
                                div()
                                    .text_size(ui_rems(11.0))
                                    .text_color(rgb(if parsed.is_some() {
                                        theme.text_muted
                                    } else {
                                        theme.error
                                    }))
                                    .child(if parsed.is_some() {
                                        hint
                                    } else {
                                        "Invalid color"
                                    }),
                            )
                            .child(div().flex().flex_wrap().gap_2().children(
                                picker.swatches.iter().copied().map(move |color| {
                                    let selected = color == picker.current;
                                    div()
                                        .id(format!("color-picker-swatch-{color:08x}"))
                                        .w(px(28.0))
                                        .h(px(28.0))
                                        .border_2()
                                        .border_color(if selected {
                                            rgb(theme.text)
                                        } else {
                                            rgb(theme.border_variant)
                                        })
                                        .bg(if picker.with_alpha {
                                            rgba(color)
                                        } else {
                                            rgb(color)
                                        })
                                        .hover(move |style| style.border_color(rgb(theme.text)))
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, window, cx| {
                                                this.set_active_color_picker_color(
                                                    color, window, cx,
                                                );
                                                cx.stop_propagation();
                                            }),
                                        )
                                }),
                            )),
                    ),
            )
            .into_any_element(),
        )
    }

    fn theme_mode_option(
        &self,
        label: &'static str,
        is_dynamic: bool,
        current_is_dynamic: bool,
        system_appearance: ThemeAppearance,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = is_dynamic == current_is_dynamic;

        div()
            .id(format!("theme-mode-{label}"))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let (new_theme, save_dynamic) = {
                        let store = cx.global::<SettingsStore>();
                        match &store.settings.theme {
                            ThemeSelection::Static(name) if is_dynamic => {
                                let (light, dark) =
                                    match (&store.last_dynamic_light, &store.last_dynamic_dark) {
                                        (Some(l), Some(d)) => (l.clone(), d.clone()),
                                        _ => (name.clone(), name.clone()),
                                    };
                                (
                                    Some(ThemeSelection::Dynamic {
                                        mode: ThemeAppearanceMode::System,
                                        light,
                                        dark,
                                    }),
                                    None,
                                )
                            }
                            ThemeSelection::Dynamic { mode, light, dark } if !is_dynamic => {
                                let active = match mode {
                                    ThemeAppearanceMode::Light => light.clone(),
                                    ThemeAppearanceMode::Dark => dark.clone(),
                                    ThemeAppearanceMode::System => match system_appearance {
                                        ThemeAppearance::Light => light.clone(),
                                        ThemeAppearance::Dark => dark.clone(),
                                    },
                                };
                                (
                                    Some(ThemeSelection::Static(active)),
                                    Some((light.clone(), dark.clone())),
                                )
                            }
                            _ => (None, None),
                        }
                    };
                    if let Some((light, dark)) = save_dynamic {
                        let store = cx.global_mut::<SettingsStore>();
                        store.last_dynamic_light = Some(light);
                        store.last_dynamic_dark = Some(dark);
                    }
                    if let Some(new_theme) = new_theme {
                        this.change_settings(
                            move |settings| settings.theme = new_theme,
                            window,
                            cx,
                        );
                    }
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn appearance_mode_option(
        &self,
        label: &'static str,
        mode: ThemeAppearanceMode,
        selected_mode: ThemeAppearanceMode,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = mode == selected_mode;

        div()
            .id(format!("appearance-mode-{label}"))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| {
                            if let ThemeSelection::Dynamic {
                                mode: mode_field, ..
                            } = &mut settings.theme
                            {
                                *mode_field = mode;
                            }
                        },
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn background_option(
        &self,
        label: &'static str,
        mode: WindowBackgroundMode,
        selected_mode: WindowBackgroundMode,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = mode == selected_mode;

        div()
            .id(format!("background-mode-{label}"))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.window.background = mode,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn tab_bar_visibility_option(
        &self,
        label: &'static str,
        visibility: TabBarVisibility,
        selected_visibility: TabBarVisibility,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = visibility == selected_visibility;

        div()
            .id(format!("tab-bar-visibility-{label}"))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.tab_bar_visibility = visibility,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn language_option(
        &self,
        language: AppLanguage,
        selected_language: AppLanguage,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = language == selected_language;

        div()
            .id(format!("settings-language-{}", language.label()))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(move |settings| settings.language = language, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(language.label())
    }

    fn keymap_option(
        &self,
        preset: KeymapPreset,
        selected_preset: KeymapPreset,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = preset == selected_preset;

        div()
            .id(format!("settings-keymap-{}", preset.label()))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .border_1()
            .border_color(if selected {
                rgb(theme.accent)
            } else {
                rgb(theme.border_variant)
            })
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.change_settings(
                        move |settings| settings.keymap_preset = preset,
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(preset.label())
    }

    fn keybinding_button(
        &self,
        label: impl IntoElement,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("settings-open-keybindings")
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    on_click(this, window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn font_picker_entries(&self, selected_family: &str, query: &str) -> Vec<String> {
        let query = query.trim().to_ascii_lowercase();
        let mut fonts = self.font_picker.fonts.clone();
        if !fonts
            .iter()
            .any(|font| font.eq_ignore_ascii_case(selected_family))
        {
            fonts.push(selected_family.to_string());
            fonts.sort_by_cached_key(|font| font.to_ascii_lowercase());
            fonts.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        }
        if !query.is_empty() {
            fonts.retain(|font| font.to_ascii_lowercase().contains(&query));
        }
        fonts
    }

    fn font_family_row(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .w(px(132.0))
                    .text_color(rgb(theme.text))
                    .child(crate::fl!("settings-ui-font-family")),
            )
            .child(self.font_family_control(settings, theme, cx))
    }

    fn font_family_control(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.font_picker.open;
        let font_family = settings.font.ui_font_family.clone();

        div()
            .w(px(280.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(rgb(if open {
                theme.accent
            } else {
                theme.border_variant
            }))
            .bg(rgb(theme.surface_background))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let open = !this.font_picker.open;
                    this.main_model_picker.open = false;
                    this.main_model_picker.anchor = None;
                    this.title_model_picker.open = false;
                    this.title_model_picker.anchor = None;
                    this.font_picker.open = open;
                    this.font_picker.anchor = open
                        .then(|| point(event.position.x - px(8.0), event.position.y + px(20.0)));
                    this.font_search_input.update(cx, |input, cx| {
                        input.set_content("".into(), cx);
                    });
                    if open {
                        this.font_search_input
                            .read(cx)
                            .focus_handle
                            .clone()
                            .focus(window, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(div().flex_1().min_w_0().truncate().child(font_family))
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if open { "⌃" } else { "⌄" }),
            )
    }

    fn select_font_family(&mut self, family: String, cx: &mut Context<Self>) {
        self.font_picker.open = false;
        self.font_picker.anchor = None;
        self.font_search_input.update(cx, |input, cx| {
            input.set_content("".into(), cx);
        });
        self.update_font_settings(move |font| font.ui_font_family = family, cx);
    }

    fn font_picker_option(
        family: String,
        selected_family: String,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = family == selected_family;
        let family_for_click = family.clone();

        div()
            .id(format!("settings-font-{family}"))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded(px(4.0))
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected { theme.accent } else { theme.text }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_font_family(family_for_click.clone(), cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(18.0))
                    .flex_none()
                    .text_color(rgb(theme.accent))
                    .child(if selected { "✓" } else { "" }),
            )
            .child(div().flex_1().min_w_0().truncate().child(family))
            .into_any_element()
    }

    fn font_picker_menu(
        &self,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = settings.font.ui_font_family.clone();
        let query = self.font_search_input.read(cx).content.to_string();
        let fonts = self.font_picker_entries(&selected, &query);
        let entry_count = fonts.len();
        let scroll_handle = self.font_picker.scroll_handle.clone();
        let search_input = self.font_search_input.clone();
        let rows = fonts
            .into_iter()
            .map(|font| Self::font_picker_option(font, selected.clone(), theme, cx))
            .collect::<Vec<_>>();

        div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                div()
                    .px_2()
                    .py_2()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border_variant))
                    .flex()
                    .child(search_input),
            )
            .when(entry_count == 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_color(rgb(theme.text_muted))
                        .child(crate::fl!("settings-no-matching-fonts")),
                )
            })
            .when(entry_count > 0, |this| {
                this.child(
                    div()
                        .id("settings-font-family-list")
                        .track_scroll(&scroll_handle)
                        .max_h(px(320.0))
                        .py_1()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .children(rows)
                        .vertical_scrollbar_for(&scroll_handle, window, cx),
                )
            })
    }

    fn font_picker_overlay(
        &self,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.font_picker.open {
            return None;
        }
        let anchor = self.font_picker.anchor?;
        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.font_picker_menu(settings, theme, window, cx)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn model_selector_control(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let picker = self.model_picker(target);
        let selected = match target {
            ProviderDropdownTarget::Main => settings.ai.main_model.as_ref(),
            ProviderDropdownTarget::Title => settings.ai.title_model.as_ref(),
        };
        let open = picker.open;
        let model_label = selected
            .map(|model_ref| settings.ai.model_label(model_ref))
            .unwrap_or_else(|| "Select model".to_string());
        let search_input = self.model_search_input(target).clone();

        div()
            .w(px(280.0))
            .h(px(32.0))
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_1()
            .border_color(rgb(if open {
                theme.accent
            } else {
                theme.border_variant
            }))
            .bg(rgb(theme.surface_background))
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.main_model_picker.open = false;
                    this.main_model_picker.anchor = None;
                    this.title_model_picker.open = false;
                    this.title_model_picker.anchor = None;
                    let picker = this.model_picker_mut(target);
                    picker.open = true;
                    picker.anchor = Some(point(
                        event.position.x - px(8.0),
                        event.position.y + px(20.0),
                    ));
                    this.model_search_input(target)
                        .read(cx)
                        .focus_handle
                        .clone()
                        .focus(window, cx);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(!open, |this| {
                        this.child(div().truncate().child(model_label))
                    })
                    .when(open, |this| this.child(search_input)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(rgb(theme.text_muted))
                    .child(if open { "⌃" } else { "⌄" }),
            )
    }

    fn settings_model_entries(settings: &Settings, query: &str) -> Vec<SettingsModelMenuEntry> {
        let query = query.trim().to_ascii_lowercase();
        let mut entries = Vec::new();
        for provider in AiProvider::ALL {
            let config = settings.ai.provider_config(provider);
            if !config.enabled || config.models.is_empty() {
                continue;
            }
            let provider_matches = provider.label().to_ascii_lowercase().contains(&query);
            let mut model_entries = config
                .models
                .into_iter()
                .filter(|model| {
                    query.is_empty()
                        || provider_matches
                        || model.id.to_ascii_lowercase().contains(&query)
                        || model.name.to_ascii_lowercase().contains(&query)
                        || model.group.to_ascii_lowercase().contains(&query)
                })
                .map(move |model| SettingsModelMenuEntry::Model(provider, model))
                .collect::<Vec<_>>();
            if !model_entries.is_empty() {
                entries.push(SettingsModelMenuEntry::Header(provider));
                entries.append(&mut model_entries);
            }
        }
        entries
    }

    fn model_picker_option(
        target: ProviderDropdownTarget,
        entry: SettingsModelMenuEntry,
        selected: Option<ModelRef>,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match entry {
            SettingsModelMenuEntry::Header(provider) => div()
                .h(px(26.0))
                .px_3()
                .flex()
                .items_center()
                .text_size(ui_rems(11.0))
                .text_color(rgb(theme.text_muted))
                .child(provider.label())
                .into_any_element(),
            SettingsModelMenuEntry::Model(provider, model) => {
                let model_id = model.id.clone();
                let is_selected = selected.as_ref().is_some_and(|model_ref| {
                    model_ref.provider == provider && model_ref.id == model.id
                });
                div()
                    .id(format!("settings-model-{}-{}", provider.label(), model.id))
                    .h(px(30.0))
                    .pl(px(24.0))
                    .pr_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .rounded(px(4.0))
                    .bg(if is_selected {
                        rgb(theme.element_selected)
                    } else {
                        rgba(0x00000000)
                    })
                    .text_color(rgb(if is_selected {
                        theme.accent
                    } else {
                        theme.text
                    }))
                    .hover(move |style| style.bg(rgb(theme.element_hover)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.select_model(target, provider, model_id.clone(), cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(div().truncate().min_w_0().child(model.name))
                    .child(
                        div()
                            .flex_none()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.group),
                    )
                    .into_any_element()
            }
        }
    }

    fn model_picker_menu(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.model_picker(target).query.to_string();
        let entries = Self::settings_model_entries(settings, &query);
        let entry_count = entries.len();
        let selected = match target {
            ProviderDropdownTarget::Main => settings.ai.main_model.clone(),
            ProviderDropdownTarget::Title => settings.ai.title_model.clone(),
        };
        let scroll_handle = self.model_picker(target).scroll_handle.clone();
        let list_id: ElementId = match target {
            ProviderDropdownTarget::Main => "settings-main-model-list".into(),
            ProviderDropdownTarget::Title => "settings-title-model-list".into(),
        };

        let rows = entries
            .into_iter()
            .map(|entry| Self::model_picker_option(target, entry, selected.clone(), theme, cx))
            .collect::<Vec<_>>();

        div()
            .w(px(280.0))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .shadow_sm()
            .overflow_hidden()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            })
            .when(entry_count == 0, |this| {
                this.child(div().px_3().py_2().text_color(rgb(theme.text_muted)).child(
                    if query.trim().is_empty() {
                        crate::fl!("settings-no-enabled-provider-models")
                    } else {
                        crate::fl!("no-matching-models")
                    },
                ))
            })
            .when(entry_count > 0, |this| {
                this.child(
                    div()
                        .id(list_id)
                        .track_scroll(&scroll_handle)
                        .max_h(px(320.0))
                        .py_1()
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                        .children(rows)
                        .vertical_scrollbar_for(&scroll_handle, window, cx),
                )
            })
    }

    fn model_picker_overlay(
        &self,
        target: ProviderDropdownTarget,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.model_picker(target);
        if !picker.open {
            return None;
        }
        let anchor = picker.anchor?;
        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(self.model_picker_menu(target, settings, theme, window, cx)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn ai_action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl IntoElement,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    on_click(this, cx);
                    cx.stop_propagation();
                }),
            )
            .child(label)
    }

    fn provider_status(&self, theme: Theme) -> Option<impl IntoElement> {
        self.provider_model_picker
            .status
            .clone()
            .map(move |status| {
                div()
                    .text_size(ui_rems(12.0))
                    .text_color(rgb(theme.text_muted))
                    .child(status)
            })
    }

    fn base_url_default_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("settings-base-url-default-{}", provider.label()))
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let base_url = provider.default_base_url().to_string();
                    this.ai_base_url_input
                        .update(cx, |input, cx| input.set_content(base_url.into(), cx));
                    this.update_provider_config(
                        provider,
                        move |_, config| config.base_url = None,
                        cx,
                    );
                    cx.stop_propagation();
                }),
            )
            .child(crate::fl!("settings-default"))
    }

    fn section_title(label: impl IntoElement, theme: Theme) -> impl IntoElement {
        div()
            .pt_2()
            .pb_1()
            .text_color(rgb(theme.text_muted))
            .child(label)
    }

    fn keymap_section(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(Self::section_title(
                crate::fl!("settings-section-keymap"),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .child(
                        div()
                            .text_color(rgb(theme.text))
                            .child(crate::fl!("settings-base-preset")),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(self.keymap_option(
                                KeymapPreset::Modern,
                                settings.keymap_preset,
                                theme,
                                cx,
                            ))
                            .child(self.keymap_option(
                                KeymapPreset::Vim,
                                settings.keymap_preset,
                                theme,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .h(px(44.0))
                    .child(
                        div()
                            .text_color(rgb(theme.text))
                            .child(crate::fl!("settings-keymap-editor")),
                    )
                    .child(self.keybinding_button(
                        crate::fl!("settings-open-keymap"),
                        theme,
                        |this, _, cx| (this.open_keymap)(cx),
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn keymap_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![self.keymap_section(settings, theme, cx)]
    }

    fn general_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        vec![
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-language"),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(theme.text))
                                .child(crate::fl!("settings-language")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.language_option(
                                    AppLanguage::System,
                                    settings.language,
                                    theme,
                                    cx,
                                ))
                                .child(self.language_option(
                                    AppLanguage::English,
                                    settings.language,
                                    theme,
                                    cx,
                                ))
                                .child(self.language_option(
                                    AppLanguage::SimplifiedChinese,
                                    settings.language,
                                    theme,
                                    cx,
                                )),
                        ),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-zoom"),
                    theme,
                ))
                .child(self.number_row(
                    "Default zoom",
                    settings.behavior.default_zoom,
                    |settings| settings.behavior.default_zoom -= NUMBER_STEP_SMALL,
                    |settings| settings.behavior.default_zoom += NUMBER_STEP_SMALL,
                    theme,
                    cx,
                ))
                .child(self.number_row(
                    "Zoom step",
                    settings.behavior.zoom_step,
                    |settings| settings.behavior.zoom_step -= NUMBER_STEP_SMALL,
                    |settings| settings.behavior.zoom_step += NUMBER_STEP_SMALL,
                    theme,
                    cx,
                ))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-scroll"),
                    theme,
                ))
                .child(self.toggle_row(
                    "Smooth scrolling",
                    settings.behavior.smooth_scrolling,
                    |settings| {
                        settings.behavior.smooth_scrolling = !settings.behavior.smooth_scrolling
                    },
                    theme,
                    cx,
                ))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-window"),
                    theme,
                ))
                .child(self.number_row(
                    "Opacity",
                    settings.window.opacity,
                    |settings| settings.window.opacity -= OPACITY_STEP,
                    |settings| settings.window.opacity += OPACITY_STEP,
                    theme,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(theme.text))
                                .child(crate::fl!("settings-background")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.background_option(
                                    "Blurred",
                                    WindowBackgroundMode::Blurred,
                                    settings.window.background,
                                    theme,
                                    cx,
                                ))
                                .child(self.background_option(
                                    "Transparent",
                                    WindowBackgroundMode::Transparent,
                                    settings.window.background,
                                    theme,
                                    cx,
                                ))
                                .child(self.background_option(
                                    "Opaque",
                                    WindowBackgroundMode::Opaque,
                                    settings.window.background,
                                    theme,
                                    cx,
                                )),
                        ),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-tabs"),
                    theme,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(theme.text))
                                .child(crate::fl!("settings-tab-bar")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.tab_bar_visibility_option(
                                    "Auto",
                                    TabBarVisibility::Auto,
                                    settings.tab_bar_visibility,
                                    theme,
                                    cx,
                                ))
                                .child(self.tab_bar_visibility_option(
                                    "Always",
                                    TabBarVisibility::Always,
                                    settings.tab_bar_visibility,
                                    theme,
                                    cx,
                                )),
                        ),
                )
                .child(self.toggle_row(
                    "Dim inactive panes",
                    settings.behavior.dim_inactive_panes,
                    |settings| {
                        settings.behavior.dim_inactive_panes = !settings.behavior.dim_inactive_panes
                    },
                    theme,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn ai_general_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let input_row = |label: String, child: AnyElement| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .min_h(px(48.0))
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(104.0))
                        .text_color(rgb(theme.text))
                        .child(label),
                )
                .child(child)
        };

        vec![
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Self::section_title(
                    crate::fl!("settings-default-models"),
                    theme,
                ))
                .child(input_row(
                    crate::fl!("settings-main-model"),
                    self.model_selector_control(ProviderDropdownTarget::Main, settings, theme, cx)
                        .into_any_element(),
                ))
                .child(input_row(
                    crate::fl!("settings-title-model"),
                    self.model_selector_control(ProviderDropdownTarget::Title, settings, theme, cx)
                        .into_any_element(),
                ))
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(crate::fl!("settings-model-list-hint")),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-quick-ai-chat"),
                    theme,
                ))
                .child(self.number_row(
                    "Transparency",
                    settings.ai.quick_ask.transparency,
                    |settings| settings.ai.quick_ask.transparency -= OPACITY_STEP,
                    |settings| settings.ai.quick_ask.transparency += OPACITY_STEP,
                    theme,
                    cx,
                ))
                .child(self.number_row(
                    "Text size",
                    settings.ai.quick_ask.text_size,
                    |settings| settings.ai.quick_ask.text_size -= FONT_SIZE_STEP,
                    |settings| settings.ai.quick_ask.text_size += FONT_SIZE_STEP,
                    theme,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn translation_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let provider_option = |provider: TranslationProvider| {
            let selected = settings.translation.active_provider == provider;
            div()
                .id(format!("translation-provider-{}", provider.label()))
                .px_2()
                .py_1()
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(if selected { theme.accent } else { theme.border }))
                .bg(rgb(if selected {
                    theme.element_selected
                } else {
                    theme.surface_background
                }))
                .text_size(ui_rems(12.0))
                .text_color(rgb(theme.text))
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(theme.element_hover)))
                .child(provider.label())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        let mut next = cx.global::<SettingsStore>().settings.clone();
                        next.translation.active_provider = provider;
                        next.translation.provider_config_mut(provider).enabled = true;
                        this.persist_translation_settings(next, cx);
                        this.select_translation_provider(provider, cx);
                        cx.stop_propagation();
                    }),
                )
        };
        let input_row = |label: &str, input: Entity<SettingsTextInput>| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .min_h(px(42.0))
                .gap_3()
                .child(
                    div()
                        .w(px(118.0))
                        .flex_none()
                        .text_color(rgb(theme.text))
                        .child(label.to_string()),
                )
                .child(div().flex_1().min_w_0().child(input))
                .into_any_element()
        };
        let provider = self.selected_translation_provider;
        let provider_config = settings.translation.provider_config(provider);

        vec![
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Self::section_title("Translation", theme))
                .child(input_row(
                    "Target language",
                    self.translation_target_input.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(118.0))
                                .flex_none()
                                .text_color(rgb(theme.text))
                                .child("Provider"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap_2()
                                .children(
                                    TranslationProvider::ALL
                                        .into_iter()
                                        .map(provider_option)
                                        .collect::<Vec<_>>(),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child("Source language is detected automatically. A document can override the target language from its translation controls."),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(Self::section_title("Provider credentials", theme))
                .when(provider != TranslationProvider::Ai, |this| {
                    this.child(input_row("API key", self.translation_key_input.clone()))
                        .child(input_row(
                            "API endpoint",
                            self.translation_base_url_input.clone(),
                        ))
                        .when(provider == TranslationProvider::Microsoft, |this| {
                            this.child(input_row(
                                "Azure region",
                                self.translation_region_input.clone(),
                            ))
                        })
                })
                .when(provider == TranslationProvider::Ai, |this| {
                    this.child(
                        div()
                            .text_color(rgb(theme.text_muted))
                            .child("Uses the configured AI main model and its credentials."),
                    )
                })
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(if provider_config.enabled {
                            theme.success
                        } else {
                            theme.text_muted
                        }))
                        .child(if provider_config.enabled {
                            "Enabled"
                        } else {
                            "Disabled"
                        }),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title("Pinned translation", theme))
                .child(self.toggle_row(
                    "Translate selections automatically",
                    settings.translation.pinned.enabled,
                    |settings| {
                        settings.translation.pinned.enabled = !settings.translation.pinned.enabled
                    },
                    theme,
                    cx,
                ))
                .child(self.number_row(
                    "Floating panel width",
                    settings.translation.pinned.width,
                    |settings| settings.translation.pinned.width -= FONT_SIZE_STEP * 8.0,
                    |settings| settings.translation.pinned.width += FONT_SIZE_STEP * 8.0,
                    theme,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(theme.text))
                                .child("Panel corner"),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .children(
                                    [
                                        TranslationBoxCorner::TopLeft,
                                        TranslationBoxCorner::TopRight,
                                        TranslationBoxCorner::BottomLeft,
                                        TranslationBoxCorner::BottomRight,
                                    ]
                                    .into_iter()
                                    .map(|corner| {
                                        let selected = settings.translation.pinned.corner == corner;
                                        div()
                                            .id(format!(
                                                "translation-panel-corner-{}",
                                                corner.as_str()
                                            ))
                                            .px_2()
                                            .py_1()
                                            .rounded(px(4.0))
                                            .border_1()
                                            .border_color(rgb(if selected {
                                                theme.accent
                                            } else {
                                                theme.border
                                            }))
                                            .cursor_pointer()
                                            .hover(move |style| {
                                                style.bg(rgb(theme.element_hover))
                                            })
                                            .child(corner.as_str())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    let mut next =
                                                        cx.global::<SettingsStore>().settings.clone();
                                                    next.translation.pinned.corner = corner;
                                                    this.persist_translation_settings(next, cx);
                                                    cx.stop_propagation();
                                                }),
                                            )
                                    })
                                    .collect::<Vec<_>>(),
                                ),
                        ),
                )
                .into_any_element(),
        ]
    }

    fn ai_providers_tab(
        &self,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let provider = self.selected_ai_provider;
        let config = settings.ai.provider_config(provider);
        let provider_entries = AiProvider::ALL
            .into_iter()
            .map(|provider| {
                self.provider_sidebar_entry(
                    provider,
                    settings.ai.provider_config(provider).enabled,
                    theme,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let model_rows = config
            .models
            .iter()
            .cloned()
            .map(|model| self.provider_model_row(provider, model, theme, cx))
            .collect::<Vec<_>>();

        // Provider list column: full height, independent scroll.
        let provider_list_column = div()
            .w(px(232.0))
            .flex_none()
            .flex()
            .flex_col()
            .min_h_0()
            .border_r(px(1.0))
            .border_color(rgb(theme.border_variant))
            .child(
                div()
                    .id("settings-provider-list")
                    .track_scroll(&self.provider_list_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(provider_entries)
                    .vertical_scrollbar_for(&self.provider_list_scroll, window, cx),
            );

        // API Key field: stacked titled field (label above, control row below).
        let key_masked = self.ai_key_input.read(cx).mask;
        let api_key_field = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(ui_rems(13.0))
                    .text_color(rgb(theme.text))
                    .child(crate::fl!("settings-api-key")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(self.ai_key_input.clone())
                    .child(
                        div()
                            .id("settings-toggle-api-key-mask")
                            .h(px(28.0))
                            .min_w(px(40.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_1()
                            .border_color(rgb(theme.border_variant))
                            .bg(rgb(theme.surface_background))
                            .text_color(rgb(theme.text_muted))
                            .text_size(ui_rems(12.0))
                            .hover(move |style| style.bg(rgb(theme.element_hover)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.ai_key_input.update(cx, |input, cx| {
                                        input.mask = !input.mask;
                                        cx.notify();
                                    });
                                    cx.stop_propagation();
                                }),
                            )
                            .child(if key_masked { "Show" } else { "Hide" }),
                    )
                    .child(self.ai_action_button(
                        "settings-test-ai-connection",
                        "Test",
                        theme,
                        move |this, cx| this.test_connection(provider, cx),
                        cx,
                    )),
            );

        // API Host field: stacked titled field.
        let api_host_field = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(ui_rems(13.0))
                    .text_color(rgb(theme.text))
                    .child(crate::fl!("settings-api-host")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(self.ai_base_url_input.clone())
                    .child(self.base_url_default_button(provider, theme, cx)),
            )
            .when_some(self.provider_status(theme), |this, status| {
                this.child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(theme.text_muted))
                        .child(status),
                )
            });

        // Model List section: title + action row + list rows.
        let model_list_section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Self::section_title(
                crate::fl!("settings-model-list"),
                theme,
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .child(self.fetch_models_button(provider, theme, cx))
                    .child(self.ai_model_input.clone())
                    .child(self.add_manual_model_button(provider, theme, cx)),
            )
            .child(div().flex().flex_col().gap_1().children(model_rows).when(
                config.models.is_empty(),
                |this| {
                    this.child(
                        div()
                            .py_2()
                            .text_color(rgb(theme.text_muted))
                            .child(crate::fl!("settings-no-models-added")),
                    )
                },
            ));

        // Details column: header (name only) + scrollable sectioned body.
        let details_column = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .px_6()
                    .pt_4()
                    .pb_3()
                    .border_b(px(1.0))
                    .border_color(rgb(theme.border_variant))
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .text_color(rgb(theme.text))
                            .child(provider.label()),
                    ),
            )
            .child(
                div()
                    .id("settings-provider-details")
                    .track_scroll(&self.content_scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(api_key_field)
                            .child(api_host_field)
                            .child(model_list_section),
                    )
                    .vertical_scrollbar_for(&self.content_scroll, window, cx),
            );

        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .child(provider_list_column)
            .child(details_column)
            .into_any_element()
    }

    fn provider_sidebar_entry(
        &self,
        provider: AiProvider,
        enabled: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_ai_provider == provider;
        div()
            .id(format!("settings-provider-{}", provider.label()))
            .h(px(34.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .bg(if selected {
                rgb(theme.element_selected)
            } else {
                rgba(0x00000000)
            })
            .text_color(rgb(if selected {
                theme.text
            } else {
                theme.text_muted
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_ai_provider(provider, cx);
                    cx.stop_propagation();
                }),
            )
            .child(div().truncate().child(provider.label()))
            .child(self.compact_toggle(
                format!("settings-provider-toggle-{}", provider.label()),
                enabled,
                move |settings| {
                    let config = settings.ai.provider_config_mut(provider);
                    config.enabled = !config.enabled;
                },
                theme,
                cx,
            ))
            .into_any_element()
    }

    fn provider_model_row(
        &self,
        provider: AiProvider,
        model: AiModel,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let model_id = model.id.clone();
        div()
            .min_h(px(42.0))
            .px_2()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.panel_background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(div().truncate().child(model.name))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.id),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.group),
                    )
                    .child(self.remove_model_button(provider, model_id, theme, cx)),
            )
            .into_any_element()
    }

    fn remove_model_button(
        &self,
        provider: AiProvider,
        model_id: String,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let button_id = format!("settings-remove-model-{}-{model_id}", provider.label());
        div()
            .id(button_id)
            .h(px(24.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let mut settings = cx.global::<SettingsStore>().settings.clone();
                    settings
                        .ai
                        .provider_config_mut(provider)
                        .remove_model(&model_id);
                    if settings.ai.main_model.as_ref().is_some_and(|model_ref| {
                        model_ref.provider == provider && model_ref.id == model_id
                    }) {
                        settings.ai.main_model = None;
                    }
                    if settings.ai.title_model.as_ref().is_some_and(|model_ref| {
                        model_ref.provider == provider && model_ref.id == model_id
                    }) {
                        settings.ai.title_model = None;
                    }
                    this.persist_settings(settings, cx);
                    cx.stop_propagation();
                }),
            )
            .child(crate::fl!("remove"))
    }

    fn add_manual_model_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(format!("settings-add-model-{}", provider.label()))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    let model_id = this.ai_model_input.read(cx).content.trim().to_string();
                    if model_id.is_empty() {
                        cx.stop_propagation();
                        return;
                    }
                    let mut settings = cx.global::<SettingsStore>().settings.clone();
                    settings
                        .ai
                        .provider_config_mut(provider)
                        .ensure_model(AiModel::new(model_id.clone(), model_id, provider.label()));
                    this.persist_settings(settings, cx);
                    this.ai_model_input
                        .update(cx, |input, cx| input.set_content("".into(), cx));
                    cx.stop_propagation();
                }),
            )
            .child(crate::fl!("add"))
    }

    fn fetch_models_button(
        &self,
        provider: AiProvider,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loading = self.provider_model_picker.loading;
        div()
            .id(format!("settings-fetch-models-{}", provider.label()))
            .h(px(28.0))
            .px_2()
            .flex()
            .items_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let anchor = point(event.position.x - px(420.0), event.position.y + px(18.0));
                    this.fetch_provider_models(provider, Some(anchor), cx);
                    cx.stop_propagation();
                }),
            )
            .child(if loading {
                "Fetching…"
            } else {
                "Fetch models"
            })
    }

    fn toggle_fetched_model(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(index) = self
            .provider_model_selection
            .iter()
            .position(|selected| selected == &id)
        {
            self.provider_model_selection.remove(index);
        } else {
            self.provider_model_selection.push(id);
        }
        cx.notify();
    }

    fn confirm_fetched_models(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let selected = self.provider_model_selection.clone();
        if selected.is_empty() {
            self.provider_model_picker.open = false;
            self.provider_model_picker.anchor = None;
            cx.notify();
            return;
        }

        let fetched = self.provider_model_picker.models.clone();
        let mut settings = cx.global::<SettingsStore>().settings.clone();
        let config = settings.ai.provider_config_mut(provider);
        for model in fetched {
            if !selected.iter().any(|id| id == &model.id) {
                continue;
            }
            let group = model
                .owned_by
                .unwrap_or_else(|| provider.label().to_string());
            config.ensure_model(AiModel::new(model.id, model.label, group));
        }
        self.persist_settings(settings, cx);
        self.provider_model_picker.open = false;
        self.provider_model_picker.anchor = None;
        self.provider_model_selection.clear();
        cx.notify();
    }

    fn provider_model_fetch_overlay(
        &self,
        provider: AiProvider,
        settings: &Settings,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.provider_model_picker.open {
            return None;
        }
        let anchor = self.provider_model_picker.anchor?;
        let query = self.provider_model_picker.query.trim().to_ascii_lowercase();
        let models = self
            .provider_model_picker
            .models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.id.to_ascii_lowercase().contains(&query)
                    || model.label.to_ascii_lowercase().contains(&query)
            })
            .cloned()
            .collect::<Vec<_>>();
        let total_count = self.provider_model_picker.models.len();
        let selected = self.provider_model_selection.clone();
        let existing = settings.ai.provider_config(provider).models;
        let entry_count = models.len();
        let scroll_handle = self.provider_model_picker.fetch_scroll_handle.clone();
        let search_input = self.fetch_model_search.clone();

        Some(
            deferred(
                anchored()
                    .position(anchor)
                    .anchor(Anchor::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .w(px(440.0))
                            .flex()
                            .flex_col()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(rgb(theme.border_variant))
                            .bg(rgb(theme.surface_background))
                            .shadow_sm()
                            .overflow_hidden()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_b(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .text_color(rgb(theme.text))
                                    .child(crate::fl!("settings-choose-models-to-add")),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_b(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .flex()
                                    .child(search_input),
                            )
                            .when(entry_count == 0, |this| {
                                this.child(
                                    div().px_3().py_2().text_color(rgb(theme.text_muted)).child(
                                        if total_count == 0 {
                                            crate::fl!("settings-no-models-returned")
                                        } else {
                                            crate::fl!("settings-no-matching-models-period")
                                        },
                                    ),
                                )
                            })
                            .when(entry_count > 0, |this| {
                                let list_height = 42.0 * entry_count.min(9) as f32;
                                this.child(
                                    div()
                                        .h(px(list_height))
                                        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                        .child(
                                            uniform_list(
                                                "settings-provider-fetched-models",
                                                entry_count,
                                                cx.processor(
                                                    move |_, visible_range: Range<usize>, _, cx| {
                                                        visible_range
                                                            .map(|ix| {
                                                                let model = models[ix].clone();
                                                                let already_added =
                                                                    existing.iter().any(|entry| {
                                                                        entry.id == model.id
                                                                    });
                                                                let checked = selected
                                                                    .iter()
                                                                    .any(|id| id == &model.id);
                                                                Self::fetched_model_row(
                                                                    provider,
                                                                    model,
                                                                    already_added,
                                                                    checked,
                                                                    theme,
                                                                    cx,
                                                                )
                                                            })
                                                            .collect::<Vec<_>>()
                                                    },
                                                ),
                                            )
                                            .track_scroll(&scroll_handle)
                                            .with_decoration(uniform_list_scrollbar_for(
                                                &scroll_handle,
                                                "settings-provider-fetched-models",
                                                window,
                                                cx,
                                            ))
                                            .size_full(),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .border_t(px(1.0))
                                    .border_color(rgb(theme.border_variant))
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(self.ai_action_button(
                                        "settings-cancel-fetched-models",
                                        crate::fl!("cancel"),
                                        theme,
                                        |this, cx| {
                                            this.provider_model_picker.open = false;
                                            this.provider_model_picker.anchor = None;
                                            this.provider_model_selection.clear();
                                            cx.notify();
                                        },
                                        cx,
                                    ))
                                    .child(self.ai_action_button(
                                        "settings-add-fetched-models",
                                        "Add selected",
                                        theme,
                                        move |this, cx| this.confirm_fetched_models(provider, cx),
                                        cx,
                                    )),
                            ),
                    ),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn fetched_model_row(
        provider: AiProvider,
        model: AiModelInfo,
        already_added: bool,
        checked: bool,
        theme: Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = model.id.clone();
        let row_id = format!("settings-fetched-model-{}-{id}", provider.label());
        let meta = model
            .owned_by
            .unwrap_or_else(|| provider.label().to_string());
        div()
            .id(row_id)
            .h(px(42.0))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .text_color(rgb(if already_added {
                theme.text_muted
            } else {
                theme.text
            }))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if !already_added {
                        this.toggle_fetched_model(id.clone(), cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(28.0))
                    .text_color(rgb(if checked {
                        theme.accent
                    } else {
                        theme.text_muted
                    }))
                    .child(if already_added {
                        "✓"
                    } else if checked {
                        "☑"
                    } else {
                        "☐"
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().child(model.label))
                    .child(
                        div()
                            .truncate()
                            .text_size(ui_rems(11.0))
                            .text_color(rgb(theme.text_muted))
                            .child(model.id),
                    ),
            )
            .child(
                div()
                    .max_w(px(96.0))
                    .truncate()
                    .text_size(ui_rems(11.0))
                    .text_color(rgb(theme.text_muted))
                    .child(meta),
            )
            .into_any_element()
    }

    fn appearance_tab(
        &self,
        settings: &Settings,
        resolved: Theme,
        _themes: Vec<ThemeMetadata>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let page_gap = resolved.page_gap;
        let system_appearance = cx.global::<SettingsStore>().system_appearance;
        let current_is_dynamic = matches!(settings.theme, ThemeSelection::Dynamic { .. });

        let theme_section = if current_is_dynamic {
            let current_mode = match &settings.theme {
                ThemeSelection::Dynamic { mode, .. } => *mode,
                _ => unreachable!(),
            };

            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-theme"),
                    resolved,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(resolved.text))
                                .child(crate::fl!("settings-theme-mode")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.theme_mode_option(
                                    "Single Theme",
                                    false,
                                    current_is_dynamic,
                                    system_appearance,
                                    resolved,
                                    cx,
                                ))
                                .child(self.theme_mode_option(
                                    "Light/Dark",
                                    true,
                                    current_is_dynamic,
                                    system_appearance,
                                    resolved,
                                    cx,
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(resolved.text))
                                .child(crate::fl!("settings-mode")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.appearance_mode_option(
                                    "Light",
                                    ThemeAppearanceMode::Light,
                                    current_mode,
                                    resolved,
                                    cx,
                                ))
                                .child(self.appearance_mode_option(
                                    "Dark",
                                    ThemeAppearanceMode::Dark,
                                    current_mode,
                                    resolved,
                                    cx,
                                ))
                                .child(self.appearance_mode_option(
                                    "System",
                                    ThemeAppearanceMode::System,
                                    current_mode,
                                    resolved,
                                    cx,
                                )),
                        ),
                )
                .child(Self::section_title(
                    crate::fl!("settings-light-theme"),
                    resolved,
                ))
                .child(Self::section_title(
                    crate::fl!("settings-dark-theme"),
                    resolved,
                ))
                .child(Self::section_title("Dark Theme", resolved))
                .child(self.dark_theme_dropdown.clone())
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-theme"),
                    resolved,
                ))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .h(px(44.0))
                        .child(
                            div()
                                .text_color(rgb(resolved.text))
                                .child(crate::fl!("settings-theme-mode")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(self.theme_mode_option(
                                    "Single Theme",
                                    false,
                                    current_is_dynamic,
                                    system_appearance,
                                    resolved,
                                    cx,
                                ))
                                .child(self.theme_mode_option(
                                    "Light/Dark",
                                    true,
                                    current_is_dynamic,
                                    system_appearance,
                                    resolved,
                                    cx,
                                )),
                        ),
                )
                .child(self.static_theme_dropdown.clone())
                .into_any_element()
        };

        vec![
            theme_section,
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-fonts"),
                    resolved,
                ))
                .child(self.font_family_row(settings, resolved, cx))
                .child(self.text_input_row(
                    "UI font fallbacks",
                    self.ui_font_fallbacks_input.clone(),
                    resolved,
                ))
                .child(
                    div()
                        .text_size(ui_rems(12.0))
                        .text_color(rgb(resolved.text_muted))
                        .child(crate::fl!("settings-font-fallbacks-hint")),
                )
                .child(self.number_row(
                    "UI font size",
                    settings.font.ui_font_size,
                    |settings| settings.font.ui_font_size -= FONT_SIZE_STEP,
                    |settings| settings.font.ui_font_size += FONT_SIZE_STEP,
                    resolved,
                    cx,
                ))
                .child(self.number_row(
                    "UI font weight",
                    settings.font.ui_font_weight,
                    |settings| settings.font.ui_font_weight -= FONT_WEIGHT_STEP,
                    |settings| settings.font.ui_font_weight += FONT_WEIGHT_STEP,
                    resolved,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .child(self.reset_font_settings_button(resolved, cx)),
                )
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Self::section_title(
                    crate::fl!("settings-section-overrides"),
                    resolved,
                ))
                .child(self.number_row(
                    "Page gap",
                    page_gap,
                    move |settings| settings.overrides.page_gap = Some(page_gap - PAGE_GAP_STEP),
                    move |settings| settings.overrides.page_gap = Some(page_gap + PAGE_GAP_STEP),
                    resolved,
                    cx,
                ))
                .when(settings.overrides.page_gap.is_some(), |this| {
                    this.child(div().flex().justify_end().child(self.small_button(
                        "settings-reset-page-gap",
                        "Reset page gap",
                        |settings| settings.overrides.page_gap = None,
                        resolved,
                        cx,
                    )))
                })
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Background",
                        current: resolved.background,
                        override_value: settings.overrides.background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.background = Some(color),
                    |overrides| overrides.background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Page",
                        current: resolved.page,
                        override_value: settings.overrides.page,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.page = Some(color),
                    |overrides| overrides.page = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Selection highlight",
                        current: resolved.selection_highlight,
                        override_value: settings.overrides.selection_highlight,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.selection_highlight = Some(color),
                    |overrides| overrides.selection_highlight = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Highlight yellow",
                        current: resolved.highlight_yellow,
                        override_value: settings.overrides.highlight_yellow,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.highlight_yellow = Some(color),
                    |overrides| overrides.highlight_yellow = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Highlight green",
                        current: resolved.highlight_green,
                        override_value: settings.overrides.highlight_green,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.highlight_green = Some(color),
                    |overrides| overrides.highlight_green = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Highlight blue",
                        current: resolved.highlight_blue,
                        override_value: settings.overrides.highlight_blue,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.highlight_blue = Some(color),
                    |overrides| overrides.highlight_blue = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Highlight pink",
                        current: resolved.highlight_pink,
                        override_value: settings.overrides.highlight_pink,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.highlight_pink = Some(color),
                    |overrides| overrides.highlight_pink = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Highlight orange",
                        current: resolved.highlight_orange,
                        override_value: settings.overrides.highlight_orange,
                        swatches: &HIGHLIGHT_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.highlight_orange = Some(color),
                    |overrides| overrides.highlight_orange = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer background",
                        current: resolved.footer_background,
                        override_value: settings.overrides.footer_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_background = Some(color),
                    |overrides| overrides.footer_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer border",
                        current: resolved.footer_border,
                        override_value: settings.overrides.footer_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_border = Some(color),
                    |overrides| overrides.footer_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Footer text",
                        current: resolved.footer_text,
                        override_value: settings.overrides.footer_text,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.footer_text = Some(color),
                    |overrides| overrides.footer_text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Panel background",
                        current: resolved.panel_background,
                        override_value: settings.overrides.panel_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.panel_background = Some(color),
                    |overrides| overrides.panel_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Panel border",
                        current: resolved.panel_border,
                        override_value: settings.overrides.panel_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.panel_border = Some(color),
                    |overrides| overrides.panel_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Surface background",
                        current: resolved.surface_background,
                        override_value: settings.overrides.surface_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.surface_background = Some(color),
                    |overrides| overrides.surface_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border",
                        current: resolved.border,
                        override_value: settings.overrides.border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border = Some(color),
                    |overrides| overrides.border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border variant",
                        current: resolved.border_variant,
                        override_value: settings.overrides.border_variant,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border_variant = Some(color),
                    |overrides| overrides.border_variant = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Border focused",
                        current: resolved.border_focused,
                        override_value: settings.overrides.border_focused,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.border_focused = Some(color),
                    |overrides| overrides.border_focused = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab bar background",
                        current: resolved.tab_bar_background,
                        override_value: settings.overrides.tab_bar_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_bar_background = Some(color),
                    |overrides| overrides.tab_bar_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab active background",
                        current: resolved.tab_active_background,
                        override_value: settings.overrides.tab_active_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_active_background = Some(color),
                    |overrides| overrides.tab_active_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Tab inactive background",
                        current: resolved.tab_inactive_background,
                        override_value: settings.overrides.tab_inactive_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.tab_inactive_background = Some(color),
                    |overrides| overrides.tab_inactive_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Element hover",
                        current: resolved.element_hover,
                        override_value: settings.overrides.element_hover,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.element_hover = Some(color),
                    |overrides| overrides.element_hover = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Element selected",
                        current: resolved.element_selected,
                        override_value: settings.overrides.element_selected,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.element_selected = Some(color),
                    |overrides| overrides.element_selected = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Text",
                        current: resolved.text,
                        override_value: settings.overrides.text,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.text = Some(color),
                    |overrides| overrides.text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Text muted",
                        current: resolved.text_muted,
                        override_value: settings.overrides.text_muted,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.text_muted = Some(color),
                    |overrides| overrides.text_muted = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Placeholder text",
                        current: resolved.placeholder_text,
                        override_value: settings.overrides.placeholder_text,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.placeholder_text = Some(color),
                    |overrides| overrides.placeholder_text = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Accent",
                        current: resolved.accent,
                        override_value: settings.overrides.accent,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.accent = Some(color),
                    |overrides| overrides.accent = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Match highlight",
                        current: resolved.match_highlight,
                        override_value: settings.overrides.match_highlight,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.match_highlight = Some(color),
                    |overrides| overrides.match_highlight = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Selected row",
                        current: resolved.selected_row,
                        override_value: settings.overrides.selected_row,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.selected_row = Some(color),
                    |overrides| overrides.selected_row = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Overlay",
                        current: resolved.overlay,
                        override_value: settings.overrides.overlay,
                        swatches: &ALPHA_SWATCHES,
                        with_alpha: true,
                    },
                    |overrides, color| overrides.overlay = Some(color),
                    |overrides| overrides.overlay = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control background",
                        current: resolved.control_background,
                        override_value: settings.overrides.control_background,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_background = Some(color),
                    |overrides| overrides.control_background = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control border",
                        current: resolved.control_border,
                        override_value: settings.overrides.control_border,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_border = Some(color),
                    |overrides| overrides.control_border = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Control hover",
                        current: resolved.control_hover,
                        override_value: settings.overrides.control_hover,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.control_hover = Some(color),
                    |overrides| overrides.control_hover = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Success",
                        current: resolved.success,
                        override_value: settings.overrides.success,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.success = Some(color),
                    |overrides| overrides.success = None,
                    resolved,
                    cx,
                ))
                .child(self.color_row(
                    ColorRowConfig {
                        label: "Error",
                        current: resolved.error,
                        override_value: settings.overrides.error,
                        swatches: &COLOR_SWATCHES,
                        with_alpha: false,
                    },
                    |overrides, color| overrides.error = Some(color),
                    |overrides| overrides.error = None,
                    resolved,
                    cx,
                ))
                .into_any_element(),
        ]
    }

    fn export_button(&self, theme: Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings-export")
            .h(px(28.0))
            .min_w(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(theme.border_variant))
            .bg(rgb(theme.surface_background))
            .hover(move |style| style.bg(rgb(theme.element_hover)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.export(window, cx);
                    cx.stop_propagation();
                }),
            )
            .child(crate::fl!("export"))
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui_font = setup_ui_font(window, cx);
        let (settings, resolved, themes) = {
            let store = cx.global::<SettingsStore>();
            (
                store.settings.clone(),
                store.resolved_theme,
                store.registry.metadata(),
            )
        };
        let ai_providers_view = if self.active_tab == SettingsTab::AiProviders {
            Some(self.ai_providers_tab(&settings, resolved, window, cx))
        } else {
            None
        };
        let content = match self.active_tab {
            SettingsTab::General => self.general_tab(&settings, resolved, cx),
            SettingsTab::Keymap => self.keymap_tab(&settings, resolved, cx),
            SettingsTab::Appearance => self.appearance_tab(&settings, resolved, themes, cx),
            SettingsTab::AiGeneral => self.ai_general_tab(&settings, resolved, cx),
            SettingsTab::AiProviders => Vec::new(),
            SettingsTab::Translation => self.translation_tab(&settings, resolved, cx),
        };
        let (main_model_overlay, title_model_overlay) = if self.active_tab == SettingsTab::AiGeneral
        {
            (
                self.model_picker_overlay(
                    ProviderDropdownTarget::Main,
                    &settings,
                    resolved,
                    window,
                    cx,
                ),
                self.model_picker_overlay(
                    ProviderDropdownTarget::Title,
                    &settings,
                    resolved,
                    window,
                    cx,
                ),
            )
        } else {
            (None, None)
        };
        let provider_model_overlay = if self.active_tab == SettingsTab::AiProviders {
            self.provider_model_fetch_overlay(
                self.selected_ai_provider,
                &settings,
                resolved,
                window,
                cx,
            )
        } else {
            None
        };
        let font_picker_overlay = if self.active_tab == SettingsTab::Appearance {
            self.font_picker_overlay(&settings, resolved, window, cx)
        } else {
            None
        };
        let color_picker_overlay = if self.active_tab == SettingsTab::Appearance {
            self.color_picker_overlay(resolved, cx)
        } else {
            None
        };

        div()
            .key_context("SettingsPanel")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(resolved.surface_background))
            .font(ui_font)
            .text_color(rgb(resolved.text))
            .text_size(ui_rems(13.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    let mut changed = false;
                    if this.main_model_picker.open {
                        this.main_model_picker.open = false;
                        this.main_model_picker.anchor = None;
                        changed = true;
                    }
                    if this.title_model_picker.open {
                        this.title_model_picker.open = false;
                        this.title_model_picker.anchor = None;
                        changed = true;
                    }
                    if this.font_picker.open {
                        this.font_picker.open = false;
                        this.font_picker.anchor = None;
                        changed = true;
                    }
                    if this.active_color_picker.take().is_some() {
                        changed = true;
                    }
                    this.focus_handle.clone().focus(window, cx);
                    if changed {
                        cx.notify();
                    }
                    cx.stop_propagation();
                }),
            )
            .on_action(cx.listener(Self::close))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(50.0))
                    .px_4()
                    .border_b(px(1.0))
                    .border_color(rgb(resolved.border))
                    .child(
                        div()
                            .text_size(ui_rems(16.0))
                            .child(crate::fl!("settings-title")),
                    )
                    .child(
                        div()
                            .text_color(rgb(resolved.text_muted))
                            .child(crate::fl!("escape-key")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.settings_nav(resolved, cx))
                    .child(if let Some(view) = ai_providers_view {
                        view
                    } else {
                        div()
                            .id("settings-content")
                            .track_scroll(&self.content_scroll)
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .min_w_0()
                            .overflow_y_scroll()
                            .px_4()
                            .py_3()
                            .children(content)
                            .vertical_scrollbar_for(&self.content_scroll, window, cx)
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t(px(1.0))
                    .border_color(rgb(resolved.border))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_end()
                            .child(self.export_button(resolved, cx)),
                    )
                    .when_some(self.status.clone(), |this, status| {
                        this.child(div().text_color(rgb(resolved.success)).child(status))
                    })
                    .when_some(self.save_error.clone(), |this, error| {
                        this.child(div().text_color(rgb(resolved.error)).child(error))
                    }),
            )
            .when_some(main_model_overlay, |this, overlay| this.child(overlay))
            .when_some(title_model_overlay, |this, overlay| this.child(overlay))
            .when_some(provider_model_overlay, |this, overlay| this.child(overlay))
            .when_some(font_picker_overlay, |this, overlay| this.child(overlay))
            .when_some(color_picker_overlay, |this, overlay| this.child(overlay))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SETTINGS_NAV_ENTRIES, SettingsNavEntry, SettingsSection, SettingsTab,
        visible_settings_nav_entries,
    };

    #[test]
    fn expanded_navigation_keeps_ai_children_adjacent_to_ai() {
        let entries = visible_settings_nav_entries(true).collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![
                SettingsNavEntry::Root(SettingsTab::General),
                SettingsNavEntry::Root(SettingsTab::Keymap),
                SettingsNavEntry::Root(SettingsTab::Appearance),
                SettingsNavEntry::AiParent,
                SettingsNavEntry::AiChild(SettingsSection::AiGeneral),
                SettingsNavEntry::AiChild(SettingsSection::AiProviders),
                SettingsNavEntry::Root(SettingsTab::Translation),
            ]
        );
    }

    #[test]
    fn collapsed_navigation_hides_only_ai_children() {
        let entries = visible_settings_nav_entries(false).collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![
                SettingsNavEntry::Root(SettingsTab::General),
                SettingsNavEntry::Root(SettingsTab::Keymap),
                SettingsNavEntry::Root(SettingsTab::Appearance),
                SettingsNavEntry::AiParent,
                SettingsNavEntry::Root(SettingsTab::Translation),
            ]
        );
    }

    #[test]
    fn navigation_entry_ids_are_unique_and_stable_when_ai_collapses() {
        let ids = SETTINGS_NAV_ENTRIES
            .iter()
            .map(|entry| entry.element_id())
            .collect::<Vec<_>>();
        let mut unique_ids = ids.clone();
        unique_ids.sort_unstable();
        unique_ids.dedup();

        assert_eq!(unique_ids.len(), ids.len());
        assert_eq!(
            visible_settings_nav_entries(true)
                .find(|entry| *entry == SettingsNavEntry::Root(SettingsTab::Translation))
                .map(SettingsNavEntry::element_id),
            Some("settings-nav-translation")
        );
        assert_eq!(
            visible_settings_nav_entries(false)
                .find(|entry| *entry == SettingsNavEntry::Root(SettingsTab::Translation))
                .map(SettingsNavEntry::element_id),
            Some("settings-nav-translation")
        );
    }
}
