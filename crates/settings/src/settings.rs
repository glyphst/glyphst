use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result};
use gpui::{App, Font, FontFallbacks, FontWeight, Global, Window, WindowBackgroundAppearance};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::theme::{
    DEFAULT_DARK_THEME_ID, DEFAULT_LIGHT_THEME_ID, DEFAULT_THEME_ID, PartialTheme, Theme,
    ThemeAppearance, ThemeRegistry, ThemeSelection, push_value_node, value_to_f32,
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const DEFAULT_ZOOM: f32 = 1.0;
pub const DEFAULT_ZOOM_STEP: f32 = 1.1;
pub const DEFAULT_SMOOTH_SCROLLING: bool = true;
pub const DEFAULT_UI_FONT_FAMILY: &str = ".ZedSans";
pub const DEFAULT_UI_FONT_SIZE: f32 = 13.0;
pub const DEFAULT_UI_FONT_WEIGHT: f32 = 400.0;

const MIN_ZOOM_SETTING: f32 = 0.25;
const MAX_ZOOM_SETTING: f32 = 8.0;
const MIN_ZOOM_STEP: f32 = 1.01;
const MAX_ZOOM_STEP: f32 = 4.0;
const MIN_UI_FONT_SIZE: f32 = 6.0;
const MAX_UI_FONT_SIZE: f32 = 100.0;
const MIN_UI_FONT_WEIGHT: f32 = 100.0;
const MAX_UI_FONT_WEIGHT: f32 = 900.0;
const DEFAULT_WINDOW_OPACITY: f32 = 1.0;
const MIN_WINDOW_OPACITY: f32 = 0.1;
const MAX_WINDOW_OPACITY: f32 = 1.0;
const DEFAULT_QUICK_ASK_TRANSPARENCY: f32 = 23.0 / 255.0;
const DEFAULT_QUICK_ASK_TEXT_SIZE: f32 = 13.0;
const MIN_QUICK_ASK_TRANSPARENCY: f32 = 0.0;
const MAX_QUICK_ASK_TRANSPARENCY: f32 = 1.0;
const MIN_QUICK_ASK_TEXT_SIZE: f32 = 8.0;
const MAX_QUICK_ASK_TEXT_SIZE: f32 = 32.0;
const DEFAULT_UI_FONT_FALLBACKS: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
];

pub fn ui_rems(px: f32) -> gpui::Rems {
    gpui::rems(px / DEFAULT_UI_FONT_SIZE)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BehaviorSettings {
    pub default_zoom: f32,
    pub zoom_step: f32,
    pub smooth_scrolling: bool,
    pub dim_inactive_panes: bool,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            default_zoom: DEFAULT_ZOOM,
            zoom_step: DEFAULT_ZOOM_STEP,
            smooth_scrolling: DEFAULT_SMOOTH_SCROLLING,
            dim_inactive_panes: false,
        }
    }
}

impl BehaviorSettings {
    pub fn sanitize(&mut self) {
        self.default_zoom = self.default_zoom.clamp(MIN_ZOOM_SETTING, MAX_ZOOM_SETTING);
        self.zoom_step = self.zoom_step.clamp(MIN_ZOOM_STEP, MAX_ZOOM_STEP);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontSettings {
    pub ui_font_family: String,
    pub ui_font_fallbacks: Vec<String>,
    pub ui_font_size: f32,
    pub ui_font_weight: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            ui_font_family: DEFAULT_UI_FONT_FAMILY.to_string(),
            ui_font_fallbacks: DEFAULT_UI_FONT_FALLBACKS
                .iter()
                .map(|fallback| fallback.to_string())
                .collect(),
            ui_font_size: DEFAULT_UI_FONT_SIZE,
            ui_font_weight: DEFAULT_UI_FONT_WEIGHT,
        }
    }
}

impl FontSettings {
    pub fn sanitize(&mut self) {
        self.ui_font_family = self.ui_font_family.trim().to_string();
        if self.ui_font_family.is_empty() {
            self.ui_font_family = DEFAULT_UI_FONT_FAMILY.to_string();
        }

        let mut fallbacks = Vec::new();
        for fallback in self.ui_font_fallbacks.drain(..) {
            let fallback = fallback.trim().to_string();
            if fallback.is_empty() || fallbacks.iter().any(|entry| entry == &fallback) {
                continue;
            }
            fallbacks.push(fallback);
        }
        self.ui_font_fallbacks = fallbacks;

        self.ui_font_size = if self.ui_font_size.is_finite() {
            self.ui_font_size.clamp(MIN_UI_FONT_SIZE, MAX_UI_FONT_SIZE)
        } else {
            DEFAULT_UI_FONT_SIZE
        };
        self.ui_font_weight = if self.ui_font_weight.is_finite() {
            self.ui_font_weight
                .clamp(MIN_UI_FONT_WEIGHT, MAX_UI_FONT_WEIGHT)
        } else {
            DEFAULT_UI_FONT_WEIGHT
        };
    }

    pub fn ui_font(&self) -> Font {
        Font {
            family: self.ui_font_family.clone().into(),
            features: Default::default(),
            fallbacks: (!self.ui_font_fallbacks.is_empty())
                .then(|| FontFallbacks::from_fonts(self.ui_font_fallbacks.clone())),
            weight: FontWeight(self.ui_font_weight),
            style: Default::default(),
        }
    }
}

pub fn setup_ui_font(window: &mut Window, cx: &mut App) -> Font {
    let font_settings = cx.global::<SettingsStore>().settings.font.clone();
    window.set_rem_size(gpui::px(font_settings.ui_font_size));
    font_settings.ui_font()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowBackgroundMode {
    Opaque,
    Transparent,
    #[default]
    Blurred,
}

impl WindowBackgroundMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
            Self::Blurred => "blurred",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "opaque" => Some(Self::Opaque),
            "transparent" => Some(Self::Transparent),
            "blurred" | "blur" => Some(Self::Blurred),
            _ => None,
        }
    }

    fn into_gpui(self) -> WindowBackgroundAppearance {
        match self {
            Self::Opaque => WindowBackgroundAppearance::Opaque,
            Self::Transparent => WindowBackgroundAppearance::Transparent,
            Self::Blurred => WindowBackgroundAppearance::Blurred,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowSettings {
    pub opacity: f32,
    pub background: WindowBackgroundMode,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            opacity: DEFAULT_WINDOW_OPACITY,
            background: WindowBackgroundMode::Blurred,
        }
    }
}

