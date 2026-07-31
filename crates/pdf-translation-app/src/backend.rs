use glyphst_pdf_translation::{
    ProtectedTokenKind, TranslationBackend, TranslationBackendError, TranslationErrorCategory,
    TranslationUnit, TranslationUnitOutput,
};
use glyphst_settings::{Settings, TranslationProvider};
use glyphst_translation::{
    LanguageCode, SemanticProtectedToken, SemanticProtectedTokenKind, SemanticTranslationUnit,
    TranslationError, TranslationErrorCategory as GlyphstTranslationErrorCategory,
    TranslationService,
};

#[derive(Clone)]
pub(crate) struct GlyphstTranslationBackend {
    service: TranslationService,
    settings: Settings,
    provider: TranslationProvider,
    source_language: Option<LanguageCode>,
    target_language: LanguageCode,
}

impl GlyphstTranslationBackend {
    pub(crate) fn new(
        service: TranslationService,
        settings: Settings,
        source_language: Option<LanguageCode>,
        target_language: LanguageCode,
    ) -> Self {
        let provider = settings.translation.active_provider;
        Self {
            service,
            settings,
            provider,
            source_language,
            target_language,
        }
    }
}

impl TranslationBackend for GlyphstTranslationBackend {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
        let semantic_units = units.iter().map(semantic_unit).collect();
        self.service
            .translate_semantic_batch(
                &self.settings,
                self.provider,
                self.source_language.clone(),
                self.target_language.clone(),
                semantic_units,
            )
            .map(|translations| {
                translations
                    .into_iter()
                    .map(|translation| TranslationUnitOutput {
                        id: translation.id,
                        text: translation.text,
                        cached: translation.cached,
                        validation_error: translation.validation_error,
                    })
                    .collect()
            })
            .map_err(translation_error)
    }

    fn reject_unchanged_output(&self) -> bool {
        self.source_language
            .as_ref()
            .is_none_or(|source| source != &self.target_language)
    }

    fn validate_translated_text(
        &self,
        unit: &TranslationUnit,
        translated: &str,
    ) -> Result<(), String> {
        validate_target_script(unit, None, translated, self.target_language.as_str())
    }

    fn validate_translated_output(
        &self,
        unit: &TranslationUnit,
        protected_translation: &str,
        translated: &str,
    ) -> Result<(), String> {
        validate_target_script(
            unit,
            Some(protected_translation),
            translated,
            self.target_language.as_str(),
        )
    }

    fn cache_validated_translation(&self, unit: &TranslationUnit, translated: &str) {
        self.service.cache_validated_semantic_translation(
            &self.settings,
            self.provider,
            self.source_language.as_ref(),
            &self.target_language,
            &semantic_unit(unit),
            translated,
        );
    }

    fn cache_validated_translations(&self, translations: &[(TranslationUnit, String)]) {
        let semantic_translations = translations
            .iter()
            .map(|(unit, translated)| (semantic_unit(unit), translated.clone()))
            .collect::<Vec<_>>();
        self.service.cache_validated_semantic_translations(
            &self.settings,
            self.provider,
            self.source_language.as_ref(),
            &self.target_language,
            &semantic_translations,
        );
    }

    fn supports_final_single_recovery(&self) -> bool {
        self.provider == TranslationProvider::Ai
    }

    fn translate_single_recovery(
        &self,
        unit: &TranslationUnit,
    ) -> Result<TranslationUnitOutput, TranslationBackendError> {
        self.service
            .translate_semantic_single_recovery(
                &self.settings,
                self.source_language.as_ref(),
                &self.target_language,
                semantic_unit(unit),
            )
            .map(|translation| TranslationUnitOutput {
                id: translation.id,
                text: translation.text,
                cached: translation.cached,
                validation_error: translation.validation_error,
            })
            .map_err(translation_error)
    }
}

