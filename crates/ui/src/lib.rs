use std::time::Duration;

use gpui::App;

#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        glyphst_settings::localize::tr($message_id)
    }};

    ($message_id:literal, $($argument:ident = $value:expr),+ $(,)?) => {{
        let mut arguments = ::std::collections::HashMap::new();
        $(
            arguments.insert(
                stringify!($argument),
                ::std::convert::Into::into($value),
            );
        )+
        glyphst_settings::localize::LANGUAGE_LOADER.get_args_concrete($message_id, arguments)
    }};
}

pub mod activity;
pub mod assets;
pub mod icons;
pub mod markdown;
pub mod motion;
pub mod picker;
pub mod prompt;
pub mod root;
pub mod scrollbar;
pub mod smooth_scroll;
pub mod sticky_items;
pub mod style;
pub mod tooltip;

pub use glyphst_actions::menu;
pub use glyphst_settings as settings;
pub use glyphst_settings::theme;

/// Initialize the shared Glyphst UI services for an application.
pub fn init(cx: &mut App) {
    gpui_component::init(cx);
    prompt::init(cx);
    activity::init(cx);
}

/// Shared lifetime for brief, non-blocking UI feedback.
pub const TRANSIENT_FEEDBACK_DURATION: Duration = Duration::from_secs(2);

/// Shared lifetime for actionable feedback that users need time to read or
/// interact with.
pub const LONG_FEEDBACK_DURATION: Duration = Duration::from_secs(10);
