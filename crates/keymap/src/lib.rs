//! Keybinding registration and editing UI.

pub mod keybindings;
mod keymap_editor;

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

pub use glyphst_actions::input;
pub use glyphst_settings::{self as settings, localize, theme};
pub use glyphst_ui::{icons, scrollbar};
pub use keymap_editor::KeymapEditor;
