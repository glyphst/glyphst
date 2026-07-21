use std::collections::HashMap;
use std::fmt;
use std::thread;
use std::time::Duration;

use glyphst_settings::{Settings, TranslationProvider};
use glyphst_translation::{
    LanguageCode, SemanticTranslationUnit, TranslationError, TranslationService,
};
use serde::{Deserialize, Serialize};

use crate::{BlockRole, CancellationToken, DocumentV1};

const MAX_BATCH_UNITS: usize = 32;
const MAX_BATCH_CHARACTERS: usize = 12_000;
const MAX_REPAIR_ATTEMPTS: usize = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationUnit {
    pub id: String,
    pub member_block_ids: Vec<String>,
    pub role: BlockRole,
    pub source_text: String,
    pub protected: ProtectedText,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationUnitOutput {
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslatedUnit {
    pub id: String,
    pub member_block_ids: Vec<String>,
    pub role: BlockRole,
    pub source_text: String,
    pub translated_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslatedDocument {
    pub document: DocumentV1,
    pub units: Vec<TranslatedUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedText {
    pub text: String,
    pub tokens: Vec<ProtectedToken>,
}

impl ProtectedText {
    pub fn validate_and_restore(&self, translated: &str) -> Result<String, String> {
        if translated.trim().is_empty() {
            return Err("translation is empty".to_string());
        }
        for token in &self.tokens {
            let count = translated.matches(&token.placeholder).count();
            if count != 1 {
                return Err(format!(
                    "placeholder {} occurred {count} times",
                    token.placeholder
                ));
            }
        }
        let mut restored = translated.to_string();
        for token in &self.tokens {
            restored = restored.replace(&token.placeholder, &token.original);
        }
        if restored.contains("[[GLYPHST_") {
            return Err("translation contains an unresolved placeholder".to_string());
        }
        Ok(restored)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedToken {
    pub placeholder: String,
    pub original: String,
    pub kind: ProtectedTokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedTokenKind {
    Formula,
    Citation,
    Url,
    NumericLiteral,
    Identifier,
}

pub trait TranslationBackend {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError>;

    fn reject_unchanged_output(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationBackendError {
    pub message: String,
    pub retryable: bool,
    pub retry_after: Option<Duration>,
}

impl fmt::Display for TranslationBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for TranslationBackendError {}

impl From<TranslationError> for TranslationBackendError {
    fn from(error: TranslationError) -> Self {
        Self {
            message: error.message,
            retryable: error.retryable,
            retry_after: error.retry_after,
        }
    }
}

#[derive(Clone)]
pub struct GlyphstTranslationBackend {
    service: TranslationService,
    settings: Settings,
    provider: TranslationProvider,
    source_language: Option<LanguageCode>,
    target_language: LanguageCode,
}

impl GlyphstTranslationBackend {
    pub fn new(
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
        let semantic_units = units
            .iter()
            .map(|unit| SemanticTranslationUnit {
                id: unit.id.clone(),
                role: role_name(unit.role).to_string(),
                text: unit.protected.text.clone(),
                context_before: unit.context_before.clone(),
                context_after: unit.context_after.clone(),
            })
            .collect();
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
                    })
                    .collect()
            })
            .map_err(Into::into)
    }

    fn reject_unchanged_output(&self) -> bool {
        self.source_language
            .as_ref()
            .is_none_or(|source| source != &self.target_language)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationDocumentError {
    pub message: String,
    pub unresolved_unit_ids: Vec<String>,
}

impl fmt::Display for TranslationDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for TranslationDocumentError {}

pub fn translate_document(
    document: DocumentV1,
    backend: &dyn TranslationBackend,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(usize, usize),
) -> Result<TranslatedDocument, TranslationDocumentError> {
    let units = build_translation_units(&document);
    let total = units.len();
    if total == 0 {
        return Ok(TranslatedDocument {
            document,
            units: Vec::new(),
        });
    }

    let batches = split_batches(&units);
    let mut translated = Vec::with_capacity(total);
    let mut unresolved = Vec::new();
    for batch in batches {
        if cancellation.is_cancelled() {
            return Err(TranslationDocumentError {
                message: "PDF translation was cancelled".to_string(),
                unresolved_unit_ids: Vec::new(),
            });
        }
        let outputs = translate_with_batch_fallback(backend, batch);
        match outputs {
            Ok(outputs) => {
                let mut outputs_by_id = outputs
                    .into_iter()
                    .map(|output| (output.id.clone(), output))
                    .collect::<HashMap<_, _>>();
                for unit in batch {
                    let result = outputs_by_id
                        .remove(&unit.id)
                        .ok_or_else(|| "translation response omitted the unit".to_string())
                        .and_then(|output| {
                            validate_translation(
                                unit,
                                &output.text,
                                backend.reject_unchanged_output(),
                            )
                        })
                        .or_else(|_| repair_unit(backend, unit, cancellation));
                    match result {
                        Ok(translated_text) => translated.push(TranslatedUnit {
                            id: unit.id.clone(),
                            member_block_ids: unit.member_block_ids.clone(),
                            role: unit.role,
                            source_text: unit.source_text.clone(),
                            translated_text,
                        }),
                        Err(_) => unresolved.push(unit.id.clone()),
                    }
                    on_progress(translated.len() + unresolved.len(), total);
                }
            }
            Err(_) => {
                for unit in batch {
                    match repair_unit(backend, unit, cancellation) {
                        Ok(translated_text) => translated.push(TranslatedUnit {
                            id: unit.id.clone(),
                            member_block_ids: unit.member_block_ids.clone(),
                            role: unit.role,
                            source_text: unit.source_text.clone(),
                            translated_text,
                        }),
                        Err(_) => unresolved.push(unit.id.clone()),
                    }
                    on_progress(translated.len() + unresolved.len(), total);
                }
            }
        }
    }

    if !unresolved.is_empty() {
        return Err(TranslationDocumentError {
            message: format!(
                "{} translation unit(s) remain unresolved; export is blocked",
                unresolved.len()
            ),
            unresolved_unit_ids: unresolved,
        });
    }
    translated.sort_by_key(|translated_unit| {
        units
            .iter()
            .position(|unit| unit.id == translated_unit.id)
            .unwrap_or(usize::MAX)
    });
    Ok(TranslatedDocument {
        document,
        units: translated,
    })
}

pub fn protect_text(text: &str) -> ProtectedText {
    let mut protected = String::with_capacity(text.len());
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        if let Some((kind, length)) = protected_prefix(rest) {
            let original = &rest[..length];
            let placeholder = placeholder(kind, tokens.len(), original);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: original.to_string(),
                kind,
            });
            cursor += length;
            continue;
        }
        let character = rest
            .chars()
            .next()
            .expect("cursor is on a character boundary");
        protected.push(character);
        cursor += character.len_utf8();
    }
    ProtectedText {
        text: protected,
        tokens,
    }
}

fn build_translation_units(document: &DocumentV1) -> Vec<TranslationUnit> {
    let mut units = Vec::<TranslationUnit>::new();
    let mut continuation_units = HashMap::<String, usize>::new();
    for page in &document.pages {
        for block in &page.blocks {
            if !block.policy.translate || block.content.text.trim().is_empty() {
                continue;
            }
            let continuation_id = block
                .continuation
                .as_ref()
                .map(|continuation| continuation.group_id.clone());
            if let Some(group_id) = continuation_id.as_ref()
                && let Some(unit_index) = continuation_units.get(group_id).copied()
            {
                let unit = &mut units[unit_index];
                append_continuation_text(&mut unit.source_text, &block.content.text);
                unit.member_block_ids.push(block.id.clone());
                unit.protected = protect_text(&unit.source_text);
                continue;
            }
            let id = continuation_id
                .clone()
                .unwrap_or_else(|| format!("unit-{}", block.id));
            let source_text = block.content.text.trim().to_string();
            let unit_index = units.len();
            units.push(TranslationUnit {
                id,
                member_block_ids: vec![block.id.clone()],
                role: block.role,
                protected: protect_text(&source_text),
                source_text,
                context_before: None,
                context_after: None,
            });
            if let Some(group_id) = continuation_id {
                continuation_units.insert(group_id, unit_index);
            }
        }
    }
    for index in 0..units.len() {
        units[index].context_before = index
            .checked_sub(1)
            .and_then(|previous| units.get(previous))
            .map(|unit| context_excerpt(&unit.source_text));
        units[index].context_after = units
            .get(index + 1)
            .map(|unit| context_excerpt(&unit.source_text));
    }
    units
}

fn split_batches(units: &[TranslationUnit]) -> Vec<&[TranslationUnit]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut characters = 0;
    for (index, unit) in units.iter().enumerate() {
        let exceeds_count = index - start >= MAX_BATCH_UNITS;
        let exceeds_characters =
            index > start && characters + unit.protected.text.len() > MAX_BATCH_CHARACTERS;
        if exceeds_count || exceeds_characters {
            batches.push(&units[start..index]);
            start = index;
            characters = 0;
        }
        characters += unit.protected.text.len();
    }
    if start < units.len() {
        batches.push(&units[start..]);
    }
    batches
}

fn translate_with_batch_fallback(
    backend: &dyn TranslationBackend,
    batch: &[TranslationUnit],
) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
    match backend.translate_batch(batch) {
        Ok(outputs) if output_ids_match(batch, &outputs) => Ok(outputs),
        Ok(_) if batch.len() > 1 => translate_singly(backend, batch),
        Ok(_) => Err(TranslationBackendError {
            message: "translation response did not preserve unit IDs".to_string(),
            retryable: true,
            retry_after: None,
        }),
        Err(error) if batch.len() > 1 => translate_singly(backend, batch).map_err(|_| error),
        Err(error) => Err(error),
    }
}

fn translate_singly(
    backend: &dyn TranslationBackend,
    batch: &[TranslationUnit],
) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
    let mut outputs = Vec::with_capacity(batch.len());
    for unit in batch {
        let mut translated = backend.translate_batch(std::slice::from_ref(unit))?;
        if translated.len() != 1 || translated[0].id != unit.id {
            return Err(TranslationBackendError {
                message: "translation response did not preserve a unit ID".to_string(),
                retryable: true,
                retry_after: None,
            });
        }
        outputs.push(translated.remove(0));
    }
    Ok(outputs)
}

fn repair_unit(
    backend: &dyn TranslationBackend,
    unit: &TranslationUnit,
    cancellation: &CancellationToken,
) -> Result<String, String> {
    let mut last_error = "translation repair failed".to_string();
    for attempt in 0..MAX_REPAIR_ATTEMPTS {
        cancellation.check().map_err(|error| error.to_string())?;
        match backend.translate_batch(std::slice::from_ref(unit)) {
            Ok(outputs) if outputs.len() == 1 && outputs[0].id == unit.id => {
                match validate_translation(
                    unit,
                    &outputs[0].text,
                    backend.reject_unchanged_output(),
                ) {
                    Ok(restored) => return Ok(restored),
                    Err(error) => last_error = error,
                }
            }
            Ok(_) => last_error = "translation repair did not preserve the unit ID".to_string(),
            Err(error) => {
                last_error = error.message;
                if !error.retryable {
                    break;
                }
                if let Some(delay) = error.retry_after {
                    sleep_cancellable(delay.min(Duration::from_secs(5)), cancellation)?;
                } else if attempt + 1 < MAX_REPAIR_ATTEMPTS {
                    sleep_cancellable(
                        Duration::from_millis(250 * (attempt as u64 + 1)),
                        cancellation,
                    )?;
                }
            }
        }
    }
    Err(last_error)
}

fn sleep_cancellable(duration: Duration, cancellation: &CancellationToken) -> Result<(), String> {
    let mut remaining = duration;
    while remaining > Duration::ZERO {
        cancellation.check().map_err(|error| error.to_string())?;
        let interval = remaining.min(Duration::from_millis(100));
        thread::sleep(interval);
        remaining = remaining.saturating_sub(interval);
    }
    Ok(())
}

fn validate_translation(
    unit: &TranslationUnit,
    translated: &str,
    reject_unchanged: bool,
) -> Result<String, String> {
    let restored = unit.protected.validate_and_restore(translated)?;
    if reject_unchanged
        && should_reject_unchanged(&unit.source_text)
        && normalize_for_comparison(&restored) == normalize_for_comparison(&unit.source_text)
    {
        return Err("translation is unchanged from the source".to_string());
    }
    let lowercase = restored.trim().to_ascii_lowercase();
    if lowercase.starts_with("i cannot translate")
        || lowercase.starts_with("i can't translate")
        || lowercase.starts_with("unable to translate")
    {
        return Err("translation provider refused the unit".to_string());
    }
    Ok(restored.trim().to_string())
}

fn output_ids_match(units: &[TranslationUnit], outputs: &[TranslationUnitOutput]) -> bool {
    if units.len() != outputs.len() {
        return false;
    }
    let mut output_ids = outputs
        .iter()
        .map(|output| output.id.as_str())
        .collect::<Vec<_>>();
    output_ids.sort_unstable();
    output_ids.dedup();
    units
        .iter()
        .all(|unit| output_ids.binary_search(&unit.id.as_str()).is_ok())
}

fn protected_prefix(text: &str) -> Option<(ProtectedTokenKind, usize)> {
    if let Some(length) =
        delimited_length(text, "$$", "$$").or_else(|| delimited_length(text, "$", "$"))
    {
        return Some((ProtectedTokenKind::Formula, length));
    }
    if let Some(length) =
        delimited_length(text, "\\(", "\\)").or_else(|| delimited_length(text, "\\[", "\\]"))
    {
        return Some((ProtectedTokenKind::Formula, length));
    }
    if text.starts_with("http://") || text.starts_with("https://") {
        let length = text.find(char::is_whitespace).unwrap_or(text.len());
        return Some((
            ProtectedTokenKind::Url,
            trim_trailing_punctuation(&text[..length]),
        ));
    }
    if text.starts_with('[')
        && let Some(end) = text.find(']')
        && end > 1
        && text[1..end].chars().all(|character| {
            character.is_ascii_digit() || matches!(character, ',' | '-' | '–' | ' ')
        })
    {
        return Some((ProtectedTokenKind::Citation, end + 1));
    }
    if text.starts_with('(')
        && let Some(end) = text.find(')')
    {
        let candidate = &text[1..end];
        if candidate
            .chars()
            .any(|character| character.is_ascii_digit())
            && candidate.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || character.is_whitespace()
                    || matches!(character, ',' | ';' | '-' | '–' | '&' | '.')
            })
        {
            return Some((ProtectedTokenKind::Citation, end + 1));
        }
    }
    if text
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        let length = text
            .char_indices()
            .take_while(|(_, character)| {
                character.is_ascii_alphanumeric()
                    || matches!(
                        character,
                        '.' | ',' | '%' | '°' | '/' | ':' | '-' | '+' | '×'
                    )
            })
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .unwrap_or(0);
        if length > 0 {
            return Some((ProtectedTokenKind::NumericLiteral, length));
        }
    }
    if text
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
    {
        let length = text
            .char_indices()
            .take_while(|(_, character)| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
            })
            .map(|(index, character)| index + character.len_utf8())
            .last()
            .unwrap_or(0);
        let candidate = &text[..length];
        if candidate.len() >= 2
            && (candidate.contains('_')
                || candidate
                    .chars()
                    .filter(|character| character.is_ascii_uppercase())
                    .count()
                    >= 2)
        {
            return Some((ProtectedTokenKind::Identifier, length));
        }
    }
    None
}

