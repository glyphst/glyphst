use glyphst_pdf_translation::{
    PolicyReviewDecision, PolicyReviewError, PolicyReviewRequest, TranslationPolicyReviewer,
};
use glyphst_settings::{ResolvedAiConfig, Settings};

pub(crate) struct GlyphstPolicyReviewer {
    settings: ResolvedAiConfig,
}

impl GlyphstPolicyReviewer {
    pub(crate) fn from_settings(settings: &Settings) -> Option<Self> {
        let settings = settings.ai.resolved_translation();
        settings.can_request().then_some(Self { settings })
    }
}

impl TranslationPolicyReviewer for GlyphstPolicyReviewer {
    fn review(
        &self,
        request: &PolicyReviewRequest,
    ) -> Result<Vec<PolicyReviewDecision>, PolicyReviewError> {
        let payload = serde_json::to_value(request).map_err(|error| {
            PolicyReviewError::new(format!("could not encode page map: {error}"))
        })?;
        glyphst_ai::translation::review_pdf_translation_policy(self.settings.clone(), payload)
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
