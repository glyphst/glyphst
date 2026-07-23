use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{Block, BlockRole, BoundingBox, CancellationToken, DocumentV1, Line, TextStyle};

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
    #[serde(default)]
    pub style_definitions: Vec<StyleDefinition>,
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
    pub styled_runs: Vec<StyledRun>,
    #[serde(default)]
    pub style_fallback: bool,
    #[serde(default)]
    pub semantic_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleDefinition {
    pub id: usize,
    pub style: TextStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyledRun {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<TextStyle>,
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

    fn restore_translation(&self, translated: &str) -> Result<(String, bool), String> {
        if translated.trim().is_empty() {
            return Err("translation is empty".to_string());
        }
        let mut style_fallback = false;
        for token in &self.tokens {
            let count = translated.matches(&token.placeholder).count();
            if matches!(
                token.kind,
                ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
            ) {
                style_fallback |= count != 1;
            } else if count != 1 {
                return Err(format!(
                    "placeholder {} occurred {count} times",
                    token.placeholder
                ));
            }
        }
        for pair in self.tokens.windows(2) {
            let adjacent = format!("{}{}", pair[0].placeholder, pair[1].placeholder);
            if self.text.contains(&adjacent) && !translated.contains(&adjacent) {
                return Err(format!(
                    "adjacent placeholders {} and {} were separated",
                    pair[0].placeholder, pair[1].placeholder
                ));
            }
        }
        let mut restored = translated.to_string();
        for token in &self.tokens {
            if restored.contains(&token.placeholder) {
                restored = restored.replace(
                    &token.placeholder,
                    token
                        .styled_restoration
                        .as_deref()
                        .unwrap_or(&token.original),
                );
            }
        }
        restored = strip_style_placeholders(&restored);
        if restored.contains("[[GLYPHST_") {
            return Err("translation contains an unresolved placeholder".to_string());
        }
        Ok((restored, style_fallback))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedToken {
    pub placeholder: String,
    pub original: String,
    pub kind: ProtectedTokenKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub styled_restoration: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedTokenKind {
    Formula,
    Citation,
    Url,
    NumericLiteral,
    Identifier,
    LineBreak,
    StyleStart,
    StyleEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationErrorCategory {
    Authentication,
    RateLimited,
    ServiceUnavailable,
    Timeout,
    InvalidRequest,
    InvalidResponse,
    Network,
    Cache,
}

pub trait TranslationBackend: Sync {
    fn translate_batch(
        &self,
        units: &[TranslationUnit],
    ) -> Result<Vec<TranslationUnitOutput>, TranslationBackendError>;

    fn reject_unchanged_output(&self) -> bool {
        true
    }

    fn validate_translated_text(
        &self,
        _unit: &TranslationUnit,
        _translated: &str,
    ) -> Result<(), String> {
        Ok(())
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
    #[serde(default)]
    pub provider_retry_requests: usize,
    #[serde(default)]
    pub validation_retry_requests: usize,
    pub peak_active_requests: usize,
    pub peak_active_complex_requests: usize,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub total_request_latency_ms: u64,
    #[serde(default)]
    pub max_request_latency_ms: u64,
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
    let started_at = Instant::now();
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
        queue_snapshot(&queue, started_at),
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
                    let request_started_at = Instant::now();
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
                    let _ = sender.send(CompletedRequest {
                        request,
                        result,
                        duration_ms: duration_millis(request_started_at.elapsed()),
                    });
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
            queue.total_request_latency_ms = queue
                .total_request_latency_ms
                .saturating_add(completed.duration_ms);
            queue.max_request_latency_ms = queue.max_request_latency_ms.max(completed.duration_ms);
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
                queue_snapshot(&queue, started_at),
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
    duration_ms: u64,
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
    let provider_request_failed = completed.result.is_err();
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
                let normalized_output =
                    normalize_translated_whitespace(&output.text, &unit.source_text, unit.role);
                validate_translation(&unit, &normalized_output, backend.reject_unchanged_output())
                    .and_then(|translated| {
                        backend
                            .validate_translated_text(&unit, &translated.text)
                            .map(|()| translated)
                    })
                    .map(|translated| (translated, output.cached, normalized_output))
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
            Ok((translated, cached, validated_output)) => {
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
                        translated_text: translated.text,
                        styled_runs: translated.runs,
                        style_fallback: translated.style_fallback,
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
                    if provider_request_failed {
                        queue.provider_retry_requests += 1;
                    } else {
                        queue.validation_retry_requests += 1;
                    }
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

fn queue_snapshot(queue: &TranslationQueueStats, started_at: Instant) -> TranslationQueueStats {
    let mut snapshot = queue.clone();
    snapshot.elapsed_ms = duration_millis(started_at.elapsed());
    snapshot
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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
    protect_text_with_policies(
        text,
        !matches!(
            role,
            BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
        ),
        preserves_role_line_breaks(role),
    )
}

#[derive(Debug, Clone, Copy)]
struct StyleBoundary {
    offset: usize,
    style_id: usize,
    start: bool,
}

fn protect_block_text(
    block: &Block,
    role: BlockRole,
) -> (String, ProtectedText, Vec<StyleDefinition>) {
    let source = block.content.text.trim();
    let Some(dominant) = dominant_text_style(block) else {
        let source = normalize_pdf_hard_wraps(source, role);
        return (
            source.clone(),
            protect_text_for_role(&source, role),
            Vec::new(),
        );
    };
    let mut definitions = Vec::<StyleDefinition>::new();
    let mut boundaries = Vec::<StyleBoundary>::new();
    let mut search_from = 0usize;
    for line in &block.content.lines {
        let Some(relative_line_start) = source[search_from..].find(line.text.trim()) else {
            continue;
        };
        let line_start = search_from + relative_line_start;
        let mut segment_offset = line_start;
        for segment in &line.segments {
            let segment_text = segment.text.as_str();
            let Some(relative_segment_start) = source[segment_offset..].find(segment_text) else {
                continue;
            };
            let start = segment_offset + relative_segment_start;
            let end = start + segment_text.len();
            segment_offset = end;
            let styled_start = start + segment_text.len() - segment_text.trim_start().len();
            let styled_end = end - (segment_text.len() - segment_text.trim_end().len());
            if styled_start >= styled_end || !meaningful_style_difference(&dominant, &segment.style)
            {
                continue;
            }
            let style_id = definitions
                .iter()
                .find(|definition| definition.style == segment.style)
                .map(|definition| definition.id)
                .unwrap_or_else(|| {
                    let id = definitions.len();
                    definitions.push(StyleDefinition {
                        id,
                        style: segment.style.clone(),
                    });
                    id
                });
            boundaries.push(StyleBoundary {
                offset: styled_start,
                style_id,
                start: true,
            });
            boundaries.push(StyleBoundary {
                offset: styled_end,
                style_id,
                start: false,
            });
        }
        search_from = line_start + line.text.trim().len();
    }
    boundaries.sort_by(|left, right| {
        left.offset
            .cmp(&right.offset)
            .then(left.start.cmp(&right.start))
    });
    let normalized = normalize_styled_source(source, &boundaries, role, &block.content.lines);
    let protected = if normalized.boundaries.is_empty() {
        protect_text_for_role(&normalized.text, role)
    } else {
        protect_text_with_style_boundaries(
            &normalized.text,
            !matches!(
                role,
                BlockRole::Title | BlockRole::Heading | BlockRole::TableOfContents
            ),
            preserves_role_line_breaks(role),
            &normalized.boundaries,
        )
    };
    (normalized.text, protected, definitions)
}

#[derive(Debug, Clone, Copy)]
struct StyledSourceCharacter {
    character: char,
    style_id: Option<usize>,
}

struct NormalizedStyledSource {
    text: String,
    boundaries: Vec<StyleBoundary>,
}

fn normalize_styled_source(
    source: &str,
    boundaries: &[StyleBoundary],
    role: BlockRole,
    source_lines: &[Line],
) -> NormalizedStyledSource {
    let annotated = annotate_source_styles(source, boundaries);
    let mut lines = vec![Vec::<StyledSourceCharacter>::new()];
    for character in annotated {
        match character.character {
            '\r' => {}
            '\n' => lines.push(Vec::new()),
            _ => lines
                .last_mut()
                .expect("at least one styled line exists")
                .push(character),
        }
    }

    let normalized = match role {
        BlockRole::Dialogue => normalize_styled_dialogue_lines(lines),
        BlockRole::Prompt => normalize_styled_prompt_lines(lines, source_lines),
        _ if preserves_semantic_line_breaks(role, source) => normalize_styled_verbatim_lines(lines),
        _ => normalize_styled_visual_lines(lines),
    };
    styled_characters_to_source(normalized)
}

fn annotate_source_styles(
    source: &str,
    boundaries: &[StyleBoundary],
) -> Vec<StyledSourceCharacter> {
    let mut characters = Vec::with_capacity(source.chars().count());
    let mut boundary_index = 0usize;
    let mut active_style = None;
    for (offset, character) in source.char_indices() {
        while let Some(boundary) = boundaries.get(boundary_index)
            && boundary.offset <= offset
        {
            if boundary.start {
                active_style = Some(boundary.style_id);
            } else if active_style == Some(boundary.style_id) {
                active_style = None;
            }
            boundary_index += 1;
        }
        characters.push(StyledSourceCharacter {
            character,
            style_id: active_style,
        });
    }
    characters
}

fn normalize_styled_verbatim_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line_end(line);
    }
    while lines.first().is_some_and(|line| line.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut output = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            output.push(unstyled_character('\n'));
        }
        output.extend(line);
    }
    trim_styled_output(&mut output);
    output
}

fn normalize_styled_visual_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let mut output = Vec::new();
    let mut paragraph_has_text = false;
    let mut previous_was_empty = true;
    for line in lines {
        if line.is_empty() {
            if paragraph_has_text {
                previous_was_empty = true;
            }
            continue;
        }
        if previous_was_empty && paragraph_has_text {
            output.push(unstyled_character('\n'));
            output.push(unstyled_character('\n'));
        } else if paragraph_has_text {
            let dehyphenate = output
                .last()
                .is_some_and(|character| character.character == '-')
                && line
                    .first()
                    .is_some_and(|character| character.character.is_lowercase());
            if dehyphenate {
                output.pop();
            } else if !output
                .last()
                .is_some_and(|character| character.character.is_whitespace())
            {
                output.push(unstyled_character(' '));
            }
        }
        output.extend(line);
        paragraph_has_text = true;
        previous_was_empty = false;
    }
    trim_styled_output(&mut output);
    output
}

fn normalize_styled_dialogue_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let mut output = Vec::new();
    for line in lines.into_iter().filter(|line| !line.is_empty()) {
        let line_text = styled_line_text(&line);
        let starts_turn = looks_like_speaker_turn(&line_text);
        if output.is_empty() {
            output.extend(line);
        } else if starts_turn {
            output.push(unstyled_character('\n'));
            output.extend(line);
        } else {
            append_styled_visual_line(&mut output, line);
        }
    }
    trim_styled_output(&mut output);
    output
}

fn normalize_styled_prompt_lines(
    mut lines: Vec<Vec<StyledSourceCharacter>>,
    source_lines: &[Line],
) -> Vec<StyledSourceCharacter> {
    for line in &mut lines {
        trim_styled_line(line);
    }
    let block_width = source_lines
        .iter()
        .map(|line| line.bbox)
        .reduce(BoundingBox::union)
        .map_or(0.0, BoundingBox::width);
    let mut output = Vec::new();
    let mut previous_text = String::new();
    let mut previous_source_line = None::<&Line>;
    for (line_index, line) in lines.into_iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        let line_text = styled_line_text(&line);
        let source_line = source_lines.get(line_index);
        let strong_boundary = looks_like_prompt_boundary(&line_text)
            || looks_like_bracketed_prompt_heading(&previous_text)
            || prompt_sentence_boundary(&previous_text, &line_text);
        let visual_continuation =
            source_line
                .zip(previous_source_line)
                .is_some_and(|(current, previous)| {
                    prompt_line_is_visual_continuation(previous, current, block_width, &line_text)
                });
        let structural_break = !output.is_empty() && strong_boundary && !visual_continuation;
        if structural_break {
            output.push(unstyled_character('\n'));
            output.extend(line);
        } else if output.is_empty() {
            output.extend(line);
        } else {
            append_styled_visual_line(&mut output, line);
        }
        previous_text = line_text;
        previous_source_line = source_line;
    }
    trim_styled_output(&mut output);
    output
}

fn prompt_line_is_visual_continuation(
    previous: &Line,
    current: &Line,
    block_width: f32,
    current_text: &str,
) -> bool {
    if looks_like_bracketed_prompt_heading(current_text)
        || looks_like_numbered_prompt_item(current_text)
        || looks_like_speaker_turn(current_text)
    {
        return false;
    }
    let left_tolerance = previous
        .bbox
        .height()
        .max(current.bbox.height())
        .mul_add(0.35, 1.0);
    let aligned = (previous.bbox.x0 - current.bbox.x0).abs() <= left_tolerance;
    let previous_fills_line = block_width > 0.0 && previous.bbox.width() >= block_width * 0.82;
    let starts_lowercase = current_text
        .chars()
        .find(|character| character.is_alphabetic())
        .is_some_and(|character| character.is_lowercase());
    aligned && (previous_fills_line || starts_lowercase)
}

fn append_styled_visual_line(
    output: &mut Vec<StyledSourceCharacter>,
    line: Vec<StyledSourceCharacter>,
) {
    let dehyphenate = output
        .last()
        .is_some_and(|character| character.character == '-')
        && line
            .first()
            .is_some_and(|character| character.character.is_lowercase());
    if dehyphenate {
        output.pop();
    } else if !output
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.push(unstyled_character(' '));
    }
    output.extend(line);
}

fn styled_line_text(line: &[StyledSourceCharacter]) -> String {
    line.iter().map(|character| character.character).collect()
}

fn trim_styled_line(line: &mut Vec<StyledSourceCharacter>) {
    while line
        .first()
        .is_some_and(|character| character.character.is_whitespace())
    {
        line.remove(0);
    }
    trim_styled_line_end(line);
}

fn trim_styled_line_end(line: &mut Vec<StyledSourceCharacter>) {
    while line
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        line.pop();
    }
}

