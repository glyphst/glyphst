use crate::{settings::ui_rems, theme::Theme};
use gpui::{BoxShadow, Rems, hsla, px};

pub(crate) const RADIUS_SM: f32 = 4.0;
pub(crate) const RADIUS_MD: f32 = 6.0;
pub(crate) const RADIUS_LG: f32 = 8.0;

pub(crate) const CONTROL_SIZE: f32 = 28.0;
pub(crate) const CONTROL_ICON_SIZE: f32 = 14.0;
pub(crate) const INPUT_ROW_HEIGHT: f32 = 40.0;
pub(crate) const SEARCH_BAR_HEIGHT: f32 = 36.0;

#[derive(Clone, Copy)]
pub(crate) enum TextSize {
    XSmall,
    Small,
    Default,
}

impl TextSize {
    pub(crate) fn rems(self) -> Rems {
        ui_rems(match self {
            Self::XSmall => 11.0,
            Self::Small => 12.0,
            Self::Default => 13.0,
        })
    }
}

pub(crate) fn elevated_shadow(theme: Theme) -> Vec<BoxShadow> {
    let is_light = is_light_surface(theme);

    vec![
        BoxShadow::new(px(0.0), px(2.0), hsla(0.0, 0.0, 0.0, 0.12)).blur_radius(px(3.0)),
        BoxShadow::new(
            px(0.0),
            px(1.0),
            hsla(0.0, 0.0, 0.0, if is_light { 0.03 } else { 0.06 }),
        ),
    ]
}

pub(crate) fn modal_shadow(theme: Theme) -> Vec<BoxShadow> {
    let is_light = is_light_surface(theme);

    vec![
        BoxShadow::new(
            px(0.0),
            px(2.0),
            hsla(0.0, 0.0, 0.0, if is_light { 0.06 } else { 0.12 }),
        )
        .blur_radius(px(3.0)),
        BoxShadow::new(
            px(0.0),
            px(3.0),
            hsla(0.0, 0.0, 0.0, if is_light { 0.06 } else { 0.08 }),
        )
        .blur_radius(px(6.0)),
        BoxShadow::new(px(0.0), px(6.0), hsla(0.0, 0.0, 0.0, 0.04)).blur_radius(px(12.0)),
        BoxShadow::new(
            px(0.0),
            px(1.0),
            hsla(0.0, 0.0, 0.0, if is_light { 0.04 } else { 0.12 }),
        ),
    ]
}

fn is_light_surface(theme: Theme) -> bool {
    let color = theme.surface_background;
    let r = ((color >> 16) & 0xff) as f32 / 255.0;
    let g = ((color >> 8) & 0xff) as f32 / 255.0;
    let b = (color & 0xff) as f32 / 255.0;
    0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5
}
