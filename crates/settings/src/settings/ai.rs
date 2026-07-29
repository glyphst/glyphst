use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiApiStyle {
    OpenAi,
    OpenAiResponses,
    Anthropic,
}

impl AiApiStyle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai-compatible",
            Self::OpenAiResponses => "openai-responses",
            Self::Anthropic => "anthropic-compatible",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "openai" | "openai-compatible" | "open_ai_compatible" => Some(Self::OpenAi),
            "anthropic" | "anthropic-compatible" | "anthropic_compatible" => Some(Self::Anthropic),
            "openai-responses" | "openai_responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AiThinkingLevel {
    Off,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
}

impl AiThinkingLevel {
    const FALLBACK: [Self; 4] = [Self::Off, Self::Low, Self::Medium, Self::High];
    const OPENAI_REASONING: [Self; 6] = [
        Self::Off,
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
    ];
    const CHATGPT_56_REASONING: [Self; 6] = [
        Self::Off,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::XHigh,
        Self::Max,
    ];
    const CHATGPT_55_REASONING: [Self; 5] =
        [Self::Off, Self::Low, Self::Medium, Self::High, Self::XHigh];
    const ANTHROPIC_THINKING: [Self; 5] =
        [Self::Off, Self::Low, Self::Medium, Self::High, Self::Max];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Off => crate::fl!("thinking-level-off"),
            Self::Minimal => crate::fl!("thinking-level-minimal"),
            Self::Low => crate::fl!("thinking-level-low"),
            Self::Medium => crate::fl!("thinking-level-medium"),
            Self::High => crate::fl!("thinking-level-high"),
            Self::XHigh => crate::fl!("thinking-level-x-high"),
            Self::Max => crate::fl!("thinking-level-max"),
        }
    }

    pub fn supported_for(provider: AiProvider, model: &str) -> &'static [Self] {
        Self::supported_for_style(provider.api_style(), model)
    }

    pub fn supported_for_style(api_style: AiApiStyle, model: &str) -> &'static [Self] {
        match api_style {
            AiApiStyle::OpenAi if Self::openai_supports_native_thinking(model) => {
                &Self::OPENAI_REASONING
            }
            AiApiStyle::OpenAiResponses if model.starts_with("gpt-5.6-") => {
                &Self::CHATGPT_56_REASONING
            }
            AiApiStyle::OpenAiResponses => &Self::CHATGPT_55_REASONING,
            AiApiStyle::Anthropic if Self::anthropic_supports_native_thinking(model) => {
                &Self::ANTHROPIC_THINKING
            }
            _ => &Self::FALLBACK,
        }
    }

    pub fn clamp_for(self, provider: AiProvider, model: &str) -> Self {
        self.clamp_for_style(provider.api_style(), model)
    }

    pub fn clamp_for_style(self, api_style: AiApiStyle, model: &str) -> Self {
        let supported = Self::supported_for_style(api_style, model);
        if supported.contains(&self) {
            return self;
        }

        match self {
            Self::Minimal => {
                if supported.contains(&Self::Low) {
                    Self::Low
                } else {
                    Self::Off
                }
            }
            Self::XHigh | Self::Max => supported
                .iter()
                .rev()
                .copied()
                .find(|level| *level != Self::Off)
                .unwrap_or(Self::Off),
            other => other,
        }
    }

    pub fn supports_native_for(self, provider: AiProvider, model: &str) -> bool {
        self.supports_native_for_style(provider.api_style(), model)
    }

    pub fn supports_native_for_style(self, api_style: AiApiStyle, model: &str) -> bool {
        self != Self::Off
            && match api_style {
                AiApiStyle::OpenAi => Self::openai_supports_native_thinking(model),
                AiApiStyle::OpenAiResponses => true,
                AiApiStyle::Anthropic => Self::anthropic_supports_native_thinking(model),
            }
    }

    fn openai_supports_native_thinking(model: &str) -> bool {
        let model = Self::model_family(model);
        model.starts_with("o1")
            || model.starts_with("o3")
            || model.starts_with("o4")
            || model.starts_with("gpt-5")
    }

    fn anthropic_supports_native_thinking(model: &str) -> bool {
        let model = Self::model_family(model);
        model.contains("claude-3-7")
            || model.contains("claude-4")
            || model.contains("opus-4")
            || model.contains("sonnet-4")
            || model.contains("haiku-4")
    }

    fn model_family(model: &str) -> String {
        model
            .rsplit('/')
            .next()
            .unwrap_or(model)
            .trim()
            .to_ascii_lowercase()
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "off" | "none" | "disabled" => Some(Self::Off),
            "minimal" | "min" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "x-high" | "x_high" => Some(Self::XHigh),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageTextFormat {
    #[default]
    Block,
    Semantic,
}

impl PageTextFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Semantic => "semantic",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "block" => Some(Self::Block),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageImageQuality {
    Low,
    #[default]
    Medium,
    High,
}

impl PageImageQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn max_dimension(self) -> u32 {
        match self {
            Self::Low => 1024,
            Self::Medium => 1600,
            Self::High => 2400,
        }
    }

    pub fn jpeg_quality(self) -> u8 {
        match self {
            Self::Low => 65,
            Self::Medium => 75,
            Self::High => 85,
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AiProvider {
    ChatGptSubscription,
    #[default]
    OpenAi,
    Anthropic,
    DeepSeek,
    OpenRouter,
    Fireworks,
    Custom(u128),
}

impl AiProvider {
    pub const BUILT_INS: [Self; 6] = [
        Self::ChatGptSubscription,
        Self::OpenAi,
        Self::Anthropic,
        Self::DeepSeek,
        Self::OpenRouter,
        Self::Fireworks,
    ];

    pub fn custom() -> Self {
        Self::Custom(uuid::Uuid::new_v4().as_u128())
    }

    pub fn is_custom(self) -> bool {
        matches!(self, Self::Custom(_))
    }

    pub fn as_str(self) -> String {
        match self {
            Self::OpenAi => "openai".to_string(),
            Self::ChatGptSubscription => "openai-subscribed".to_string(),
            Self::Anthropic => "anthropic".to_string(),
            Self::DeepSeek => "deepseek".to_string(),
            Self::OpenRouter => "openrouter".to_string(),
            Self::Fireworks => "fireworks".to_string(),
            Self::Custom(id) => format!("custom-{}", uuid::Uuid::from_u128(id)),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::ChatGptSubscription => "ChatGPT Subscription",
            Self::Anthropic => "Anthropic",
            Self::DeepSeek => "DeepSeek",
            Self::OpenRouter => "OpenRouter",
            Self::Fireworks => "Fireworks",
            Self::Custom(_) => "Custom Provider",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        let provider = match value {
            "openai" | "open-ai" | "open_ai" => Some(Self::OpenAi),
            "openai-subscribed"
            | "openai_subscribed"
            | "chatgpt"
            | "chatgpt-subscription"
            | "chatgpt_subscription" => Some(Self::ChatGptSubscription),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "deepseek" | "deep-seek" | "deep_seek" => Some(Self::DeepSeek),
            "openrouter" | "open-router" | "open_router" => Some(Self::OpenRouter),
            "fireworks" | "fireworks-ai" | "fireworks_ai" => Some(Self::Fireworks),
            _ => None,
        };
        provider.or_else(|| {
            value
                .strip_prefix("custom-")
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
                .map(|id| Self::Custom(id.as_u128()))
        })
    }

    pub fn default_enabled(self) -> bool {
        self == Self::OpenAi
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4o",
            Self::ChatGptSubscription => "gpt-5.5",
            Self::Anthropic => "claude-3-5-sonnet-latest",
            Self::DeepSeek => "deepseek-chat",
            Self::OpenRouter => "openai/gpt-4o",
            Self::Fireworks => "accounts/fireworks/models/llama-v3p1-70b-instruct",
            Self::Custom(_) => "",
        }
    }

    pub fn default_title_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4o-mini",
            Self::ChatGptSubscription => "gpt-5.6-luna",
            Self::Anthropic => "claude-3-5-haiku-latest",
            Self::DeepSeek => "deepseek-chat",
            Self::OpenRouter => "openai/gpt-4o-mini",
            Self::Fireworks => "accounts/fireworks/models/llama-v3p1-8b-instruct",
            Self::Custom(_) => "",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com",
            Self::ChatGptSubscription => "https://chatgpt.com/backend-api/codex",
            Self::Anthropic => "https://api.anthropic.com",
            Self::DeepSeek => "https://api.deepseek.com",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Fireworks => "https://api.fireworks.ai/inference",
            Self::Custom(_) => "",
        }
    }

    pub fn api_style(self) -> AiApiStyle {
        match self {
            Self::Anthropic => AiApiStyle::Anthropic,
            Self::ChatGptSubscription => AiApiStyle::OpenAiResponses,
            Self::OpenAi
            | Self::DeepSeek
            | Self::OpenRouter
            | Self::Fireworks
            | Self::Custom(_) => AiApiStyle::OpenAi,
        }
    }

    pub fn default_models(self) -> Vec<AiModel> {
        let label = self.label();
        let models = match self {
            Self::OpenAi => &[
                ("gpt-4o", "GPT-4o", "OpenAI"),
                ("gpt-4o-mini", "GPT-4o mini", "OpenAI"),
                ("gpt-4.1", "GPT-4.1", "OpenAI"),
                ("gpt-4.1-mini", "GPT-4.1 mini", "OpenAI"),
                ("o3", "o3", "OpenAI"),
                ("o4-mini", "o4 mini", "OpenAI"),
            ][..],
            Self::ChatGptSubscription => &[
                ("gpt-5.6-sol", "GPT-5.6 Sol", "ChatGPT"),
                ("gpt-5.6-terra", "GPT-5.6 Terra", "ChatGPT"),
                ("gpt-5.6-luna", "GPT-5.6 Luna", "ChatGPT"),
                ("gpt-5.5", "GPT-5.5", "ChatGPT"),
                ("gpt-5.4", "GPT-5.4", "ChatGPT"),
                ("gpt-5.4-mini", "GPT-5.4 Mini", "ChatGPT"),
            ],
            Self::Anthropic => &[
                ("claude-3-5-sonnet-latest", "Claude 3.5 Sonnet", "Claude"),
                ("claude-3-5-haiku-latest", "Claude 3.5 Haiku", "Claude"),
                ("claude-sonnet-4-5-latest", "Claude Sonnet 4.5", "Claude"),
            ],
            Self::DeepSeek => &[
                ("deepseek-chat", "DeepSeek Chat", "DeepSeek"),
                ("deepseek-reasoner", "DeepSeek Reasoner", "DeepSeek"),
            ],
            Self::OpenRouter => &[
                ("openai/gpt-4o", "GPT-4o", "OpenAI"),
                ("openai/gpt-4o-mini", "GPT-4o mini", "OpenAI"),
                (
                    "anthropic/claude-3.5-sonnet",
                    "Claude 3.5 Sonnet",
                    "Anthropic",
                ),
                ("deepseek/deepseek-chat", "DeepSeek Chat", "DeepSeek"),
            ],
            Self::Fireworks => &[
                (
                    "accounts/fireworks/models/llama-v3p1-70b-instruct",
                    "Llama 3.1 70B Instruct",
                    "Llama",
                ),
                (
                    "accounts/fireworks/models/llama-v3p1-8b-instruct",
                    "Llama 3.1 8B Instruct",
                    "Llama",
                ),
            ],
            Self::Custom(_) => &[],
        };
        models
            .iter()
            .map(|(id, name, group)| {
                let group = if group.is_empty() { label } else { *group };
                AiModel::new(*id, *name, group)
            })
            .collect()
    }

    pub fn default_config(self) -> AiProviderConfig {
        let mut config = AiProviderConfig {
            name: self.label().to_string(),
            api_style: self.api_style(),
            enabled: self.default_enabled(),
            api_key: String::new(),
            base_url: None,
            models: self.default_models(),
        };
        config.sanitize(self);
        config
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiModel {
    pub id: String,
    pub name: String,
    pub group: String,
}

impl AiModel {
    pub fn new(id: impl Into<String>, name: impl Into<String>, group: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            group: group.into(),
        }
    }

    pub(super) fn sanitize(&mut self, provider: AiProvider) {
        self.id = self.id.trim().to_string();
        self.name = self.name.trim().to_string();
        self.group = self.group.trim().to_string();
        if self.name.is_empty() {
            self.name = self.id.clone();
        }
        if self.group.is_empty() {
            self.group = provider.label().to_string();
        }
    }

    pub(super) fn from_node(provider: AiProvider, node: &KdlNode) -> Option<Self> {
        let id = if node.name().value() == "model" {
            node.get(0).and_then(KdlValue::as_string)?.to_string()
        } else {
            node.name().value().to_string()
        };
        let name = node
            .get(1)
            .and_then(KdlValue::as_string)
            .unwrap_or(&id)
            .to_string();
        let group = node
            .get(2)
            .and_then(KdlValue::as_string)
            .unwrap_or(provider.label())
            .to_string();
        let mut model = Self { id, name, group };
        model.sanitize(provider);
        (!model.id.is_empty()).then_some(model)
    }

    pub(super) fn write_to_document(&self, document: &mut KdlDocument) {
        let mut node = KdlNode::new("model");
        node.push(self.id.clone());
        node.push(self.name.clone());
        node.push(self.group.clone());
        document.nodes_mut().push(node);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: AiProvider,
    pub id: String,
}

impl ModelRef {
    pub fn new(provider: AiProvider, id: impl Into<String>) -> Self {
        Self {
            provider,
            id: id.into(),
        }
    }

    pub(super) fn sanitize(&mut self) {
        self.id = self.id.trim().to_string();
    }

    pub(super) fn from_node(node: &KdlNode) -> Option<Self> {
        let provider = node.get(0).and_then(KdlValue::as_string)?;
        let model = node.get(1).and_then(KdlValue::as_string)?;
        Some(Self::new(AiProvider::from_str(provider)?, model))
    }

    pub(super) fn write_to_document(&self, document: &mut KdlDocument, name: &str) {
        let mut node = KdlNode::new(name);
        node.push(self.provider.as_str());
        node.push(self.id.clone());
        document.nodes_mut().push(node);
    }
}

fn sanitize_fixed_provider_model(model_ref: &mut Option<ModelRef>, title: bool) {
    let Some(model_ref) = model_ref.as_mut() else {
        return;
    };
    if model_ref.provider != AiProvider::ChatGptSubscription {
        return;
    }
    let provider = model_ref.provider;
    if !provider
        .default_models()
        .iter()
        .any(|model| model.id == model_ref.id.trim())
    {
        model_ref.id = if title {
            provider.default_title_model()
        } else {
            provider.default_model()
        }
        .to_string();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiProviderConfig {
    pub name: String,
    pub api_style: AiApiStyle,
    pub enabled: bool,
    pub api_key: String,
    pub base_url: Option<String>,
    pub models: Vec<AiModel>,
}

impl AiProviderConfig {
    pub fn custom(
        name: impl Into<String>,
        api_style: AiApiStyle,
        api_host: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            api_style: match api_style {
                AiApiStyle::Anthropic => AiApiStyle::Anthropic,
                AiApiStyle::OpenAi | AiApiStyle::OpenAiResponses => AiApiStyle::OpenAi,
            },
            enabled: false,
            api_key: api_key.into(),
            base_url: Some(api_host.into()),
            models: Vec::new(),
        }
    }

    pub(super) fn sanitize(&mut self, provider: AiProvider) {
        self.name = self.name.trim().to_string();
        self.api_key = self.api_key.trim().to_string();

        if !provider.is_custom() {
            self.name = provider.label().to_string();
            self.api_style = provider.api_style();
            self.base_url = None;
        } else {
            if self.name.is_empty() {
                self.name = provider.label().to_string();
            }
            if self.api_style == AiApiStyle::OpenAiResponses {
                self.api_style = AiApiStyle::OpenAi;
            }
            self.base_url = self
                .base_url
                .take()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| !value.is_empty());
        }

        if provider == AiProvider::ChatGptSubscription {
            self.api_key.clear();
            let canonical = provider.default_models();
            let mut models = Vec::with_capacity(canonical.len());
            for current in self.models.drain(..) {
                if let Some(model) = canonical.iter().find(|model| model.id == current.id)
                    && !models.iter().any(|entry: &AiModel| entry.id == model.id)
                {
                    models.push(model.clone());
                }
            }
            for model in canonical {
                if !models.iter().any(|entry| entry.id == model.id) {
                    models.push(model);
                }
            }
            self.models = models;
            return;
        }

        let mut models = Vec::new();
        for mut model in self.models.drain(..) {
            model.sanitize(provider);
            if provider.is_custom() && model.group == provider.label() {
                model.group = self.name.clone();
            }
            if model.id.is_empty() || models.iter().any(|entry: &AiModel| entry.id == model.id) {
                continue;
            }
            models.push(model);
        }
        self.models = models;
    }

    pub(super) fn from_document(provider: AiProvider, document: &KdlDocument) -> Self {
        let mut config = provider.default_config();
        if provider.is_custom() {
            if let Some(value) = document.get_arg("name").and_then(KdlValue::as_string) {
                config.name = value.to_string();
            }
            if let Some(value) = document
                .get_arg("api_style")
                .and_then(KdlValue::as_string)
                .and_then(AiApiStyle::from_str)
            {
                config.api_style = value;
            }
        }
        if let Some(value) = document.get_arg("enabled").and_then(KdlValue::as_bool) {
            config.enabled = value;
        }
        if let Some(value) = document.get_arg("api_key").and_then(KdlValue::as_string) {
            config.api_key = value.to_string();
        }
        if provider.is_custom() {
            config.base_url = document
                .get_arg("api_host")
                .or_else(|| document.get_arg("base_url"))
                .and_then(KdlValue::as_string)
                .map(str::to_string);
        }
        if let Some(models) = document.get("models").and_then(KdlNode::children) {
            config.models = models
                .nodes()
                .iter()
                .filter_map(|node| AiModel::from_node(provider, node))
                .collect();
        }
        if let Some(value) = document.get_arg("model").and_then(KdlValue::as_string) {
            config.ensure_model(AiModel::new(value, value, config.name.clone()));
        }
        config.sanitize(provider);
        config
    }

    pub(super) fn write_to_document(&self, provider: AiProvider, document: &mut KdlDocument) {
        if provider.is_custom() {
            push_value_node(document, "name", self.name.clone());
            push_value_node(document, "api_style", self.api_style.as_str());
            if let Some(base_url) = &self.base_url {
                push_value_node(document, "api_host", base_url.clone());
            }
        }
        push_value_node(document, "enabled", self.enabled);
        if !self.api_key.is_empty() {
            push_value_node(document, "api_key", self.api_key.clone());
        }
        if self.models != provider.default_models() {
            let mut models = KdlNode::new("models");
            let models_children = models.ensure_children();
            for model in &self.models {
                model.write_to_document(models_children);
            }
            document.nodes_mut().push(models);
        }
    }

    pub fn model(&self, id: &str) -> Option<&AiModel> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn model_name(&self, id: &str) -> String {
        self.model(id)
            .map(|model| model.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    pub fn ensure_model(&mut self, model: AiModel) {
        if !self.models.iter().any(|entry| entry.id == model.id) {
            self.models.push(model);
        }
    }

    pub fn remove_model(&mut self, id: &str) {
        self.models.retain(|model| model.id != id);
    }

    pub fn move_model(&mut self, from: usize, to: usize) -> bool {
        if from >= self.models.len() || to >= self.models.len() || from == to {
            return false;
        }
        let model = self.models.remove(from);
        self.models.insert(to, model);
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAiConfig {
    pub provider: AiProvider,
    pub provider_name: String,
    pub api_style: AiApiStyle,
    pub enabled: bool,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub base_url_overridden: bool,
    pub thinking_level: AiThinkingLevel,
}

impl ResolvedAiConfig {
    pub fn can_request(&self) -> bool {
        if !self.enabled || self.model.is_empty() || self.base_url.trim().is_empty() {
            return false;
        }
        match self.api_style {
            AiApiStyle::OpenAi => !self.api_key.is_empty() || self.base_url_overridden,
            AiApiStyle::OpenAiResponses => true,
            AiApiStyle::Anthropic => !self.api_key.is_empty() || self.base_url_overridden,
        }
    }

    pub fn missing_credentials_message(&self) -> String {
        if !self.enabled || self.model.is_empty() {
            return "No enabled AI model is configured".to_string();
        }
        if self.base_url.trim().is_empty() {
            return format!("missing {} API Host", self.provider_name);
        }
        match self.api_style {
            AiApiStyle::OpenAi => format!(
                "missing {} API key, or set a custom base URL for an OpenAI-compatible server",
                self.provider_name
            ),
            AiApiStyle::OpenAiResponses => {
                "sign in to ChatGPT Subscription in Settings > AI before making requests"
                    .to_string()
            }
            AiApiStyle::Anthropic => format!("missing {} API key", self.provider_name),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuickAskSettings {
    pub transparency: f32,
    pub text_size: f32,
}

impl Default for QuickAskSettings {
    fn default() -> Self {
        Self {
            transparency: DEFAULT_QUICK_ASK_TRANSPARENCY,
            text_size: DEFAULT_QUICK_ASK_TEXT_SIZE,
        }
    }
}

impl QuickAskSettings {
    pub(super) fn sanitize(&mut self) {
        self.transparency = if self.transparency.is_finite() {
            self.transparency
                .clamp(MIN_QUICK_ASK_TRANSPARENCY, MAX_QUICK_ASK_TRANSPARENCY)
        } else {
            DEFAULT_QUICK_ASK_TRANSPARENCY
        };
        self.text_size = if self.text_size.is_finite() {
            self.text_size
                .clamp(MIN_QUICK_ASK_TEXT_SIZE, MAX_QUICK_ASK_TEXT_SIZE)
        } else {
            DEFAULT_QUICK_ASK_TEXT_SIZE
        };
    }

    pub fn background_alpha(self) -> u8 {
        let transparency = if self.transparency.is_finite() {
            self.transparency
                .clamp(MIN_QUICK_ASK_TRANSPARENCY, MAX_QUICK_ASK_TRANSPARENCY)
        } else {
            DEFAULT_QUICK_ASK_TRANSPARENCY
        };
        ((1.0 - transparency) * u8::MAX as f32).round() as u8
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AiSettings {
    pub providers: BTreeMap<AiProvider, AiProviderConfig>,
    pub provider_order: Vec<AiProvider>,
    pub main_model: Option<ModelRef>,
    pub title_model: Option<ModelRef>,
    pub translation_model: Option<ModelRef>,
    pub thinking_level: AiThinkingLevel,
    pub page_text_format: PageTextFormat,
    pub page_image_quality: PageImageQuality,
    pub page_image_limit: usize,
    pub quick_ask: QuickAskSettings,
}

impl Default for AiSettings {
    fn default() -> Self {
        let provider = AiProvider::OpenAi;
        Self {
            providers: AiProvider::BUILT_INS
                .into_iter()
                .map(|provider| (provider, provider.default_config()))
                .collect(),
            provider_order: AiProvider::BUILT_INS.to_vec(),
            main_model: Some(ModelRef::new(provider, provider.default_model())),
            title_model: Some(ModelRef::new(provider, provider.default_title_model())),
            translation_model: Some(ModelRef::new(provider, provider.default_model())),
            thinking_level: AiThinkingLevel::Medium,
            page_text_format: PageTextFormat::Block,
            page_image_quality: PageImageQuality::Medium,
            page_image_limit: DEFAULT_PAGE_IMAGE_LIMIT,
            quick_ask: QuickAskSettings::default(),
        }
    }
}

impl AiSettings {
    pub(super) fn sanitize(&mut self) {
        self.page_image_limit = self.page_image_limit.max(1);
        self.quick_ask.sanitize();

        for provider in AiProvider::BUILT_INS {
            self.providers
                .entry(provider)
                .or_insert_with(|| provider.default_config());
        }
        for (provider, config) in self.providers.iter_mut() {
            config.sanitize(*provider);
        }
        self.normalize_provider_order();

        sanitize_fixed_provider_model(&mut self.main_model, false);
        sanitize_fixed_provider_model(&mut self.title_model, true);
        sanitize_fixed_provider_model(&mut self.translation_model, false);

        if self
            .main_model
            .as_ref()
            .is_none_or(|model_ref| !self.is_enabled_model_ref(model_ref))
        {
            self.main_model = self.first_model_ref();
        }
        if self
            .title_model
            .as_ref()
            .is_none_or(|model_ref| !self.is_enabled_model_ref(model_ref))
        {
            self.title_model = self.first_model_ref();
        }
        if self
            .translation_model
            .as_ref()
            .is_none_or(|model_ref| !self.is_enabled_model_ref(model_ref))
        {
            self.translation_model = self
                .main_model
                .clone()
                .filter(|model_ref| self.is_enabled_model_ref(model_ref))
                .or_else(|| self.first_model_ref());
        }

        if let Some(model_ref) = self.main_model.as_mut() {
            model_ref.sanitize();
        }
        if let Some(model_ref) = self.main_model.as_ref() {
            let config = self.provider_config(model_ref.provider);
            self.thinking_level = self
                .thinking_level
                .clamp_for_style(config.api_style, &config.model_name(&model_ref.id));
        }
        if let Some(model_ref) = self.title_model.as_mut() {
            model_ref.sanitize();
        }
        if let Some(model_ref) = self.translation_model.as_mut() {
            model_ref.sanitize();
        }
    }

    pub fn provider_config(&self, provider: AiProvider) -> AiProviderConfig {
        self.providers
            .get(&provider)
            .cloned()
            .unwrap_or_else(|| provider.default_config())
    }

    pub fn provider_config_mut(&mut self, provider: AiProvider) -> &mut AiProviderConfig {
        self.providers
            .entry(provider)
            .or_insert_with(|| provider.default_config())
    }

    pub fn ordered_providers(&self) -> &[AiProvider] {
        &self.provider_order
    }

    pub fn provider_name(&self, provider: AiProvider) -> String {
        self.providers
            .get(&provider)
            .map(|config| config.name.clone())
            .unwrap_or_else(|| provider.label().to_string())
    }

    pub fn add_custom_provider(
        &mut self,
        name: impl Into<String>,
        api_style: AiApiStyle,
        api_host: impl Into<String>,
        api_key: impl Into<String>,
    ) -> AiProvider {
        let provider = AiProvider::custom();
        let mut config = AiProviderConfig::custom(name, api_style, api_host, api_key);
        config.sanitize(provider);
        self.providers.insert(provider, config);
        self.provider_order.push(provider);
        provider
    }

    pub fn remove_custom_provider(&mut self, provider: AiProvider) -> bool {
        if !provider.is_custom() || self.providers.remove(&provider).is_none() {
            return false;
        }
        self.provider_order.retain(|entry| *entry != provider);
        self.sanitize();
        true
    }

    pub fn move_provider(&mut self, from: usize, to: usize) -> bool {
        if from >= self.provider_order.len() || to >= self.provider_order.len() || from == to {
            return false;
        }
        let provider = self.provider_order.remove(from);
        self.provider_order.insert(to, provider);
        self.sanitize();
        true
    }

    pub fn set_provider_enabled(&mut self, provider: AiProvider, enabled: bool) {
        if let Some(config) = self.providers.get_mut(&provider) {
            config.enabled = enabled;
            self.sanitize();
        }
    }

    pub fn resolved_main(&self) -> ResolvedAiConfig {
        let Some(model_ref) = self
            .main_model
            .clone()
            .filter(|model_ref| self.is_enabled_model_ref(model_ref))
            .or_else(|| self.first_model_ref())
        else {
            return Self::unavailable_resolved();
        };
        let api_style = self.provider_config(model_ref.provider).api_style;
        let thinking_level = self
            .thinking_level
            .clamp_for_style(api_style, &model_ref.id);
        self.resolved_for(model_ref.provider, model_ref.id, thinking_level)
    }

    pub fn resolved_title(&self) -> Option<ResolvedAiConfig> {
        let title_model = self
            .title_model
            .clone()
            .filter(|model_ref| self.is_enabled_model_ref(model_ref))?;
        let resolved =
            self.resolved_for(title_model.provider, title_model.id, AiThinkingLevel::Off);
        resolved.can_request().then_some(resolved)
    }

    pub fn resolved_translation(&self) -> ResolvedAiConfig {
        let Some(model_ref) = self
            .translation_model
            .clone()
            .filter(|model_ref| self.is_enabled_model_ref(model_ref))
            .or_else(|| {
                self.main_model
                    .clone()
                    .filter(|model_ref| self.is_enabled_model_ref(model_ref))
            })
            .or_else(|| self.first_model_ref())
        else {
            return Self::unavailable_resolved();
        };
        self.resolved_for(model_ref.provider, model_ref.id, AiThinkingLevel::Off)
    }

    pub fn model_label(&self, model_ref: &ModelRef) -> String {
        self.provider_config(model_ref.provider)
            .model_name(&model_ref.id)
    }

    fn resolved_for(
        &self,
        provider: AiProvider,
        model: String,
        thinking_level: AiThinkingLevel,
    ) -> ResolvedAiConfig {
        let config = self.provider_config(provider);
        let thinking_level = thinking_level.clamp_for_style(config.api_style, &model);
        let base_url_overridden = provider.is_custom() && config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            provider_name: config.name,
            api_style: config.api_style,
            enabled: config.enabled,
            model,
            api_key: config.api_key,
            base_url: if provider.is_custom() {
                config.base_url.unwrap_or_default()
            } else {
                provider.default_base_url().to_string()
            },
            base_url_overridden,
            thinking_level,
        }
    }

    pub fn first_model_ref(&self) -> Option<ModelRef> {
        for provider in &self.provider_order {
            let Some(config) = self.providers.get(provider) else {
                continue;
            };
            if !config.enabled || config.models.is_empty() {
                continue;
            }
            return config
                .models
                .first()
                .map(|model| ModelRef::new(*provider, model.id.clone()));
        }
        None
    }

    fn normalize_provider_order(&mut self) {
        let mut order = Vec::with_capacity(self.providers.len());
        for provider in self.provider_order.drain(..) {
            if self.providers.contains_key(&provider) && !order.contains(&provider) {
                order.push(provider);
            }
        }
        for provider in AiProvider::BUILT_INS {
            if !order.contains(&provider) {
                order.push(provider);
            }
        }
        for provider in self.providers.keys().copied() {
            if !order.contains(&provider) {
                order.push(provider);
            }
        }
        self.provider_order = order;
    }

    fn is_enabled_model_ref(&self, model_ref: &ModelRef) -> bool {
        self.providers
            .get(&model_ref.provider)
            .is_some_and(|config| {
                config.enabled && config.models.iter().any(|model| model.id == model_ref.id)
            })
    }

    fn unavailable_resolved() -> ResolvedAiConfig {
        ResolvedAiConfig {
            provider: AiProvider::OpenAi,
            provider_name: AiProvider::OpenAi.label().to_string(),
            api_style: AiApiStyle::OpenAi,
            enabled: false,
            model: String::new(),
            api_key: String::new(),
            base_url: AiProvider::OpenAi.default_base_url().to_string(),
            base_url_overridden: false,
            thinking_level: AiThinkingLevel::Off,
        }
    }
}
