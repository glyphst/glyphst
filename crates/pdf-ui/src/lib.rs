//! GPUI reader presentation layer.

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

pub mod bookmarks;
pub mod go_to_page;
pub mod history;
pub mod note_editor;
pub mod notes;
pub mod pdf_view;
pub mod search;
pub mod toc;

pub use glyphst_actions::input;
pub use glyphst_ai_ui::ai_panel;
pub use glyphst_library as library;
pub use glyphst_settings::{self as settings, localize, theme};
pub use glyphst_ui::{icons, picker, scrollbar, smooth_scroll, style, text_input};
pub use pdf_view::DocumentView;
