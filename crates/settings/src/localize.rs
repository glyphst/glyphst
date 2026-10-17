use std::{collections::HashMap, sync::LazyLock};

use i18n_embed::fluent::{FluentLanguageLoader, fluent_language_loader};
use i18n_embed::{DefaultLocalizer, LanguageLoader, Localizer};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub static LANGUAGE_LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader: FluentLanguageLoader = fluent_language_loader!();

    loader
        .load_fallback_language(&Localizations)
        .expect("failed to load fallback language");

    #[cfg(test)]
    loader.set_use_isolating(false);

    loader
});

#[macro_export]
macro_rules! fl {
    ($message_id:literal) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id)
    }};

    ($message_id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::localize::LANGUAGE_LOADER, $message_id, $($args), *)
    }};
}

pub fn localizer() -> Box<dyn Localizer> {
    Box::new(DefaultLocalizer::new(&*LANGUAGE_LOADER, &Localizations))
}

/// The translation service language code corresponding to the currently
/// selected UI locale. Unsupported UI locales use the English fallback.
pub fn ui_translation_language_code() -> &'static str {
    let language = LANGUAGE_LOADER.current_language().to_string();
    if language.eq_ignore_ascii_case("zh") || language.to_ascii_lowercase().starts_with("zh-") {
        "zh-hans"
    } else {
        "en"
    }
}

pub fn tr(message_id: &str) -> String {
    LANGUAGE_LOADER.get(message_id)
}

pub fn tr_with_number(message_id: &str, argument: &str, value: usize) -> String {
    LANGUAGE_LOADER.get_args(message_id, HashMap::from([(argument, value as i64)]))
}

#[cfg(test)]
mod tests {
    use i18n_embed::unic_langid::LanguageIdentifier;

    use super::*;

    #[test]
    fn loads_fallback_language() {
        assert_eq!(
            i18n_embed_fl::fl!(LANGUAGE_LOADER, "app-name"),
            "Glyphst Reader".to_string()
        );
        assert_eq!(
            tr_with_number("ai-mention-preview-truncated", "limit", 4_000),
            "Preview limited to 4000 characters. The complete attached context is still sent to AI."
        );
    }

    #[test]
    fn loads_simplified_chinese_language() {
        let loader: FluentLanguageLoader = fluent_language_loader!();
        let languages = ["zh-CN".parse::<LanguageIdentifier>().unwrap()];

        loader.load_fallback_language(&Localizations).unwrap();
        loader.load_languages(&Localizations, &languages).unwrap();

        assert_eq!(loader.get("settings-language"), "语言".to_string());
    }
}
