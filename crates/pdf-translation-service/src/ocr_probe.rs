//! A connection test of the OCR services, for settings pages.

use std::time::Duration;

use glyphst_pdf_translation::{
    CancellationToken, PaddleClient, PaddleClientConfig, PaddleErrorCategory, UnlimitedClient,
    UnlimitedClientConfig, UnlimitedErrorCategory,
};
use glyphst_settings::{OcrProvider, OcrSettings};

/// Why an OCR service refused a connection test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OcrProbeError {
    InvalidAuth,
    Retryable,
    Provider(Option<String>),
}

/// Checks that the OCR service in `settings` answers and accepts its keys.
/// Blocks for up to about 15 seconds.
pub fn test_ocr_connection(settings: OcrSettings) -> Result<(), OcrProbeError> {
    let cancellation = CancellationToken::new();
    match settings.provider {
        // Loading the models is the test.
        OcrProvider::Local => match glyphst_ocr::LocalOcr::installed() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(OcrProbeError::Provider(Some("not installed".to_string()))),
            Err(error) => Err(OcrProbeError::Provider(Some(format!("{error:#}")))),
        },
        OcrProvider::Paddle => PaddleClient::new(
            PaddleClientConfig {
                base_url: settings.paddle.endpoint,
                request_timeout: Duration::from_secs(15),
                ..PaddleClientConfig::default()
            },
            settings.paddle.token,
        )
        .map_err(|_| OcrProbeError::InvalidAuth)?
        .probe(&cancellation)
        .map(|_| ())
        .map_err(|error| match error.category {
            PaddleErrorCategory::Unauthorized | PaddleErrorCategory::PermissionDenied => {
                OcrProbeError::InvalidAuth
            }
            PaddleErrorCategory::QueueFull
            | PaddleErrorCategory::ServiceUnavailable
            | PaddleErrorCategory::PollTimeout => OcrProbeError::Retryable,
            _ => OcrProbeError::Provider(error.provider_code),
        }),
        OcrProvider::Unlimited => UnlimitedClient::new(
            UnlimitedClientConfig {
                base_url: settings.unlimited.endpoint,
                request_timeout: Duration::from_secs(15),
                download_timeout: Duration::from_secs(15),
                ..UnlimitedClientConfig::default()
            },
            settings.unlimited.api_key,
            settings.unlimited.secret_key,
        )
        .map_err(|_| OcrProbeError::InvalidAuth)?
        .probe(&cancellation)
        .map(|_| ())
        .map_err(|error| match error.category {
            UnlimitedErrorCategory::Unauthorized | UnlimitedErrorCategory::PermissionDenied => {
                OcrProbeError::InvalidAuth
            }
            UnlimitedErrorCategory::QueueFull
            | UnlimitedErrorCategory::ServiceUnavailable
            | UnlimitedErrorCategory::PollTimeout => OcrProbeError::Retryable,
            _ => OcrProbeError::Provider(error.provider_code.map(|code| code.to_string())),
        }),
    }
}

impl std::fmt::Display for OcrProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAuth => f.write_str("the service refused the key or token"),
            Self::Retryable => f.write_str("the service is busy; try again later"),
            Self::Provider(Some(code)) => write!(f, "the service answered with error {code}"),
            Self::Provider(None) => f.write_str("the service answered with an error"),
        }
    }
}