fn trim_styled_output(output: &mut Vec<StyledSourceCharacter>) {
    while output
        .first()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.remove(0);
    }
    while output
        .last()
        .is_some_and(|character| character.character.is_whitespace())
    {
        output.pop();
    }
}

fn unstyled_character(character: char) -> StyledSourceCharacter {
    StyledSourceCharacter {
        character,
        style_id: None,
    }
}

fn styled_characters_to_source(characters: Vec<StyledSourceCharacter>) -> NormalizedStyledSource {
    let mut text = String::new();
    let mut boundaries = Vec::new();
    let mut active_style = None;
    for character in characters {
        if character.style_id != active_style {
            if let Some(style_id) = active_style {
                boundaries.push(StyleBoundary {
                    offset: text.len(),
                    style_id,
                    start: false,
                });
            }
            if let Some(style_id) = character.style_id {
                boundaries.push(StyleBoundary {
                    offset: text.len(),
                    style_id,
                    start: true,
                });
            }
            active_style = character.style_id;
        }
        text.push(character.character);
    }
    if let Some(style_id) = active_style {
        boundaries.push(StyleBoundary {
            offset: text.len(),
            style_id,
            start: false,
        });
    }
    NormalizedStyledSource { text, boundaries }
}

fn dominant_text_style(block: &Block) -> Option<TextStyle> {
    let mut candidates = Vec::<(TextStyle, usize)>::new();
    for segment in block
        .content
        .lines
        .iter()
        .flat_map(|line| line.segments.iter())
    {
        let weight = segment
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .count();
        if weight == 0 {
            continue;
        }
        if let Some((_, total)) = candidates
            .iter_mut()
            .find(|(style, _)| !meaningful_style_difference(style, &segment.style))
        {
            *total += weight;
        } else {
            candidates.push((segment.style.clone(), weight));
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(_, weight)| *weight)
        .map(|(style, _)| style)
}

fn meaningful_style_difference(left: &TextStyle, right: &TextStyle) -> bool {
    left.font_class != right.font_class
        || left.baseline_position != right.baseline_position
        || left.bold != right.bold
        || left.italic != right.italic
        || left.underline != right.underline
        || left.strikeout != right.strikeout
        || left.color_argb != right.color_argb
        || match (left.font_size, right.font_size) {
            (Some(left), Some(right)) => (left - right).abs() > 0.5,
            (None, None) => false,
            _ => true,
        }
}

fn protect_text_with_identifier_policy(text: &str, protect_identifiers: bool) -> ProtectedText {
    protect_text_with_policies(text, protect_identifiers, false)
}

fn protect_text_with_policies(
    text: &str,
    protect_identifiers: bool,
    protect_line_breaks: bool,
) -> ProtectedText {
    protect_text_with_style_boundaries(text, protect_identifiers, protect_line_breaks, &[])
}

fn protect_text_with_style_boundaries(
    text: &str,
    protect_identifiers: bool,
    protect_line_breaks: bool,
    boundaries: &[StyleBoundary],
) -> ProtectedText {
    let mut protected = String::with_capacity(text.len());
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut boundary_index = 0;
    while cursor <= text.len() {
        while let Some(boundary) = boundaries.get(boundary_index)
            && boundary.offset <= cursor
        {
            let kind = if boundary.start {
                ProtectedTokenKind::StyleStart
            } else {
                ProtectedTokenKind::StyleEnd
            };
            let original = style_marker(boundary.style_id, boundary.start);
            let placeholder = placeholder(kind, tokens.len(), &original);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original,
                kind,
                styled_restoration: None,
            });
            boundary_index += 1;
        }
        if cursor == text.len() {
            break;
        }
        let rest = &text[cursor..];
        if protect_line_breaks && rest.starts_with('\n') {
            let original = "\n";
            let kind = ProtectedTokenKind::LineBreak;
            let placeholder = placeholder(kind, tokens.len(), original);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: original.to_string(),
                kind,
                styled_restoration: None,
            });
            cursor += original.len();
            continue;
        }
        if let Some((kind, length)) = protected_prefix(rest, protect_identifiers) {
            let original = &rest[..length];
            let placeholder = placeholder(kind, tokens.len(), original);
            let end = cursor + length;
            let mut restoration = String::with_capacity(length);
            let mut restoration_cursor = cursor;
            while let Some(boundary) = boundaries.get(boundary_index)
                && boundary.offset < end
            {
                restoration.push_str(&text[restoration_cursor..boundary.offset]);
                restoration.push_str(&style_marker(boundary.style_id, boundary.start));
                restoration_cursor = boundary.offset;
                boundary_index += 1;
            }
            restoration.push_str(&text[restoration_cursor..end]);
            let styled_restoration = (restoration != original).then_some(restoration);
            protected.push_str(&placeholder);
            tokens.push(ProtectedToken {
                placeholder,
                original: original.to_string(),
                kind,
                styled_restoration,
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

fn style_marker(style_id: usize, start: bool) -> String {
    format!(
        "\u{e000}{}{:04}\u{e001}",
        if start { 'S' } else { 'E' },
        style_id
    )
}

fn strip_style_placeholders(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find("[[GLYPHST_STYLE_") {
        output.push_str(&remaining[..start]);
        let token = &remaining[start..];
        let Some(end) = token.find("]]") else {
            output.push_str(token);
            return output;
        };
        remaining = &token[end + 2..];
    }
    output.push_str(remaining);
    output
}

fn parse_styled_translation(
    value: &str,
    definitions: &[StyleDefinition],
) -> (String, Vec<StyledRun>, bool) {
    let mut runs = Vec::<StyledRun>::new();
    let mut active_style = None::<usize>;
    let mut cursor = 0usize;
    let mut invalid = false;
    while let Some(relative_start) = value[cursor..].find('\u{e000}') {
        let start = cursor + relative_start;
        append_styled_run(
            &mut runs,
            &value[cursor..start],
            active_style.and_then(|id| {
                definitions
                    .iter()
                    .find(|definition| definition.id == id)
                    .map(|definition| definition.style.clone())
            }),
        );
        let marker = &value[start + '\u{e000}'.len_utf8()..];
        let Some(relative_end) = marker.find('\u{e001}') else {
            invalid = true;
            break;
        };
        let marker_text = &marker[..relative_end];
        let marker_end = start + '\u{e000}'.len_utf8() + relative_end + '\u{e001}'.len_utf8();
        let parsed = marker_text
            .get(1..)
            .and_then(|value| value.parse::<usize>().ok());
        match (marker_text.chars().next(), parsed) {
            (Some('S'), Some(style_id))
                if active_style.is_none()
                    && definitions
                        .iter()
                        .any(|definition| definition.id == style_id) =>
            {
                active_style = Some(style_id);
            }
            (Some('E'), Some(style_id)) if active_style == Some(style_id) => {
                active_style = None;
            }
            _ => invalid = true,
        }
        cursor = marker_end;
    }
    if !invalid {
        append_styled_run(
            &mut runs,
            &value[cursor..],
            active_style.and_then(|id| {
                definitions
                    .iter()
                    .find(|definition| definition.id == id)
                    .map(|definition| definition.style.clone())
            }),
        );
        invalid |= active_style.is_some();
    }
    if invalid {
        let plain = strip_style_markers(value).trim().to_string();
        let runs = (!plain.is_empty())
            .then(|| StyledRun {
                text: plain.clone(),
                style: None,
            })
            .into_iter()
            .collect();
        return (plain, runs, true);
    }
    trim_styled_runs(&mut runs);
    let plain = runs.iter().map(|run| run.text.as_str()).collect::<String>();
    (plain, runs, false)
}

fn append_styled_run(runs: &mut Vec<StyledRun>, text: &str, style: Option<TextStyle>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.style == style
    {
        last.text.push_str(text);
    } else {
        runs.push(StyledRun {
            text: text.to_string(),
            style,
        });
    }
}

fn trim_styled_runs(runs: &mut Vec<StyledRun>) {
    while runs.first().is_some_and(|run| run.text.trim().is_empty()) {
        runs.remove(0);
    }
    while runs.last().is_some_and(|run| run.text.trim().is_empty()) {
        runs.pop();
    }
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start().to_string();
    }
    if let Some(last) = runs.last_mut() {
        last.text = last.text.trim_end().to_string();
    }
    runs.retain(|run| !run.text.is_empty());
}

fn normalize_cjk_styled_spacing(runs: &mut Vec<StyledRun>) {
    let characters = runs
        .iter()
        .flat_map(|run| {
            run.text
                .chars()
                .map(|character| (character, run.style.clone()))
        })
        .collect::<Vec<_>>();
    if !characters
        .iter()
        .any(|(character, _)| is_cjk_character(*character))
    {
        return;
    }

    let mut normalized = Vec::<(char, Option<TextStyle>)>::with_capacity(characters.len());
    let mut index = 0usize;
    while index < characters.len() {
        let (character, style) = &characters[index];
        if !is_horizontal_whitespace(*character) {
            normalized.push((*character, style.clone()));
            index += 1;
            continue;
        }

        let whitespace_style = style.clone();
        let mut next_index = index + 1;
        while next_index < characters.len() && is_horizontal_whitespace(characters[next_index].0) {
            next_index += 1;
        }
        let previous = normalized.last().map(|(character, _)| *character);
        let next = characters.get(next_index).map(|(character, _)| *character);
        let remove = previous
            .zip(next)
            .is_some_and(|(left, right)| cjk_boundary_needs_no_space(left, right));
        if !remove
            && !normalized
                .last()
                .is_some_and(|(character, _)| is_horizontal_whitespace(*character))
        {
            normalized.push((' ', whitespace_style));
        }
        index = next_index;
    }

    runs.clear();
    for (character, style) in normalized {
        let mut encoded = [0; 4];
        append_styled_run(runs, character.encode_utf8(&mut encoded), style);
    }
    trim_styled_runs(runs);
}

fn is_horizontal_whitespace(character: char) -> bool {
    character.is_whitespace() && !matches!(character, '\n' | '\r')
}

fn cjk_boundary_needs_no_space(left: char, right: char) -> bool {
    (is_cjk_character(left) && (is_cjk_character(right) || right.is_ascii_alphanumeric()))
        || (left.is_ascii_alphanumeric() && is_cjk_character(right))
        || is_cjk_closing_punctuation(right)
        || is_cjk_opening_punctuation(left)
}

fn is_cjk_character(character: char) -> bool {
    matches!(
        character as u32,
        0x2e80..=0x2fff
            | 0x3040..=0x30ff
            | 0x3100..=0x312f
            | 0x31a0..=0x31bf
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0x20000..=0x2fa1f
    )
}

fn is_cjk_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '！'
            | '？'
            | '）'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '”'
            | '’'
    )
}