fn delimited_length(text: &str, start: &str, end: &str) -> Option<usize> {
    if !text.starts_with(start) {
        return None;
    }
    let search_start = start.len();
    text[search_start..]
        .find(end)
        .map(|relative| search_start + relative + end.len())
        .filter(|length| *length > start.len() + end.len())
}

fn trim_trailing_punctuation(value: &str) -> usize {
    value.trim_end_matches(['.', ',', ';', ':', ')', ']']).len()
}

fn placeholder(kind: ProtectedTokenKind, index: usize, original: &str) -> String {
    let kind = match kind {
        ProtectedTokenKind::Formula => "FORMULA",
        ProtectedTokenKind::Citation => "CITATION",
        ProtectedTokenKind::Url => "URL",
        ProtectedTokenKind::NumericLiteral => "NUMBER",
        ProtectedTokenKind::Identifier => "IDENTIFIER",
    };
    format!(
        "[[GLYPHST_{kind}_{index:04}_{:08X}]]",
        checksum(original.as_bytes())
    )
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn append_continuation_text(target: &mut String, continuation: &str) {
    let continuation = continuation.trim();
    if continuation.is_empty() {
        return;
    }
    let should_join_without_space = target.ends_with('-')
        && continuation
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase());
    if should_join_without_space {
        target.pop();
    } else if !target.ends_with(char::is_whitespace) {
        target.push(' ');
    }
    target.push_str(continuation);
}

