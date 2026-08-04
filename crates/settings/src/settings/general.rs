use super::*;

pub fn ui_rems(px: f32) -> gpui::Rems {
    gpui::rems(px / DEFAULT_UI_FONT_SIZE)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RestoreOnStartup {
    #[default]
    LastSession,
    None,
}

impl RestoreOnStartup {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LastSession => "last_session",
            Self::None => "none",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "last_session" | "last" => Some(Self::LastSession),
            "none" | "new_window" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewTabViewMode {
    #[default]
    Grid,
    List,
    Details,
}

impl NewTabViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::List => "list",
            Self::Details => "details",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "grid" => Some(Self::Grid),
            "list" | "line" => Some(Self::List),
            "details" | "detailed" | "detailed_list" => Some(Self::Details),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NewTabSettings {
    pub view_mode: NewTabViewMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BehaviorSettings {
    pub default_zoom: f32,
    pub zoom_step: f32,
    pub smooth_scrolling: bool,
    pub reduce_motion: bool,
    pub dim_inactive_panes: bool,
    pub copy_format: CopyFormat,
    pub semantic_copy: bool,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            default_zoom: DEFAULT_ZOOM,
            zoom_step: DEFAULT_ZOOM_STEP,
            smooth_scrolling: DEFAULT_SMOOTH_SCROLLING,
            reduce_motion: false,
            dim_inactive_panes: false,
            copy_format: CopyFormat::PlainText,
            semantic_copy: true,
        }
    }
}

impl BehaviorSettings {
    pub fn sanitize(&mut self) {
        self.default_zoom = self.default_zoom.clamp(MIN_ZOOM_SETTING, MAX_ZOOM_SETTING);
        self.zoom_step = self.zoom_step.clamp(MIN_ZOOM_STEP, MAX_ZOOM_STEP);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CopyFormat {
    #[default]
    PlainText,
    Markdown,
}

impl CopyFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlainText => "plain_text",
            Self::Markdown => "markdown",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "plain_text" | "plain" | "text" => Some(Self::PlainText),
            "markdown" | "md" => Some(Self::Markdown),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontSettings {
    pub ui_font_family: String,
    pub ui_font_fallbacks: Vec<String>,
    pub ui_font_size: f32,
    pub ui_font_weight: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            ui_font_family: DEFAULT_UI_FONT_FAMILY.to_string(),
            ui_font_fallbacks: DEFAULT_UI_FONT_FALLBACKS
                .iter()
                .map(|fallback| fallback.to_string())
                .collect(),
            ui_font_size: DEFAULT_UI_FONT_SIZE,
            ui_font_weight: DEFAULT_UI_FONT_WEIGHT,
        }
    }
}

impl FontSettings {
    pub fn sanitize(&mut self) {
        self.ui_font_family = self.ui_font_family.trim().to_string();
        if self.ui_font_family.is_empty() {
            self.ui_font_family = DEFAULT_UI_FONT_FAMILY.to_string();
        }

        let mut fallbacks = Vec::new();
        for fallback in self.ui_font_fallbacks.drain(..) {
            let fallback = fallback.trim().to_string();
            if fallback.is_empty() || fallbacks.iter().any(|entry| entry == &fallback) {
                continue;
            }
            fallbacks.push(fallback);
        }
        self.ui_font_fallbacks = fallbacks;

        self.ui_font_size = if self.ui_font_size.is_finite() {
            self.ui_font_size.clamp(MIN_UI_FONT_SIZE, MAX_UI_FONT_SIZE)
        } else {
            DEFAULT_UI_FONT_SIZE
        };
        self.ui_font_weight = if self.ui_font_weight.is_finite() {
            self.ui_font_weight
                .clamp(MIN_UI_FONT_WEIGHT, MAX_UI_FONT_WEIGHT)
        } else {
            DEFAULT_UI_FONT_WEIGHT
        };
    }

    pub fn ui_font(&self) -> Font {
        Font {
            family: self.ui_font_family.clone().into(),
            features: Default::default(),
            fallbacks: (!self.ui_font_fallbacks.is_empty())
                .then(|| FontFallbacks::from_fonts(self.ui_font_fallbacks.clone())),
            weight: FontWeight(self.ui_font_weight),
            style: Default::default(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowBackgroundMode {
    Opaque,
    Transparent,
    #[default]
    Blurred,
}

impl WindowBackgroundMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
            Self::Blurred => "blurred",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "opaque" => Some(Self::Opaque),
            "transparent" => Some(Self::Transparent),
            "blurred" | "blur" => Some(Self::Blurred),
            _ => None,
        }
    }

    fn into_gpui(self) -> WindowBackgroundAppearance {
        match self {
            Self::Opaque => WindowBackgroundAppearance::Opaque,
            Self::Transparent => WindowBackgroundAppearance::Transparent,
            Self::Blurred => WindowBackgroundAppearance::Blurred,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSettings {
    pub opacity: f32,
    pub background: WindowBackgroundMode,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            opacity: DEFAULT_WINDOW_OPACITY,
            background: WindowBackgroundMode::Blurred,
        }
    }
}

impl WindowSettings {
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(MIN_WINDOW_OPACITY, MAX_WINDOW_OPACITY);
    }

    pub fn transparency_enabled_by_default(self) -> bool {
        self.opacity < MAX_WINDOW_OPACITY && self.background != WindowBackgroundMode::Opaque
    }

    pub fn effective_opacity(self, enabled: bool) -> f32 {
        if enabled && self.transparency_enabled_by_default() {
            self.opacity
        } else {
            MAX_WINDOW_OPACITY
        }
    }

    pub fn background_appearance(self, enabled: bool) -> WindowBackgroundAppearance {
        if enabled && self.transparency_enabled_by_default() {
            self.background.into_gpui()
        } else {
            WindowBackgroundAppearance::Opaque
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabBarVisibility {
    #[default]
    Auto,
    Always,
}

impl TabBarVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "auto" | "multiple" => Some(Self::Auto),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppLanguage {
    #[default]
    System,
    English,
    SimplifiedChinese,
}

impl AppLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::System => crate::fl!("language-system"),
            Self::English => crate::fl!("language-english"),
            Self::SimplifiedChinese => crate::fl!("language-simplified-chinese"),
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "system" | "auto" => Some(Self::System),
            "en" | "en-US" | "en-us" => Some(Self::English),
            "zh-CN" | "zh-cn" | "zh-Hans" | "zh-hans" => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapPreset {
    #[default]
    Modern,
    Vim,
}

impl KeymapPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Vim => "vim",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Modern => crate::fl!("keymap-preset-modern"),
            Self::Vim => crate::fl!("keymap-preset-vim"),
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "modern" => Some(Self::Modern),
            "vim" | "vim-like" | "vim_like" => Some(Self::Vim),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingOverride {
    pub keymap: KeymapPreset,
    pub action: String,
    pub keystrokes: String,
    pub context: Option<String>,
}
