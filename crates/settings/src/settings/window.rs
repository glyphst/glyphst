use super::*;

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