fn context_excerpt(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= 280 {
        text.to_string()
    } else {
        characters[..280].iter().collect()
    }
}

fn normalize_for_comparison(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
        .flat_map(char::to_lowercase)
        .collect()
}

fn should_reject_unchanged(text: &str) -> bool {
    let alphabetic = text
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    alphabetic >= 12
}

fn role_name(role: BlockRole) -> &'static str {
    match role {
        BlockRole::Title => "title",
        BlockRole::Heading => "heading",
        BlockRole::Body => "body",
        BlockRole::Abstract => "abstract",
        BlockRole::TableOfContents => "table_of_contents",
        BlockRole::FigureCaption => "figure_caption",
        BlockRole::TableCaption => "table_caption",
        BlockRole::Footnote => "footnote",
        BlockRole::Header => "header",
        BlockRole::Footer => "footer",
        BlockRole::PageNumber => "page_number",
        BlockRole::Metadata => "metadata",
        BlockRole::Reference => "reference",
        BlockRole::FormulaNumber => "formula_number",
        BlockRole::Image => "image",
        BlockRole::Table => "table",
        BlockRole::Formula => "formula",
        BlockRole::Code => "code",
        BlockRole::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::{
        Block, BlockContent, BlockKind, BoundingBox, DocumentMetadata, Line, Page, PageGeometry,
        SourceProvenance, TranslationPolicy,
    };

    struct EchoBackend {
        calls: Mutex<usize>,
        corrupt_first_placeholder: bool,
    }

    impl TranslationBackend for EchoBackend {
        fn translate_batch(
            &self,
            units: &[TranslationUnit],
        ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            Ok(units
                .iter()
                .map(|unit| {
                    let text = if self.corrupt_first_placeholder && *calls == 1 {
                        unit.protected.text.replace("[[GLYPHST_", "[[BROKEN_")
                    } else {
                        format!("Translated {}", unit.protected.text)
                    };
                    TranslationUnitOutput {
                        id: unit.id.clone(),
                        text,
                    }
                })
                .collect())
        }
    }

    #[test]
    fn placeholders_round_trip() {
        let source = "See [12] and $E = mc^2$ at https://example.com/test.";
        let protected = protect_text(source);

        assert_eq!(
            protected.validate_and_restore(&protected.text).unwrap(),
            source
        );
        assert_eq!(protected.tokens.len(), 3);
    }

    #[test]
    fn repairs_placeholder_corruption() {
        let document = test_document("Equation $x + y$ is discussed in [3].");
        let backend = EchoBackend {
            calls: Mutex::new(0),
            corrupt_first_placeholder: true,
        };

        let translated =
            translate_document(document, &backend, &CancellationToken::new(), |_, _| {}).unwrap();

        assert!(translated.units[0].translated_text.contains("$x + y$"));
        assert!(translated.units[0].translated_text.contains("[3]"));
        assert!(*backend.calls.lock().unwrap() >= 2);
    }

    #[test]
    fn blocks_export_when_repairs_fail() {
        struct InvalidBackend;
        impl TranslationBackend for InvalidBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                Ok(units
                    .iter()
                    .map(|unit| TranslationUnitOutput {
                        id: unit.id.clone(),
                        text: String::new(),
                    })
                    .collect())
            }
        }

        let error = translate_document(
            test_document("This paragraph must be translated."),
            &InvalidBackend,
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap_err();

        assert_eq!(error.unresolved_unit_ids, ["unit-p001-b0000"]);
    }

    fn test_document(text: &str) -> DocumentV1 {
        let bbox = BoundingBox::new(10.0, 10.0, 90.0, 40.0);
        DocumentV1::new(
            DocumentMetadata {
                document_id: "test".to_string(),
                source_path: "test.pdf".into(),
                provider: "test".to_string(),
                provider_version: "1".to_string(),
                extra: BTreeMap::new(),
            },
            vec![Page {
                page_index: 0,
                geometry: PageGeometry::new(100.0, 100.0),
                blocks: vec![Block {
                    id: "p001-b0000".to_string(),
                    page_index: 0,
                    order: 0,
                    reading_order: 0,
                    kind: BlockKind::Text,
                    role: BlockRole::Body,
                    bbox,
                    content: BlockContent {
                        text: text.to_string(),
                        lines: vec![Line {
                            bbox,
                            text: text.to_string(),
                            segments: Vec::new(),
                        }],
                        segments: Vec::new(),
                        markdown: None,
                        html: None,
                    },
                    policy: TranslationPolicy::translate("test"),
                    continuation: None,
                    source: SourceProvenance {
                        provider: "test".to_string(),
                        raw_label: "text".to_string(),
                        raw_bbox: bbox,
                        raw_path: None,
                    },
                    tags: Vec::new(),
                    metadata: BTreeMap::new(),
                }],
                metadata: BTreeMap::new(),
            }],
        )
    }
}
