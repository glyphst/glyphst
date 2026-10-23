use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OcrMode {
    Always,
    /// Only pages without usable text: scans, photos, garbled fonts.
    #[default]
    Auto,
    Off,
}

impl OcrMode {
    pub const ALL: [Self; 3] = [Self::Always, Self::Auto, Self::Off];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "always" => Some(Self::Always),
            "auto" => Some(Self::Auto),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OcrProvider {
    /// PP-OCRv6 on this computer (glyphst-ocr), once downloaded.
    #[default]
    Local,
    Unlimited,
    Paddle,
}

impl OcrProvider {
    pub const ALL: [Self; 3] = [Self::Local, Self::Unlimited, Self::Paddle];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Unlimited => "unlimited",
            Self::Paddle => "paddle",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "local" | "this-computer" | "pp-ocrv6" => Some(Self::Local),
            "unlimited" | "unlimited-ocr" | "baidu-unlimited" => Some(Self::Unlimited),
            "paddle" | "paddleocr" | "paddle-ocr" => Some(Self::Paddle),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PaddleOcrSettings {
    pub token: String,
    pub endpoint: String,
    pub model: String,
}

impl std::fmt::Debug for PaddleOcrSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaddleOcrSettings")
            .field("has_token", &!self.token.is_empty())
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .finish()
    }
}

impl Default for PaddleOcrSettings {
    fn default() -> Self {
        Self {
            token: String::new(),
            endpoint: DEFAULT_PADDLE_OCR_ENDPOINT.to_string(),
            model: DEFAULT_PADDLE_OCR_MODEL.to_string(),
        }
    }
}

impl PaddleOcrSettings {
    pub(super) fn sanitize(&mut self) {
        self.token = self.token.trim().to_string();
        self.endpoint = self.endpoint.trim().trim_end_matches('/').to_string();
        if self.endpoint.is_empty() {
            self.endpoint = DEFAULT_PADDLE_OCR_ENDPOINT.to_string();
        }
        self.model = self.model.trim().to_string();
        if self.model.is_empty() {
            self.model = DEFAULT_PADDLE_OCR_MODEL.to_string();
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct UnlimitedOcrSettings {
    pub api_key: String,
    pub secret_key: String,
    pub endpoint: String,
}

impl std::fmt::Debug for UnlimitedOcrSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UnlimitedOcrSettings")
            .field("has_api_key", &!self.api_key.is_empty())
            .field("has_secret_key", &!self.secret_key.is_empty())
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl Default for UnlimitedOcrSettings {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            endpoint: DEFAULT_UNLIMITED_OCR_ENDPOINT.to_string(),
        }
    }
}

impl UnlimitedOcrSettings {
    pub(super) fn sanitize(&mut self) {
        self.api_key = self.api_key.trim().to_string();
        self.secret_key = self.secret_key.trim().to_string();
        self.endpoint = self.endpoint.trim().trim_end_matches('/').to_string();
        if self.endpoint.is_empty() {
            self.endpoint = DEFAULT_UNLIMITED_OCR_ENDPOINT.to_string();
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OcrSettings {
    pub mode: OcrMode,
    pub provider: OcrProvider,
    pub paddle: PaddleOcrSettings,
    pub unlimited: UnlimitedOcrSettings,
}

impl std::fmt::Debug for OcrSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OcrSettings")
            .field("mode", &self.mode)
            .field("provider", &self.provider)
            .field("paddle", &self.paddle)
            .field("unlimited", &self.unlimited)
            .finish()
    }
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            mode: OcrMode::Auto,
            provider: OcrProvider::Local,
            paddle: PaddleOcrSettings::default(),
            unlimited: UnlimitedOcrSettings::default(),
        }
    }
}

impl OcrSettings {
    pub(super) fn sanitize(&mut self) {
        self.paddle.sanitize();
        self.unlimited.sanitize();
    }
}
