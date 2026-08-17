use glyphst_pdf_translation::{
    PolicyReviewDecision, PolicyReviewError, PolicyReviewRequest, TranslationPolicyReviewer,
};
use glyphst_settings::{PdfContentReviewMode, ResolvedAiConfig};

use crate::PdfTranslationConfig;

pub(crate) struct GlyphstPolicyReviewer {
    settings: Option<ResolvedAiConfig>,
    configuration_error: Option<String>,
}

impl GlyphstPolicyReviewer {
    pub(crate) fn from_config(config: &PdfTranslationConfig) -> Option<Self> {
        if config.content_review.mode == PdfContentReviewMode::Off {
            return None;
        }
        let resolved = config.content_review.resolved.clone();
        let configuration_error = match &resolved {
            Some(resolved) if resolved.can_request() => None,
            Some(resolved) => Some(resolved.missing_credentials_message()),
            None => Some(
                "The configured PDF content review model is unavailable or disabled".to_string(),
            ),
        };
        Some(Self {
            settings: resolved,
            configuration_error,
        })
    }
}

impl TranslationPolicyReviewer for GlyphstPolicyReviewer {
    fn review(
        &self,
        request: &PolicyReviewRequest,
    ) -> Result<Vec<PolicyReviewDecision>, PolicyReviewError> {
        if let Some(error) = &self.configuration_error {
            return Err(PolicyReviewError::new(error.clone()));
        }
        let settings = self
            .settings
            .clone()
            .ok_or_else(|| PolicyReviewError::new("PDF content review model is unavailable"))?;
        let payload = serde_json::to_value(request).map_err(|error| {
            PolicyReviewError::new(format!("could not encode page map: {error}"))
        })?;
        glyphst_ai::translation::review_pdf_translation_policy(settings, payload)
            .map(|decisions| {
                decisions
                    .into_iter()
                    .map(|decision| PolicyReviewDecision {
                        block_id: decision.block_id,
                        translate: decision.translate,
                        confidence: decision.confidence,
                        reason: decision.reason,
                    })
                    .collect()
            })
            .map_err(|error| PolicyReviewError::new(error.to_string()))
    }
}