fn is_cjk_opening_punctuation(character: char) -> bool {
    matches!(
        character,
        '（' | '【' | '《' | '〈' | '「' | '『' | '“' | '‘'
    )
}

fn strip_style_markers(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find('\u{e000}') {
        output.push_str(&remaining[..start]);
        let marker = &remaining[start + '\u{e000}'.len_utf8()..];
        let Some(end) = marker.find('\u{e001}') else {
            output.push_str(marker);
            return output;
        };
        remaining = &marker[end + '\u{e001}'.len_utf8()..];
    }
    output.push_str(remaining);
    output
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
                append_continuation_text(
                    &mut unit.source_text,
                    &normalize_pdf_hard_wraps(&block.content.text, block.role),
                );
                unit.member_block_ids.push(block.id.clone());
                unit.protected = protect_text_for_role(&unit.source_text, unit.role);
                unit.style_definitions.clear();
                continue;
            }
            let id = continuation_id
                .clone()
                .unwrap_or_else(|| format!("unit-{}", block.id));
            let (source_text, protected, style_definitions) = protect_block_text(block, block.role);
            let unit_index = units.len();
            units.push(TranslationUnit {
                id,
                member_block_ids: vec![block.id.clone()],
                role: block.role,
                protected,
                style_definitions,
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
        let batchable = matches!(
            unit.role,
            BlockRole::Body | BlockRole::Abstract | BlockRole::Prompt | BlockRole::Dialogue
        ) && characters <= MAX_BATCHABLE_UNIT_CHARACTERS
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

fn semantic_unit_hash(unit: &TranslationUnit) -> u64 {
    let mut hasher = DefaultHasher::new();
    unit.id.hash(&mut hasher);
    unit.member_block_ids.hash(&mut hasher);
    role_name(unit.role).hash(&mut hasher);
    unit.source_text.hash(&mut hasher);
    unit.protected.text.hash(&mut hasher);
    if let Ok(tokens) = serde_json::to_string(&unit.protected.tokens) {
        tokens.hash(&mut hasher);
    }
    if let Ok(styles) = serde_json::to_string(&unit.style_definitions) {
        styles.hash(&mut hasher);
    }
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

struct ValidatedTranslation {
    text: String,
    runs: Vec<StyledRun>,
    style_fallback: bool,
}

fn validate_translation(
    unit: &TranslationUnit,
    translated: &str,
    reject_unchanged: bool,
) -> Result<ValidatedTranslation, String> {
    let (restored, marker_fallback) = unit.protected.restore_translation(translated)?;
    let (_plain, mut runs, parse_fallback) =
        parse_styled_translation(&restored, &unit.style_definitions);
    normalize_cjk_styled_spacing(&mut runs);
    let plain = runs.iter().map(|run| run.text.as_str()).collect::<String>();
    if reject_unchanged
        && should_reject_unchanged(&unit.source_text)
        && normalize_for_comparison(&plain) == normalize_for_comparison(&unit.source_text)
    {
        return Err("translation is unchanged from the source".to_string());
    }
    let lowercase = plain.trim().to_ascii_lowercase();
    if lowercase.starts_with("i cannot translate")
        || lowercase.starts_with("i can't translate")
        || lowercase.starts_with("unable to translate")
    {
        return Err("translation provider refused the unit".to_string());
    }
    Ok(ValidatedTranslation {
        text: plain.trim().to_string(),
        runs,
        style_fallback: marker_fallback || parse_fallback,
    })
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
        .filter(|token| {
            !matches!(
                token.kind,
                ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
            )
        })
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
        let length = numeric_literal_length(text);
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
        let candidate = text[..length].trim_end_matches(['.', ',', ';', ':']);
        if candidate.len() >= 2
            && (candidate.contains('_')
                || candidate
                    .chars()
                    .filter(|character| character.is_ascii_uppercase())
                    .count()
                    >= 2)
        {
            return Some((
                ProtectedTokenKind::Identifier,
                identifier_prefix_length(candidate),
            ));
        }
    }
    None
}

fn numeric_literal_length(text: &str) -> usize {
    let mut length = 0usize;
    let mut digit_count = 0usize;
    let mut only_digits = true;
    for (index, character) in text.char_indices() {
        if character.is_ascii_digit() {
            length = index + character.len_utf8();
            digit_count += 1;
            continue;
        }
        if matches!(
            character,
            '.' | ',' | '%' | '°' | '/' | ':' | '-' | '+' | '×'
        ) {
            let next = text[index + character.len_utf8()..].chars().next();
            if matches!(character, '-' | '+') && next.is_some_and(|next| next.is_ascii_alphabetic())
            {
                break;
            }
            length = index + character.len_utf8();
            only_digits = false;
            continue;
        }
        break;
    }

    let suffix = &text[length..];
    let suffix_length = suffix
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_alphabetic())
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    if only_digits
        && digit_count >= 2
        && suffix_length > 0
        && suffix
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
    {
        length += suffix_length;
    }
    while length > 0
        && text[..length]
            .chars()
            .last()
            .is_some_and(|character| matches!(character, '.' | ',' | ';' | ':'))
    {
        length -= text[..length]
            .chars()
            .last()
            .map(char::len_utf8)
            .unwrap_or_default();
    }
    length
}

fn identifier_prefix_length(candidate: &str) -> usize {
    let mut prefix_end = 0usize;
    let mut uppercase = 0usize;
    for (index, character) in candidate.char_indices() {
        if character.is_ascii_uppercase()
            || character.is_ascii_digit()
            || matches!(character, '_' | '-')
        {
            prefix_end = index + character.len_utf8();
            uppercase += usize::from(character.is_ascii_uppercase());
        } else {
            break;
        }
    }
    let suffix = &candidate[prefix_end..];
    if uppercase >= 2
        && suffix.len() >= 4
        && suffix
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        prefix_end
    } else {
        candidate.len()
    }
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
        ProtectedTokenKind::LineBreak => "LINE_BREAK",
        ProtectedTokenKind::StyleStart => "STYLE_START",
        ProtectedTokenKind::StyleEnd => "STYLE_END",
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

fn normalize_pdf_hard_wraps(text: &str, role: BlockRole) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if role == BlockRole::Dialogue {
        return normalize_dialogue_lines(&text);
    }
    if role == BlockRole::Prompt {
        return normalize_prompt_lines(&text);
    }
    if preserves_semantic_line_breaks(role, &text) {
        return text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
    }

    let mut paragraphs = Vec::new();
    let mut lines = Vec::new();
    let flush = |paragraphs: &mut Vec<String>, lines: &mut Vec<&str>| {
        if lines.is_empty() {
            return;
        }
        paragraphs.push(join_visual_lines(lines));
        lines.clear();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut paragraphs, &mut lines);
        } else {
            lines.push(line);
        }
    }
    flush(&mut paragraphs, &mut lines);
    paragraphs
        .into_iter()
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string()
}

