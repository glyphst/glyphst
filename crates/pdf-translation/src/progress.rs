use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfTranslationStage {
    Preparing,
    Ocr,
    Normalizing,
    Translating,
    Layout,
    Compiling,
    Composing,
    Validating,
}

impl PdfTranslationStage {
    const ALL: [Self; 8] = [
        Self::Preparing,
        Self::Ocr,
        Self::Normalizing,
        Self::Translating,
        Self::Layout,
        Self::Compiling,
        Self::Composing,
        Self::Validating,
    ];

    fn weight(self) -> f32 {
        match self {
            Self::Preparing => 0.03,
            Self::Ocr => 0.27,
            Self::Normalizing => 0.05,
            Self::Translating => 0.30,
            Self::Layout => 0.10,
            Self::Compiling => 0.10,
            Self::Composing => 0.12,
            Self::Validating => 0.03,
        }
    }

    fn completed_weight_before(self) -> f32 {
        Self::ALL
            .into_iter()
            .take_while(|stage| *stage != self)
            .map(Self::weight)
            .sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfTranslationProgress {
    pub stage: PdfTranslationStage,
    pub detail: String,
    pub completed: Option<u32>,
    pub total: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translated: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrying: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
    pub overall_fraction: f32,
    pub indeterminate: bool,
}

pub trait TranslationProgressSink: Send {
    fn publish(&mut self, progress: PdfTranslationProgress);
}

impl<F> TranslationProgressSink for F
where
    F: FnMut(PdfTranslationProgress) + Send,
{
    fn publish(&mut self, progress: PdfTranslationProgress) {
        self(progress);
    }
}

pub struct ProgressTracker {
    last_fraction: f32,
    sink: Box<dyn TranslationProgressSink>,
}

impl ProgressTracker {
    pub fn new(sink: impl TranslationProgressSink + 'static) -> Self {
        Self {
            last_fraction: 0.0,
            sink: Box::new(sink),
        }
    }

    pub fn report(
        &mut self,
        stage: PdfTranslationStage,
        detail: impl Into<String>,
        completed: Option<u32>,
        total: Option<u32>,
    ) {
        let stage_fraction = match (completed, total) {
            (Some(completed), Some(total)) if total > 0 => completed as f32 / total as f32,
            _ => 0.0,
        }
        .clamp(0.0, 1.0);
        let computed = stage.completed_weight_before() + stage.weight() * stage_fraction;
        self.last_fraction = self.last_fraction.max(computed).clamp(0.0, 1.0);
        self.sink.publish(PdfTranslationProgress {
            stage,
            detail: detail.into(),
            completed,
            total,
            translated: None,
            cached: None,
            retrying: None,
            failed: None,
            overall_fraction: self.last_fraction,
            indeterminate: total.is_none(),
        });
    }

    pub fn report_translation(
        &mut self,
        detail: impl Into<String>,
        translated: u32,
        cached: u32,
        retrying: u32,
        failed: u32,
        total: u32,
    ) {
        let completed = translated.saturating_add(cached).min(total);
        let stage = PdfTranslationStage::Translating;
        let stage_fraction = if total > 0 {
            completed as f32 / total as f32
        } else {
            1.0
        };
        let computed = stage.completed_weight_before() + stage.weight() * stage_fraction;
        self.last_fraction = self.last_fraction.max(computed).clamp(0.0, 1.0);
        self.sink.publish(PdfTranslationProgress {
            stage,
            detail: detail.into(),
            completed: Some(completed),
            total: Some(total),
            translated: Some(translated),
            cached: Some(cached),
            retrying: Some(retrying),
            failed: Some(failed),
            overall_fraction: self.last_fraction,
            indeterminate: false,
        });
    }

    pub fn finish_stage(&mut self, stage: PdfTranslationStage, detail: impl Into<String>) {
        self.report(stage, detail, Some(1), Some(1));
    }

    pub fn finish(&mut self, detail: impl Into<String>) {
        self.last_fraction = 1.0;
        self.sink.publish(PdfTranslationProgress {
            stage: PdfTranslationStage::Validating,
            detail: detail.into(),
            completed: Some(1),
            total: Some(1),
            translated: None,
            cached: None,
            retrying: None,
            failed: None,
            overall_fraction: 1.0,
            indeterminate: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn overall_progress_is_monotonic() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let mut tracker = ProgressTracker::new(move |progress| {
            captured.lock().unwrap().push(progress);
        });

        tracker.report(PdfTranslationStage::Ocr, "running", Some(8), Some(10));
        tracker.report(
            PdfTranslationStage::Ocr,
            "provider correction",
            Some(7),
            Some(10),
        );
        tracker.report(
            PdfTranslationStage::Translating,
            "translating",
            Some(1),
            Some(4),
        );
        tracker.finish("done");

        let fractions: Vec<f32> = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.overall_fraction)
            .collect();
        assert!(fractions.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(fractions.last().copied(), Some(1.0));
    }

    #[test]
    fn translation_progress_counts_only_resolved_units_as_completed() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&events);
        let mut tracker = ProgressTracker::new(move |progress| {
            captured.lock().unwrap().push(progress);
        });

        tracker.report_translation("recovering", 3, 2, 1, 4, 10);

        let progress = events.lock().unwrap()[0].clone();
        assert_eq!(progress.completed, Some(5));
        assert_eq!(progress.translated, Some(3));
        assert_eq!(progress.cached, Some(2));
        assert_eq!(progress.retrying, Some(1));
        assert_eq!(progress.failed, Some(4));
    }

    #[test]
    fn progress_deserializes_without_new_optional_counters() {
        let progress: PdfTranslationProgress = serde_json::from_str(
            r#"{
                "stage":"translating",
                "detail":"legacy",
                "completed":1,
                "total":2,
                "overall_fraction":0.5,
                "indeterminate":false
            }"#,
        )
        .unwrap();

        assert_eq!(progress.translated, None);
        assert_eq!(progress.cached, None);
        assert_eq!(progress.retrying, None);
        assert_eq!(progress.failed, None);
    }
}
