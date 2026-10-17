use super::*;

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
pub enum PathChooserBackend {
    #[default]
    Custom,
    System,
}

impl PathChooserBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::System => "system",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "custom" => Some(Self::Custom),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewTabViewMode {
    Grid,
    Grouped,
    #[default]
    Table,
}

impl NewTabViewMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Grouped => "grouped",
            Self::Table => "table",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "grid" => Some(Self::Grid),
            "grouped" | "list" | "line" => Some(Self::Grouped),
            "table" | "details" | "detailed" | "detailed_list" => Some(Self::Table),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NewTabSettings {
    pub view_mode: NewTabViewMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentThemeSettings {
    pub follow_app_theme: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DocumentProgressDisplay {
    #[default]
    Pages,
    Percentage,
}

impl DocumentProgressDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pages => "pages",
            Self::Percentage => "percentage",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "pages" => Some(Self::Pages),
            "percentage" => Some(Self::Percentage),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentViewSettings {
    pub progress_display: DocumentProgressDisplay,
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
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowBackgroundMode {
    #[default]
    Opaque,
    Transparent,
    /// Transparent, with what is behind the window blurred by the
    /// compositor where it can.
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSettings {
    pub opacity: f32,
    pub background: WindowBackgroundMode,
    /// Fullscreen shows the theme instead of what is behind the window.
    pub fullscreen_opaque: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            opacity: DEFAULT_WINDOW_OPACITY,
            // Transparency is off until turned on; then it starts at the
            // default opacity.
            background: WindowBackgroundMode::Opaque,
            fullscreen_opaque: DEFAULT_FULLSCREEN_OPAQUE,
        }
    }
}

impl WindowSettings {
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(MIN_WINDOW_OPACITY, MAX_WINDOW_OPACITY);
    }

    /// The lowest opacity a transparent window takes.
    pub const MIN_OPACITY: f32 = MIN_WINDOW_OPACITY;

    /// Whether transparency is turned on (it shows once the opacity is
    /// below full).
    pub fn transparent(self) -> bool {
        self.background != WindowBackgroundMode::Opaque
    }

    /// Whether what is behind a transparent window is blurred.
    pub fn blur(self) -> bool {
        self.background == WindowBackgroundMode::Blurred
    }

    /// Blurs what is behind the window, or stops; nothing while the window
    /// is opaque.
    pub fn set_blur(&mut self, on: bool) {
        if self.transparent() {
            self.background = if on {
                WindowBackgroundMode::Blurred
            } else {
                WindowBackgroundMode::Transparent
            };
        }
    }

    /// Turns transparency on (blurred, as the GPUI Reader's default) or
    /// off; on at full opacity starts from the default opacity, so turning
    /// it on shows.
    pub fn set_transparent(&mut self, on: bool) {
        if on {
            if !self.transparent() {
                self.background = WindowBackgroundMode::Blurred;
            }
            if self.opacity >= MAX_WINDOW_OPACITY {
                self.opacity = DEFAULT_WINDOW_OPACITY;
            }
        } else {
            self.background = WindowBackgroundMode::Opaque;
        }
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

    /// The window's opacity, full in fullscreen when that is opaque.
    pub fn opacity_in(self, fullscreen: bool) -> f32 {
        self.effective_opacity(!(fullscreen && self.fullscreen_opaque))
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
