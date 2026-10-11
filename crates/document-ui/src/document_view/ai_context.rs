use super::*;

impl DocumentView {
    pub(super) fn ai_page_state(&self) -> AiPageState {
        if self.capabilities.reflow {
            AiPageState::default()
        } else {
            AiPageState {
                page_count: self.page_count(),
                current_page: self.current_page(),
            }
        }
    }

    // This resolver owns page-scoped mentions and expands document-image mentions.
    // Selection and document-text mentions are resolved synchronously by the AI
    // panel and are ignored here.
    pub(super) fn prepare_page_references(
        references: Vec<MentionReference>,
        document_layout: DocumentLayout,
        page_count: usize,
    ) -> Result<Vec<MentionReference>> {
        let page_references = references
            .into_iter()
            .filter(|reference| reference.kind.is_page_reference())
            .collect::<Vec<_>>();
        if page_references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("page references are unavailable for reflowable documents");
        }

        let mut unique = Vec::with_capacity(page_references.len());
        let mut seen = HashSet::with_capacity(page_references.len());
        for reference in page_references {
            if reference.kind == MentionKind::DocumentImage {
                if reference.page.is_some() {
                    bail!("{} does not accept a page number", reference.kind.token());
                }
                for page in 1..=page_count {
                    let page_reference =
                        MentionReference::new(MentionKind::DocumentImage, Some(page));
                    if seen.insert(page_reference) {
                        unique.push(page_reference);
                    }
                }
                continue;
            }

            let page = reference.page.ok_or_else(|| {
                anyhow!("{} requires a 1-based page number", reference.kind.token())
            })?;
            if page == 0 || page > page_count {
                bail!(
                    "invalid page reference {}: page must be between 1 and {}",
                    reference.display_text(),
                    page_count
                );
            }
            if seen.insert(reference) {
                unique.push(reference);
            }
        }
        Ok(unique)
    }

    #[ztracing::instrument(skip_all, fields(reference_count = references.len()))]
    pub(super) fn resolve_page_contexts(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        expected_page_count: usize,
        text_format: PageTextFormat,
        image_quality: PageImageQuality,
        rotation: DocumentRotation,
        references: Vec<MentionReference>,
    ) -> Result<Vec<ResolvedPageContext>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("page references are unavailable for reflowable documents");
        }

        let opened = snapshot
            .open_with_layout(document_layout)
            .context("failed to open document for page context")?;
        if opened.is_reflowable() {
            bail!("page references are unavailable for reflowable documents");
        }
        let document = opened.into_inner();
        let actual_page_count = document
            .page_count()
            .context("failed to read document page count for page context")?;
        if actual_page_count < 0 || actual_page_count as usize != expected_page_count {
            bail!(
                "document page count changed while resolving page context: expected {}, got {}",
                expected_page_count,
                actual_page_count
            );
        }

        let mut resolved = Vec::with_capacity(references.len());
        let mut page_groups: Vec<(usize, Vec<MentionReference>)> = Vec::new();
        let mut page_group_indexes = HashMap::new();
        for reference in references {
            let page_number = reference
                .page
                .context("validated page reference lost its page number")?;
            let group_ix = *page_group_indexes.entry(page_number).or_insert_with(|| {
                page_groups.push((page_number, Vec::new()));
                page_groups.len() - 1
            });
            page_groups[group_ix].1.push(reference);
        }

        let mut rendered_image_bytes = 0usize;
        for (page_number, page_references) in page_groups {
            let page_idx = page_number - 1;
            let page = document
                .load_page(page_idx as i32)
                .with_context(|| format!("failed to load PDF page {page_number}"))?;
            let page_bounds = page
                .bounds()
                .with_context(|| format!("failed to read bounds for PDF page {page_number}"))?;
            let page_width = page_bounds.x1 - page_bounds.x0;
            let page_height = page_bounds.y1 - page_bounds.y0;
            let mut display_list = DisplayList::new(page_bounds).with_context(|| {
                format!("failed to create display list for PDF page {page_number}")
            })?;
            let device = Device::from_display_list(&mut display_list).with_context(|| {
                format!("failed to create display-list device for PDF page {page_number}")
            })?;
            page.run(&device, &Matrix::IDENTITY)
                .with_context(|| format!("failed to record PDF page {page_number}"))?;
            drop(device);

            let markdown = page_references
                .iter()
                .any(|reference| {
                    matches!(
                        reference.kind,
                        MentionKind::Page | MentionKind::SelectedPagesText
                    )
                })
                .then(|| Self::extract_page_markdown(&display_list, text_format))
                .transpose()
                .with_context(|| format!("failed to extract text from PDF page {page_number}"))?;
            let image_bytes = if page_references
                .iter()
                .any(|reference| reference.kind.is_image())
            {
                let bytes = Self::render_page_jpeg(
                    &display_list,
                    page_bounds,
                    page_width,
                    page_height,
                    image_quality,
                    rotation,
                )
                .with_context(|| format!("failed to render PDF page {page_number} as JPEG"))?;
                rendered_image_bytes = rendered_image_bytes.saturating_add(bytes.len());
                if rendered_image_bytes > MAX_PAGE_IMAGE_BYTES {
                    bail!(
                        "attached page images are too large: more than {} MiB",
                        MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
                    );
                }
                Some(Arc::<[u8]>::from(bytes))
            } else {
                None
            };

            for reference in page_references {
                match reference.kind {
                    MentionKind::Page | MentionKind::SelectedPagesText => {
                        resolved.push(ResolvedPageContext::Text(ContextBlock::new(
                            reference.kind,
                            Some(page_number),
                            markdown.clone().unwrap_or_default(),
                        )));
                    }
                    MentionKind::PageImage
                    | MentionKind::SelectedPagesImage
                    | MentionKind::DocumentImage => {
                        resolved.push(ResolvedPageContext::Image(ImageContext::new(
                            reference.kind,
                            Some(page_number),
                            "image/jpeg",
                            image_bytes.clone().context("page image was not rendered")?,
                        )));
                    }
                    MentionKind::Selection | MentionKind::DocumentText => {
                        unreachable!(
                            "non-page references are filtered before background resolution"
                        )
                    }
                    MentionKind::RegionImage => {
                        unreachable!("region references use the region resolver")
                    }
                }
            }
        }

        Ok(resolved)
    }

    #[ztracing::instrument(skip_all, fields(reference_count = references.len()))]
    pub(super) fn resolve_region_contexts(
        snapshot: &DocumentSnapshot,
        document_layout: DocumentLayout,
        expected_page_count: usize,
        image_quality: PageImageQuality,
        page_image_limit: usize,
        rotation: DocumentRotation,
        references: Vec<RegionImageReference>,
    ) -> Result<Vec<ResolvedRegionContext>> {
        if references.is_empty() {
            return Ok(Vec::new());
        }
        if !matches!(document_layout, DocumentLayout::Fixed) {
            bail!("region images are unavailable for reflowable documents");
        }
        let region_page_count = references
            .iter()
            .map(|reference| reference.regions.len())
            .sum::<usize>();
        if region_page_count > page_image_limit {
            bail!(
                "too many pages attached through region images: {region_page_count} requested, maximum is {page_image_limit}"
            );
        }

        let opened = snapshot
            .open_with_layout(document_layout)
            .context("failed to open document for region image context")?;
        if opened.is_reflowable() {
            bail!("region images are unavailable for reflowable documents");
        }
        let document = opened.into_inner();
        let actual_page_count = document
            .page_count()
            .context("failed to read document page count for region image context")?;
        if actual_page_count < 0 || actual_page_count as usize != expected_page_count {
            bail!(
                "document page count changed while resolving region images: expected {}, got {}",
                expected_page_count,
                actual_page_count
            );
        }

        let mut display_lists = HashMap::<usize, Arc<DisplayList>>::new();
        let mut page_bounds_by_index = HashMap::<usize, Rect>::new();
        let mut resolved = Vec::with_capacity(references.len());
        let mut total_bytes = 0usize;
        for reference in references {
            if reference.is_empty() {
                bail!("PDF image region is empty");
            }
            let mut exports = Vec::with_capacity(reference.regions.len());
            for region in &reference.regions {
                let page_idx = region.page.0;
                if page_idx >= expected_page_count {
                    bail!("PDF image region references missing page {}", page_idx + 1);
                }
                let display_list = if let Some(display_list) = display_lists.get(&page_idx) {
                    display_list.clone()
                } else {
                    let page = document
                        .load_page(page_idx as i32)
                        .with_context(|| format!("failed to load PDF page {}", page_idx + 1))?;
                    let page_bounds = page.bounds().with_context(|| {
                        format!("failed to read bounds for PDF page {}", page_idx + 1)
                    })?;
                    let mut display_list = DisplayList::new(page_bounds).with_context(|| {
                        format!(
                            "failed to create display list for PDF page {}",
                            page_idx + 1
                        )
                    })?;
                    let device =
                        Device::from_display_list(&mut display_list).with_context(|| {
                            format!(
                                "failed to create display-list device for PDF page {}",
                                page_idx + 1
                            )
                        })?;
                    page.run(&device, &Matrix::IDENTITY)
                        .with_context(|| format!("failed to record PDF page {}", page_idx + 1))?;
                    drop(device);
                    let display_list = Arc::new(display_list);
                    page_bounds_by_index.insert(page_idx, page_bounds);
                    display_lists.insert(page_idx, display_list.clone());
                    display_list
                };
                let page_bounds = page_bounds_by_index
                    .get(&page_idx)
                    .copied()
                    .context("PDF page bounds were not cached")?;
                let normalized = region.normalized();
                if !normalized.x0.is_finite()
                    || !normalized.y0.is_finite()
                    || !normalized.x1.is_finite()
                    || !normalized.y1.is_finite()
                    || normalized.x0 < page_bounds.x0
                    || normalized.y0 < page_bounds.y0
                    || normalized.x1 > page_bounds.x1
                    || normalized.y1 > page_bounds.y1
                    || normalized.x1 <= normalized.x0
                    || normalized.y1 <= normalized.y0
                {
                    bail!("PDF image region is outside page {} bounds", page_idx + 1);
                }
                exports.push(RegionImageExport {
                    display_list,
                    page_bounds,
                    region: *region,
                    rotation,
                });
            }

            let bytes = Self::render_region_jpeg(&exports, image_quality)
                .context("failed to render PDF image region")?;
            total_bytes = total_bytes.saturating_add(bytes.len());
            if total_bytes > MAX_PAGE_IMAGE_BYTES {
                bail!(
                    "attached page images are too large: more than {} MiB",
                    MAX_PAGE_IMAGE_BYTES / (1024 * 1024)
                );
            }
            let image = ImageContext::new_region(reference.clone(), "image/jpeg", bytes);
            resolved.push(ResolvedRegionContext { reference, image });
        }

        Ok(resolved)
    }

    fn extract_page_markdown(display_list: &DisplayList, format: PageTextFormat) -> Result<String> {
        let mut flags = TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::ACCURATE_BBOXES;
        if format == PageTextFormat::Semantic {
            flags |= TextPageFlags::PARAGRAPH_BREAK | TextPageFlags::TABLE_HUNT;
        }
        let text_page = display_list
            .to_text_page(flags)
            .context("failed to create MuPDF structured text page")?;
        let mut blocks = Vec::new();

        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            let mut lines = Vec::new();
            let mut total_font_size = 0.0_f32;
            let mut character_count = 0_usize;
            let mut bold_count = 0_usize;
            for line in block.lines() {
                let mut text = String::new();
                for ch in line.chars() {
                    let Some(c) = ch.char() else {
                        continue;
                    };
                    text.push(c);
                    if !c.is_whitespace() {
                        total_font_size += ch.size().max(0.0);
                        character_count += 1;
                        let is_bold = ch.flags().contains(TextCharFlags::BOLD)
                            || ch.font().is_some_and(|font| font.is_bold());
                        bold_count += usize::from(is_bold);
                    }
                }
                lines.push(text.trim_end_matches(['\r', '\n']).to_string());
            }

            if lines.iter().all(|line| line.trim().is_empty()) {
                continue;
            }
            blocks.push(TextFormatBlock {
                lines,
                average_font_size: if character_count == 0 {
                    0.0
                } else {
                    total_font_size / character_count as f32
                },
                bold_ratio: if character_count == 0 {
                    0.0
                } else {
                    bold_count as f32 / character_count as f32
                },
                layout: None,
            });
        }

        Ok(match format {
            PageTextFormat::Block => Self::format_block_markdown(&blocks),
            PageTextFormat::Semantic => Self::format_semantic_markdown(&blocks),
        })
    }

    pub(super) fn format_block_markdown(blocks: &[TextFormatBlock]) -> String {
        blocks
            .iter()
            .map(|block| {
                block
                    .lines
                    .iter()
                    .map(|line| Self::escape_commonmark(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub(super) fn format_semantic_markdown(blocks: &[TextFormatBlock]) -> String {
        let mut font_sizes = blocks
            .iter()
            .filter(|block| block.average_font_size.is_finite() && block.average_font_size > 0.0)
            .map(|block| block.average_font_size)
            .collect::<Vec<_>>();
        font_sizes.sort_by(f32::total_cmp);
        let baseline = font_sizes
            .get(font_sizes.len().saturating_sub(1) / 2)
            .copied()
            .unwrap_or(0.0);

        blocks
            .iter()
            .map(|block| {
                if let Some(list) = Self::semantic_list_markdown(&block.lines) {
                    return list;
                }
                if let Some(level) = Self::semantic_heading_level(block, baseline) {
                    let heading = block.lines[0].trim();
                    return format!("{} {}", "#".repeat(level), Self::escape_commonmark(heading));
                }

                block
                    .lines
                    .iter()
                    .map(|line| Self::escape_commonmark(line))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn semantic_heading_level(block: &TextFormatBlock, baseline: f32) -> Option<usize> {
        if baseline <= 0.0 || block.lines.len() != 1 {
            return None;
        }
        let text = block.lines[0].trim();
        if text.is_empty()
            || text.chars().count() > 120
            || text.ends_with(['.', ',', ';', ':', '?', '!'])
        {
            return None;
        }

        let relative_size = block.average_font_size / baseline;
        if relative_size >= 1.6 {
            Some(1)
        } else if relative_size >= 1.35 {
            Some(2)
        } else if relative_size >= 1.18 && block.bold_ratio >= 0.5 {
            Some(3)
        } else {
            None
        }
    }

    fn semantic_list_markdown(lines: &[String]) -> Option<String> {
        let non_empty = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if non_empty.is_empty() {
            return None;
        }

        let mut items = Vec::with_capacity(non_empty.len());
        for line in non_empty {
            let trimmed = line.trim_start();
            if let Some(item) = Self::strip_unordered_list_marker(trimmed) {
                items.push(format!("- {}", Self::escape_commonmark(item)));
                continue;
            }
            if let Some((number, item)) = Self::strip_ordered_list_marker(trimmed) {
                items.push(format!("{number}. {}", Self::escape_commonmark(item)));
                continue;
            }
            return None;
        }
        Some(items.join("\n"))
    }

    pub(super) fn strip_unordered_list_marker(line: &str) -> Option<&str> {
        ["•", "◦", "▪", "‣", "\u{f0a2}", "-", "*", "+"]
            .into_iter()
            .find_map(|marker| {
                line.strip_prefix(marker)
                    .and_then(|rest| rest.strip_prefix(char::is_whitespace))
                    .map(str::trim_start)
                    .filter(|item| !item.is_empty())
            })
    }

    pub(super) fn strip_ordered_list_marker(line: &str) -> Option<(&str, &str)> {
        let digit_count = line.bytes().take_while(u8::is_ascii_digit).count();
        if digit_count == 0 {
            return None;
        }
        let number = &line[..digit_count];
        let rest = &line[digit_count..];
        let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
        let item = rest.strip_prefix(char::is_whitespace)?.trim_start();
        (!item.is_empty()).then_some((number, item))
    }

    fn escape_commonmark(text: &str) -> String {
        let mut escaped = String::with_capacity(text.len());
        for c in text.chars() {
            if matches!(
                c,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '#'
                    | '+'
                    | '-'
                    | '.'
                    | '!'
                    | '|'
            ) {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        escaped
    }

    pub(super) fn page_image_dimensions(
        page_width: f32,
        page_height: f32,
        max_dimension: u32,
    ) -> Result<(u32, u32)> {
        if !page_width.is_finite()
            || !page_height.is_finite()
            || page_width <= 0.0
            || page_height <= 0.0
            || max_dimension == 0
        {
            bail!("PDF page has invalid image dimensions");
        }
        let scale = max_dimension as f32 / page_width.max(page_height);
        let width = (page_width * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        let height = (page_height * scale)
            .round()
            .clamp(1.0, max_dimension as f32) as u32;
        Ok((width, height))
    }

    pub(super) fn render_page_jpeg(
        display_list: &DisplayList,
        page_bounds: Rect,
        page_width: f32,
        page_height: f32,
        quality: PageImageQuality,
        rotation: DocumentRotation,
    ) -> Result<Vec<u8>> {
        let (display_width, display_height) =
            Self::rotated_dimensions(page_width, page_height, rotation);
        let (width, height) =
            Self::page_image_dimensions(display_width, display_height, quality.max_dimension())?;
        let mut pixmap = Pixmap::new_with_w_h(
            &Colorspace::device_rgb(),
            width as i32,
            height as i32,
            false,
        )
        .context("failed to allocate page image")?;
        pixmap.samples_mut().fill(255);

        let zoom_x = width as f32 / display_width;
        let zoom_y = height as f32 / display_height;
        let matrix = Self::render_matrix(
            page_bounds,
            rotation,
            zoom_x,
            zoom_y,
            mupdf::Point { x: 0.0, y: 0.0 },
        );
        {
            let device =
                Device::from_pixmap(&pixmap).context("failed to create page image device")?;
            display_list
                .run(&device, &matrix, Rect::INF)
                .context("failed to rasterize page image")?;
        }

        let samples = pixmap.samples();
        let expected_len = width as usize * height as usize * 3;
        if samples.len() != expected_len || pixmap.n() != 3 {
            bail!(
                "unexpected page image pixmap layout: {} samples, {} components",
                samples.len(),
                pixmap.n()
            );
        }
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, quality.jpeg_quality())
            .encode(samples, width, height, image::ExtendedColorType::Rgb8)
            .context("failed to encode page image as JPEG")?;
        Ok(bytes)
    }
}

impl AgentToolExecutor for DocumentAgentTools {
    fn definitions(&self) -> Vec<AgentToolDefinition> {
        let mut search = AgentToolDefinition::new(
            "search_document",
            "Search the open document for text. Returns concise matches with page numbers for PDFs and character locations for reflowable documents.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Text to search for"},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": 20}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        );
        search.strict = false;
        let mut read = AgentToolDefinition::new(
            "read_document",
            "Read text from the open document. For PDFs pass pages (1-based); for EPUB pass offset and max_chars. If omitted, reads near the current location.",
            json!({
                "type": "object",
                "properties": {
                    "pages": {
                        "type": "array",
                        "items": {"type": "integer", "minimum": 1},
                        "maxItems": 12
                    },
                    "start_page": {"type": "integer", "minimum": 1},
                    "end_page": {"type": "integer", "minimum": 1},
                    "offset": {"type": "integer", "minimum": 0},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": 12000}
                },
                "additionalProperties": false
            }),
        );
        read.strict = false;
        let mut render = AgentToolDefinition::new(
            "render_document_pages",
            "Render one or more 1-based PDF pages as images. This tool is unavailable for EPUB documents.",
            json!({
                "type": "object",
                "properties": {
                    "pages": {
                        "type": "array",
                        "items": {"type": "integer", "minimum": 1},
                        "minItems": 1
                    }
                },
                "required": ["pages"],
                "additionalProperties": false
            }),
        );
        render.strict = false;
        let mut definitions = vec![search, read];
        if self.reflow_text.is_none() && matches!(self.document_layout, DocumentLayout::Fixed) {
            definitions.push(render);
        }
        definitions
    }

    fn execute(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> AgentToolResult {
        let result = match call.name.as_str() {
            "search_document" => self.search_document(call, cancellation),
            "read_document" => self.read_document(call, cancellation),
            "render_document_pages" => self.render_document_pages(call, cancellation),
            _ => Err(anyhow!("unknown document tool: {}", call.name)),
        };
        match result {
            Ok(content) => AgentToolResult {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                content,
                is_error: false,
            },
            Err(error) => {
                AgentToolResult::error(call.id.clone(), call.name.clone(), format!("{error:#}"))
            }
        }
    }
}

impl DocumentAgentTools {
    fn search_document(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> Result<Vec<AgentToolResultPart>> {
        let arguments = call.arguments_json()?;
        let query = arguments["query"]
            .as_str()
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| anyhow!("query must be a non-empty string"))?;
        let max_results = arguments["max_results"].as_u64().unwrap_or(8).clamp(1, 20) as usize;
        if cancellation.is_canceled() {
            bail!("agent run canceled");
        }

        let mut matches = Vec::new();
        if let Some(text) = &self.reflow_text {
            let mut char_offset = 0usize;
            for (line_index, line) in text.lines().enumerate() {
                if cancellation.is_canceled() {
                    bail!("agent run canceled");
                }
                if line.to_lowercase().contains(&query.to_lowercase()) {
                    matches.push(format!(
                        "- location {char_offset} (line {}): {}",
                        line_index + 1,
                        compact_tool_snippet(line, 420)
                    ));
                    if matches.len() >= max_results {
                        break;
                    }
                }
                char_offset = char_offset.saturating_add(line.chars().count() + 1);
            }
        } else {
            let references = (1..=self.page_count)
                .map(|page| MentionReference::new(MentionKind::Page, Some(page)))
                .collect();
            let pages = DocumentView::resolve_page_contexts(
                &self.snapshot,
                self.document_layout,
                self.page_count,
                self.text_format,
                self.image_quality,
                self.rotation,
                references,
            )?;
            let query = query.to_lowercase();
            for page in pages {
                if cancellation.is_canceled() {
                    bail!("agent run canceled");
                }
                let ResolvedPageContext::Text(page) = page else {
                    continue;
                };
                for line in page.text.lines() {
                    if line.to_lowercase().contains(&query) {
                        matches.push(format!(
                            "- page {}: {}",
                            page.page.unwrap_or(0),
                            compact_tool_snippet(line, 420)
                        ));
                        if matches.len() >= max_results {
                            break;
                        }
                    }
                }
                if matches.len() >= max_results {
                    break;
                }
            }
        }
        let text = if matches.is_empty() {
            format!("No matches found for {query:?}.")
        } else {
            format!("Matches for {query:?}:\n{}", matches.join("\n"))
        };
        Ok(vec![AgentToolResultPart::Text(text)])
    }

    fn read_document(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> Result<Vec<AgentToolResultPart>> {
        let arguments = call.arguments_json()?;
        if cancellation.is_canceled() {
            bail!("agent run canceled");
        }
        if let Some(text) = &self.reflow_text {
            let offset = arguments["offset"].as_u64().unwrap_or(0) as usize;
            let max_chars = arguments["max_chars"]
                .as_u64()
                .unwrap_or(8_000)
                .clamp(1, 12_000) as usize;
            let content = text
                .chars()
                .skip(offset)
                .take(max_chars)
                .collect::<String>();
            if content.is_empty() {
                bail!("offset {offset} is past the end of the document");
            }
            return Ok(vec![AgentToolResultPart::Text(format!(
                "Document text at character offset {offset}:\n{content}"
            ))]);
        }

        let pages = self.requested_pages(&arguments, 12, true)?;
        let references = pages
            .iter()
            .map(|page| MentionReference::new(MentionKind::Page, Some(*page)))
            .collect();
        let resolved = DocumentView::resolve_page_contexts(
            &self.snapshot,
            self.document_layout,
            self.page_count,
            self.text_format,
            self.image_quality,
            self.rotation,
            references,
        )?;
        let mut output = String::new();
        for page in resolved {
            if cancellation.is_canceled() {
                bail!("agent run canceled");
            }
            let ResolvedPageContext::Text(page) = page else {
                continue;
            };
            output.push_str(&format!(
                "## Page {}\n\n{}\n\n",
                page.page.unwrap_or(0),
                page.text
            ));
        }
        Ok(vec![AgentToolResultPart::Text(
            output.trim_end().to_string(),
        )])
    }

    fn render_document_pages(
        &self,
        call: &AgentToolCall,
        cancellation: &AgentCancellationToken,
    ) -> Result<Vec<AgentToolResultPart>> {
        if self.reflow_text.is_some() || !matches!(self.document_layout, DocumentLayout::Fixed) {
            bail!("render_document_pages is available only for PDF documents");
        }
        let arguments = call.arguments_json()?;
        let pages = self.requested_pages(&arguments, self.page_image_limit, false)?;
        let references = pages
            .iter()
            .map(|page| MentionReference::new(MentionKind::PageImage, Some(*page)))
            .collect();
        let resolved = DocumentView::resolve_page_contexts(
            &self.snapshot,
            self.document_layout,
            self.page_count,
            self.text_format,
            self.image_quality,
            self.rotation,
            references,
        )?;
        let mut content = Vec::with_capacity(resolved.len() * 2);
        for page in resolved {
            if cancellation.is_canceled() {
                bail!("agent run canceled");
            }
            let ResolvedPageContext::Image(image) = page else {
                continue;
            };
            let decoded = image::load_from_memory(&image.bytes)
                .context("failed to inspect rendered page image")?;
            let (width, height) = decoded.dimensions();
            let page_number = image.page.unwrap_or(0);
            content.push(AgentToolResultPart::Text(format!(
                "Rendered PDF page {page_number}."
            )));
            content.push(AgentToolResultPart::Image(
                AgentImage::new(image.media_type, width, height, image.bytes)
                    .with_name(format!("page-{page_number}.jpg")),
            ));
        }
        Ok(content)
    }

    fn requested_pages(
        &self,
        arguments: &serde_json::Value,
        limit: usize,
        default_current: bool,
    ) -> Result<Vec<usize>> {
        let mut pages = arguments["pages"]
            .as_array()
            .map(|pages| {
                pages
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .map(|page| page as usize)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if pages.is_empty()
            && let Some(start) = arguments["start_page"].as_u64()
        {
            let start = start as usize;
            let end = arguments["end_page"].as_u64().unwrap_or(start as u64) as usize;
            if end < start {
                bail!("end_page must not be before start_page");
            }
            pages.extend(start..=end);
        }
        if pages.is_empty() && default_current {
            pages.push(self.current_page.max(1));
        }
        pages.sort_unstable();
        pages.dedup();
        if pages.is_empty() {
            bail!("at least one page is required");
        }
        if pages.len() > limit.max(1) {
            bail!("too many pages requested: maximum is {}", limit.max(1));
        }
        if let Some(page) = pages
            .iter()
            .find(|page| **page == 0 || **page > self.page_count)
        {
            bail!(
                "invalid page {page}: page must be between 1 and {}",
                self.page_count
            );
        }
        Ok(pages)
    }
}

fn compact_tool_snippet(text: &str, max_chars: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max_chars {
        text
    } else {
        format!("{}…", text.chars().take(max_chars).collect::<String>())
    }
}
