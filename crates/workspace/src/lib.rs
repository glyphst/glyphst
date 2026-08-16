//! Workspace, pane, tab, and command presentation.

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

pub mod command_palette;
pub mod document_browser;
mod external_drop;
mod navigation_rail;
pub mod new_tab;
pub mod pane;
mod pdf_password_store;
mod synchronized_tabs;
pub mod theme_selector;
pub mod workspace;

pub use glyphst_document_ui::document_view;
pub use glyphst_keymap::{KeymapEditor, keybindings};
pub use glyphst_library as library;
pub use glyphst_settings::{self as settings, localize, theme};
pub use glyphst_ui::{icons, picker, scrollbar, style};
pub use new_tab::NewTab;
pub use workspace::{
    Workspace, WorkspaceCallbacks, WorkspaceSurfaceMode, WorkspaceWindowHandle,
    WorkspaceWindowRequest,
};

pub(crate) fn synchronized_group_color(
    theme: glyphst_settings::theme::Theme,
    color_slot: usize,
) -> gpui::Hsla {
    gpui::rgb(theme.synchronized_group_color(color_slot)).into()
}
