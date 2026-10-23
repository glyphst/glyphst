use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::policy::enforce_unmapped_glyph_policy;
use crate::scope::{ScopeView, TranslationScope};
use crate::{
    Block, BlockRole, BoundingBox, CancellationToken, DocumentV1, Line, SegmentKind, TextFlow,
    TextStyle,
};

const MAX_BATCH_UNITS: usize = 6;
const MAX_BATCH_CHARACTERS: usize = 6_000;
const MAX_BATCH_PLACEHOLDERS: usize = 36;
const MAX_BATCHABLE_UNIT_CHARACTERS: usize = 1_800;
const MAX_BATCHABLE_UNIT_PLACEHOLDERS: usize = 8;
const MAX_ACTIVE_REQUESTS: usize = 6;
const MAX_ACTIVE_COMPLEX_REQUESTS: usize = 2;
const MAX_UNIT_ATTEMPTS: usize = 3;
/// How often a scheduler with work out of scope looks at the scope again.
const SCOPE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationUnit {
    pub id: String,
    #[serde(default)]
    pub page_index: usize,
    /// Every page the unit's blocks are on (a paragraph may run on).
    #[serde(default)]
    pub member_pages: Vec<usize>,
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
        if restored.contains("GLYPHST_") {
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
        let mut normalized = translated.to_string();
        self.normalize_leading_structural_marker(&mut normalized);
        for pair in self.tokens.windows(2) {
            if pair.iter().any(|token| {
                matches!(
                    token.kind,
                    ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
                )
            }) {
                continue;
            }
            let adjacent = format!("{}{}", pair[0].placeholder, pair[1].placeholder);
            if self.text.contains(&adjacent) && !normalized.contains(&adjacent) {
                let left_end = normalized
                    .find(&pair[0].placeholder)
                    .map(|start| start + pair[0].placeholder.len());
                let right_start = left_end.and_then(|left_end| {
                    normalized[left_end..]
                        .find(&pair[1].placeholder)
                        .map(|offset| left_end + offset)
                });
                let whitespace_only =
                    left_end
                        .zip(right_start)
                        .is_some_and(|(left_end, right_start)| {
                            normalized[left_end..right_start]
                                .chars()
                                .all(char::is_whitespace)
                        });
                if !whitespace_only {
                    return Err(format!(
                        "adjacent placeholders {} and {} were separated",
                        pair[0].placeholder, pair[1].placeholder
                    ));
                }
                let (left_end, right_start) = left_end.zip(right_start).expect("checked above");
                normalized.replace_range(left_end..right_start, "");
            }
        }
        let mut restored = normalized;
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
        if restored.contains("GLYPHST_") {
            return Err("translation contains an unresolved placeholder".to_string());
        }
        Ok((restored, style_fallback))
    }

    fn normalize_leading_structural_marker(&self, translated: &mut String) {
        let source_without_styles = strip_style_placeholders(&self.text);
        let Some((marker_index, _)) = self.tokens.iter().enumerate().find(|(_, token)| {
            token.kind == ProtectedTokenKind::StructuralMarker
                && source_without_styles
                    .trim_start()
                    .starts_with(&token.placeholder)
        }) else {
            return;
        };

        let mut group_start = marker_index;
        while group_start > 0 {
            let previous = &self.tokens[group_start - 1];
            let current = &self.tokens[group_start];
            let adjacent = format!("{}{}", previous.placeholder, current.placeholder);
            if !matches!(
                previous.kind,
                ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
            ) || !self.text.contains(&adjacent)
            {
                break;
            }
            group_start -= 1;
        }
        let mut group_end = marker_index + 1;
        while group_end < self.tokens.len() {
            let previous = &self.tokens[group_end - 1];
            let next = &self.tokens[group_end];
            let adjacent = format!("{}{}", previous.placeholder, next.placeholder);
            if !matches!(
                next.kind,
                ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
            ) || !self.text.contains(&adjacent)
            {
                break;
            }
            group_end += 1;
        }

        let group = self.tokens[group_start..group_end]
            .iter()
            .filter(|token| translated.matches(&token.placeholder).count() == 1)
            .map(|token| token.placeholder.as_str())
            .collect::<Vec<_>>();
        for placeholder in &group {
            if let Some(start) = translated.find(placeholder) {
                translated.replace_range(start..start + placeholder.len(), "");
            }
        }
        let insert_at = translated
            .find(|character: char| !character.is_whitespace())
            .unwrap_or(translated.len());
        translated.insert_str(insert_at, &group.concat());
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
    InlineObject,
    StructuralMarker,
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

    fn validate_translated_output(
        &self,
        unit: &TranslationUnit,
        _protected_translation: &str,
        translated: &str,
    ) -> Result<(), String> {
        self.validate_translated_text(unit, translated)
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
    pub peak_active_earliest_page_requests: usize,
    #[serde(default)]
    pub peak_active_later_page_requests: usize,
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
    #[serde(default)]
    pub phase: TranslationFailurePhase,
    pub reason: String,
    pub attempts: usize,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_block_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<BlockRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_output_excerpt: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationFailurePhase {
    Ocr,
    #[default]
    Translation,
    Layout,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TranslationRunProgress {
    pub translated: usize,
    pub cached: usize,
    pub retrying: usize,
    pub failed: usize,
    pub total: usize,
    /// Units on pages the scope covers; `total` for a whole document.
    #[serde(default)]
    pub in_scope: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationRunSnapshot {
    pub resolved_units: Vec<TranslatedUnit>,
    pub failures: Vec<TranslationFailureDiagnostic>,
    pub progress: TranslationRunProgress,
    pub queue: TranslationQueueStats,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationRunOutcome {
    pub document: TranslatedDocument,
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
    on_update: impl FnMut(&TranslationRunSnapshot) -> Result<(), String>,
) -> Result<TranslatedDocument, TranslationDocumentError> {
    let outcome = translate_document_partial_resuming(
        document,
        backend,
        cancellation,
        resumed_units,
        on_update,
    )?;
    if outcome.failures.is_empty() {
        return Ok(outcome.document);
    }

    let retryable = outcome.failures.iter().any(|failure| failure.retryable);
    Err(TranslationDocumentError {
        message: format!(
            "{} translation unit(s) remain unresolved after recovery; export is blocked",
            outcome.failures.len()
        ),
        unresolved_unit_ids: outcome
            .failures
            .iter()
            .map(|failure| failure.id.clone())
            .collect(),
        retryable,
    })
}

pub fn translate_document_partial_resuming(
    document: DocumentV1,
    backend: &dyn TranslationBackend,
    cancellation: &CancellationToken,
    resumed_units: Vec<TranslatedUnit>,
    on_update: impl FnMut(&TranslationRunSnapshot) -> Result<(), String>,
) -> Result<TranslationRunOutcome, TranslationDocumentError> {
    let scope = TranslationScope::whole();
    translate_document_scoped(
        document,
        backend,
        cancellation,
        resumed_units,
        ScopedRun::whole(&scope),
        on_update,
    )
}

/// What a scoped run may translate and when it stops.
pub struct ScopedRun<'a> {
    /// The pages in scope and their order.
    pub scope: &'a TranslationScope,
    /// Whether a page's text is final (its OCR is done); work touching a
    /// page that is not waits.
    pub ready: &'a (dyn Fn(usize) -> bool + Sync),
    /// Wait for the scope to grow when everything left is out of it,
    /// instead of returning with that work undone.
    pub park_when_idle: bool,
    /// Asked between dispatches: once it answers yes, nothing more is sent,
    /// the requests in flight finish and the run returns.
    pub stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl<'a> ScopedRun<'a> {
    /// Every page, all ready, returning when done.
    pub fn whole(scope: &'a TranslationScope) -> Self {
        Self {
            scope,
            ready: &|_| true,
            park_when_idle: false,
            stop: None,
        }
    }
}

/// Translates the units the scope covers, most urgent first; units out of
/// scope are left out of the outcome unless the scope grows to them.
pub fn translate_document_scoped(
    mut document: DocumentV1,
    backend: &dyn TranslationBackend,
    cancellation: &CancellationToken,
    resumed_units: Vec<TranslatedUnit>,
    run: ScopedRun<'_>,
    mut on_update: impl FnMut(&TranslationRunSnapshot) -> Result<(), String>,
) -> Result<TranslationRunOutcome, TranslationDocumentError> {
    let started_at = Instant::now();
    enforce_unmapped_glyph_policy(&mut document);
    let units = build_translation_units(&document);
    let total = units.len();
    if total == 0 {
        let snapshot = TranslationRunSnapshot {
            resolved_units: Vec::new(),
            failures: Vec::new(),
            progress: TranslationRunProgress::default(),
            queue: TranslationQueueStats::default(),
        };
        on_update(&snapshot).map_err(|message| TranslationDocumentError {
            message,
            unresolved_unit_ids: Vec::new(),
            retryable: false,
        })?;
        return Ok(TranslationRunOutcome {
            document: TranslatedDocument {
                document,
                units: Vec::new(),
            },
            failures: Vec::new(),
            progress: TranslationRunProgress::default(),
            queue: TranslationQueueStats::default(),
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
            && validate_resumed_inline_objects(unit, &resumed)
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
            slot_page: 0,
        })
        .collect::<VecDeque<_>>();
    let mut queue = TranslationQueueStats {
        planned_requests: pending.len(),
        ..TranslationQueueStats::default()
    };
    let mut failures = HashMap::<String, TranslationFailureDiagnostic>::new();
    let mut retrying_ids = HashSet::<String>::new();
    let mut cached_ids = resolved.keys().cloned().collect::<HashSet<_>>();

    let mut view = run.scope.view();
    let initial = translation_snapshot(
        &units,
        &resolved,
        &failures,
        &retrying_ids,
        &cached_ids,
        queue_snapshot(&queue, started_at),
        &view,
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
        let mut active_by_page = HashMap::<usize, usize>::new();
        let mut cancelled = false;
        let mut stopping = false;

        while !pending.is_empty() || active > 0 {
            if cancellation.is_cancelled() {
                cancelled = true;
                pending.clear();
            }
            if !stopping && run.stop.is_some_and(|stop| stop()) {
                stopping = true;
            }
            view = run.scope.view();
            while !cancelled && !stopping && active < MAX_ACTIVE_REQUESTS {
                let Some(index) = next_dispatchable_request(
                    &pending,
                    &active_by_page,
                    active,
                    active_complex,
                    &view,
                    run.ready,
                ) else {
                    break;
                };
                let mut request = pending.remove(index).expect("pending request exists");
                let is_complex = request.plan.complex;
                let page_index = request.plan.slot_page(&view);
                request.slot_page = page_index;
                active += 1;
                active_complex += usize::from(is_complex);
                *active_by_page.entry(page_index).or_default() += 1;
                queue.dispatched_requests += 1;
                queue.peak_active_requests = queue.peak_active_requests.max(active);
                queue.peak_active_complex_requests =
                    queue.peak_active_complex_requests.max(active_complex);
                update_page_allocation_peaks(&mut queue, &active_by_page);
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
                if stopping {
                    break;
                }
                if let Some(delay) = next_ready_delay(&pending, active_complex, &view, run.ready) {
                    let _ = sleep_cancellable(delay, cancellation);
                    continue;
                }
                if run.park_when_idle && !pending.is_empty() {
                    // Everything left is out of scope or waits for its text:
                    // sleep until the reader moves or a page becomes ready.
                    run.scope.wait_changed(view.generation(), SCOPE_POLL * 2);
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
            let parked = pending
                .iter()
                .any(|request| !request_eligible(request, &view, run.ready));
            let delay = next_ready_delay(&pending, active_complex, &view, run.ready)
                .filter(|_| active < MAX_ACTIVE_REQUESTS);
            let delay = match (delay, parked || run.stop.is_some()) {
                (Some(delay), true) => Some(delay.min(SCOPE_POLL)),
                (None, true) => Some(SCOPE_POLL),
                (delay, false) => delay,
            };
            let completed = if let Some(delay) = delay {
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
            let completed_page = completed.request.slot_page;
            if let Some(count) = active_by_page.get_mut(&completed_page) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    active_by_page.remove(&completed_page);
                }
            }
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
                &view,
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

    let snapshot = translation_snapshot(
        &units,
        &resolved,
        &failures,
        &retrying_ids,
        &cached_ids,
        queue_snapshot(&queue, started_at),
        &run.scope.view(),
    );
    let translated = units
        .iter()
        .filter_map(|unit| resolved.remove(&unit.id))
        .collect();
    Ok(TranslationRunOutcome {
        document: TranslatedDocument {
            document,
            units: translated,
        },
        failures: snapshot.failures,
        progress: snapshot.progress,
        queue: snapshot.queue,
    })
}

#[derive(Clone)]
struct PlannedRequest {
    units: Vec<TranslationUnit>,
    complex: bool,
}

impl PlannedRequest {
    /// Every page the request's units are on.
    fn pages(&self) -> impl Iterator<Item = usize> + '_ {
        self.units.iter().flat_map(|unit| {
            let pages: &[usize] = if unit.member_pages.is_empty() {
                std::slice::from_ref(&unit.page_index)
            } else {
                &unit.member_pages
            };
            pages.iter().copied()
        })
    }

    /// How urgent the request is and the page that makes it so.
    fn rank(&self, view: &ScopeView) -> Option<(u32, usize)> {
        view.rank_pages(self.pages())
    }

    /// The page the request is counted against while in flight.
    fn slot_page(&self, view: &ScopeView) -> usize {
        self.rank(view)
            .map(|(_, page)| page)
            .unwrap_or_else(|| self.pages().min().unwrap_or_default())
    }
}

struct PendingRequest {
    plan: PlannedRequest,
    attempt: usize,
    ready_at: Instant,
    final_recovery: bool,
    /// The page it counts against while in flight, fixed at dispatch.
    slot_page: usize,
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
                let repaired_output =
                    repair_duplicate_protected_placeholders(&unit.protected, &output.text);
                let normalized_output =
                    normalize_translated_whitespace(&repaired_output, &unit.source_text, unit.role);
                let validation = validate_translation(
                    &unit,
                    &normalized_output,
                    backend.reject_unchanged_output(),
                )
                .and_then(|translated| {
                    backend
                        .validate_translated_output(&unit, &normalized_output, &translated.text)
                        .map(|()| translated)
                });
                match validation {
                    Ok(translated) => Ok((translated, output.cached, normalized_output)),
                    Err(message) => Err(UnitFailure {
                        message,
                        retryable: true,
                        retry_after: None,
                        category: Some(TranslationErrorCategory::InvalidResponse),
                        status: None,
                        rejected_output: Some(normalized_output),
                    }),
                }
            }
            Some(output) => {
                let rejected_output = (!output.text.trim().is_empty()).then_some(output.text);
                Err(UnitFailure {
                    message: output
                        .validation_error
                        .expect("validation error was checked above"),
                    retryable: false,
                    retry_after: None,
                    category: Some(TranslationErrorCategory::InvalidResponse),
                    status: None,
                    rejected_output,
                })
            }
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
                    rejected_output: None,
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
                let source_excerpt =
                    focused_diagnostic_excerpt(&unit.source_text, &failure.message);
                let rejected_output_excerpt = failure
                    .rejected_output
                    .as_deref()
                    .and_then(|output| focused_diagnostic_excerpt(output, &failure.message));
                state.failures.insert(
                    unit.id.clone(),
                    TranslationFailureDiagnostic {
                        id: unit.id.clone(),
                        phase: TranslationFailurePhase::Translation,
                        reason: failure.message,
                        attempts: attempt,
                        retryable: failure.retryable,
                        category: failure.category.map(translation_error_category_name),
                        status: failure.status,
                        member_block_ids: unit.member_block_ids.clone(),
                        role: Some(unit.role),
                        source_excerpt,
                        rejected_output_excerpt,
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
                        slot_page: 0,
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

fn repair_duplicate_protected_placeholders(protected: &ProtectedText, translated: &str) -> String {
    let mut repaired = translated.to_string();
    for token in &protected.tokens {
        if matches!(
            token.kind,
            ProtectedTokenKind::StyleStart | ProtectedTokenKind::StyleEnd
        ) || repaired.matches(&token.placeholder).count() <= 1
        {
            continue;
        }
        let first_end = repaired
            .find(&token.placeholder)
            .expect("duplicate placeholder has a first occurrence")
            + token.placeholder.len();
        let suffix = repaired[first_end..].replace(&token.placeholder, "");
        repaired.truncate(first_end);
        repaired.push_str(&suffix);
    }
    repaired
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
    rejected_output: Option<String>,
}

const DIAGNOSTIC_EXCERPT_CHARACTERS: usize = 480;

fn focused_diagnostic_excerpt(text: &str, reason: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let characters = normalized.chars().collect::<Vec<_>>();
    if characters.len() <= DIAGNOSTIC_EXCERPT_CHARACTERS {
        return Some(normalized);
    }

    let focus = diagnostic_focus_hint(reason);
    let focus_character = focus
        .as_deref()
        .and_then(|focus| {
            normalized
                .to_ascii_lowercase()
                .find(&focus.to_ascii_lowercase())
                .map(|byte_index| normalized[..byte_index].chars().count())
        })
        .unwrap_or_default();
    let focus_length = focus
        .as_deref()
        .map(|focus| focus.chars().count())
        .unwrap_or_default();
    let leading = DIAGNOSTIC_EXCERPT_CHARACTERS
        .saturating_sub(focus_length)
        .div_ceil(2);
    let mut start = focus_character.saturating_sub(leading);
    let end = (start + DIAGNOSTIC_EXCERPT_CHARACTERS).min(characters.len());
    start = end.saturating_sub(DIAGNOSTIC_EXCERPT_CHARACTERS);

    let mut excerpt = String::new();
    if start > 0 {
        excerpt.push('…');
    }
    excerpt.extend(characters[start..end].iter());
    if end < characters.len() {
        excerpt.push('…');
    }
    Some(excerpt)
}

fn diagnostic_focus_hint(reason: &str) -> Option<String> {
    if let Some((_, detail)) = reason.rsplit_once(": ") {
        let detail_word = detail
            .split(|character: char| !character.is_ascii_alphabetic())
            .filter(|word| word.len() >= 4)
            .max_by_key(|word| word.len());
        if let Some(word) = detail_word {
            return Some(word.to_string());
        }
    }
    let start = reason.find("[[GLYPHST_")?;
    let relative_end = reason[start..].find("]]")?;
    Some(reason[start..start + relative_end + 2].to_string())
}

fn translation_snapshot(
    units: &[TranslationUnit],
    resolved: &HashMap<String, TranslatedUnit>,
    failures: &HashMap<String, TranslationFailureDiagnostic>,
    retrying_ids: &HashSet<String>,
    cached_ids: &HashSet<String>,
    queue: TranslationQueueStats,
    view: &ScopeView,
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
            in_scope: units
                .iter()
                .filter(|unit| {
                    let pages: &[usize] = if unit.member_pages.is_empty() {
                        std::slice::from_ref(&unit.page_index)
                    } else {
                        &unit.member_pages
                    };
                    view.rank_pages(pages.iter().copied()).is_some()
                })
                .count(),
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

/// Whether a request may go now as far as the scope and its pages go.
fn request_eligible(
    request: &PendingRequest,
    view: &ScopeView,
    ready: &(dyn Fn(usize) -> bool + Sync),
) -> bool {
    request.plan.rank(view).is_some() && request.plan.pages().all(ready)
}

fn next_dispatchable_request(
    pending: &VecDeque<PendingRequest>,
    active_by_page: &HashMap<usize, usize>,
    active: usize,
    active_complex: usize,
    view: &ScopeView,
    ready: &(dyn Fn(usize) -> bool + Sync),
) -> Option<usize> {
    let now = Instant::now();
    let dispatchable = |request: &PendingRequest| {
        request.ready_at <= now
            && (!request.plan.complex || active_complex < MAX_ACTIVE_COMPLEX_REQUESTS)
            && request_eligible(request, view, ready)
    };
    // The most urgent page with work, waiting or in flight, gets the larger
    // reservation; with a whole scope from the start that is the earliest.
    let urgent_page = pending
        .iter()
        .filter_map(|request| request.plan.rank(view))
        .chain(
            active_by_page
                .keys()
                .map(|&page| (view.rank(page).unwrap_or(u32::MAX), page)),
        )
        .min()?
        .1;
    let urgent_active = active_by_page
        .get(&urgent_page)
        .copied()
        .unwrap_or_default();
    let later_active = active.saturating_sub(urgent_active);
    let best = |on_urgent: bool| {
        pending
            .iter()
            .enumerate()
            .filter(|(_, request)| dispatchable(request))
            .filter_map(|(index, request)| {
                let (rank, page) = request.plan.rank(view)?;
                ((page == urgent_page) == on_urgent).then_some((rank, index))
            })
            .min()
            .map(|(_, index)| index)
    };
    let urgent = best(true);
    let later = best(false);

    if urgent_active < 4 && urgent.is_some() {
        return urgent;
    }
    if later_active < 2 && later.is_some() {
        return later;
    }

    // Keep the queue work-conserving when one reservation has no ready work,
    // including while a page-priority retry is waiting for its backoff.
    urgent.or(later)
}

fn update_page_allocation_peaks(
    queue: &mut TranslationQueueStats,
    active_by_page: &HashMap<usize, usize>,
) {
    let Some(earliest_page) = active_by_page.keys().copied().min() else {
        return;
    };
    let earliest = active_by_page
        .get(&earliest_page)
        .copied()
        .unwrap_or_default();
    let later = active_by_page
        .iter()
        .filter(|(page, _)| **page != earliest_page)
        .map(|(_, count)| *count)
        .sum();
    queue.peak_active_earliest_page_requests =
        queue.peak_active_earliest_page_requests.max(earliest);
    queue.peak_active_later_page_requests = queue.peak_active_later_page_requests.max(later);
}

fn next_ready_delay(
    pending: &VecDeque<PendingRequest>,
    active_complex: usize,
    view: &ScopeView,
    ready: &(dyn Fn(usize) -> bool + Sync),
) -> Option<Duration> {
    let now = Instant::now();
    pending
        .iter()
        .filter(|request| !request.plan.complex || active_complex < MAX_ACTIVE_COMPLEX_REQUESTS)
        .filter(|request| request_eligible(request, view, ready))
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

mod planning;
mod protection;

pub use protection::protect_text;

use planning::*;
use protection::*;

#[cfg(test)]
mod tests;