fn join_visual_lines(lines: &[&str]) -> String {
    let mut joined = String::new();
    for line in lines {
        if joined.is_empty() {
            joined.push_str(line);
            continue;
        }
        let dehyphenate = joined.ends_with('-')
            && line
                .chars()
                .next()
                .is_some_and(|character| character.is_lowercase());
        if dehyphenate {
            joined.pop();
        } else if !joined.ends_with(char::is_whitespace) {
            joined.push(' ');
        }
        joined.push_str(line);
    }
    joined
}

fn normalize_dialogue_lines(text: &str) -> String {
    let mut turns = Vec::<String>::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if turns.is_empty() || looks_like_speaker_turn(line) {
            turns.push(line.to_string());
        } else {
            append_continuation_text(
                turns.last_mut().expect("dialogue has a previous turn"),
                line,
            );
        }
    }
    turns.join("\n")
}

fn normalize_prompt_lines(text: &str) -> String {
    let mut parts = Vec::<String>::new();
    let mut previous = String::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if parts.is_empty()
            || looks_like_prompt_boundary(line)
            || looks_like_bracketed_prompt_heading(&previous)
            || prompt_sentence_boundary(&previous, line)
        {
            parts.push(line.to_string());
        } else {
            append_continuation_text(parts.last_mut().expect("prompt has a previous part"), line);
        }
        previous = line.to_string();
    }
    parts.join("\n")
}

