use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiApiStyle {
    OpenAi,
    OpenAiResponses,
    Anthropic,
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
        match provider.api_style() {
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
        let supported = Self::supported_for(provider, model);
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
        self != Self::Off
            && match provider.api_style() {
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
    #[default]
    OpenAi,
    ChatGptSubscription,
    Anthropic,
    DeepSeek,
    OpenRouter,
    Fireworks,
    Groq,
    Together,
    Mistral,
    Perplexity,
    Moonshot,
    DashScope,
    Zhipu,
    SiliconFlow,
    XAi,
    Hyperbolic,
    Nvidia,
    Cerebras,
    StepFun,
    Yi,
    MiniMax,
    Doubao,
    LmStudio,
    Ollama,
}

impl AiProvider {
    pub const ALL: [Self; 24] = [
        Self::OpenAi,
        Self::ChatGptSubscription,
        Self::Anthropic,
        Self::DeepSeek,
        Self::OpenRouter,
        Self::Fireworks,
        Self::Groq,
        Self::Together,
        Self::Mistral,
        Self::Perplexity,
        Self::Moonshot,
        Self::DashScope,
        Self::Zhipu,
        Self::SiliconFlow,
        Self::XAi,
        Self::Hyperbolic,
        Self::Nvidia,
        Self::Cerebras,
        Self::StepFun,
        Self::Yi,
        Self::MiniMax,
        Self::Doubao,
        Self::LmStudio,
        Self::Ollama,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::ChatGptSubscription => "openai-subscribed",
            Self::Anthropic => "anthropic",
            Self::DeepSeek => "deepseek",
            Self::OpenRouter => "openrouter",
            Self::Fireworks => "fireworks",
            Self::Groq => "groq",
            Self::Together => "together",
            Self::Mistral => "mistral",
            Self::Perplexity => "perplexity",
            Self::Moonshot => "moonshot",
            Self::DashScope => "dashscope",
            Self::Zhipu => "zhipu",
            Self::SiliconFlow => "silicon",
            Self::XAi => "xai",
            Self::Hyperbolic => "hyperbolic",
            Self::Nvidia => "nvidia",
            Self::Cerebras => "cerebras",
            Self::StepFun => "stepfun",
            Self::Yi => "yi",
            Self::MiniMax => "minimax",
            Self::Doubao => "doubao",
            Self::LmStudio => "lmstudio",
            Self::Ollama => "ollama",
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
            Self::Groq => "Groq",
            Self::Together => "Together",
            Self::Mistral => "Mistral",
            Self::Perplexity => "Perplexity",
            Self::Moonshot => "Moonshot AI",
            Self::DashScope => "Bailian",
            Self::Zhipu => "ZhiPu",
            Self::SiliconFlow => "SiliconFlow",
            Self::XAi => "xAI",
            Self::Hyperbolic => "Hyperbolic",
            Self::Nvidia => "NVIDIA",
            Self::Cerebras => "Cerebras",
            Self::StepFun => "StepFun",
            Self::Yi => "Yi",
            Self::MiniMax => "MiniMax",
            Self::Doubao => "doubao",
            Self::LmStudio => "LM Studio",
            Self::Ollama => "Ollama",
        }
    }

    pub(super) fn from_str(value: &str) -> Option<Self> {
        match value {
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
            "groq" => Some(Self::Groq),
            "together" | "together-ai" | "together_ai" => Some(Self::Together),
            "mistral" | "mistral-ai" | "mistral_ai" => Some(Self::Mistral),
            "perplexity" => Some(Self::Perplexity),
            "moonshot" | "moonshot-ai" | "moonshot_ai" => Some(Self::Moonshot),
            "dashscope" | "bailian" => Some(Self::DashScope),
            "zhipu" | "zhipu-ai" | "zhipu_ai" => Some(Self::Zhipu),
            "silicon" | "siliconflow" | "silicon-flow" | "silicon_flow" => Some(Self::SiliconFlow),
            "xai" | "x-ai" | "x_ai" | "grok" => Some(Self::XAi),
            "hyperbolic" => Some(Self::Hyperbolic),
            "nvidia" => Some(Self::Nvidia),
            "cerebras" => Some(Self::Cerebras),
            "stepfun" | "step-fun" | "step_fun" => Some(Self::StepFun),
            "yi" => Some(Self::Yi),
            "minimax" | "mini-max" | "mini_max" => Some(Self::MiniMax),
            "doubao" => Some(Self::Doubao),
            "lmstudio" | "lm-studio" | "lm_studio" => Some(Self::LmStudio),
            "ollama" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-4o",
            Self::ChatGptSubscription => "gpt-5.5",
            Self::Anthropic => "claude-3-5-sonnet-latest",
            Self::DeepSeek => "deepseek-chat",
            Self::OpenRouter => "openai/gpt-4o",
            Self::Fireworks => "accounts/fireworks/models/llama-v3p1-70b-instruct",
            Self::Groq => "llama-3.3-70b-versatile",
            Self::Together => "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            Self::Mistral => "mistral-large-latest",
            Self::Perplexity => "sonar-pro",
            Self::Moonshot => "moonshot-v1-8k",
            Self::DashScope => "qwen-plus",
            Self::Zhipu => "glm-4-plus",
            Self::SiliconFlow => "deepseek-ai/DeepSeek-V3",
            Self::XAi => "grok-2-latest",
            Self::Hyperbolic => "meta-llama/Meta-Llama-3.1-70B-Instruct",
            Self::Nvidia => "meta/llama-3.1-70b-instruct",
            Self::Cerebras => "llama-3.3-70b",
            Self::StepFun => "step-2-16k",
            Self::Yi => "yi-large",
            Self::MiniMax => "MiniMax-Text-01",
            Self::Doubao => "doubao-1-5-pro-32k-250115",
            Self::LmStudio => "local-model",
            Self::Ollama => "llama3.1",
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
            Self::Groq => "llama-3.1-8b-instant",
            Self::Together => "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
            Self::Mistral => "mistral-small-latest",
            Self::Perplexity => "sonar",
            Self::Moonshot => "moonshot-v1-8k",
            Self::DashScope => "qwen-turbo",
            Self::Zhipu => "glm-4-flash",
            Self::SiliconFlow => "Qwen/Qwen2.5-7B-Instruct",
            Self::XAi => "grok-2-latest",
            Self::Hyperbolic => "meta-llama/Meta-Llama-3.1-8B-Instruct",
            Self::Nvidia => "meta/llama-3.1-8b-instruct",
            Self::Cerebras => "llama3.1-8b",
            Self::StepFun => "step-1-8k",
            Self::Yi => "yi-lightning",
            Self::MiniMax => "MiniMax-Text-01",
            Self::Doubao => "doubao-1-5-lite-32k-250115",
            Self::LmStudio => "local-model",
            Self::Ollama => "llama3.1",
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
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::Together => "https://api.together.xyz/v1",
            Self::Mistral => "https://api.mistral.ai/v1",
            Self::Perplexity => "https://api.perplexity.ai",
            Self::Moonshot => "https://api.moonshot.cn/v1",
            Self::DashScope => "https://dashscope.aliyuncs.com/compatible-mode/v1",
            Self::Zhipu => "https://open.bigmodel.cn/api/paas/v4",
            Self::SiliconFlow => "https://api.siliconflow.cn/v1",
            Self::XAi => "https://api.x.ai/v1",
            Self::Hyperbolic => "https://api.hyperbolic.xyz/v1",
            Self::Nvidia => "https://integrate.api.nvidia.com/v1",
            Self::Cerebras => "https://api.cerebras.ai/v1",
            Self::StepFun => "https://api.stepfun.com/v1",
            Self::Yi => "https://api.lingyiwanwu.com/v1",
            Self::MiniMax => "https://api.minimaxi.com/v1",
            Self::Doubao => "https://ark.cn-beijing.volces.com/api/v3",
            Self::LmStudio => "http://localhost:1234/v1",
            Self::Ollama => "http://localhost:11434/v1",
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
            | Self::Groq
            | Self::Together
            | Self::Mistral
            | Self::Perplexity
            | Self::Moonshot
            | Self::DashScope
            | Self::Zhipu
            | Self::SiliconFlow
            | Self::XAi
            | Self::Hyperbolic
            | Self::Nvidia
            | Self::Cerebras
            | Self::StepFun
            | Self::Yi
            | Self::MiniMax
            | Self::Doubao
            | Self::LmStudio
            | Self::Ollama => AiApiStyle::OpenAi,
        }
    }

    pub fn allows_keyless_requests(self) -> bool {
        matches!(self, Self::LmStudio | Self::Ollama)
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
            Self::Groq => &[
                (
                    "llama-3.3-70b-versatile",
                    "Llama 3.3 70B Versatile",
                    "Llama",
                ),
                ("llama-3.1-8b-instant", "Llama 3.1 8B Instant", "Llama"),
                ("mixtral-8x7b-32768", "Mixtral 8x7B", "Mixtral"),
            ],
            Self::Together => &[
                (
                    "meta-llama/Llama-3.3-70B-Instruct-Turbo",
                    "Llama 3.3 70B Instruct Turbo",
                    "Llama",
                ),
                (
                    "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
                    "Llama 3.1 8B Instruct Turbo",
                    "Llama",
                ),
            ],
            Self::Mistral => &[
                ("mistral-large-latest", "Mistral Large", "Mistral"),
                ("mistral-small-latest", "Mistral Small", "Mistral"),
            ],
            Self::Perplexity => &[
                ("sonar-pro", "Sonar Pro", "Sonar"),
                ("sonar", "Sonar", "Sonar"),
            ],
            Self::Moonshot => &[("moonshot-v1-8k", "Moonshot v1 8K", "Moonshot")],
            Self::DashScope => &[
                ("qwen-plus", "Qwen Plus", "Qwen"),
                ("qwen-turbo", "Qwen Turbo", "Qwen"),
            ],
            Self::Zhipu => &[
                ("glm-4-plus", "GLM-4 Plus", "GLM"),
                ("glm-4-flash", "GLM-4 Flash", "GLM"),
            ],
            Self::SiliconFlow => &[
                ("deepseek-ai/DeepSeek-V3", "DeepSeek V3", "DeepSeek"),
                ("Qwen/Qwen2.5-7B-Instruct", "Qwen2.5 7B Instruct", "Qwen"),
            ],
            Self::XAi => &[("grok-2-latest", "Grok 2", "Grok")],
            Self::Hyperbolic => &[
                (
                    "meta-llama/Meta-Llama-3.1-70B-Instruct",
                    "Llama 3.1 70B Instruct",
                    "Llama",
                ),
                (
                    "meta-llama/Meta-Llama-3.1-8B-Instruct",
                    "Llama 3.1 8B Instruct",
                    "Llama",
                ),
            ],
            Self::Nvidia => &[
                (
                    "meta/llama-3.1-70b-instruct",
                    "Llama 3.1 70B Instruct",
                    "Llama",
                ),
                (
                    "meta/llama-3.1-8b-instruct",
                    "Llama 3.1 8B Instruct",
                    "Llama",
                ),
            ],
            Self::Cerebras => &[
                ("llama-3.3-70b", "Llama 3.3 70B", "Llama"),
                ("llama3.1-8b", "Llama 3.1 8B", "Llama"),
            ],
            Self::StepFun => &[
                ("step-2-16k", "Step 2 16K", "Step"),
                ("step-1-8k", "Step 1 8K", "Step"),
            ],
            Self::Yi => &[
                ("yi-large", "Yi Large", "Yi"),
                ("yi-lightning", "Yi Lightning", "Yi"),
            ],
            Self::MiniMax => &[("MiniMax-Text-01", "MiniMax Text 01", "MiniMax")],
            Self::Doubao => &[
                ("doubao-1-5-pro-32k-250115", "Doubao 1.5 Pro 32K", "Doubao"),
                (
                    "doubao-1-5-lite-32k-250115",
                    "Doubao 1.5 Lite 32K",
                    "Doubao",
                ),
            ],
            Self::LmStudio => &[("local-model", "Local model", "Local")],
            Self::Ollama => &[("llama3.1", "Llama 3.1", "Local")],
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
            enabled: self == Self::OpenAi,
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
    pub enabled: bool,
    pub api_key: String,
    pub base_url: Option<String>,
    pub models: Vec<AiModel>,
}

impl AiProviderConfig {
    pub(super) fn sanitize(&mut self, provider: AiProvider) {
        if provider == AiProvider::ChatGptSubscription {
            self.api_key.clear();
            self.base_url = None;
            self.models = provider.default_models();
            return;
        }
        self.api_key = self.api_key.trim().to_string();
        self.base_url = self
            .base_url
            .take()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .filter(|value| value != provider.default_base_url());

        let mut models = Vec::new();
        for mut model in self.models.drain(..) {
            model.sanitize(provider);
            if model.id.is_empty() || models.iter().any(|entry: &AiModel| entry.id == model.id) {
                continue;
            }
            models.push(model);
        }
        self.models = models;
    }

    pub(super) fn from_document(provider: AiProvider, document: &KdlDocument) -> Self {
        let mut config = provider.default_config();
        if let Some(value) = document.get_arg("enabled").and_then(KdlValue::as_bool) {
            config.enabled = value;
        }
        if let Some(value) = document.get_arg("api_key").and_then(KdlValue::as_string) {
            config.api_key = value.to_string();
        }
        config.base_url = document
            .get_arg("base_url")
            .and_then(KdlValue::as_string)
            .map(str::to_string);
        if let Some(models) = document.get("models").and_then(KdlNode::children) {
            config.models = models
                .nodes()
                .iter()
                .filter_map(|node| AiModel::from_node(provider, node))
                .collect();
        }
        if let Some(value) = document.get_arg("model").and_then(KdlValue::as_string) {
            config.ensure_model(AiModel::new(value, value, provider.label()));
        }
        config.sanitize(provider);
        config
    }

    pub(super) fn write_to_document(&self, document: &mut KdlDocument) {
        push_value_node(document, "enabled", self.enabled);
        if !self.api_key.is_empty() {
            push_value_node(document, "api_key", self.api_key.clone());
        }
        if let Some(base_url) = &self.base_url {
            push_value_node(document, "base_url", base_url.clone());
        }
        let mut models = KdlNode::new("models");
        let models_children = models.ensure_children();
        for model in &self.models {
            model.write_to_document(models_children);
        }
        document.nodes_mut().push(models);
    }

    pub(super) fn is_default_for(&self, provider: AiProvider) -> bool {
        self == &provider.default_config()
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAiConfig {
    pub provider: AiProvider,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub base_url_overridden: bool,
    pub thinking_level: AiThinkingLevel,
}

impl ResolvedAiConfig {
    pub fn can_request(&self) -> bool {
        match self.provider.api_style() {
            AiApiStyle::OpenAi => {
                !self.api_key.is_empty()
                    || self.base_url_overridden
                    || self.provider.allows_keyless_requests()
            }
            AiApiStyle::OpenAiResponses => true,
            AiApiStyle::Anthropic => !self.api_key.is_empty(),
        }
    }

    pub fn missing_credentials_message(&self) -> String {
        match self.provider.api_style() {
            AiApiStyle::OpenAi => format!(
                "missing {} API key, or set a custom base URL for an OpenAI-compatible server",
                self.provider.label()
            ),
            AiApiStyle::OpenAiResponses => {
                "sign in to ChatGPT Subscription in Settings > AI before making requests"
                    .to_string()
            }
            AiApiStyle::Anthropic => format!("missing {} API key", self.provider.label()),
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
            providers: BTreeMap::from([(provider, provider.default_config())]),
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

        for (provider, config) in self.providers.iter_mut() {
            config.sanitize(*provider);
        }
        sanitize_fixed_provider_model(&mut self.main_model, false);
        sanitize_fixed_provider_model(&mut self.title_model, true);
        sanitize_fixed_provider_model(&mut self.translation_model, false);

        if self.main_model.is_none() {
            self.main_model = self.first_model_ref(false);
        }
        if self.title_model.is_none() {
            self.title_model = self.first_model_ref(true);
        }
        if self.translation_model.is_none() {
            self.translation_model = self
                .main_model
                .clone()
                .filter(|model_ref| self.provider_config(model_ref.provider).enabled)
                .or_else(|| self.first_model_ref(false));
        }

        let refs = [
            self.main_model.clone(),
            self.title_model.clone(),
            self.translation_model.clone(),
        ];
        for mut model_ref in refs.into_iter().flatten() {
            model_ref.sanitize();
            if model_ref.id.is_empty() {
                continue;
            }
            let config = self.provider_config_mut(model_ref.provider);
            if config.enabled {
                config.ensure_model(AiModel::new(
                    model_ref.id.clone(),
                    model_ref.id.clone(),
                    model_ref.provider.label(),
                ));
            }
        }

        if self
            .main_model
            .as_ref()
            .is_none_or(|model_ref| !self.provider_config(model_ref.provider).enabled)
        {
            self.main_model = self.first_model_ref(false);
        }
        if self
            .title_model
            .as_ref()
            .is_none_or(|model_ref| !self.provider_config(model_ref.provider).enabled)
        {
            self.title_model = self.first_model_ref(true);
        }
        if self
            .translation_model
            .as_ref()
            .is_none_or(|model_ref| !self.provider_config(model_ref.provider).enabled)
        {
            self.translation_model = self
                .main_model
                .clone()
                .filter(|model_ref| self.provider_config(model_ref.provider).enabled)
                .or_else(|| self.first_model_ref(false));
        }

        if let Some(model_ref) = self.main_model.as_mut() {
            model_ref.sanitize();
        }
        if let Some(model_ref) = self.main_model.as_ref() {
            let config = self.provider_config(model_ref.provider);
            self.thinking_level = self
                .thinking_level
                .clamp_for(model_ref.provider, &config.model_name(&model_ref.id));
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

    pub fn resolved_main(&self) -> ResolvedAiConfig {
        let Some(model_ref) = self
            .main_model
            .clone()
            .filter(|model_ref| self.provider_config(model_ref.provider).enabled)
            .or_else(|| self.first_model_ref(false))
        else {
            return ResolvedAiConfig {
                provider: AiProvider::OpenAi,
                model: AiProvider::OpenAi.default_model().to_string(),
                api_key: String::new(),
                base_url: AiProvider::OpenAi.default_base_url().to_string(),
                base_url_overridden: false,
                thinking_level: AiThinkingLevel::Off,
            };
        };
        let thinking_level = self
            .thinking_level
            .clamp_for(model_ref.provider, &model_ref.id);
        self.resolved_for(model_ref.provider, model_ref.id, thinking_level)
    }

    pub fn resolved_title(&self) -> Option<ResolvedAiConfig> {
        let title_model = self
            .title_model
            .clone()
            .filter(|model_ref| self.provider_config(model_ref.provider).enabled)?;
        let resolved =
            self.resolved_for(title_model.provider, title_model.id, AiThinkingLevel::Off);
        resolved.can_request().then_some(resolved)
    }

    pub fn resolved_translation(&self) -> ResolvedAiConfig {
        let Some(model_ref) = self
            .translation_model
            .clone()
            .filter(|model_ref| self.provider_config(model_ref.provider).enabled)
            .or_else(|| {
                self.main_model
                    .clone()
                    .filter(|model_ref| self.provider_config(model_ref.provider).enabled)
            })
            .or_else(|| self.first_model_ref(false))
        else {
            return ResolvedAiConfig {
                provider: AiProvider::OpenAi,
                model: AiProvider::OpenAi.default_model().to_string(),
                api_key: String::new(),
                base_url: AiProvider::OpenAi.default_base_url().to_string(),
                base_url_overridden: false,
                thinking_level: AiThinkingLevel::Off,
            };
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
        let thinking_level = thinking_level.clamp_for(provider, &model);
        let base_url_overridden = config.base_url.is_some();
        ResolvedAiConfig {
            provider,
            model,
            api_key: config.api_key,
            base_url: config
                .base_url
                .unwrap_or_else(|| provider.default_base_url().to_string()),
            base_url_overridden,
            thinking_level,
        }
    }

    fn first_model_ref(&self, title: bool) -> Option<ModelRef> {
        for provider in AiProvider::ALL {
            let config = self.provider_config(provider);
            if !config.enabled || config.models.is_empty() {
                continue;
            }
            let preferred = if title {
                provider.default_title_model()
            } else {
                provider.default_model()
            };
            let model = config
                .models
                .iter()
                .find(|model| model.id == preferred)
                .or_else(|| config.models.first())?;
            return Some(ModelRef::new(provider, model.id.clone()));
        }
        None
    }
}
