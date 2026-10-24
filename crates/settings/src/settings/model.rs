use super::*;

/// The settings the Reader keeps in `config.kdl`: the window, and the AI,
/// translation and OCR providers. Its other preferences live in its own
/// store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub window: WindowSettings,
    pub ai: AiSettings,
    pub translation: TranslationSettings,
    pub ocr: OcrSettings,
}

impl Settings {
    pub fn sanitize(&mut self) {
        self.window.sanitize();
        self.ai.sanitize();
        self.translation.sanitize();
        self.ocr.sanitize();
    }

    pub(super) fn from_document(document: &KdlDocument) -> Self {
        let mut settings = Self::default();

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
            if let Some(value) = window
                .get_arg("fullscreen_opaque")
                .and_then(KdlValue::as_bool)
            {
                settings.window.fullscreen_opaque = value;
            }
        }

        if let Some(ai) = document.get("ai").and_then(KdlNode::children) {
            let mut legacy_provider = AiProvider::OpenAi;
            let mut legacy_provider_set = false;
            if let Some(value) = ai
                .get_arg("provider")
                .and_then(KdlValue::as_string)
                .and_then(AiProvider::parse)
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
            if let Some(value) = ai
                .get_arg("page_text_format")
                .and_then(KdlValue::as_string)
                .and_then(PageTextFormat::from_str)
            {
                settings.ai.page_text_format = value;
            }
            if let Some(value) = ai
                .get_arg("page_image_quality")
                .and_then(KdlValue::as_string)
                .and_then(PageImageQuality::from_str)
            {
                settings.ai.page_image_quality = value;
            }
            if let Some(value) = ai
                .get_arg("page_image_limit")
                .and_then(KdlValue::as_integer)
                .and_then(|value| usize::try_from(value).ok())
            {
                settings.ai.page_image_limit = value;
            }

            if let Some(quick_ask) = ai.get("quick_ask").and_then(KdlNode::children) {
                if let Some(value) = quick_ask
                    .get_arg("position")
                    .and_then(KdlValue::as_string)
                    .and_then(QuickAskPosition::parse)
                {
                    settings.ai.quick_ask.position = value;
                }
                if let Some(value) = quick_ask.get_arg("transparency").and_then(value_to_f32) {
                    settings.ai.quick_ask.transparency = value;
                }
                if let Some(value) = quick_ask.get_arg("text_size").and_then(value_to_f32) {
                    settings.ai.quick_ask.text_size = value;
                }
            }

            settings.ai.acp_agents = ai
                .nodes()
                .iter()
                .filter(|node| node.name().value() == "acp_agent")
                .filter_map(AcpAgentConfig::from_node)
                .collect();

            if let Some(providers) = ai.get("providers").and_then(KdlNode::children) {
                let mut ordered_providers = Vec::new();
                let mut has_ordered_provider_nodes = false;
                for node in providers.nodes() {
                    let provider_name = if node.name().value() == "provider" {
                        has_ordered_provider_nodes = true;
                        node.get(0).and_then(KdlValue::as_string)
                    } else {
                        Some(node.name().value())
                    };
                    let Some(provider) = provider_name.and_then(AiProvider::parse) else {
                        continue;
                    };
                    let Some(children) = node.children() else {
                        continue;
                    };
                    if !ordered_providers.contains(&provider) {
                        ordered_providers.push(provider);
                    }
                    settings.ai.providers.insert(
                        provider,
                        AiProviderConfig::from_document(provider, children),
                    );
                }
                if has_ordered_provider_nodes {
                    settings.ai.provider_order = ordered_providers;
                }
            }
            if legacy_provider_set {
                settings.ai.provider_config_mut(legacy_provider).enabled = true;
            }

            if let Some(model_ref) = ai.get("main_model").and_then(ModelRef::from_node) {
                settings.ai.main_model = Some(model_ref);
            }
            if let Some(model_ref) = ai.get("library_model").and_then(ModelRef::from_node) {
                settings.ai.library_model = Some(model_ref);
            }
            if let Some(model_ref) = ai.get("translation_model").and_then(ModelRef::from_node) {
                settings.ai.translation_model = Some(model_ref);
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
                        .and_then(AiProvider::parse)
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
            if ai.get("translation_model").is_none() {
                settings.ai.translation_model = settings.ai.main_model.clone();
            }
        }

        if let Some(translation) = document.get("translation").and_then(KdlNode::children) {
            if let Some(value) = translation
                .get_arg("provider")
                .and_then(KdlValue::as_string)
                .and_then(TranslationProvider::parse)
            {
                settings.translation.active_provider = value;
            }
            settings.translation.source_language = translation
                .get_arg("source_language")
                .and_then(KdlValue::as_string)
                .map(str::to_string);
            if let Some(value) = translation
                .get_arg("target_language")
                .and_then(KdlValue::as_string)
            {
                settings.translation.target_language = TranslationTargetLanguage::parse(value);
            }
            if let Some(value) = translation
                .get_arg("document_mode")
                .and_then(KdlValue::as_string)
                .and_then(DocumentTranslationMode::parse)
            {
                settings.translation.document_mode = value;
            }
            if let Some(pinned) = translation.get("pinned").and_then(KdlNode::children) {
                if let Some(value) = pinned.get_arg("width").and_then(value_to_f32) {
                    settings.translation.pinned.width = value;
                }
                if let Some(value) = pinned
                    .get_arg("corner")
                    .and_then(KdlValue::as_string)
                    .and_then(TranslationBoxCorner::parse)
                {
                    settings.translation.pinned.corner = value;
                }
            }
            if let Some(content_review) = translation
                .get("content_review")
                .and_then(KdlNode::children)
            {
                if let Some(value) = content_review
                    .get_arg("mode")
                    .and_then(KdlValue::as_string)
                    .and_then(PdfContentReviewMode::parse)
                {
                    settings.translation.content_review.mode = value;
                }
                if let Some(model) = content_review.get("model").and_then(ModelRef::from_node) {
                    settings.translation.content_review.model = Some(model);
                }
            }
            if let Some(providers) = translation.get("providers").and_then(KdlNode::children) {
                for node in providers.nodes() {
                    let Some(provider) = TranslationProvider::parse(node.name().value()) else {
                        continue;
                    };
                    let Some(children) = node.children() else {
                        continue;
                    };
                    settings.translation.providers.insert(
                        provider,
                        TranslationProviderConfig::from_document(provider, children),
                    );
                }
            }
        }

        if let Some(ocr) = document.get("ocr").and_then(KdlNode::children) {
            if let Some(value) = ocr
                .get_arg("mode")
                .and_then(KdlValue::as_string)
                .and_then(OcrMode::from_str)
            {
                settings.ocr.mode = value;
            }
            if let Some(value) = ocr
                .get_arg("provider")
                .and_then(KdlValue::as_string)
                .and_then(OcrProvider::from_str)
            {
                settings.ocr.provider = value;
            }
            if let Some(value) = ocr.get_arg("local-model").and_then(KdlValue::as_string) {
                settings.ocr.local.model = value.to_string();
            }
            if let Some(value) = ocr
                .get_arg("local-device")
                .and_then(KdlValue::as_string)
                .and_then(OcrDevice::from_str)
            {
                settings.ocr.local.device = value;
            }
            // Read the original flat Paddle configuration so existing development
            // settings remain recoverable after the provider model becomes nested.
            if let Some(value) = ocr.get_arg("token").and_then(KdlValue::as_string) {
                settings.ocr.paddle.token = value.to_string();
            }
            if let Some(value) = ocr.get_arg("endpoint").and_then(KdlValue::as_string) {
                settings.ocr.paddle.endpoint = value.to_string();
            }
            if let Some(value) = ocr.get_arg("model").and_then(KdlValue::as_string) {
                settings.ocr.paddle.model = value.to_string();
            }
            if let Some(providers) = ocr.get("providers").and_then(KdlNode::children) {
                if let Some(paddle) = providers.get("paddle").and_then(KdlNode::children) {
                    if let Some(value) = paddle.get_arg("token").and_then(KdlValue::as_string) {
                        settings.ocr.paddle.token = value.to_string();
                    }
                    if let Some(value) = paddle.get_arg("endpoint").and_then(KdlValue::as_string) {
                        settings.ocr.paddle.endpoint = value.to_string();
                    }
                    if let Some(value) = paddle.get_arg("model").and_then(KdlValue::as_string) {
                        settings.ocr.paddle.model = value.to_string();
                    }
                }
                if let Some(unlimited) = providers.get("unlimited").and_then(KdlNode::children) {
                    if let Some(value) = unlimited.get_arg("api_key").and_then(KdlValue::as_string)
                    {
                        settings.ocr.unlimited.api_key = value.to_string();
                    }
                    if let Some(value) = unlimited
                        .get_arg("secret_key")
                        .and_then(KdlValue::as_string)
                    {
                        settings.ocr.unlimited.secret_key = value.to_string();
                    }
                    if let Some(value) = unlimited.get_arg("endpoint").and_then(KdlValue::as_string)
                    {
                        settings.ocr.unlimited.endpoint = value.to_string();
                    }
                }
            }
        }

        settings.sanitize();
        settings
    }

