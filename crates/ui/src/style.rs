use crate::settings::ui_rems;
use gpui::Rems;

pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_MD: f32 = 6.0;
pub const RADIUS_LG: f32 = 8.0;

pub const CONTROL_SIZE: f32 = 28.0;
pub const CONTROL_ICON_SIZE: f32 = 14.0;
pub const INPUT_ROW_HEIGHT: f32 = 40.0;
pub const SEARCH_BAR_HEIGHT: f32 = 36.0;

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
