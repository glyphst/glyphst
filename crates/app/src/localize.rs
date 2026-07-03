use std::sync::LazyLock;

use i18n_embed::fluent::{FluentLanguageLoader, fluent_language_loader};
use i18n_embed::unic_langid::LanguageIdentifier;
use i18n_embed::{DefaultLocalizer, DesktopLanguageRequester, LanguageLoader, Localizer};
use rust_embed::RustEmbed;

use crate::settings::AppLanguage;

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

pub fn select_language(language: AppLanguage) {
    let requested_languages = match language {
        AppLanguage::System => DesktopLanguageRequester::requested_languages(),
        AppLanguage::English => explicit_language("en"),
        AppLanguage::SimplifiedChinese => explicit_language("zh-CN"),
    };

    if let Err(error) = localizer().select(&requested_languages) {
        eprintln!("failed to load localizations: {error:#}");
    }
}

fn explicit_language(language: &str) -> Vec<LanguageIdentifier> {
    match language.parse() {
        Ok(language) => vec![language],
        Err(error) => {
            eprintln!("failed to parse language identifier {language:?}: {error:#}");
            DesktopLanguageRequester::requested_languages()
        }
    }
}

pub fn tr(message_id: &str) -> String {
    LANGUAGE_LOADER.get(message_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_fallback_language() {
        assert_eq!(
            i18n_embed_fl::fl!(LANGUAGE_LOADER, "app-name"),
            "Glyphst".to_string()
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