    pub fn to_kdl_string(&self) -> String {
        let mut document = KdlDocument::new();

        let mut window = KdlNode::new("window");
        let window_children = window.ensure_children();
        push_value_node(window_children, "opacity", self.window.opacity as f64);
        push_value_node(
            window_children,
            "appearance",
            self.window.background.as_str(),
        );
        push_value_node(
            window_children,
            "fullscreen_opaque",
            self.window.fullscreen_opaque,
        );
        document.nodes_mut().push(window);

        if self.ai != AiSettings::default() {
            let mut ai = KdlNode::new("ai");
            let ai_children = ai.ensure_children();
            if let Some(main_model) = &self.ai.main_model {
                main_model.write_to_document(ai_children, "main_model");
            }
            if let Some(title_model) = &self.ai.title_model {
                title_model.write_to_document(ai_children, "title_model");
            }
            if let Some(library_model) = &self.ai.library_model {
                library_model.write_to_document(ai_children, "library_model");
            }
            if let Some(translation_model) = &self.ai.translation_model {
                translation_model.write_to_document(ai_children, "translation_model");
            }
            if self.ai.thinking_level != AiThinkingLevel::default() {
                push_value_node(ai_children, "thinking", self.ai.thinking_level.as_str());
            }
            if self.ai.page_text_format != PageTextFormat::default() {
                push_value_node(
                    ai_children,
                    "page_text_format",
                    self.ai.page_text_format.as_str(),
                );
            }
            if self.ai.page_image_quality != PageImageQuality::default() {
                push_value_node(
                    ai_children,
                    "page_image_quality",
                    self.ai.page_image_quality.as_str(),
                );
            }
            if self.ai.page_image_limit != DEFAULT_PAGE_IMAGE_LIMIT {
                push_value_node(
                    ai_children,
                    "page_image_limit",
                    self.ai.page_image_limit as i128,
                );
            }
            if self.ai.quick_ask != QuickAskSettings::default() {
                let mut quick_ask = KdlNode::new("quick_ask");
                let quick_ask_children = quick_ask.ensure_children();
                push_value_node(
                    quick_ask_children,
                    "position",
                    self.ai.quick_ask.position.as_str(),
                );
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
            for agent in &self.ai.acp_agents {
                agent.write_to_document(ai_children);
            }

            let mut providers = KdlNode::new("providers");
            let providers_children = providers.ensure_children();
            for provider in &self.ai.provider_order {
                let Some(config) = self.ai.providers.get(provider) else {
                    continue;
                };

                let mut provider_node = KdlNode::new("provider");
                provider_node.push(provider.as_str());
                config.write_to_document(*provider, provider_node.ensure_children());
                providers_children.nodes_mut().push(provider_node);
            }
            if !providers_children.nodes().is_empty() {
                ai_children.nodes_mut().push(providers);
            }
            document.nodes_mut().push(ai);
        }

        if self.translation != TranslationSettings::default() {
            let mut translation = KdlNode::new("translation");
            let translation_children = translation.ensure_children();
            push_value_node(
                translation_children,
                "provider",
                self.translation.active_provider.as_str(),
            );
            if let Some(source_language) = &self.translation.source_language {
                push_value_node(
                    translation_children,
                    "source_language",
                    source_language.clone(),
                );
            }
            push_value_node(
                translation_children,
                "target_language",
                self.translation.target_language.as_str(),
            );
            push_value_node(
                translation_children,
                "document_mode",
                self.translation.document_mode.as_str(),
            );
            let default_pinned = PinnedTranslationSettings::default();
            if self.translation.pinned.width != default_pinned.width
                || self.translation.pinned.corner != default_pinned.corner
            {
                let mut pinned = KdlNode::new("pinned");
                let pinned_children = pinned.ensure_children();
                push_value_node(
                    pinned_children,
                    "width",
                    self.translation.pinned.width as f64,
                );
                push_value_node(
                    pinned_children,
                    "corner",
                    self.translation.pinned.corner.as_str(),
                );
                translation_children.nodes_mut().push(pinned);
            }

            let default_content_review = PdfContentReviewSettings::default();
            if self.translation.content_review != default_content_review {
                let mut content_review = KdlNode::new("content_review");
                let content_review_children = content_review.ensure_children();
                push_value_node(
                    content_review_children,
                    "mode",
                    self.translation.content_review.mode.as_str(),
                );
                if let Some(model) = &self.translation.content_review.model {
                    model.write_to_document(content_review_children, "model");
                }
                translation_children.nodes_mut().push(content_review);
            }

            let mut providers = KdlNode::new("providers");
            let providers_children = providers.ensure_children();
            for provider in TranslationProvider::ALL {
                let Some(config) = self.translation.providers.get(&provider) else {
                    continue;
                };
                if config.is_default() {
                    continue;
                }
                let mut provider_node = KdlNode::new(provider.as_str());
                config.write_to_document(provider_node.ensure_children());
                providers_children.nodes_mut().push(provider_node);
            }
            if !providers_children.nodes().is_empty() {
                translation_children.nodes_mut().push(providers);
            }
            document.nodes_mut().push(translation);
        }

        if self.ocr != OcrSettings::default() {
            let mut ocr = KdlNode::new("ocr");
            let ocr_children = ocr.ensure_children();
            push_value_node(ocr_children, "mode", self.ocr.mode.as_str());
            push_value_node(ocr_children, "provider", self.ocr.provider.as_str());
            if self.ocr.local != LocalOcrSettings::default() {
                push_value_node(ocr_children, "local-model", self.ocr.local.model.clone());
                push_value_node(ocr_children, "local-device", self.ocr.local.device.as_str());
            }
            let mut providers = KdlNode::new("providers");
            let provider_children = providers.ensure_children();
            if self.ocr.paddle != PaddleOcrSettings::default() {
                let mut paddle = KdlNode::new("paddle");
                let paddle_children = paddle.ensure_children();
                if !self.ocr.paddle.token.is_empty() {
                    push_value_node(paddle_children, "token", self.ocr.paddle.token.clone());
                }
                push_value_node(
                    paddle_children,
                    "endpoint",
                    self.ocr.paddle.endpoint.clone(),
                );
                push_value_node(paddle_children, "model", self.ocr.paddle.model.clone());
                provider_children.nodes_mut().push(paddle);
            }
            if self.ocr.unlimited != UnlimitedOcrSettings::default() {
                let mut unlimited = KdlNode::new("unlimited");
                let unlimited_children = unlimited.ensure_children();
                if !self.ocr.unlimited.api_key.is_empty() {
                    push_value_node(
                        unlimited_children,
                        "api_key",
                        self.ocr.unlimited.api_key.clone(),
                    );
                }
                if !self.ocr.unlimited.secret_key.is_empty() {
                    push_value_node(
                        unlimited_children,
                        "secret_key",
                        self.ocr.unlimited.secret_key.clone(),
                    );
                }
                push_value_node(
                    unlimited_children,
                    "endpoint",
                    self.ocr.unlimited.endpoint.clone(),
                );
                provider_children.nodes_mut().push(unlimited);
            }
            if !provider_children.nodes().is_empty() {
                ocr_children.nodes_mut().push(providers);
            }
            document.nodes_mut().push(ocr);
        }

        document.autoformat();
        document.to_string()
    }
}