fn semantic_unit(unit: &TranslationUnit) -> SemanticTranslationUnit {
    SemanticTranslationUnit {
        id: unit.id.clone(),
        role: unit.role.as_str().to_string(),
        text: unit.protected.text.clone(),
        protected_tokens: unit
            .protected
            .tokens
            .iter()
            .map(|token| SemanticProtectedToken {
                placeholder: token.placeholder.clone(),
                original: token.original.clone(),
                kind: match token.kind {
                    ProtectedTokenKind::Formula => SemanticProtectedTokenKind::Formula,
                    ProtectedTokenKind::Citation => SemanticProtectedTokenKind::Citation,
                    ProtectedTokenKind::Url => SemanticProtectedTokenKind::Url,
                    ProtectedTokenKind::NumericLiteral => {
                        SemanticProtectedTokenKind::NumericLiteral
                    }
                    ProtectedTokenKind::Identifier => SemanticProtectedTokenKind::Identifier,
                    ProtectedTokenKind::StructuralMarker => {
                        SemanticProtectedTokenKind::StructuralMarker
                    }
                    ProtectedTokenKind::InlineObject => {
                        SemanticProtectedTokenKind::StructuralMarker
                    }
                    ProtectedTokenKind::LineBreak => SemanticProtectedTokenKind::LineBreak,
                    ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd => {
                        SemanticProtectedTokenKind::StyleBoundary
                    }
                },
            })
            .collect(),
        context_before: unit.context_before.clone(),
        context_after: unit.context_after.clone(),
    }
}

fn translation_error(error: TranslationError) -> TranslationBackendError {
    TranslationBackendError {
        category: Some(match error.category {
            GlyphstTranslationErrorCategory::Authentication => {
                TranslationErrorCategory::Authentication
            }
            GlyphstTranslationErrorCategory::RateLimited => TranslationErrorCategory::RateLimited,
            GlyphstTranslationErrorCategory::ServiceUnavailable => {
                TranslationErrorCategory::ServiceUnavailable
            }
            GlyphstTranslationErrorCategory::Timeout => TranslationErrorCategory::Timeout,
            GlyphstTranslationErrorCategory::InvalidRequest => {
                TranslationErrorCategory::InvalidRequest
            }
            GlyphstTranslationErrorCategory::InvalidResponse => {
                TranslationErrorCategory::InvalidResponse
            }
            GlyphstTranslationErrorCategory::Network => TranslationErrorCategory::Network,
            GlyphstTranslationErrorCategory::Cache => TranslationErrorCategory::Cache,
        }),
        message: error.message,
        status: error.status,
        retryable: error.retryable,
        retry_after: error.retry_after,
    }
}

fn validate_target_script(
    unit: &TranslationUnit,
    protected_translation: Option<&str>,
    translated: &str,
    target_language: &str,
) -> Result<(), String> {
    let cjk_target = matches!(
        target_language,
        "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" | "ja" | "ko"
    );
    if cjk_target
        && let Some(residue) =
            protected_translation.and_then(|text| attached_source_language_residue(unit, text))
    {
        return Err(format!(
            "translation left source-language text attached to a protected token: {residue}"
        ));
    }
    if cjk_target
        && unit.role == glyphst_pdf_translation::BlockRole::Prompt
        && let Some(heading) = unchanged_prompt_heading(unit, translated)
    {
        return Err(format!(
            "translation left a prompt heading unchanged: {heading}"
        ));
    }
    if cjk_target
        && let Some((source_label, source_entry_count)) =
            labeled_proper_name_list(&unit.source_text)
        && let Some((translated_label, translated_values)) = translated.split_once([':', '：'])
    {
        let translated_entry_count = translated_values
            .split([',', '，', ';', '；'])
            .filter(|entry| !entry.trim().is_empty())
            .count();
        if translated_entry_count < source_entry_count.div_ceil(2) {
            return Err("translation omitted entries from a proper-name list".to_string());
        }
        let target_characters =
            target_script_character_count(translated_label, target_language).unwrap_or_default();
        if target_characters < 2 {
            return Err(format!(
                "translation left a proper-name list label without enough {target_language} target-script text"
            ));
        }
        if normalize_label(source_label) == normalize_label(translated_label) {
            return Err("translation left a proper-name list label unchanged".to_string());
        }
        return Ok(());
    }
    if !matches!(
        unit.role,
        glyphst_pdf_translation::BlockRole::Body
            | glyphst_pdf_translation::BlockRole::Abstract
            | glyphst_pdf_translation::BlockRole::Prompt
            | glyphst_pdf_translation::BlockRole::Dialogue
            | glyphst_pdf_translation::BlockRole::FigureCaption
            | glyphst_pdf_translation::BlockRole::TableCaption
            | glyphst_pdf_translation::BlockRole::Footnote
    ) || unit
        .source_text
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .count()
        < 60
    {
        return Ok(());
    }
    let target_characters =
        target_script_character_count(translated, target_language).unwrap_or_default();
    let ascii_letters = translated
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .count();
    if target_characters < 8 && ascii_letters >= 40 {
        Err(format!(
            "translation contains too little {target_language} target-script text"
        ))
    } else {
        Ok(())
    }
}