fn looks_like_speaker_turn(line: &str) -> bool {
    speaker_label(line).is_some()
}

fn speaker_label(line: &str) -> Option<String> {
    let (speaker, text) = line.split_once([':', '：'])?;
    let speaker = speaker.trim();
    let words = speaker.split_whitespace().collect::<Vec<_>>();
    (!text.trim().is_empty()
        && speaker.chars().count() <= 24
        && (1..=3).contains(&words.len())
        && words.iter().all(|word| {
            word.chars()
                .all(|character| character.is_alphabetic() || matches!(character, '-' | '\'' | '’'))
        }))
    .then(|| speaker.to_lowercase())
}

fn looks_like_prompt_boundary(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    if looks_like_bracketed_prompt_heading(line) {
        return true;
    }
    if looks_like_numbered_prompt_item(line) {
        return true;
    }
    line.split_once([':', '：']).is_some_and(|(label, _)| {
        let label = label.trim();
        !label.is_empty()
            && label.chars().count() <= 48
            && label.split_whitespace().count() <= 7
            && !label.contains(['.', '?', '!'])
            && label_starts_like_prompt_field(label)
    })
}

fn looks_like_numbered_prompt_item(line: &str) -> bool {
    line.trim()
        .split_once([')', '）', '.', '．'])
        .is_some_and(|(prefix, _)| {
            !prefix.is_empty()
                && prefix.len() <= 3
                && prefix.chars().all(|character| character.is_ascii_digit())
        })
}

