use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use glyphst_settings::{Settings, TranslationProvider};
use glyphst_translation::{
    LanguageCode, SemanticProtectedToken, SemanticProtectedTokenKind, SemanticTranslationUnit,
    TranslationError, TranslationErrorCategory, TranslationService,
};
use serde::{Deserialize, Serialize};

use crate::{BlockRole, CancellationToken, DocumentV1};

const MAX_BATCH_UNITS: usize = 6;
const MAX_BATCH_CHARACTERS: usize = 6_000;
const MAX_BATCH_PLACEHOLDERS: usize = 36;
const MAX_BATCHABLE_UNIT_CHARACTERS: usize = 1_800;
const MAX_BATCHABLE_UNIT_PLACEHOLDERS: usize = 8;
const MAX_ACTIVE_REQUESTS: usize = 6;
const MAX_ACTIVE_COMPLEX_REQUESTS: usize = 2;
const MAX_UNIT_ATTEMPTS: usize = 3;

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
    pub cached: bool,
    pub validation_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslatedUnit {
    pub id: String,
    pub member_block_ids: Vec<String>,
    pub role: BlockRole,
    pub source_text: String,
    pub translated_text: String,
    #[serde(default)]
    pub semantic_hash: u64,
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

pub trait TranslationBackend: Sync {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError>;

    fn reject_unchanged_output(&self) -> bool {
        true
    }

    fn cache_validated_translation(&self, _unit: &TranslationUnit, _translated: &str) {}

    fn cache_validated_translations(&self, translations: &[(TranslationUnit, String)]) {
        for (unit, translated) in translations {
            self.cache_validated_translation(unit, translated);
        }
    }

    fn supports_final_single_recovery(&self) -> bool {
        false
    }

    fn translate_single_recovery(
        &self,
        unit: &TranslationUnit,
    ) -> Result<TranslationUnitOutput, TranslationBackendError> {
        let mut outputs = self.translate_batch(std::slice::from_ref(unit))?;
        if outputs.len() == 1 && outputs[0].id == unit.id {
            Ok(outputs.remove(0))
        } else {
            Err(TranslationBackendError {
                category: Some(TranslationErrorCategory::InvalidResponse),
                message: "translation recovery did not preserve the unit ID".to_string(),
                status: None,
                retryable: true,
                retry_after: None,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationBackendError {
    pub category: Option<TranslationErrorCategory>,
    pub message: String,
    pub status: Option<u16>,
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
            category: Some(error.category),
            message: error.message,
            status: error.status,
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
            .map_err(Into::into)
    }

    fn reject_unchanged_output(&self) -> bool {
        self.source_language
            .as_ref()
            .is_none_or(|source| source != &self.target_language)
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
            .map_err(Into::into)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationDocumentError {
    pub message: String,
    pub unresolved_unit_ids: Vec<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TranslationQueueStats {
    pub planned_requests: usize,
    pub dispatched_requests: usize,
    pub completed_requests: usize,
    pub retry_requests: usize,
    pub peak_active_requests: usize,
    pub peak_active_complex_requests: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationFailureDiagnostic {
    pub id: String,
    pub reason: String,
    pub attempts: usize,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TranslationRunProgress {
    pub translated: usize,
    pub cached: usize,
    pub retrying: usize,
    pub failed: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationRunSnapshot {
    pub resolved_units: Vec<TranslatedUnit>,
    pub failures: Vec<TranslationFailureDiagnostic>,
    pub progress: TranslationRunProgress,
    pub queue: TranslationQueueStats,
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
    translate_document_resuming(document, backend, cancellation, Vec::new(), |snapshot| {
        on_progress(snapshot.resolved_units.len(), snapshot.progress.total);
        Ok(())
    })
}

pub fn translate_document_resuming(
    document: DocumentV1,
    backend: &dyn TranslationBackend,
    cancellation: &CancellationToken,
    resumed_units: Vec<TranslatedUnit>,
    mut on_update: impl FnMut(&TranslationRunSnapshot) -> Result<(), String>,
) -> Result<TranslatedDocument, TranslationDocumentError> {
    let units = build_translation_units(&document);
    let total = units.len();
    if total == 0 {
        return Ok(TranslatedDocument {
            document,
            units: Vec::new(),
        });
    }

    let units_by_id = units
        .iter()
        .map(|unit| (unit.id.clone(), unit))
        .collect::<HashMap<_, _>>();
    let mut resolved = HashMap::with_capacity(total);
    for resumed in resumed_units {
        if let Some(unit) = units_by_id.get(&resumed.id)
            && resumed.semantic_hash == semantic_unit_hash(unit)
            && validate_resumed_translation(
                unit,
                &resumed.translated_text,
                backend.reject_unchanged_output(),
            )
        {
            resolved.insert(resumed.id.clone(), resumed);
        }
    }

    let missing = units
        .iter()
        .filter(|unit| !resolved.contains_key(&unit.id))
        .cloned()
        .collect::<Vec<_>>();
    let plans = plan_translation_requests(&missing);
    let mut pending = plans
        .into_iter()
        .map(|plan| PendingRequest {
            plan,
            attempt: 1,
            ready_at: Instant::now(),
            final_recovery: false,
        })
        .collect::<VecDeque<_>>();
    let mut queue = TranslationQueueStats {
        planned_requests: pending.len(),
        ..TranslationQueueStats::default()
    };
    let mut failures = HashMap::<String, TranslationFailureDiagnostic>::new();
    let mut retrying_ids = HashSet::<String>::new();
    let mut cached_ids = resolved.keys().cloned().collect::<HashSet<_>>();

    let initial = translation_snapshot(
        &units,
        &resolved,
        &failures,
        &retrying_ids,
        &cached_ids,
        queue.clone(),
    );
    on_update(&initial).map_err(|message| TranslationDocumentError {
        message,
        unresolved_unit_ids: missing.iter().map(|unit| unit.id.clone()).collect(),
        retryable: false,
    })?;

    let scheduler_result = thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel::<CompletedRequest>();
        let mut active = 0usize;
        let mut active_complex = 0usize;
        let mut cancelled = false;

        while !pending.is_empty() || active > 0 {
            if cancellation.is_cancelled() {
                cancelled = true;
                pending.clear();
            }
            while !cancelled && active < MAX_ACTIVE_REQUESTS {
                let Some(index) = next_dispatchable_request(&pending, active_complex) else {
                    break;
                };
                let request = pending.remove(index).expect("pending request exists");
                let is_complex = request.plan.complex;
                active += 1;
                active_complex += usize::from(is_complex);
                queue.dispatched_requests += 1;
                queue.peak_active_requests = queue.peak_active_requests.max(active);
                queue.peak_active_complex_requests =
                    queue.peak_active_complex_requests.max(active_complex);
                let sender = sender.clone();
                scope.spawn(move || {
                    let result =
                        catch_unwind(AssertUnwindSafe(|| execute_request(backend, &request)))
                            .unwrap_or_else(|_| {
                                Err(TranslationBackendError {
                                    category: Some(TranslationErrorCategory::InvalidResponse),
                                    message: "PDF translation worker panicked".to_string(),
                                    status: None,
                                    retryable: true,
                                    retry_after: None,
                                })
                            });
                    let _ = sender.send(CompletedRequest { request, result });
                });
            }

            if active == 0 {
                if let Some(delay) = next_ready_delay(&pending, active_complex) {
                    let _ = sleep_cancellable(delay, cancellation);
                    continue;
                }
                break;
            }
            let receive_error = || TranslationDocumentError {
                message: "PDF translation worker stopped unexpectedly".to_string(),
                unresolved_unit_ids: units
                    .iter()
                    .filter(|unit| !resolved.contains_key(&unit.id))
                    .map(|unit| unit.id.clone())
                    .collect(),
                retryable: true,
            };
            let completed = if active < MAX_ACTIVE_REQUESTS
                && let Some(delay) = next_ready_delay(&pending, active_complex)
            {
                match receiver.recv_timeout(delay) {
                    Ok(completed) => completed,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return Err(receive_error()),
                }
            } else {
                receiver.recv().map_err(|_| receive_error())?
            };
            active -= 1;
            active_complex -= usize::from(completed.request.plan.complex);
            queue.completed_requests += 1;
            let mut state = TranslationResolutionState {
                resolved: &mut resolved,
                failures: &mut failures,
                retrying_ids: &mut retrying_ids,
                cached_ids: &mut cached_ids,
            };
            process_completed_request(backend, completed, &mut state, &mut pending, &mut queue);
            let snapshot = translation_snapshot(
                &units,
                &resolved,
                &failures,
                &retrying_ids,
                &cached_ids,
                queue.clone(),
            );
            on_update(&snapshot).map_err(|message| TranslationDocumentError {
                message,
                unresolved_unit_ids: units
                    .iter()
                    .filter(|unit| !resolved.contains_key(&unit.id))
                    .map(|unit| unit.id.clone())
                    .collect(),
                retryable: false,
            })?;
        }
        if cancelled {
            Err(TranslationDocumentError {
                message: "PDF translation was cancelled".to_string(),
                unresolved_unit_ids: Vec::new(),
                retryable: false,
            })
        } else {
            Ok(())
        }
    });
    scheduler_result?;

    let unresolved = units
        .iter()
        .filter(|unit| !resolved.contains_key(&unit.id))
        .map(|unit| unit.id.clone())
        .collect::<Vec<_>>();
    if !unresolved.is_empty() {
        let retryable = unresolved
            .iter()
            .any(|id| failures.get(id).is_some_and(|failure| failure.retryable));
        return Err(TranslationDocumentError {
            message: format!(
                "{} translation unit(s) remain unresolved after recovery; export is blocked",
                unresolved.len()
            ),
            unresolved_unit_ids: unresolved,
            retryable,
        });
    }

    let translated = units
        .iter()
        .filter_map(|unit| resolved.remove(&unit.id))
        .collect();
    Ok(TranslatedDocument {
        document,
        units: translated,
    })
}

#[derive(Clone)]
struct PlannedRequest {
    units: Vec<TranslationUnit>,
    complex: bool,
}

struct PendingRequest {
    plan: PlannedRequest,
    attempt: usize,
    ready_at: Instant,
    final_recovery: bool,
}

struct CompletedRequest {
    request: PendingRequest,
    result: Result<Vec<TranslationUnitOutput>, TranslationBackendError>,
}

struct TranslationResolutionState<'a> {
    resolved: &'a mut HashMap<String, TranslatedUnit>,
    failures: &'a mut HashMap<String, TranslationFailureDiagnostic>,
    retrying_ids: &'a mut HashSet<String>,
    cached_ids: &'a mut HashSet<String>,
}

fn execute_request(
    backend: &dyn TranslationBackend,
    request: &PendingRequest,
) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
    if request.final_recovery && request.plan.units.len() == 1 {
        backend
            .translate_single_recovery(&request.plan.units[0])
            .map(|output| vec![output])
    } else {
        backend.translate_batch(&request.plan.units)
    }
}

fn process_completed_request(
    backend: &dyn TranslationBackend,
    completed: CompletedRequest,
    state: &mut TranslationResolutionState<'_>,
    pending: &mut VecDeque<PendingRequest>,
    queue: &mut TranslationQueueStats,
) {
    let mut outputs = HashMap::new();
    let mut cache_entries = Vec::new();
    let request_error = match completed.result {
        Ok(request_outputs) => {
            let mut duplicate_ids = HashSet::new();
            for output in request_outputs {
                let id = output.id.clone();
                if outputs.insert(id.clone(), output).is_some() {
                    duplicate_ids.insert(id);
                }
            }
            for id in duplicate_ids {
                outputs.remove(&id);
            }
            None
        }
        Err(error) => Some(error),
    };

    for unit in completed.request.plan.units {
        let output = outputs.remove(&unit.id);
        let result = match output {
            Some(output) if output.validation_error.is_none() => {
                validate_translation(&unit, &output.text, backend.reject_unchanged_output())
                    .map(|translated| (translated, output.cached, output.text))
                    .map_err(|message| UnitFailure {
                        message,
                        retryable: true,
                        retry_after: None,
                        category: Some(TranslationErrorCategory::InvalidResponse),
                        status: None,
                    })
            }
            Some(output) => Err(UnitFailure {
                message: output
                    .validation_error
                    .expect("validation error was checked above"),
                retryable: false,
                retry_after: None,
                category: Some(TranslationErrorCategory::InvalidResponse),
                status: None,
            }),
            None => {
                let error = request_error.as_ref();
                Err(UnitFailure {
                    message: error
                        .map(|error| error.message.clone())
                        .unwrap_or_else(|| "translation response omitted the unit".to_string()),
                    retryable: error.is_none_or(|error| error.retryable),
                    retry_after: error.and_then(|error| error.retry_after),
                    category: error.and_then(|error| error.category),
                    status: error.and_then(|error| error.status),
                })
            }
        };

        match result {
            Ok((translated_text, cached, validated_output)) => {
                if !cached {
                    cache_entries.push((unit.clone(), validated_output));
                } else {
                    state.cached_ids.insert(unit.id.clone());
                }
                state.retrying_ids.remove(&unit.id);
                state.failures.remove(&unit.id);
                let semantic_hash = semantic_unit_hash(&unit);
                state.resolved.insert(
                    unit.id.clone(),
                    TranslatedUnit {
                        id: unit.id,
                        member_block_ids: unit.member_block_ids,
                        role: unit.role,
                        source_text: unit.source_text,
                        translated_text,
                        semantic_hash,
                    },
                );
            }
            Err(failure) => {
                let attempt = completed.request.attempt;
                let can_retry = attempt < MAX_UNIT_ATTEMPTS && failure.retryable;
                state.failures.insert(
                    unit.id.clone(),
                    TranslationFailureDiagnostic {
                        id: unit.id.clone(),
                        reason: failure.message,
                        attempts: attempt,
                        retryable: failure.retryable,
                        category: failure.category.map(translation_error_category_name),
                        status: failure.status,
                    },
                );
                if can_retry {
                    state.retrying_ids.insert(unit.id.clone());
                    queue.retry_requests += 1;
                    let now = Instant::now();
                    let delay = retry_delay(attempt, failure.retry_after);
                    pending.push_back(PendingRequest {
                        plan: PlannedRequest {
                            units: vec![unit],
                            complex: true,
                        },
                        attempt: attempt + 1,
                        ready_at: now.checked_add(delay).unwrap_or(now),
                        final_recovery: backend.supports_final_single_recovery()
                            && attempt + 1 == MAX_UNIT_ATTEMPTS,
                    });
                } else {
                    state.retrying_ids.remove(&unit.id);
                }
            }
        }
    }
    backend.cache_validated_translations(&cache_entries);
}

struct UnitFailure {
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
    category: Option<TranslationErrorCategory>,
    status: Option<u16>,
}

fn translation_snapshot(
    units: &[TranslationUnit],
    resolved: &HashMap<String, TranslatedUnit>,
    failures: &HashMap<String, TranslationFailureDiagnostic>,
    retrying_ids: &HashSet<String>,
    cached_ids: &HashSet<String>,
    queue: TranslationQueueStats,
) -> TranslationRunSnapshot {
    let resolved_units = units
        .iter()
        .filter_map(|unit| resolved.get(&unit.id).cloned())
        .collect::<Vec<_>>();
    let cached = resolved_units
        .iter()
        .filter(|unit| cached_ids.contains(&unit.id))
        .count();
    let mut failure_list = failures.values().cloned().collect::<Vec<_>>();
    failure_list.sort_by_key(|failure| {
        units
            .iter()
            .position(|unit| unit.id == failure.id)
            .unwrap_or(usize::MAX)
    });
    TranslationRunSnapshot {
        progress: TranslationRunProgress {
            translated: resolved_units.len().saturating_sub(cached),
            cached,
            retrying: retrying_ids.len(),
            failed: failure_list
                .iter()
                .filter(|failure| !retrying_ids.contains(&failure.id))
                .count(),
            total: units.len(),
        },
        resolved_units,
        failures: failure_list,
        queue,
    }
}

fn retry_delay(attempt: usize, retry_after: Option<Duration>) -> Duration {
    retry_after
        .unwrap_or_else(|| Duration::from_millis(250 * attempt as u64).min(Duration::from_secs(5)))
        .min(Duration::from_secs(15 * 60))
}

fn next_dispatchable_request(
    pending: &VecDeque<PendingRequest>,
    active_complex: usize,
) -> Option<usize> {
    let now = Instant::now();
    pending.iter().position(|request| {
        request.ready_at <= now
            && (!request.plan.complex || active_complex < MAX_ACTIVE_COMPLEX_REQUESTS)
    })
}

fn next_ready_delay(pending: &VecDeque<PendingRequest>, active_complex: usize) -> Option<Duration> {
    let now = Instant::now();
    pending
        .iter()
        .filter(|request| !request.plan.complex || active_complex < MAX_ACTIVE_COMPLEX_REQUESTS)
        .map(|request| request.ready_at.saturating_duration_since(now))
        .min()
}

fn translation_error_category_name(category: TranslationErrorCategory) -> String {
    match category {
        TranslationErrorCategory::Authentication => "authentication",
        TranslationErrorCategory::RateLimited => "rate_limited",
        TranslationErrorCategory::ServiceUnavailable => "service_unavailable",
        TranslationErrorCategory::Timeout => "timeout",
        TranslationErrorCategory::InvalidRequest => "invalid_request",
        TranslationErrorCategory::InvalidResponse => "invalid_response",
        TranslationErrorCategory::Network => "network",
        TranslationErrorCategory::Cache => "cache",
    }
    .to_string()
}

pub fn protect_text(text: &str) -> ProtectedText {
    protect_text_with_identifier_policy(text, true)
}

fn protect_text_for_role(text: &str, role: BlockRole) -> ProtectedText {
    protect_text_with_identifier_policy(
        text,
        !matches!(
            role,
            BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
        ),
    )
}

fn protect_text_with_identifier_policy(text: &str, protect_identifiers: bool) -> ProtectedText {
    let mut protected = String::with_capacity(text.len());
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        let rest = &text[cursor..];
        if let Some((kind, length)) = protected_prefix(rest, protect_identifiers) {
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
                unit.protected = protect_text_for_role(&unit.source_text, unit.role);
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
                protected: protect_text_for_role(&source_text, block.role),
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

fn plan_translation_requests(units: &[TranslationUnit]) -> Vec<PlannedRequest> {
    let mut requests = Vec::new();
    let mut batch = Vec::new();
    let mut batch_characters = 0usize;
    let mut batch_placeholders = 0usize;

    let flush_batch = |requests: &mut Vec<PlannedRequest>,
                       batch: &mut Vec<TranslationUnit>,
                       characters: &mut usize,
                       placeholders: &mut usize| {
        if batch.is_empty() {
            return;
        }
        requests.push(PlannedRequest {
            units: std::mem::take(batch),
            complex: false,
        });
        *characters = 0;
        *placeholders = 0;
    };

    for unit in units {
        let characters = unit.protected.text.chars().count();
        let placeholders = unit.protected.tokens.len();
        let batchable = matches!(unit.role, BlockRole::Body | BlockRole::Abstract)
            && characters <= MAX_BATCHABLE_UNIT_CHARACTERS
            && placeholders <= MAX_BATCHABLE_UNIT_PLACEHOLDERS;
        if !batchable {
            flush_batch(
                &mut requests,
                &mut batch,
                &mut batch_characters,
                &mut batch_placeholders,
            );
            requests.push(PlannedRequest {
                units: vec![unit.clone()],
                complex: true,
            });
            continue;
        }
        if !batch.is_empty()
            && (batch.len() >= MAX_BATCH_UNITS
                || batch_characters + characters > MAX_BATCH_CHARACTERS
                || batch_placeholders + placeholders > MAX_BATCH_PLACEHOLDERS)
        {
            flush_batch(
                &mut requests,
                &mut batch,
                &mut batch_characters,
                &mut batch_placeholders,
            );
        }
        batch_characters += characters;
        batch_placeholders += placeholders;
        batch.push(unit.clone());
    }
    flush_batch(
        &mut requests,
        &mut batch,
        &mut batch_characters,
        &mut batch_placeholders,
    );
    requests
}

fn semantic_unit(unit: &TranslationUnit) -> SemanticTranslationUnit {
    SemanticTranslationUnit {
        id: unit.id.clone(),
        role: role_name(unit.role).to_string(),
        text: unit.protected.text.clone(),
        protected_tokens: unit
            .protected
            .tokens
            .iter()
            .map(|token| SemanticProtectedToken {
                placeholder: token.placeholder.clone(),
                original: token.original.clone(),
                kind: semantic_protected_token_kind(token.kind),
            })
            .collect(),
        context_before: unit.context_before.clone(),
        context_after: unit.context_after.clone(),
    }
}

fn semantic_protected_token_kind(kind: ProtectedTokenKind) -> SemanticProtectedTokenKind {
    match kind {
        ProtectedTokenKind::Formula => SemanticProtectedTokenKind::Formula,
        ProtectedTokenKind::Citation => SemanticProtectedTokenKind::Citation,
        ProtectedTokenKind::Url => SemanticProtectedTokenKind::Url,
        ProtectedTokenKind::NumericLiteral => SemanticProtectedTokenKind::NumericLiteral,
        ProtectedTokenKind::Identifier => SemanticProtectedTokenKind::Identifier,
    }
}

fn semantic_unit_hash(unit: &TranslationUnit) -> u64 {
    let mut hasher = DefaultHasher::new();
    unit.id.hash(&mut hasher);
    unit.member_block_ids.hash(&mut hasher);
    role_name(unit.role).hash(&mut hasher);
    unit.source_text.hash(&mut hasher);
    unit.protected.text.hash(&mut hasher);
    unit.context_before.hash(&mut hasher);
    unit.context_after.hash(&mut hasher);
    hasher.finish()
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

fn validate_resumed_translation(
    unit: &TranslationUnit,
    translated: &str,
    reject_unchanged: bool,
) -> bool {
    if translated.trim().is_empty() || translated.contains("[[GLYPHST_") {
        return false;
    }
    if unit
        .protected
        .tokens
        .iter()
        .any(|token| !translated.contains(&token.original))
    {
        return false;
    }
    if reject_unchanged
        && should_reject_unchanged(&unit.source_text)
        && normalize_for_comparison(translated) == normalize_for_comparison(&unit.source_text)
    {
        return false;
    }
    let lowercase = translated.trim().to_ascii_lowercase();
    !(lowercase.starts_with("i cannot translate")
        || lowercase.starts_with("i can't translate")
        || lowercase.starts_with("unable to translate"))
}

fn protected_prefix(text: &str, protect_identifiers: bool) -> Option<(ProtectedTokenKind, usize)> {
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
    if protect_identifiers
        && text
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

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
                        cached: false,
                        validation_error: None,
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
    fn semantic_units_forward_exact_protected_token_metadata() {
        let unit = build_translation_units(&test_document(
            "Use AI for 25 cases; see [12], $x < y$, and https://example.test/?a=1&b=2.",
        ))
        .remove(0);

        let semantic = semantic_unit(&unit);

        assert_eq!(semantic.text, unit.protected.text);
        assert_eq!(semantic.protected_tokens.len(), unit.protected.tokens.len());
        assert_eq!(
            semantic
                .protected_tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                SemanticProtectedTokenKind::Identifier,
                SemanticProtectedTokenKind::NumericLiteral,
                SemanticProtectedTokenKind::Citation,
                SemanticProtectedTokenKind::Formula,
                SemanticProtectedTokenKind::Url,
            ]
        );
        for (semantic, protected) in semantic.protected_tokens.iter().zip(&unit.protected.tokens) {
            assert_eq!(semantic.placeholder, protected.placeholder);
            assert_eq!(semantic.original, protected.original);
        }
    }

    #[test]
    fn heading_words_remain_translatable_while_body_identifiers_are_protected() {
        let heading = protect_text_for_role("1 INTRODUCTION", BlockRole::Heading);
        let body = protect_text_for_role("Use INTRODUCTION as an identifier.", BlockRole::Body);

        assert!(heading.text.contains("INTRODUCTION"));
        assert!(
            !heading
                .tokens
                .iter()
                .any(|token| token.kind == ProtectedTokenKind::Identifier)
        );
        assert!(
            body.tokens
                .iter()
                .any(|token| token.kind == ProtectedTokenKind::Identifier)
        );
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
        struct InvalidBackend {
            calls: AtomicUsize,
        }
        impl TranslationBackend for InvalidBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(units
                    .iter()
                    .map(|unit| TranslationUnitOutput {
                        id: unit.id.clone(),
                        text: String::new(),
                        cached: false,
                        validation_error: None,
                    })
                    .collect())
            }
        }

        let backend = InvalidBackend {
            calls: AtomicUsize::new(0),
        };
        let last_snapshot = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&last_snapshot);
        let error = translate_document_resuming(
            test_document("This paragraph must be translated."),
            &backend,
            &CancellationToken::new(),
            Vec::new(),
            move |snapshot| {
                *captured.lock().unwrap() = Some(snapshot.clone());
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.unresolved_unit_ids, ["unit-p001-b0000"]);
        assert!(error.retryable);
        assert_eq!(backend.calls.load(Ordering::SeqCst), MAX_UNIT_ATTEMPTS);
        let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.progress.failed, 1);
        assert_eq!(snapshot.failures[0].attempts, MAX_UNIT_ATTEMPTS);
    }

    #[test]
    fn provider_recovery_diagnostic_stops_identical_scheduler_retries() {
        struct ProviderRecoveryBackend {
            calls: AtomicUsize,
        }

        impl TranslationBackend for ProviderRecoveryBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(units
                    .iter()
                    .map(|unit| TranslationUnitOutput {
                        id: unit.id.clone(),
                        text: unit.protected.text.clone(),
                        cached: false,
                        validation_error: Some(
                            "Google semantic recovery failed after segmented HTML; batch HTML \
                             token mismatch; singleton HTML token mismatch; segmented HTML token \
                             mismatch"
                                .to_string(),
                        ),
                    })
                    .collect())
            }
        }

        let backend = ProviderRecoveryBackend {
            calls: AtomicUsize::new(0),
        };
        let last_snapshot = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&last_snapshot);
        let error = translate_document_resuming(
            test_document("This paragraph includes [12] and must be translated."),
            &backend,
            &CancellationToken::new(),
            Vec::new(),
            move |snapshot| {
                *captured.lock().unwrap() = Some(snapshot.clone());
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.unresolved_unit_ids, ["unit-p001-b0000"]);
        assert!(!error.retryable);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
        let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.failures[0].attempts, 1);
        assert!(!snapshot.failures[0].retryable);
        assert!(snapshot.failures[0].reason.contains("batch HTML"));
        assert!(snapshot.failures[0].reason.contains("singleton HTML"));
        assert!(snapshot.failures[0].reason.contains("segmented HTML"));
    }

    #[test]
    fn permanent_backend_failures_are_not_retryable() {
        struct PermanentBackend {
            calls: AtomicUsize,
        }
        impl TranslationBackend for PermanentBackend {
            fn translate_batch(
                &self,
                _units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(TranslationBackendError {
                    category: Some(TranslationErrorCategory::Authentication),
                    message: "credentials are invalid".to_string(),
                    status: Some(401),
                    retryable: false,
                    retry_after: None,
                })
            }
        }

        let backend = PermanentBackend {
            calls: AtomicUsize::new(0),
        };
        let error = translate_document(
            test_document("This paragraph must be translated."),
            &backend,
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap_err();

        assert!(!error.retryable);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn worker_panics_become_retryable_failures() {
        struct PanicBackend;
        impl TranslationBackend for PanicBackend {
            fn translate_batch(
                &self,
                _units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                panic!("worker panic")
            }
        }

        let error = translate_document(
            test_document("This paragraph must be translated."),
            &PanicBackend,
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap_err();

        assert!(error.retryable);
    }

    #[test]
    fn risk_aware_planning_batches_only_small_body_units() {
        let mut blocks = vec![
            (BlockRole::Heading, "A heading".to_string()),
            (BlockRole::FigureCaption, "A caption".to_string()),
            (BlockRole::Body, "long ".repeat(500)),
        ];
        blocks.extend(
            (0..7).map(|index| (BlockRole::Body, format!("Body paragraph number {index}."))),
        );
        let document = test_document_with_blocks(blocks);
        let units = build_translation_units(&document);

        let plans = plan_translation_requests(&units);

        assert_eq!(plans[0].units.len(), 1);
        assert!(plans[0].complex);
        assert_eq!(plans[1].units.len(), 1);
        assert!(plans[1].complex);
        assert_eq!(plans[2].units.len(), 1);
        assert!(plans[2].complex);
        assert_eq!(plans[3].units.len(), 6);
        assert!(!plans[3].complex);
        assert_eq!(plans[4].units.len(), 1);
    }

    #[test]
    fn bounded_parallel_translation_preserves_document_order() {
        struct ConcurrentBackend {
            active: AtomicUsize,
            peak: AtomicUsize,
            active_complex: AtomicUsize,
            peak_complex: AtomicUsize,
        }

        impl TranslationBackend for ConcurrentBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                update_peak(&self.peak, active);
                let complex = units.len() == 1 && units[0].role == BlockRole::Heading;
                if complex {
                    let active_complex = self.active_complex.fetch_add(1, Ordering::SeqCst) + 1;
                    update_peak(&self.peak_complex, active_complex);
                }
                let delay = units
                    .first()
                    .and_then(|unit| unit.member_block_ids[0].rsplit('-').next())
                    .and_then(|value| value.parse::<usize>().ok())
                    .map(|index| 15 + (index % 4) * 10)
                    .unwrap_or(25);
                thread::sleep(Duration::from_millis(delay as u64));
                if complex {
                    self.active_complex.fetch_sub(1, Ordering::SeqCst);
                }
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(valid_outputs(units))
            }
        }

        let mut blocks = (0..4)
            .map(|index| (BlockRole::Heading, format!("Heading {index}")))
            .collect::<Vec<_>>();
        blocks.extend(
            (0..36).map(|index| (BlockRole::Body, format!("Body paragraph number {index}."))),
        );
        let document = test_document_with_blocks(blocks);
        let expected_ids = build_translation_units(&document)
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<_>>();
        let backend = ConcurrentBackend {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            active_complex: AtomicUsize::new(0),
            peak_complex: AtomicUsize::new(0),
        };
        let last_snapshot = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&last_snapshot);

        let translated = translate_document_resuming(
            document,
            &backend,
            &CancellationToken::new(),
            Vec::new(),
            move |snapshot| {
                *captured.lock().unwrap() = Some(snapshot.clone());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            translated
                .units
                .iter()
                .map(|unit| unit.id.clone())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(backend.peak.load(Ordering::SeqCst), MAX_ACTIVE_REQUESTS);
        assert_eq!(
            backend.peak_complex.load(Ordering::SeqCst),
            MAX_ACTIVE_COMPLEX_REQUESTS
        );
        let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.queue.peak_active_requests, MAX_ACTIVE_REQUESTS);
        assert_eq!(
            snapshot.queue.peak_active_complex_requests,
            MAX_ACTIVE_COMPLEX_REQUESTS
        );
    }

    #[test]
    fn retains_valid_partial_results_and_retries_only_omitted_units() {
        struct PartialBackend {
            attempts: Mutex<HashMap<String, usize>>,
        }

        impl TranslationBackend for PartialBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                let mut attempts = self.attempts.lock().unwrap();
                for unit in units {
                    *attempts.entry(unit.id.clone()).or_default() += 1;
                }
                let omit_last = units.len() > 1;
                Ok(units
                    .iter()
                    .take(units.len() - usize::from(omit_last))
                    .map(valid_output)
                    .collect())
            }
        }

        let document = test_document_with_blocks(
            (0..3)
                .map(|index| {
                    (
                        BlockRole::Body,
                        format!("Paragraph {index} needs translation."),
                    )
                })
                .collect(),
        );
        let ids = build_translation_units(&document)
            .into_iter()
            .map(|unit| unit.id)
            .collect::<Vec<_>>();
        let backend = PartialBackend {
            attempts: Mutex::new(HashMap::new()),
        };

        let translated =
            translate_document(document, &backend, &CancellationToken::new(), |_, _| {}).unwrap();

        assert_eq!(translated.units.len(), 3);
        let attempts = backend.attempts.lock().unwrap();
        assert_eq!(attempts[&ids[0]], 1);
        assert_eq!(attempts[&ids[1]], 1);
        assert_eq!(attempts[&ids[2]], 2);
    }

    #[test]
    fn final_single_recovery_resolves_poisoned_batch_output() {
        struct RecoveryBackend {
            recovery_calls: AtomicUsize,
        }

        impl TranslationBackend for RecoveryBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                Ok(units
                    .iter()
                    .map(|unit| TranslationUnitOutput {
                        id: unit.id.clone(),
                        text: String::new(),
                        cached: false,
                        validation_error: None,
                    })
                    .collect())
            }

            fn supports_final_single_recovery(&self) -> bool {
                true
            }

            fn translate_single_recovery(
                &self,
                unit: &TranslationUnit,
            ) -> Result<TranslationUnitOutput, TranslationBackendError> {
                self.recovery_calls.fetch_add(1, Ordering::SeqCst);
                Ok(valid_output(unit))
            }
        }

        let backend = RecoveryBackend {
            recovery_calls: AtomicUsize::new(0),
        };
        let translated = translate_document(
            test_document("This paragraph must be translated."),
            &backend,
            &CancellationToken::new(),
            |_, _| {},
        )
        .unwrap();

        assert_eq!(translated.units.len(), 1);
        assert_eq!(backend.recovery_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn retry_delay_honors_complete_provider_hint() {
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(2))),
            Duration::from_secs(2)
        );
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(20))),
            Duration::from_secs(20)
        );
        assert_eq!(retry_delay(2, None), Duration::from_millis(500));
    }

    #[test]
    fn resumed_units_skip_backend_requests() {
        struct PanicBackend;
        impl TranslationBackend for PanicBackend {
            fn translate_batch(
                &self,
                _units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                panic!("checkpointed unit should not be requested")
            }
        }

        let document = test_document("This paragraph was translated previously.");
        let unit = build_translation_units(&document).remove(0);
        let resumed = TranslatedUnit {
            id: unit.id.clone(),
            member_block_ids: unit.member_block_ids.clone(),
            role: unit.role,
            source_text: unit.source_text.clone(),
            translated_text: "Previously translated paragraph.".to_string(),
            semantic_hash: semantic_unit_hash(&unit),
        };
        let last_snapshot = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&last_snapshot);

        let translated = translate_document_resuming(
            document,
            &PanicBackend,
            &CancellationToken::new(),
            vec![resumed],
            move |snapshot| {
                *captured.lock().unwrap() = Some(snapshot.clone());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(translated.units.len(), 1);
        let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.progress.cached, 1);
        assert_eq!(snapshot.queue.dispatched_requests, 0);
    }

    #[test]
    fn semantic_hash_includes_neighboring_context() {
        let document = test_document("This paragraph was translated previously.");
        let mut unit = build_translation_units(&document).remove(0);
        let original = semantic_unit_hash(&unit);
        unit.context_after = Some("A changed neighboring paragraph.".to_string());

        assert_ne!(semantic_unit_hash(&unit), original);
    }

    #[test]
    fn cancellation_stops_queued_translation_work() {
        struct SlowBackend {
            calls: AtomicUsize,
        }

        impl TranslationBackend for SlowBackend {
            fn translate_batch(
                &self,
                units: &[TranslationUnit],
            ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(80));
                Ok(valid_outputs(units))
            }
        }

        let document = test_document_with_blocks(
            (0..48)
                .map(|index| {
                    (
                        BlockRole::Body,
                        format!("Paragraph {index} needs translation."),
                    )
                })
                .collect(),
        );
        let backend = SlowBackend {
            calls: AtomicUsize::new(0),
        };
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(15));
            cancel.cancel();
        });

        let error = translate_document(document, &backend, &cancellation, |_, _| {}).unwrap_err();
        canceller.join().unwrap();

        assert_eq!(error.message, "PDF translation was cancelled");
        assert!(backend.calls.load(Ordering::SeqCst) <= MAX_ACTIVE_REQUESTS);
    }

    fn update_peak(peak: &AtomicUsize, value: usize) {
        let mut observed = peak.load(Ordering::SeqCst);
        while value > observed {
            match peak.compare_exchange(observed, value, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(next) => observed = next,
            }
        }
    }

    fn valid_output(unit: &TranslationUnit) -> TranslationUnitOutput {
        TranslationUnitOutput {
            id: unit.id.clone(),
            text: format!("Translated {}", unit.protected.text),
            cached: false,
            validation_error: None,
        }
    }

    fn valid_outputs(units: &[TranslationUnit]) -> Vec<TranslationUnitOutput> {
        units.iter().map(valid_output).collect()
    }

    fn test_document(text: &str) -> DocumentV1 {
        test_document_with_blocks(vec![(BlockRole::Body, text.to_string())])
    }

    fn test_document_with_blocks(blocks: Vec<(BlockRole, String)>) -> DocumentV1 {
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
                blocks: blocks
                    .into_iter()
                    .enumerate()
                    .map(|(index, (role, text))| Block {
                        id: format!("p001-b{index:04}"),
                        page_index: 0,
                        order: index,
                        reading_order: index,
                        kind: BlockKind::Text,
                        role,
                        bbox,
                        content: BlockContent {
                            text: text.clone(),
                            lines: vec![Line {
                                bbox,
                                text,
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
                    })
                    .collect(),
                metadata: BTreeMap::new(),
            }],
        )
    }
}