fn labeled_proper_name_list(source: &str) -> Option<(&str, usize)> {
    let (label, values) = source.split_once([':', '：'])?;
    let label = label.trim();
    let label_words = label.split_whitespace().collect::<Vec<_>>();
    if label.is_empty()
        || label.chars().count() > 48
        || !(1..=6).contains(&label_words.len())
        || !label_words.iter().all(|word| {
            word.chars()
                .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | '’'))
        })
    {
        return None;
    }
    let entries = values
        .split([',', '，', ';', '；'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>();
    if entries.len() < 3 {
        return None;
    }
    let name_like = entries
        .iter()
        .filter(|entry| looks_like_person_name(entry))
        .count();
    (name_like * 3 >= entries.len() * 2).then_some((label, entries.len()))
}

fn looks_like_person_name(value: &str) -> bool {
    let words = value
        .trim_matches(|character: char| {
            character.is_whitespace() || matches!(character, '*' | '†' | '‡')
        })
        .split_whitespace()
        .collect::<Vec<_>>();
    (1..=5).contains(&words.len())
        && words.iter().all(|word| {
            word.chars().all(|character| {
                character.is_alphabetic()
                    || matches!(character, '.' | '-' | '\'' | '’' | '*' | '†' | '‡')
            })
        })
        && words.iter().any(|word| {
            word.chars()
                .find(|character| character.is_alphabetic())
                .is_some_and(char::is_uppercase)
        })
}

fn normalize_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn target_script_character_count(text: &str, target_language: &str) -> Option<usize> {
    match target_language {
        "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" => {
            Some(text.chars().filter(|character| is_cjk(*character)).count())
        }
        "ja" => Some(
            text.chars()
                .filter(|character| {
                    is_cjk(*character)
                        || matches!(
                            *character,
                            '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}'
                        )
                })
                .count(),
        ),
        "ko" => Some(
            text.chars()
                .filter(|character| {
                    is_cjk(*character)
                        || matches!(
                            *character,
                            '\u{1100}'..='\u{11ff}'
                                | '\u{3130}'..='\u{318f}'
                                | '\u{ac00}'..='\u{d7af}'
                        )
                })
                .count(),
        ),
        _ => None,
    }
}

fn unchanged_prompt_heading(unit: &TranslationUnit, translated: &str) -> Option<String> {
    let source_heading = unit.source_text.lines().next()?.trim();
    let translated_heading = translated.lines().next()?.trim();
    (source_heading.starts_with('[')
        && source_heading.ends_with(']')
        && source_heading
            .chars()
            .filter(|character| character.is_ascii_alphabetic())
            .count()
            >= 8
        && source_heading.eq_ignore_ascii_case(translated_heading))
    .then(|| source_heading.to_string())
}

fn attached_source_language_residue(
    unit: &TranslationUnit,
    protected_translation: &str,
) -> Option<String> {
    let translated = protected_translation.to_ascii_lowercase();
    for token in &unit.protected.tokens {
        if !matches!(
            token.kind,
            ProtectedTokenKind::NumericLiteral | ProtectedTokenKind::Identifier
        ) || token.original.is_empty()
            || !token.original.is_ascii()
        {
            continue;
        }
        let placeholder = token.placeholder.to_ascii_lowercase();
        for (start, _) in translated.match_indices(&placeholder) {
            let end = start + placeholder.len();
            let suffix = translated[end..]
                .bytes()
                .take_while(u8::is_ascii_alphabetic)
                .collect::<Vec<_>>();
            if suffix.len() >= 4 {
                let residue = String::from_utf8(suffix).expect("ASCII suffix is valid UTF-8");
                if source_language_residue_requires_translation(unit, &residue) {
                    return Some(format!("{}{residue}", token.original));
                }
            }

            let prefix_length = translated[..start]
                .bytes()
                .rev()
                .take_while(u8::is_ascii_alphabetic)
                .count();
            if prefix_length >= 4 {
                let prefix = &translated[start - prefix_length..start];
                if source_language_residue_requires_translation(unit, prefix) {
                    return Some(format!("{prefix}{}", token.original));
                }
            }
        }
    }
    None
}

fn source_language_residue_requires_translation(unit: &TranslationUnit, residue: &str) -> bool {
    let source = &unit.source_text;
    let lowercase_source = source.to_ascii_lowercase();
    lowercase_source
        .match_indices(residue)
        .any(|(start, matched)| {
            let end = start + matched.len();
            let previous = source[..start].chars().next_back();
            let next = source[end..].chars().next();
            let is_whole_word = previous.is_none_or(|character| !character.is_ascii_alphabetic())
                && next.is_none_or(|character| !character.is_ascii_alphabetic());
            if !is_whole_word || source_word_is_url_fragment(source, start, end) {
                return false;
            }

            let source_spelling = &source[start..end];
            unit.role == glyphst_pdf_translation::BlockRole::Footnote
                || source_spelling
                    .chars()
                    .next()
                    .is_none_or(|character| !character.is_uppercase())
        })
}

fn source_word_is_url_fragment(source: &str, start: usize, end: usize) -> bool {
    let previous = source[..start].chars().next_back();
    let next = source[end..].chars().next();
    if previous.is_some_and(|character| matches!(character, '/' | ':' | '?' | '=' | '#' | '&'))
        || next.is_some_and(|character| matches!(character, '/' | ':' | '?' | '=' | '#' | '&'))
    {
        return true;
    }

    let token_start = source[..start]
        .rfind(char::is_whitespace)
        .map_or(0, |index| {
            index + source[index..].chars().next().unwrap().len_utf8()
        });
    let token_end = source[end..]
        .find(char::is_whitespace)
        .map_or(source.len(), |offset| end + offset);
    source[token_start..token_end].contains("://")
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{20000}'..='\u{2fa1f}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyphst_pdf_translation::{BlockRole, protect_text};

    fn body_unit(source: &str) -> TranslationUnit {
        TranslationUnit {
            id: "unit".to_string(),
            member_block_ids: vec!["block".to_string()],
            role: BlockRole::Body,
            source_text: source.to_string(),
            protected: protect_text(source),
            style_definitions: Vec::new(),
            context_before: None,
            context_after: None,
        }
    }

    fn prompt_unit(source: &str) -> TranslationUnit {
        let mut unit = body_unit(source);
        unit.role = BlockRole::Prompt;
        unit
    }

    #[test]
    fn rejects_long_english_residue_for_chinese_targets() {
        let source = "This is a sufficiently long English paragraph that should be translated into Chinese before it reaches layout.";
        assert!(validate_target_script(&body_unit(source), None, source, "zh-hans").is_err());
        assert!(
            validate_target_script(
                &body_unit(source),
                None,
                "这是一段已经翻译成中文的长文本，其中保留 AI 作为必要的技术名称。",
                "zh-hans"
            )
            .is_ok()
        );
    }

    #[test]
    fn accepts_translated_labels_with_retained_latin_name_lists() {
        let source = "Contributors:Qunyi Xie, Chaorun Liu, Shiqi Yang, Shaohua Wang, Zhanlong Liu, Hao Zou, Jinyue Chen, Shu Wei, Jingjing Wu, Mingxin Huang, Zhen Wu, Guibin Wang, Tengyu Du, Lei Jia";
        let unit = body_unit(source);
        let translated = "贡献者：Qunyi Xie, Chaorun Liu, Shiqi Yang, Shaohua Wang, Zhanlong Liu, Hao Zou, Jinyue Chen, Shu Wei, Jingjing Wu, Mingxin Huang, Zhen Wu, Guibin Wang, Tengyu Du, Lei Jia";

        assert!(validate_target_script(&unit, None, translated, "zh-hans").is_ok());
        assert!(validate_target_script(&unit, None, source, "zh-hans").is_err());
        assert!(
            validate_target_script(&unit, None, "贡献者：Qunyi Xie, Chaorun Liu", "zh-hans")
                .is_err()
        );
    }

    #[test]
    fn rejects_source_words_attached_to_protected_tokens() {
        for (source, translated) in [
            (
                "Figure 2 shows how the method behaves.",
                "图2shows该方法的行为。",
            ),
            (
                "WORDTOUR aligns with human judgment.",
                "WORDTOURaligns与人类判断一致。",
            ),
        ] {
            let unit = body_unit(source);
            let protected_translation = translated_with_placeholders(&unit, translated);
            assert!(
                validate_target_script(&unit, Some(&protected_translation), translated, "zh-hans")
                    .is_err(),
                "{translated}"
            );
        }

        let mut footnote = body_unit("3 This environment is created manually.");
        footnote.role = BlockRole::Footnote;
        let bad = "3This 环境由人工创建。";
        let protected_bad = translated_with_placeholders(&footnote, bad);
        assert!(validate_target_script(&footnote, Some(&protected_bad), bad, "zh-hans").is_err());
        let good = "3该环境由人工创建。";
        let protected_good = translated_with_placeholders(&footnote, good);
        assert!(validate_target_script(&footnote, Some(&protected_good), good, "zh-hans").is_ok());
    }

    #[test]
    fn accepts_proper_names_and_url_fragments_next_to_protected_tokens() {
        for (source, token_value, retained_text) in [
            (
                "The Dunn test reported a value below 0.001.",
                "0.001",
                "Dunn",
            ),
            ("Wekinator was introduced in 2010.", "2010", "Wekinator"),
            (
                "Participants were recruited through Prolific in 2022.",
                "2022",
                "Prolific",
            ),
            (
                "See https://example.test/aimag for item 1609.",
                "1609",
                "aimag",
            ),
            (
                "Reference 246 is available at https://example.test.",
                "246",
                "https",
            ),
        ] {
            let unit = body_unit(source);
            let token = unit
                .protected
                .tokens
                .iter()
                .find(|token| token.original == token_value)
                .unwrap();
            let protected_translation = format!("{}{retained_text}", token.placeholder);

            assert_eq!(
                attached_source_language_residue(&unit, &protected_translation),
                None,
                "{protected_translation}"
            );
        }
    }

    #[test]
    fn protected_placeholder_validation_handles_overlapping_identifier_values() {
        let unit = body_unit("PDF-document-specific and PDF-level");
        let full = unit
            .protected
            .tokens
            .iter()
            .find(|token| token.original == "PDF-document-specific")
            .unwrap();
        let prefix = unit
            .protected
            .tokens
            .iter()
            .find(|token| token.original == "PDF-")
            .unwrap();
        let translated = format!("{} 文档与 {} 级别", full.placeholder, prefix.placeholder);
        let restored = unit.protected.validate_and_restore(&translated).unwrap();

        assert!(validate_target_script(&unit, Some(&translated), &restored, "zh-hans").is_ok());

        let residue = format!("{}level", prefix.placeholder);
        assert_eq!(
            attached_source_language_residue(&unit, &residue).as_deref(),
            Some("PDF-level")
        );
    }

    #[test]
    fn rejects_an_unchanged_natural_language_prompt_heading() {
        let unit =
            prompt_unit("[Agent’s Summary Description]\nObservation: John saw Eddy taking a walk.");

        assert!(
            validate_target_script(
                &unit,
                None,
                "[Agent’s Summary Description]\n观察：约翰看到艾迪在散步。",
                "zh-hans"
            )
            .is_err()
        );
        assert!(
            validate_target_script(
                &unit,
                None,
                "[智能体摘要描述]\n观察：约翰看到艾迪在散步。",
                "zh-hans"
            )
            .is_ok()
        );
    }

    fn translated_with_placeholders(unit: &TranslationUnit, translated: &str) -> String {
        let mut protected = translated.to_string();
        for token in &unit.protected.tokens {
            protected = protected.replacen(&token.original, &token.placeholder, 1);
        }
        protected
    }
}
