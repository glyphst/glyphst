//! GPUI settings presentation layer.

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

pub mod settings_window;

pub use settings_window::{SettingsWindow, open_settings_window};