impl WindowSettings {
    pub fn sanitize(&mut self) {
        self.opacity = self.opacity.clamp(MIN_WINDOW_OPACITY, MAX_WINDOW_OPACITY);
    }

    pub fn transparency_enabled_by_default(self) -> bool {
        self.opacity < MAX_WINDOW_OPACITY && self.background != WindowBackgroundMode::Opaque
    }

    pub fn effective_opacity(self, enabled: bool) -> f32 {
        if enabled && self.transparency_enabled_by_default() {
            self.opacity
        } else {
            MAX_WINDOW_OPACITY
        }
    }

    pub fn background_appearance(self, enabled: bool) -> WindowBackgroundAppearance {
        if enabled && self.transparency_enabled_by_default() {
            self.background.into_gpui()
        } else {
            WindowBackgroundAppearance::Opaque
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabBarVisibility {
    #[default]
    Auto,
    Always,
}

impl TabBarVisibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "auto" | "multiple" => Some(Self::Auto),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppLanguage {
    #[default]
    System,
    English,
    SimplifiedChinese,
}

impl AppLanguage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::System => crate::fl!("language-system"),
            Self::English => crate::fl!("language-english"),
            Self::SimplifiedChinese => crate::fl!("language-simplified-chinese"),
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "system" | "auto" => Some(Self::System),
            "en" | "en-US" | "en-us" => Some(Self::English),
            "zh-CN" | "zh-cn" | "zh-Hans" | "zh-hans" => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapPreset {
    #[default]
    Modern,
    Vim,
}

impl KeymapPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Modern => "modern",
            Self::Vim => "vim",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Modern => crate::fl!("keymap-preset-modern"),
            Self::Vim => crate::fl!("keymap-preset-vim"),
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "modern" => Some(Self::Modern),
            "vim" | "vim-like" | "vim_like" => Some(Self::Vim),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingOverride {
    pub action: String,
    pub keystrokes: String,
    pub context: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiApiStyle {
    OpenAi,
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

    fn from_str(value: &str) -> Option<Self> {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AiProvider {
    #[default]
    OpenAi,
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
    pub const ALL: [Self; 23] = [
        Self::OpenAi,
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

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "openai" | "open-ai" | "open_ai" => Some(Self::OpenAi),
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

    fn sanitize(&mut self, provider: AiProvider) {
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

    fn from_node(provider: AiProvider, node: &KdlNode) -> Option<Self> {
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

    fn write_to_document(&self, document: &mut KdlDocument) {
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

    fn sanitize(&mut self) {
        self.id = self.id.trim().to_string();
    }

    fn from_node(node: &KdlNode) -> Option<Self> {
        let provider = node.get(0).and_then(KdlValue::as_string)?;
        let model = node.get(1).and_then(KdlValue::as_string)?;
        Some(Self::new(AiProvider::from_str(provider)?, model))
    }

    fn write_to_document(&self, document: &mut KdlDocument, name: &str) {
        let mut node = KdlNode::new(name);
        node.push(self.provider.as_str());
        node.push(self.id.clone());
        document.nodes_mut().push(node);
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
    fn sanitize(&mut self, provider: AiProvider) {
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

    fn from_document(provider: AiProvider, document: &KdlDocument) -> Self {
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

    fn write_to_document(&self, document: &mut KdlDocument) {
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

    fn is_default_for(&self, provider: AiProvider) -> bool {
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
            AiApiStyle::Anthropic => !self.api_key.is_empty(),
        }
    }

    pub fn missing_credentials_message(&self) -> String {
        match self.provider.api_style() {
            AiApiStyle::OpenAi => format!(
                "missing {} API key, or set a custom base URL for an OpenAI-compatible server",
                self.provider.label()
            ),
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
    fn sanitize(&mut self) {
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
    pub thinking_level: AiThinkingLevel,
    pub quick_ask: QuickAskSettings,
}

impl Default for AiSettings {
    fn default() -> Self {
        let provider = AiProvider::OpenAi;
        Self {
            providers: BTreeMap::from([(provider, provider.default_config())]),
            main_model: Some(ModelRef::new(provider, provider.default_model())),
            title_model: Some(ModelRef::new(provider, provider.default_title_model())),
            thinking_level: AiThinkingLevel::Medium,
            quick_ask: QuickAskSettings::default(),
        }
    }
}

impl AiSettings {
    fn sanitize(&mut self) {
        self.quick_ask.sanitize();

        for (provider, config) in self.providers.iter_mut() {
            config.sanitize(*provider);
        }

        if self.main_model.is_none() {
            self.main_model = self.first_model_ref(false);
        }
        if self.title_model.is_none() {
            self.title_model = self.first_model_ref(true);
        }

        let refs = [self.main_model.clone(), self.title_model.clone()];
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub behavior: BehaviorSettings,
    pub font: FontSettings,
    pub window: WindowSettings,
    pub tab_bar_visibility: TabBarVisibility,
    pub language: AppLanguage,
    pub ai: AiSettings,
    pub keymap_preset: KeymapPreset,
    pub keybinding_overrides: Vec<KeyBindingOverride>,
    pub theme: ThemeSelection,
    pub overrides: PartialTheme,
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.behavior.sanitize();
        self.font.sanitize();
        self.window.sanitize();
        self.ai.sanitize();
        self.overrides.sanitize();
        self.theme.sanitize();
        self.keybinding_overrides.retain_mut(|binding| {
            binding.action = binding.action.trim().to_string();
            binding.keystrokes = binding.keystrokes.trim().to_string();
            binding.context = binding
                .context
                .as_deref()
                .map(str::trim)
                .filter(|context| !context.is_empty())
                .map(str::to_string);
            !binding.action.is_empty()
        });
    }

    fn from_document(document: &KdlDocument) -> Self {
        let mut settings = Self::default();

        if let Some(theme_node) = document.get("theme") {
            settings.theme = ThemeSelection::from_kdl_node(theme_node);
        }

        if let Some(language) = document
            .get_arg("language")
            .and_then(KdlValue::as_string)
            .and_then(AppLanguage::from_str)
        {
            settings.language = language;
        }

        if let Some(preset) = document
            .get_arg("keymap")
            .and_then(KdlValue::as_string)
            .and_then(KeymapPreset::from_str)
        {
            settings.keymap_preset = preset;
        }

        if let Some(font) = document.get("font").and_then(KdlNode::children) {
            if let Some(value) = font.get_arg("ui_font_family").and_then(KdlValue::as_string) {
                settings.font.ui_font_family = value.to_string();
            }
            if let Some(value) = font.get_arg("ui_font_size").and_then(value_to_f32) {
                settings.font.ui_font_size = value;
            }
            if let Some(value) = font.get_arg("ui_font_weight").and_then(value_to_f32) {
                settings.font.ui_font_weight = value;
            }
            if let Some(fallbacks) = font.get("ui_font_fallbacks") {
                settings.font.ui_font_fallbacks = fallbacks
                    .entries()
                    .iter()
                    .filter(|entry| entry.name().is_none())
                    .filter_map(|entry| entry.value().as_string().map(str::to_string))
                    .collect();
            }
        }

        if let Some(zoom) = document.get("zoom").and_then(KdlNode::children) {
            if let Some(value) = zoom.get_arg("default").and_then(value_to_f32) {
                settings.behavior.default_zoom = value;
            }
            if let Some(value) = zoom.get_arg("step").and_then(value_to_f32) {
                settings.behavior.zoom_step = value;
            }
        }

        if let Some(scroll) = document.get("scroll").and_then(KdlNode::children)
            && let Some(value) = scroll.get_arg("smooth").and_then(KdlValue::as_bool)
        {
            settings.behavior.smooth_scrolling = value;
        }

        if let Some(panes) = document.get("panes").and_then(KdlNode::children)
            && let Some(value) = panes.get_arg("dim_inactive").and_then(KdlValue::as_bool)
        {
            settings.behavior.dim_inactive_panes = value;
        }

        if let Some(window) = document.get("window").and_then(KdlNode::children) {
            if let Some(value) = window.get_arg("opacity").and_then(value_to_f32) {
                settings.window.opacity = value;
            }
            if let Some(value) = window
                .get_arg("appearance")
                .and_then(KdlValue::as_string)
                .and_then(WindowBackgroundMode::from_str)
            {
                settings.window.background = value;
            }
        }

        if let Some(tabs) = document.get("tabs").and_then(KdlNode::children)
            && let Some(value) = tabs
                .get_arg("visibility")
                .and_then(KdlValue::as_string)
                .and_then(TabBarVisibility::from_str)
        {
            settings.tab_bar_visibility = value;
        }

        if let Some(ai) = document.get("ai").and_then(KdlNode::children) {
            let mut legacy_provider = AiProvider::OpenAi;
            let mut legacy_provider_set = false;
            if let Some(value) = ai
                .get_arg("provider")
                .and_then(KdlValue::as_string)
                .and_then(AiProvider::from_str)
            {
                legacy_provider = value;
                legacy_provider_set = true;
                settings.ai.main_model = Some(ModelRef::new(value, value.default_model()));
            }

            if let Some(value) = ai
                .get_arg("thinking")
                .and_then(KdlValue::as_string)
                .and_then(AiThinkingLevel::from_str)
            {
                settings.ai.thinking_level = value;
            }

            if let Some(quick_ask) = ai.get("quick_ask").and_then(KdlNode::children) {
                if let Some(value) = quick_ask.get_arg("transparency").and_then(value_to_f32) {
                    settings.ai.quick_ask.transparency = value;
                }
                if let Some(value) = quick_ask.get_arg("text_size").and_then(value_to_f32) {
                    settings.ai.quick_ask.text_size = value;
                }
            }

            if let Some(providers) = ai.get("providers").and_then(KdlNode::children) {
                for node in providers.nodes() {
                    let Some(provider) = AiProvider::from_str(node.name().value()) else {
                        continue;
                    };
                    let Some(children) = node.children() else {
                        continue;
                    };
                    settings.ai.providers.insert(
                        provider,
                        AiProviderConfig::from_document(provider, children),
                    );
                }
            }
            if legacy_provider_set {
                settings.ai.provider_config_mut(legacy_provider).enabled = true;
            }

            if let Some(model_ref) = ai.get("main_model").and_then(ModelRef::from_node) {
                settings.ai.main_model = Some(model_ref);
            }

            if ai.get("model").is_some()
                || ai.get("api_key").is_some()
                || ai.get("base_url").is_some()
            {
                let mut config = settings.ai.provider_config(legacy_provider);
                if let Some(value) = ai.get_arg("model").and_then(KdlValue::as_string) {
                    config.ensure_model(AiModel::new(value, value, legacy_provider.label()));
                    settings.ai.main_model = Some(ModelRef::new(legacy_provider, value));
                }
                if let Some(value) = ai.get_arg("api_key").and_then(KdlValue::as_string) {
                    config.api_key = value.to_string();
                }
                config.base_url = ai
                    .get_arg("base_url")
                    .and_then(KdlValue::as_string)
                    .map(str::to_string);
                config.enabled = true;
                config.sanitize(legacy_provider);
                settings.ai.providers.insert(legacy_provider, config);
            }

            if let Some(title_model) = ai.get("title_model") {
                if let Some(model_ref) = ModelRef::from_node(title_model) {
                    settings.ai.title_model = Some(model_ref);
                } else if let Some(title_model) = title_model.children() {
                    let mut provider = AiProvider::OpenAi;
                    if let Some(value) = title_model
                        .get_arg("provider")
                        .and_then(KdlValue::as_string)
                        .and_then(AiProvider::from_str)
                    {
                        provider = value;
                    }
                    let model = title_model
                        .get_arg("model")
                        .and_then(KdlValue::as_string)
                        .unwrap_or_else(|| provider.default_title_model())
                        .to_string();
                    let mut config = settings.ai.provider_config(provider);
                    if let Some(value) =
                        title_model.get_arg("api_key").and_then(KdlValue::as_string)
                        && config.api_key.is_empty()
                    {
                        config.api_key = value.to_string();
                    }
                    if let Some(value) = title_model
                        .get_arg("base_url")
                        .and_then(KdlValue::as_string)
                        && config.base_url.is_none()
                    {
                        config.base_url = Some(value.to_string());
                    }
                    config.enabled = true;
                    config.ensure_model(AiModel::new(
                        model.clone(),
                        model.clone(),
                        provider.label(),
                    ));
                    config.sanitize(provider);
                    settings.ai.providers.insert(provider, config);
                    settings.ai.title_model = Some(ModelRef::new(provider, model));
                }
            }
        }

        if let Some(overrides) = document.get("theme-overrides").and_then(KdlNode::children) {
            settings.overrides = PartialTheme::from_document(overrides);
        }

        if let Some(keybindings) = document.get("keybindings").and_then(KdlNode::children) {
            settings.keybinding_overrides = keybindings
                .nodes()
                .iter()
                .filter_map(|node| {
                    let keystrokes = node
                        .get("keys")
                        .and_then(KdlValue::as_string)
                        .or_else(|| node.get(0).and_then(KdlValue::as_string))?;
                    Some(KeyBindingOverride {
                        action: node.name().value().to_string(),
                        keystrokes: keystrokes.to_string(),
                        context: node
                            .get("context")
                            .and_then(KdlValue::as_string)
                            .map(str::to_string),
                    })
                })
                .collect();
        }

        settings.sanitize();
        settings
    }

    pub fn to_kdl_string(&self) -> String {
        let mut document = KdlDocument::new();

        let mut zoom = KdlNode::new("zoom");
        let zoom_children = zoom.ensure_children();
        push_value_node(zoom_children, "default", self.behavior.default_zoom as f64);
        push_value_node(zoom_children, "step", self.behavior.zoom_step as f64);
        document.nodes_mut().push(zoom);

        let mut scroll = KdlNode::new("scroll");
        let scroll_children = scroll.ensure_children();
        push_value_node(scroll_children, "smooth", self.behavior.smooth_scrolling);
        document.nodes_mut().push(scroll);

        let mut panes = KdlNode::new("panes");
        let panes_children = panes.ensure_children();
        push_value_node(
            panes_children,
            "dim_inactive",
            self.behavior.dim_inactive_panes,
        );
        document.nodes_mut().push(panes);

        self.theme.clone().write_to_document(&mut document);
        push_value_node(&mut document, "language", self.language.as_str());
        push_value_node(&mut document, "keymap", self.keymap_preset.as_str());

        let mut font = KdlNode::new("font");
        let font_children = font.ensure_children();
        push_value_node(
            font_children,
            "ui_font_family",
            self.font.ui_font_family.clone(),
        );
        push_value_node(font_children, "ui_font_size", self.font.ui_font_size as f64);
        push_value_node(
            font_children,
            "ui_font_weight",
            self.font.ui_font_weight as f64,
        );
        if !self.font.ui_font_fallbacks.is_empty() {
            let mut fallbacks = KdlNode::new("ui_font_fallbacks");
            for fallback in &self.font.ui_font_fallbacks {
                fallbacks.push(fallback.clone());
            }
            font_children.nodes_mut().push(fallbacks);
        }
        document.nodes_mut().push(font);

        let mut window = KdlNode::new("window");
        let window_children = window.ensure_children();
        push_value_node(window_children, "opacity", self.window.opacity as f64);
        push_value_node(
            window_children,
            "appearance",
            self.window.background.as_str(),
        );
        document.nodes_mut().push(window);

        let mut tabs = KdlNode::new("tabs");
        let tabs_children = tabs.ensure_children();
        push_value_node(
            tabs_children,
            "visibility",
            self.tab_bar_visibility.as_str(),
        );
        document.nodes_mut().push(tabs);

        if self.ai != AiSettings::default() {
            let mut ai = KdlNode::new("ai");
            let ai_children = ai.ensure_children();
            if let Some(main_model) = &self.ai.main_model {
                main_model.write_to_document(ai_children, "main_model");
            }
            if let Some(title_model) = &self.ai.title_model {
                title_model.write_to_document(ai_children, "title_model");
            }
            if self.ai.thinking_level != AiThinkingLevel::default() {
                push_value_node(ai_children, "thinking", self.ai.thinking_level.as_str());
            }
            if self.ai.quick_ask != QuickAskSettings::default() {
                let mut quick_ask = KdlNode::new("quick_ask");
                let quick_ask_children = quick_ask.ensure_children();
                push_value_node(
                    quick_ask_children,
                    "transparency",
                    self.ai.quick_ask.transparency as f64,
                );
                push_value_node(
                    quick_ask_children,
                    "text_size",
                    self.ai.quick_ask.text_size as f64,
                );
                ai_children.nodes_mut().push(quick_ask);
            }

            let mut providers = KdlNode::new("providers");
            let providers_children = providers.ensure_children();
            for provider in AiProvider::ALL {
                let Some(config) = self.ai.providers.get(&provider) else {
                    continue;
                };
                if config.is_default_for(provider) {
                    continue;
                }

                let mut provider_node = KdlNode::new(provider.as_str());
                config.write_to_document(provider_node.ensure_children());
                providers_children.nodes_mut().push(provider_node);
            }
            if !providers_children.nodes().is_empty() {
                ai_children.nodes_mut().push(providers);
            }
            document.nodes_mut().push(ai);
        }

        if !self.keybinding_overrides.is_empty() {
            let mut keybindings = KdlNode::new("keybindings");
            let keybinding_children = keybindings.ensure_children();
            for binding in &self.keybinding_overrides {
                let mut node = KdlNode::new(binding.action.clone());
                node.push(KdlEntry::new_prop("keys", binding.keystrokes.clone()));
                if let Some(context) = binding.context.as_ref() {
                    node.push(KdlEntry::new_prop("context", context.clone()));
                }
                keybinding_children.nodes_mut().push(node);
            }
            document.nodes_mut().push(keybindings);
        }

        if !self.overrides.is_empty() {
            let mut overrides = KdlNode::new("theme-overrides");
            self.overrides
                .write_to_document(overrides.ensure_children());
            document.nodes_mut().push(overrides);
        }

        document.autoformat();
        document.to_string()
    }
}

pub struct SettingsStore {
    pub settings: Settings,
    pub registry: ThemeRegistry,
    pub resolved_theme: Theme,
    pub system_appearance: ThemeAppearance,
    pub user_managed: bool,
    pub config_path: PathBuf,
    pub state_path: PathBuf,
    pub last_dynamic_light: Option<String>,
    pub last_dynamic_dark: Option<String>,
}

impl Global for SettingsStore {}

impl SettingsStore {
    pub fn load() -> Self {
        let config_path = config_path();
        let state_path = state_path();
        let themes_path = themes_path();
        let registry = ThemeRegistry::load(&themes_path);
        let user_managed = config_path.exists();
        let load_path = if user_managed {
            Some(&config_path)
        } else if state_path.exists() {
            Some(&state_path)
        } else {
            None
        };
        let mut settings = load_path
            .map(|path| match load_settings(path) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("failed to load settings from {}: {error:#}", path.display());
                    Settings::default()
                }
            })
            .unwrap_or_default();
        let system_appearance = ThemeAppearance::Light;
        validate_theme_selection(&mut settings, &registry, system_appearance);
        let resolved_theme =
            registry.resolve(settings.theme.name(system_appearance), settings.overrides);

        Self {
            settings,
            registry,
            resolved_theme,
            system_appearance,
            user_managed,
            config_path,
            state_path,
            last_dynamic_light: None,
            last_dynamic_dark: None,
        }
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<()> {
        let mut settings = settings;
        settings.sanitize();
        validate_theme_selection(&mut settings, &self.registry, self.system_appearance);
        let resolved_theme = self.registry.resolve(
            settings.theme.name(self.system_appearance),
            settings.overrides,
        );
        let path = if self.user_managed {
            &self.config_path
        } else {
            &self.state_path
        };

        write_kdl(path, settings.to_kdl_string())?;
        self.settings = settings;
        self.resolved_theme = resolved_theme;
        Ok(())
    }

    pub fn set_system_appearance(&mut self, appearance: ThemeAppearance) {
        if self.system_appearance == appearance {
            return;
        }
        self.system_appearance = appearance;
        self.resolved_theme = self.registry.resolve(
            self.settings.theme.name(appearance),
            self.settings.overrides,
        );
    }

    pub fn save(&mut self) -> Result<()> {
        let path = if self.user_managed {
            &self.config_path
        } else {
            &self.state_path
        };
        write_kdl(path, self.export())
    }

    pub fn override_settings(&mut self, settings: Settings) {
        self.settings = settings;
        self.settings.sanitize();
        validate_theme_selection(&mut self.settings, &self.registry, self.system_appearance);
        self.resolved_theme = self.registry.resolve(
            self.settings.theme.name(self.system_appearance),
            self.settings.overrides,
        );
    }

    pub fn export(&self) -> String {
        self.settings.to_kdl_string()
    }

    pub fn export_to(&self, path: &Path) -> Result<()> {
        write_kdl(path, self.export())
    }
}

fn validate_theme_selection(
    settings: &mut Settings,
    registry: &ThemeRegistry,
    system_appearance: ThemeAppearance,
) {
    match &mut settings.theme {
        ThemeSelection::Static(name) => {
            if !registry.contains(name) {
                *name = DEFAULT_THEME_ID.to_string();
            }
        }
        ThemeSelection::Dynamic { light, dark, .. } => {
            if !registry.contains(light) {
                *light = DEFAULT_LIGHT_THEME_ID.to_string();
            }
            if !registry.contains(dark) {
                *dark = DEFAULT_DARK_THEME_ID.to_string();
            }
        }
    }
    let _ = system_appearance;
}

pub fn parse_settings(text: &str) -> Result<Settings> {
    let document = text.parse::<KdlDocument>()?;
    Ok(Settings::from_document(&document))
}

fn config_path() -> PathBuf {
    crate::app_paths::config_dir().join("config.kdl")
}

fn themes_path() -> PathBuf {
    crate::app_paths::config_dir().join("themes")
}

fn state_path() -> PathBuf {
    crate::app_paths::state_dir().join("config.kdl")
}

fn load_settings(path: &Path) -> Result<Settings> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))
        .and_then(|text| parse_settings(&text))
}

fn write_kdl(path: &Path, contents: String) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_private_directory(parent)?;
    }

    atomic_write_private(path, contents.as_bytes())
}

fn create_private_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};

        let already_exists = path.exists();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        if !already_exists {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("failed to restrict {}", path.display()))?;
        }
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }

    Ok(())
}

fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings");
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.mode(0o600);
    }

    let write_result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary_path)
            .with_context(|| format!("failed to create {}", temporary_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", temporary_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary_path.display()))?;
        fs::rename(&temporary_path, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;

        #[cfg(unix)]
        {
            let directory = fs::File::open(parent)
                .with_context(|| format!("failed to open {}", parent.display()))?;
            directory
                .sync_all()
                .with_context(|| format!("failed to sync {}", parent.display()))?;
        }

        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    write_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeAppearanceMode;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_partial_settings_with_defaults() {
        let settings = parse_settings(
            r##"
theme "one-dark"
zoom {
    default 1.5
}
window {
    opacity 0.85
    appearance "transparent"
}
theme-overrides {
    colors {
        background "#101010"
    }
}
"##,
        )
        .unwrap();

        assert_eq!(settings.behavior.default_zoom, 1.5);
        assert_eq!(settings.behavior.zoom_step, DEFAULT_ZOOM_STEP);
        assert_eq!(settings.behavior.smooth_scrolling, DEFAULT_SMOOTH_SCROLLING);
        assert!(!settings.behavior.dim_inactive_panes);
        assert_eq!(settings.window.opacity, 0.85);
        assert_eq!(
            settings.window.background,
            WindowBackgroundMode::Transparent
        );
        assert_eq!(
            settings.theme,
            ThemeSelection::Static("one-dark".to_string())
        );
        assert_eq!(settings.overrides.background, Some(0x101010));
        assert_eq!(settings.overrides.selection_highlight, None);
    }

    #[test]
    fn clamps_window_opacity() {
        let settings = parse_settings(
            r##"
window {
    opacity 0.1
    appearance "blurred"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.window.opacity, MIN_WINDOW_OPACITY);
        assert_eq!(settings.window.background, WindowBackgroundMode::Blurred);
    }

    #[test]
    fn exported_settings_are_reparseable() {
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                zoom_step: 1.2,
                smooth_scrolling: false,
                dim_inactive_panes: true,
            },
            theme: ThemeSelection::Static("one-dark".to_string()),
            overrides: PartialTheme {
                background: Some(0x181818),
                selection_highlight: Some(0x61afef66),
                page_gap: Some(32.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };

        let exported = settings.clone().to_kdl_string();
        assert_eq!(parse_settings(&exported).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_keymap_settings() {
        let settings = parse_settings(
            r##"
keymap "vim"
keybindings {
    next-page "j"
    jump-backward "ctrl-o"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.keymap_preset, KeymapPreset::Vim);
        assert_eq!(
            settings.keybinding_overrides,
            vec![
                KeyBindingOverride {
                    action: "next-page".to_string(),
                    keystrokes: "j".to_string(),
                    context: None,
                },
                KeyBindingOverride {
                    action: "jump-backward".to_string(),
                    keystrokes: "ctrl-o".to_string(),
                    context: None,
                },
            ]
        );

        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_language_settings() {
        let settings = parse_settings(
            r##"
language "zh-CN"
"##,
        )
        .unwrap();

        assert_eq!(settings.language, AppLanguage::SimplifiedChinese);
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_contextual_keybindings() {
        let settings = parse_settings(
            r##"
keybindings {
    next-search-result keys="ctrl-g" context="search_results"
    next-page keys="" context="pdf_view"
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.keybinding_overrides,
            vec![
                KeyBindingOverride {
                    action: "next-search-result".to_string(),
                    keystrokes: "ctrl-g".to_string(),
                    context: Some("search_results".to_string()),
                },
                KeyBindingOverride {
                    action: "next-page".to_string(),
                    keystrokes: String::new(),
                    context: Some("pdf_view".to_string()),
                },
            ]
        );
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_tab_settings() {
        let settings = parse_settings(
            r##"
tabs {
    visibility "always"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.tab_bar_visibility, TabBarVisibility::Always);
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_font_settings() {
        let settings = parse_settings(
            r##"
font {
    ui_font_family "Noto Sans CJK SC"
    ui_font_size 14
    ui_font_weight 500
    ui_font_fallbacks "Noto Sans CJK SC" "Source Han Sans SC" "Noto Sans CJK SC" ""
}
"##,
        )
        .unwrap();

        assert_eq!(settings.font.ui_font_family, "Noto Sans CJK SC");
        assert_eq!(settings.font.ui_font_size, 14.0);
        assert_eq!(settings.font.ui_font_weight, 500.0);
        assert_eq!(
            settings.font.ui_font_fallbacks,
            vec!["Noto Sans CJK SC", "Source Han Sans SC"]
        );
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn clamps_font_settings() {
        let settings = parse_settings(
            r##"
font {
    ui_font_family ""
    ui_font_size 1000
    ui_font_weight 10
}
"##,
        )
        .unwrap();

        assert_eq!(settings.font.ui_font_family, DEFAULT_UI_FONT_FAMILY);
        assert_eq!(settings.font.ui_font_size, MAX_UI_FONT_SIZE);
        assert_eq!(settings.font.ui_font_weight, MIN_UI_FONT_WEIGHT);
    }

    #[test]
    fn migrates_legacy_ai_settings() {
        let settings = parse_settings(
            r##"
ai {
    provider "anthropic"
    model "claude-3-5-sonnet-latest"
    api_key "test-key"
    base_url "https://api.example.com/"
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.ai.main_model,
            Some(ModelRef::new(
                AiProvider::Anthropic,
                "claude-3-5-sonnet-latest"
            ))
        );
        let config = settings.ai.provider_config(AiProvider::Anthropic);
        assert!(config.enabled);
        assert!(config.model("claude-3-5-sonnet-latest").is_some());
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.base_url, Some("https://api.example.com".to_string()));
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_per_provider_ai_settings() {
        let settings = parse_settings(
            r##"
ai {
    provider "deepseek"
    providers {
        deepseek {
            model "deepseek-chat"
            api_key "deepseek-key"
            base_url "https://api.deepseek.com/v1/"
        }
        openrouter {
            model "openai/gpt-4o-mini"
            api_key "openrouter-key"
        }
    }
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.ai.main_model,
            Some(ModelRef::new(
                AiProvider::DeepSeek,
                AiProvider::DeepSeek.default_model()
            ))
        );
        assert_eq!(settings.ai.thinking_level, AiThinkingLevel::Medium);
        let deepseek = settings.ai.provider_config(AiProvider::DeepSeek);
        assert!(deepseek.enabled);
        assert!(deepseek.model("deepseek-chat").is_some());
        assert_eq!(deepseek.api_key, "deepseek-key");
        assert_eq!(
            deepseek.base_url,
            Some("https://api.deepseek.com/v1".to_string())
        );
        let openrouter = settings.ai.provider_config(AiProvider::OpenRouter);
        assert!(openrouter.model("openai/gpt-4o-mini").is_some());
        assert_eq!(openrouter.api_key, "openrouter-key");
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_ai_thinking_level() {
        let settings = parse_settings(
            r##"
ai {
    provider "openai"
    thinking "high"
}
"##,
        )
        .unwrap();

        assert_eq!(settings.ai.thinking_level, AiThinkingLevel::High);
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn parses_and_exports_quick_ask_settings() {
        let settings = parse_settings(
            r##"
ai {
    quick_ask {
        transparency 0.35
        text_size 18
    }
}
"##,
        )
        .unwrap();

        assert_eq!(settings.ai.quick_ask.transparency, 0.35);
        assert_eq!(settings.ai.quick_ask.text_size, 18.0);
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn clamps_quick_ask_settings() {
        let settings = parse_settings(
            r##"
ai {
    quick_ask {
        transparency 2
        text_size 100
    }
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.ai.quick_ask.transparency,
            MAX_QUICK_ASK_TRANSPARENCY
        );
        assert_eq!(settings.ai.quick_ask.text_size, MAX_QUICK_ASK_TEXT_SIZE);

        let settings = parse_settings(
            r##"
ai {
    quick_ask {
        transparency -1
        text_size 1
    }
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.ai.quick_ask.transparency,
            MIN_QUICK_ASK_TRANSPARENCY
        );
        assert_eq!(settings.ai.quick_ask.text_size, MIN_QUICK_ASK_TEXT_SIZE);
    }

    #[test]
    fn migrates_legacy_ai_title_model_settings() {
        let settings = parse_settings(
            r##"
ai {
    title_model {
        provider "openai"
        model "gpt-4o-mini"
        api_key "title-key"
        base_url "https://api.example.com/v1/"
    }
}
"##,
        )
        .unwrap();

        let title_model = settings.ai.title_model.as_ref().unwrap();
        assert_eq!(title_model.provider, AiProvider::OpenAi);
        assert_eq!(title_model.id, "gpt-4o-mini");
        let config = settings.ai.provider_config(AiProvider::OpenAi);
        assert_eq!(config.api_key, "title-key");
        assert_eq!(
            config.base_url,
            Some("https://api.example.com/v1".to_string())
        );
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn title_model_uses_provider_credentials() {
        let settings = parse_settings(
            r##"
ai {
    provider "openai"
    providers {
        openai {
            model "gpt-5"
            api_key "main-key"
            base_url "https://main.example.com"
        }
    }
    title_model {
        provider "openai"
        model "gpt-4o-mini"
        api_key "title-key"
        base_url "https://title.example.com"
    }
}
"##,
        )
        .unwrap();

        let main = settings.ai.resolved_main();
        let title = settings.ai.resolved_title().unwrap();
        assert_eq!(main.api_key, "main-key");
        assert_eq!(main.base_url, "https://main.example.com");
        assert_eq!(title.api_key, "main-key");
        assert_eq!(title.base_url, "https://main.example.com");
        assert_eq!(parse_settings(&settings.to_kdl_string()).unwrap(), settings);
    }

    #[test]
    fn thinking_levels_are_provider_specific() {
        assert!(
            AiThinkingLevel::supported_for(AiProvider::OpenAi, "gpt-5")
                .contains(&AiThinkingLevel::XHigh)
        );
        assert!(
            !AiThinkingLevel::supported_for(AiProvider::OpenAi, "gpt-5")
                .contains(&AiThinkingLevel::Max)
        );
        assert!(
            AiThinkingLevel::supported_for(AiProvider::Anthropic, "claude-sonnet-4-5-latest")
                .contains(&AiThinkingLevel::Max)
        );
        assert!(
            AiThinkingLevel::supported_for(AiProvider::OpenRouter, "openai/gpt-5.5")
                .contains(&AiThinkingLevel::XHigh)
        );
        assert!(
            !AiThinkingLevel::supported_for(AiProvider::OpenRouter, "openai/gpt-4o")
                .contains(&AiThinkingLevel::XHigh)
        );
        assert_eq!(
            AiThinkingLevel::Max.clamp_for(AiProvider::OpenAi, "gpt-5"),
            AiThinkingLevel::XHigh
        );
        assert_eq!(
            AiThinkingLevel::Max.clamp_for(AiProvider::OpenRouter, "openai/gpt-5.5"),
            AiThinkingLevel::XHigh
        );
    }

    #[test]
    fn theme_resolution_layers_selected_theme_and_overrides() {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let settings = Settings {
            theme: ThemeSelection::Static("one-dark".to_string()),
            overrides: PartialTheme {
                background: Some(0x101010),
                page_gap: Some(48.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };

        let theme = registry.resolve(
            settings.theme.name(ThemeAppearance::Light),
            settings.overrides,
        );

        assert_eq!(theme.accent, 0x74ade8);
        assert_eq!(theme.background, 0x101010);
        assert_eq!(theme.page_gap, 48.0);
    }

    #[test]
    fn unknown_theme_falls_back_to_default() {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let theme = registry.resolve("missing", PartialTheme::default());

        assert_eq!(theme, Theme::default());
    }

    #[test]
    fn loads_user_theme_from_themes_directory() {
        let root = unique_test_dir();
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("solar.kdl"),
            r##"
name "Solar"
appearance "light"
colors {
    background "#123456"
    accent "#abcdef"
}
page-gap 36
"##,
        )
        .unwrap();

        let registry = ThemeRegistry::load(&root);
        let theme = registry.resolve("solar", PartialTheme::default());
        let metadata = registry
            .metadata()
            .into_iter()
            .find(|metadata| metadata.id == "solar")
            .unwrap();

        assert_eq!(theme.background, 0x123456);
        assert_eq!(theme.accent, 0xabcdef);
        assert_eq!(theme.page_gap, 36.0);
        assert_eq!(metadata.name, "Solar");
        assert_eq!(metadata.appearance.as_str(), "light");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unmanaged_settings_persist_to_state_without_creating_config() {
        let root = unique_test_dir();
        let config_path = root.join("config").join("glyphst").join("config.kdl");
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                ..BehaviorSettings::default()
            },
            ..Settings::default()
        };
        let mut store = test_store(
            Settings::default(),
            false,
            config_path.clone(),
            state_path.clone(),
        );

        store.set_settings(settings.clone()).unwrap();

        assert!(!config_path.exists());
        assert_eq!(
            parse_settings(&fs::read_to_string(&state_path).unwrap()).unwrap(),
            settings
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn managed_settings_persist_to_config_without_touching_state_file() {
        let root = unique_test_dir();
        let config_path = root.join("config").join("glyphst").join("config.kdl");
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let settings = Settings {
            overrides: PartialTheme {
                page_gap: Some(48.0),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };
        let mut store = test_store(
            Settings::default(),
            true,
            config_path.clone(),
            state_path.clone(),
        );

        store.set_settings(settings.clone()).unwrap();

        assert!(!state_path.exists());
        assert_eq!(
            parse_settings(&fs::read_to_string(config_path).unwrap()).unwrap(),
            settings
        );
        assert_eq!(store.settings, settings);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn failed_persistence_preserves_live_settings() {
        let root = unique_test_dir();
        let blocked_parent = root.join("not-a-directory");
        fs::create_dir_all(&root).unwrap();
        fs::write(&blocked_parent, "blocked").unwrap();
        let mut store = test_store(
            Settings::default(),
            false,
            root.join("config.kdl"),
            blocked_parent.join("config.kdl"),
        );
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                ..BehaviorSettings::default()
            },
            ..Settings::default()
        };

        assert!(store.set_settings(settings).is_err());
        assert_eq!(store.settings, Settings::default());

        fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn persisted_settings_are_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = unique_test_dir();
        let state_path = root.join("state").join("glyphst").join("config.kdl");
        let mut store = test_store(
            Settings::default(),
            false,
            root.join("config").join("glyphst").join("config.kdl"),
            state_path.clone(),
        );

        store.set_settings(Settings::default()).unwrap();

        assert_eq!(
            fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(state_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn export_to_writes_selected_path() {
        let root = unique_test_dir();
        let export_path = root.join("picked").join("glyphst.kdl");
        let settings = Settings {
            behavior: BehaviorSettings {
                default_zoom: 1.5,
                zoom_step: 1.2,
                smooth_scrolling: false,
                dim_inactive_panes: true,
            },
            theme: ThemeSelection::Static(DEFAULT_THEME_ID.to_string()),
            overrides: PartialTheme {
                background: Some(0x181818),
                selection_highlight: Some(0x61afef66),
                ..PartialTheme::default()
            },
            ..Settings::default()
        };
        let store = test_store(
            settings.clone(),
            false,
            root.join("config.kdl"),
            root.join("state.kdl"),
        );

        store.export_to(&export_path).unwrap();

        assert_eq!(
            parse_settings(&fs::read_to_string(&export_path).unwrap()).unwrap(),
            settings
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn parses_dynamic_theme_selection() {
        let settings = parse_settings(
            r##"
theme {
    mode "system"
    light "one-light"
    dark "one-dark"
}
"##,
        )
        .unwrap();

        assert_eq!(
            settings.theme,
            ThemeSelection::Dynamic {
                mode: ThemeAppearanceMode::System,
                light: "one-light".to_string(),
                dark: "one-dark".to_string(),
            }
        );
    }

    #[test]
    fn dynamic_theme_selection_round_trips() {
        let settings = Settings {
            theme: ThemeSelection::Dynamic {
                mode: ThemeAppearanceMode::Dark,
                light: "one-light".to_string(),
                dark: "one-dark".to_string(),
            },
            ..Settings::default()
        };

        let exported = settings.to_kdl_string();
        assert_eq!(parse_settings(&exported).unwrap(), settings);
    }

    #[test]
    fn set_mode_migrates_static_to_dynamic() {
        let mut selection = ThemeSelection::Static("one-light".to_string());
        selection.set_mode(ThemeAppearanceMode::Dark);

        assert_eq!(
            selection,
            ThemeSelection::Dynamic {
                mode: ThemeAppearanceMode::System,
                light: "one-light".to_string(),
                dark: "one-dark".to_string(),
            }
        );
    }

    #[test]
    fn set_mode_updates_dynamic_mode() {
        let mut selection = ThemeSelection::Dynamic {
            mode: ThemeAppearanceMode::System,
            light: "one-light".to_string(),
            dark: "one-dark".to_string(),
        };
        selection.set_mode(ThemeAppearanceMode::Light);

        assert_eq!(
            selection,
            ThemeSelection::Dynamic {
                mode: ThemeAppearanceMode::Light,
                light: "one-light".to_string(),
                dark: "one-dark".to_string(),
            }
        );
    }

    #[test]
    fn theme_selection_name_returns_correct_theme() {
        let dynamic = ThemeSelection::Dynamic {
            mode: ThemeAppearanceMode::System,
            light: "one-light".to_string(),
            dark: "one-dark".to_string(),
        };

        assert_eq!(dynamic.name(ThemeAppearance::Light), "one-light");
        assert_eq!(dynamic.name(ThemeAppearance::Dark), "one-dark");

        let light_mode = ThemeSelection::Dynamic {
            mode: ThemeAppearanceMode::Light,
            light: "one-light".to_string(),
            dark: "one-dark".to_string(),
        };
        assert_eq!(light_mode.name(ThemeAppearance::Dark), "one-light");
    }

    fn test_store(
        settings: Settings,
        user_managed: bool,
        config_path: PathBuf,
        state_path: PathBuf,
    ) -> SettingsStore {
        let registry = ThemeRegistry::load(Path::new("/path/that/does/not/exist"));
        let system_appearance = ThemeAppearance::Light;
        let resolved_theme =
            registry.resolve(settings.theme.name(system_appearance), settings.overrides);
        SettingsStore {
            settings,
            registry,
            resolved_theme,
            system_appearance,
            user_managed,
            config_path,
            state_path,
            last_dynamic_light: None,
            last_dynamic_dark: None,
        }
    }

    fn unique_test_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "glyphst-settings-test-{}-{nanos}",
            std::process::id()
        ))
    }
}