fn label_starts_like_prompt_field(label: &str) -> bool {
    let Some(character) = label.chars().find(|character| character.is_alphabetic()) else {
        return false;
    };
    if character.is_uppercase() {
        return true;
    }
    !character.is_lowercase() && is_cjk_character(character)
}

fn looks_like_bracketed_prompt_heading(line: &str) -> bool {
    let line = line.trim();
    line.starts_with('[') && line.ends_with(']') && line.find(']').is_some_and(|end| end <= 80)
}

fn prompt_sentence_boundary(previous: &str, line: &str) -> bool {
    let previous = previous.trim_end();
    let line = line.trim_start();
    previous.ends_with(['.', '?', '!', '。', '？', '！'])
        && (line.ends_with('?')
            || line.ends_with('？')
            || line.starts_with("Should ")
            || line.starts_with("Is ")
            || line.starts_with("Are ")
            || line.starts_with("Can ")
            || line.starts_with("Could ")
            || line.starts_with("Would ")
            || line.starts_with("Will ")
            || line.starts_with("What ")
            || line.starts_with("How ")
            || line.starts_with("Which "))
}

fn normalize_translated_whitespace(translated: &str, source: &str, role: BlockRole) -> String {
    let translated = translated.replace("\r\n", "\n").replace('\r', "\n");
    if preserves_semantic_line_breaks(role, source) {
        return translated
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
    }
    let source_paragraphs = source
        .split("\n\n")
        .filter(|paragraph| !paragraph.trim().is_empty())
        .count()
        .max(1);
    let translated_paragraphs = translated
        .split("\n\n")
        .map(|paragraph| paragraph.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|paragraph| !paragraph.is_empty())
        .collect::<Vec<_>>();
    if source_paragraphs == 1 || translated_paragraphs.len() > source_paragraphs + 1 {
        translated_paragraphs.join(" ")
    } else {
        translated_paragraphs.join("\n\n")
    }
}

fn preserves_semantic_line_breaks(role: BlockRole, text: &str) -> bool {
    preserves_role_line_breaks(role) || looks_like_dialogue(text)
}

fn preserves_role_line_breaks(role: BlockRole) -> bool {
    matches!(
        role,
        BlockRole::Prompt | BlockRole::Dialogue | BlockRole::TableOfContents | BlockRole::Code
    )
}

