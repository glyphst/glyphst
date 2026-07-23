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
        validate_target_script(unit, translated, self.target_language.as_str())
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
    translated: &str,
    target_language: &str,
) -> Result<(), String> {
    let cjk_target = matches!(
        target_language,
        "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" | "ja" | "ko"
    );
    if cjk_target && let Some(residue) = attached_source_language_residue(unit, translated) {
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
    let target_characters = match target_language {
        "zh" | "zh-cn" | "zh-hans" | "zh-tw" | "zh-hant" => {
            translated.chars().filter(|character| is_cjk(*character)).count()
        }
        "ja" => translated
            .chars()
            .filter(|character| {
                is_cjk(*character)
                    || matches!(*character, '\u{3040}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}')
            })
            .count(),
        "ko" => translated
            .chars()
            .filter(|character| {
                is_cjk(*character)
                    || matches!(*character, '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}' | '\u{ac00}'..='\u{d7af}')
            })
            .count(),
        _ => return Ok(()),
    };
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

fn attached_source_language_residue(unit: &TranslationUnit, translated: &str) -> Option<String> {
    let source = unit.source_text.to_ascii_lowercase();
    let translated = translated.to_ascii_lowercase();
    for token in &unit.protected.tokens {
        if !matches!(
            token.kind,
            ProtectedTokenKind::NumericLiteral | ProtectedTokenKind::Identifier
        ) || token.original.is_empty()
            || !token.original.is_ascii()
        {
            continue;
        }
        let original = token.original.to_ascii_lowercase();
        for (start, _) in translated.match_indices(&original) {
            let end = start + original.len();
            let suffix = translated[end..]
                .bytes()
                .take_while(u8::is_ascii_alphabetic)
                .collect::<Vec<_>>();
            if suffix.len() >= 4 {
                let residue = String::from_utf8(suffix).expect("ASCII suffix is valid UTF-8");
                if source.contains(&residue) {
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
                if source.contains(prefix) {
                    return Some(format!("{prefix}{}", token.original));
                }
            }
        }
    }
    None
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
        assert!(validate_target_script(&body_unit(source), source, "zh-hans").is_err());
        assert!(
            validate_target_script(
                &body_unit(source),
                "这是一段已经翻译成中文的长文本，其中保留 AI 作为必要的技术名称。",
                "zh-hans"
            )
            .is_ok()
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
            assert!(
                validate_target_script(&body_unit(source), translated, "zh-hans").is_err(),
                "{translated}"
            );
        }

        let mut footnote = body_unit("3 This environment is created manually.");
        footnote.role = BlockRole::Footnote;
        assert!(validate_target_script(&footnote, "3This 环境由人工创建。", "zh-hans").is_err());
        assert!(validate_target_script(&footnote, "3该环境由人工创建。", "zh-hans").is_ok());
    }

    #[test]
    fn rejects_an_unchanged_natural_language_prompt_heading() {
        let unit =
            prompt_unit("[Agent’s Summary Description]\nObservation: John saw Eddy taking a walk.");

        assert!(
            validate_target_script(
                &unit,
                "[Agent’s Summary Description]\n观察：约翰看到艾迪在散步。",
                "zh-hans"
            )
            .is_err()
        );
        assert!(
            validate_target_script(
                &unit,
                "[智能体摘要描述]\n观察：约翰看到艾迪在散步。",
                "zh-hans"
            )
            .is_ok()
        );
    }
}
