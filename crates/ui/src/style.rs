use crate::settings::{WindowSettings, ui_rems};
use gpui::{Hsla, Rems, Window};
use gpui_component::ThemeColor;

pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 12.0;

pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SURFACE_INSET: f32 = SPACE_SM;
pub const SURFACE_GAP: f32 = SPACE_XS;

pub const CONTROL_SIZE: f32 = 28.0;
pub const CONTROL_ICON_SIZE: f32 = 14.0;
pub const INPUT_ROW_HEIGHT: f32 = 40.0;
pub const SEARCH_BAR_HEIGHT: f32 = 36.0;

/// The translucent tone used behind opaque inset application surfaces.
pub fn shell_background(
    theme: ThemeColor,
    settings: WindowSettings,
    transparency_enabled: bool,
) -> Hsla {
    theme
        .title_bar
        .opacity(settings.effective_opacity(transparency_enabled))
}

/// Keep platform composition in sync with the shell's transparency state.
pub fn apply_window_background(
    window: &mut Window,
    settings: WindowSettings,
    transparency_enabled: bool,
) {
    window.set_background_appearance(settings.background_appearance(transparency_enabled));
}

#[derive(Clone, Copy)]
pub enum TextSize {
    XSmall,
    Small,
    Default,
}

impl TextSize {
    pub fn rems(self) -> Rems {
        ui_rems(match self {
            Self::XSmall => 11.0,
            Self::Small => 12.0,
            Self::Default => 13.0,
        })
    }
}