fn looks_like_dialogue(text: &str) -> bool {
    let labels = text.lines().filter_map(speaker_label).collect::<Vec<_>>();
    labels.len() >= 3 && labels.iter().collect::<HashSet<_>>().len() >= 2
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
        BlockRole::Prompt => "prompt",
        BlockRole::Dialogue => "dialogue",
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
        Segment, SegmentKind, SourceProvenance, TranslationPolicy,
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
    fn translation_units_keep_exact_protected_token_metadata() {
        let unit = build_translation_units(&test_document(
            "Use AI for 25 cases; see [12], $x < y$, and https://example.test/?a=1&b=2.",
        ))
        .remove(0);

        assert_eq!(
            unit.protected
                .tokens
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [
                ProtectedTokenKind::Identifier,
                ProtectedTokenKind::NumericLiteral,
                ProtectedTokenKind::Citation,
                ProtectedTokenKind::Formula,
                ProtectedTokenKind::Url,
            ]
        );
        for token in &unit.protected.tokens {
            assert!(unit.protected.text.contains(&token.placeholder));
            assert!(!token.original.is_empty());
        }
    }

    #[test]
    fn all_caps_identifier_suffixes_remain_translatable() {
        let protected = protect_text("WORDTOURaligns with people while 20news remains a dataset.");
        let originals = protected
            .tokens
            .iter()
            .map(|token| token.original.as_str())
            .collect::<Vec<_>>();

        assert!(originals.contains(&"WORDTOUR"));
        assert!(originals.contains(&"20news"));
        assert!(!originals.contains(&"WORDTOURaligns"));
        assert!(protected.text.contains("aligns"));

        let sentence = protect_text("Use WORDTOUR. Then compare WMD, BoW; and AI:");
        assert!(sentence.tokens.iter().all(|token| {
            token
                .original
                .chars()
                .last()
                .is_none_or(|character| !matches!(character, '.' | ',' | ';' | ':'))
        }));
    }

    #[test]
    fn numeric_tokens_distinguish_identifiers_from_joined_prose() {
        let protected =
            protect_text("20news, 300-dimensional, Figure2shows, and 3This are distinct cases.");
        let originals = protected
            .tokens
            .iter()
            .filter(|token| token.kind == ProtectedTokenKind::NumericLiteral)
            .map(|token| token.original.as_str())
            .collect::<Vec<_>>();

        assert!(originals.contains(&"20news"));
        assert!(originals.contains(&"300"));
        assert!(originals.contains(&"2"));
        assert!(originals.contains(&"3"));
        assert!(!originals.contains(&"300-dimensional"));
        assert!(!originals.contains(&"2shows"));
        assert!(!originals.contains(&"3This"));

        let punctuation = protect_text("See Table 2. Compare 3.14, then Section 4.3.");
        let numeric = punctuation
            .tokens
            .iter()
            .filter(|token| token.kind == ProtectedTokenKind::NumericLiteral)
            .map(|token| token.original.as_str())
            .collect::<Vec<_>>();
        assert_eq!(numeric, ["2", "3.14", "4.3"]);
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
    fn pdf_visual_wraps_are_removed_before_translation() {
        let document = test_document(
            "Generative agents wake up, cook breakfast,\nand then plan their day. They inter-\nact with neighbors.",
        );

        let units = build_translation_units(&document);

        assert_eq!(units.len(), 1);
        assert_eq!(
            units[0].source_text,
            "Generative agents wake up, cook breakfast, and then plan their day. They interact with neighbors."
        );
        assert!(!units[0].protected.text.contains('\n'));
    }

    #[test]
    fn dialogue_units_preserve_speaker_turns_but_join_visual_wraps() {
        let document = test_document_with_blocks(vec![(
            BlockRole::Dialogue,
            "Interviewer: How does this\nsystem work?\nKlaus Mueller: It remembers prior events."
                .to_string(),
        )]);

        let unit = build_translation_units(&document).remove(0);

        assert_eq!(
            unit.source_text,
            "Interviewer: How does this system work?\nKlaus Mueller: It remembers prior events."
        );
        assert!(!unit.protected.text.contains('\n'));
        assert_eq!(
            unit.protected
                .tokens
                .iter()
                .filter(|token| token.kind == ProtectedTokenKind::LineBreak)
                .count(),
            1
        );
        let translated = unit
            .protected
            .text
            .replace(
                "Interviewer: How does this system work?",
                "采访者：这个系统如何工作？",
            )
            .replace(
                "Klaus Mueller: It remembers prior events.",
                "克劳斯·穆勒：它会记住先前的事件。",
            );
        let validated = validate_translation(&unit, &translated, false).unwrap();
        assert_eq!(
            validated.text,
            "采访者：这个系统如何工作？\n克劳斯·穆勒：它会记住先前的事件。"
        );
    }

    #[test]
    fn prompt_units_preserve_structural_lines_and_join_wrapped_prose() {
        let document = test_document_with_blocks(vec![(
            BlockRole::Prompt,
            "[Agent’s Summary Description]\nIt is February 13.\nObservation: John saw Eddy taking a short walk\naround his workplace.\nSummary of relevant context from John’s memory:\nEddy likes music.\nShould John react?"
                .to_string(),
        )]);

        let unit = build_translation_units(&document).remove(0);

        assert_eq!(
            unit.source_text,
            "[Agent’s Summary Description]\nIt is February 13.\nObservation: John saw Eddy taking a short walk around his workplace.\nSummary of relevant context from John’s memory: Eddy likes music.\nShould John react?"
        );
        assert_eq!(
            unit.protected
                .tokens
                .iter()
                .filter(|token| token.kind == ProtectedTokenKind::LineBreak)
                .count(),
            4
        );
    }

    #[test]
    fn prompt_geometry_does_not_promote_a_lowercase_wrapped_colon_to_a_field() {
        let styled_line = |text: &str| {
            text.chars()
                .map(unstyled_character)
                .collect::<Vec<StyledSourceCharacter>>()
        };
        let source_line = |text: &str, y: f32, x1: f32| Line {
            bbox: BoundingBox::new(10.0, y, x1, y + 10.0),
            text: text.to_string(),
            segments: Vec::new(),
            baseline_y: Some(y + 8.0),
            line_height: Some(10.0),
        };
        let lines = vec![
            styled_line("[Agent’s Plan]"),
            styled_line("Today is Wednesday. Here is Eddy’s"),
            styled_line("plan today in broad strokes: 1)"),
        ];
        let source_lines = vec![
            source_line("[Agent’s Plan]", 10.0, 80.0),
            source_line("Today is Wednesday. Here is Eddy’s", 20.0, 190.0),
            source_line("plan today in broad strokes: 1)", 30.0, 165.0),
        ];

        let normalized =
            styled_characters_to_source(normalize_styled_prompt_lines(lines, &source_lines));

        assert_eq!(
            normalized.text,
            "[Agent’s Plan]\nToday is Wednesday. Here is Eddy’s plan today in broad strokes: 1)"
        );
    }

    #[test]
    fn cjk_spacing_is_normalized_across_style_boundaries() {
        let mut runs = vec![
            StyledRun {
                text: "我们使用了  ".to_string(),
                style: None,
            },
            StyledRun {
                text: "300".to_string(),
                style: Some(TextStyle {
                    font_class: Some(crate::FontFamilyClass::Math),
                    ..TextStyle::default()
                }),
            },
            StyledRun {
                text: " 维的GloVe 用于WMD。".to_string(),
                style: None,
            },
        ];

        normalize_cjk_styled_spacing(&mut runs);

        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "我们使用了300维的GloVe用于WMD。"
        );
        assert_eq!(runs[1].text, "300");
        assert!(runs[1].style.is_some());
    }

    #[test]
    fn invented_ai_paragraph_breaks_are_flattened_for_single_paragraph_sources() {
        let normalized = normalize_translated_whitespace(
            "生成式智能体醒来。\n\n它们做早餐。\n\n然后规划一天。",
            "Generative agents wake up. They cook breakfast. Then they plan their day.",
            BlockRole::Body,
        );

        assert_eq!(normalized, "生成式智能体醒来。 它们做早餐。 然后规划一天。");
    }

    #[test]
    fn styled_translation_markers_restore_inline_typography() {
        let definitions = vec![StyleDefinition {
            id: 0,
            style: TextStyle {
                bold: true,
                italic: true,
                color_argb: Some(0xff44_5566),
                ..TextStyle::default()
            },
        }];

        let (plain, runs, fallback) = parse_styled_translation(
            "普通\u{e000}S0\u{e001}重点\u{e000}E0\u{e001}文本",
            &definitions,
        );

        assert_eq!(plain, "普通重点文本");
        assert!(!fallback);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[1].text, "重点");
        assert!(runs[1].style.as_ref().is_some_and(|style| {
            style.bold && style.italic && style.color_argb == Some(0xff44_5566)
        }));
    }

    #[test]
    fn protected_citations_restore_internal_color_boundaries() {
        let mut document = test_document("Compare (Joachims, 1998) carefully in this discussion.");
        let bbox = document.pages[0].blocks[0].bbox;
        let plain_style = TextStyle {
            color_argb: Some(0xff00_0000),
            ..TextStyle::default()
        };
        let purple_style = TextStyle {
            color_argb: Some(0xff00_00ff),
            ..TextStyle::default()
        };
        let segment = |text: &str, style: TextStyle| Segment {
            kind: SegmentKind::Text,
            text: text.to_string(),
            bbox,
            style,
        };
        let segments = vec![
            segment("Compare (", plain_style.clone()),
            segment("Joachims, 1998", purple_style.clone()),
            segment(") carefully in this discussion.", plain_style),
        ];
        document.pages[0].blocks[0].content.lines[0].segments = segments.clone();
        document.pages[0].blocks[0].content.segments = segments;

        let unit = build_translation_units(&document).remove(0);
        let citation = unit
            .protected
            .tokens
            .iter()
            .find(|token| token.kind == ProtectedTokenKind::Citation)
            .unwrap();
        assert!(
            citation
                .styled_restoration
                .as_deref()
                .is_some_and(|restoration| restoration.contains('\u{e000}'))
        );

        let translated = unit
            .protected
            .text
            .replace("Compare ", "比较")
            .replace(" carefully in this discussion.", "。");
        let validated = validate_translation(&unit, &translated, false).unwrap();
        assert!(validated.runs.iter().any(|run| {
            run.text == "Joachims, 1998"
                && run
                    .style
                    .as_ref()
                    .is_some_and(|style| style.color_argb == purple_style.color_argb)
        }));
    }

    #[test]
    fn malformed_style_markers_fall_back_without_losing_text() {
        let definitions = vec![StyleDefinition {
            id: 0,
            style: TextStyle {
                bold: true,
                ..TextStyle::default()
            },
        }];

        let (plain, runs, fallback) =
            parse_styled_translation("普通\u{e000}S0\u{e001}重点", &definitions);

        assert_eq!(plain, "普通重点");
        assert!(fallback);
        assert_eq!(
            runs,
            vec![StyledRun {
                text: plain,
                style: None
            }]
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
        assert_eq!(snapshot.queue.retry_requests, MAX_UNIT_ATTEMPTS - 1);
        assert_eq!(
            snapshot.queue.validation_retry_requests,
            MAX_UNIT_ATTEMPTS - 1
        );
        assert_eq!(snapshot.queue.provider_retry_requests, 0);
        assert_eq!(snapshot.queue.completed_requests, MAX_UNIT_ATTEMPTS);
        assert!(snapshot.queue.max_request_latency_ms <= snapshot.queue.total_request_latency_ms);
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

        let last_snapshot = Arc::new(Mutex::new(None));
        let captured = Arc::clone(&last_snapshot);
        let error = translate_document_resuming(
            test_document("This paragraph must be translated."),
            &PanicBackend,
            &CancellationToken::new(),
            Vec::new(),
            move |snapshot| {
                *captured.lock().unwrap() = Some(snapshot.clone());
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.retryable);
        let snapshot = last_snapshot.lock().unwrap().clone().unwrap();
        assert_eq!(
            snapshot.queue.provider_retry_requests,
            MAX_UNIT_ATTEMPTS - 1
        );
        assert_eq!(snapshot.queue.validation_retry_requests, 0);
        assert!(snapshot.queue.elapsed_ms >= 500);
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
            styled_runs: Vec::new(),
            style_fallback: false,
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
    fn resumed_styled_units_do_not_require_internal_style_markers() {
        let unit = TranslationUnit {
            id: "styled".to_string(),
            member_block_ids: vec!["block".to_string()],
            role: BlockRole::Body,
            source_text: "Styled source text".to_string(),
            protected: ProtectedText {
                text:
                    "[[GLYPHST_STYLE_START_0000_AAAAAAAA]]source[[GLYPHST_STYLE_END_0001_BBBBBBBB]]"
                        .to_string(),
                tokens: vec![
                    ProtectedToken {
                        placeholder: "[[GLYPHST_STYLE_START_0000_AAAAAAAA]]".to_string(),
                        original: style_marker(0, true),
                        kind: ProtectedTokenKind::StyleStart,
                        styled_restoration: None,
                    },
                    ProtectedToken {
                        placeholder: "[[GLYPHST_STYLE_END_0001_BBBBBBBB]]".to_string(),
                        original: style_marker(0, false),
                        kind: ProtectedTokenKind::StyleEnd,
                        styled_restoration: None,
                    },
                ],
            },
            style_definitions: Vec::new(),
            context_before: None,
            context_after: None,
        };

        assert!(validate_resumed_translation(
            &unit,
            "已翻译的样式文本",
            true
        ));
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
                                baseline_y: None,
                                line_height: Some(bbox.height()),
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
                protected_regions: Vec::new(),
                metadata: BTreeMap::new(),
            }],
        )
    }
}
