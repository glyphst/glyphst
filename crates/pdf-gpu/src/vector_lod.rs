use glyphst_pdf_engine::CancellationToken;
use glyphst_pdf_render_wgpu::PrepareError;

pub(super) fn prepare_with_headroom<T>(
    required_scale: f32,
    preferred_scale: f32,
    cancellation: &CancellationToken,
    mut prepare: impl FnMut(f32) -> Result<T, PrepareError>,
) -> Result<T, PrepareError> {
    for scale in [required_scale, preferred_scale] {
        if !scale.is_finite() || scale < 1.0 {
            return Err(PrepareError::InvalidVectorScale(scale));
        }
    }
    let preferred_scale = preferred_scale.max(required_scale);
    if cancellation.is_cancelled() {
        return Err(PrepareError::Cancelled);
    }
    match prepare(preferred_scale) {
        Err(PrepareError::Limit { .. }) if preferred_scale > required_scale => {
            if cancellation.is_cancelled() {
                return Err(PrepareError::Cancelled);
            }
            prepare(required_scale)
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit() -> PrepareError {
        PrepareError::Limit {
            resource: "retained vector bytes",
            requested: 101,
            limit: 100,
        }
    }

    #[test]
    fn successful_headroom_keeps_the_stricter_page() {
        let mut calls = Vec::new();
        let result =
            prepare_with_headroom(44.59375, 90.07938, &CancellationToken::new(), |scale| {
                calls.push(scale);
                Ok(scale)
            })
            .unwrap();
        assert_eq!(calls, [90.07938]);
        assert_eq!(result, 90.07938);
    }

    #[test]
    fn optional_budget_failure_retries_only_the_required_scale() {
        let mut calls = Vec::new();
        let result =
            prepare_with_headroom(44.59375, 90.07938, &CancellationToken::new(), |scale| {
                calls.push(scale);
                if scale > 50.0 {
                    Err(limit())
                } else {
                    Ok(scale)
                }
            })
            .unwrap();
        assert_eq!(calls, [90.07938, 44.59375]);
        assert_eq!(result, 44.59375);

        calls.clear();
        let result: Result<(), _> =
            prepare_with_headroom(44.59375, 90.07938, &CancellationToken::new(), |scale| {
                calls.push(scale);
                Err(limit())
            });
        assert!(matches!(result, Err(PrepareError::Limit { .. })));
        assert_eq!(calls, [90.07938, 44.59375]);
    }

    #[test]
    fn cancellation_and_semantic_errors_never_retry() {
        let cancellation = CancellationToken::new();
        let mut calls = 0;
        let result: Result<(), _> = prepare_with_headroom(8.0, 16.0, &cancellation, |_| {
            calls += 1;
            cancellation.cancel();
            Err(limit())
        });
        assert!(matches!(result, Err(PrepareError::Cancelled)));
        assert_eq!(calls, 1);
        let result: Result<(), _> = prepare_with_headroom(8.0, 16.0, &cancellation, |_| {
            panic!("cancelled work executed")
        });
        assert!(matches!(result, Err(PrepareError::Cancelled)));

        for error in [
            PrepareError::Cancelled,
            PrepareError::StaleDeviceGeneration,
            PrepareError::Adapter("invalid geometry".into()),
        ] {
            let mut error = Some(error);
            let result: Result<(), _> =
                prepare_with_headroom(8.0, 16.0, &CancellationToken::new(), |_| {
                    Err(error.take().expect("non-budget error retried"))
                });
            assert!(result.is_err());
        }
    }

    #[test]
    fn invalid_scales_do_no_work_and_required_scale_is_never_reduced() {
        for (required, preferred) in [
            (f32::NAN, 8.0),
            (8.0, f32::INFINITY),
            (0.5, 8.0),
            (8.0, 0.5),
        ] {
            let result: Result<(), _> =
                prepare_with_headroom(required, preferred, &CancellationToken::new(), |_| {
                    panic!("invalid scale prepared")
                });
            assert!(matches!(result, Err(PrepareError::InvalidVectorScale(_))));
        }
        let mut calls = Vec::new();
        let result: Result<(), _> =
            prepare_with_headroom(16.0, 8.0, &CancellationToken::new(), |scale| {
                calls.push(scale);
                Err(limit())
            });
        assert!(matches!(result, Err(PrepareError::Limit { .. })));
        assert_eq!(calls, [16.0]);
    }
}
