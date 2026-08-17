use super::*;

impl DocumentView {
    pub(super) fn glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        self.screen_to_pdf(page_idx, point)
            .and_then(|point| self.glyph_at_pdf(page_idx, point))
    }

    pub(super) fn word_bounds_at_glyph(&self, ix: usize) -> Option<Range<usize>> {
        Self::word_bounds_in_glyphs(&self.glyphs, ix)
    }

    pub(super) fn word_bounds_in_glyphs(glyphs: &[Glyph], ix: usize) -> Option<Range<usize>> {
        let glyph = glyphs.get(ix)?;
        if !Self::is_word_char(glyph.c) {
            return None;
        }

        let mut start = ix;
        while start > 0 {
            let previous = &glyphs[start - 1];
            if !Self::same_word_segment(previous, glyph)
                || !Self::is_word_char(previous.c)
                || Self::has_inferred_space_between(glyphs, start - 1, start)
            {
                break;
            }
            start -= 1;
        }

        let mut end = ix + 1;
        while end < glyphs.len() {
            let next = &glyphs[end];
            if !Self::same_word_segment(next, glyph)
                || !Self::is_word_char(next.c)
                || Self::has_inferred_space_between(glyphs, end - 1, end)
            {
                break;
            }
            end += 1;
        }

        Some(start..end)
    }

    #[cfg(test)]
    pub(super) fn word_bounds_in_chars(chars: &[char], ix: usize) -> Option<Range<usize>> {
        if !Self::is_word_char(*chars.get(ix)?) {
            return None;
        }

        let mut start = ix;
        while start > 0 && Self::is_word_char(chars[start - 1]) {
            start -= 1;
        }

        let mut end = ix + 1;
        while end < chars.len() && Self::is_word_char(chars[end]) {
            end += 1;
        }

        Some(start..end)
    }

    fn same_word_segment(a: &Glyph, b: &Glyph) -> bool {
        a.page_idx == b.page_idx && a.block_idx == b.block_idx && a.line_idx == b.line_idx
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || matches!(c, '_' | '\'' | '’')
    }

    fn has_inferred_space_between(glyphs: &[Glyph], left_ix: usize, right_ix: usize) -> bool {
        let Some(left) = glyphs.get(left_ix) else {
            return false;
        };
        let Some(right) = glyphs.get(right_ix) else {
            return false;
        };

        Self::same_word_segment(left, right) && left.inferred_space_after
    }

    pub(super) fn mark_inferred_spaces(glyphs: &mut [Glyph]) {
        let mut space_widths = glyphs
            .iter()
            .filter(|glyph| glyph.c.is_whitespace() && !glyph.c.is_control())
            .filter_map(|glyph| {
                let rect = Self::quad_rect(&glyph.quad);
                let width = rect.x1 - rect.x0;
                (width > 0.0).then_some(width)
            })
            .collect::<Vec<_>>();
        space_widths.sort_by(|left, right| left.total_cmp(right));
        let explicit_space_threshold = space_widths
            .get(space_widths.len() / 2)
            .map(|width| width * INFERRED_SPACE_WIDTH_RATIO);

        for index in 0..glyphs.len().saturating_sub(1) {
            let (left, right) = (&glyphs[index], &glyphs[index + 1]);
            if left.c.is_whitespace()
                || right.c.is_whitespace()
                || left.c.is_control()
                || right.c.is_control()
            {
                continue;
            }

            let left_rect = Self::quad_rect(&left.quad);
            let right_rect = Self::quad_rect(&right.quad);
            let gap = right_rect.x0 - left_rect.x1;
            if gap <= 0.0 {
                continue;
            }
            let fallback_threshold = (left_rect.y1 - left_rect.y0) * INFERRED_SPACE_HEIGHT_RATIO;
            if gap >= explicit_space_threshold.unwrap_or(fallback_threshold) {
                glyphs[index].inferred_space_after = true;
            }
        }
    }

    pub(super) fn nearby_glyph_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let bounds = self.page_image_bounds.get(page_idx)?.as_ref()?;
        let (page_width, page_height) = self.page_dimensions(page_idx);
        let scale = (bounds.size.width.as_f32() / page_width)
            .min(bounds.size.height.as_f32() / page_height);
        if scale <= 0.0 {
            return None;
        }

        let point = self.screen_to_pdf(page_idx, point)?;
        self.nearby_glyph_at_pdf(page_idx, point, INITIAL_SELECTION_TOLERANCE_PX / scale)
    }

    fn exact_glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        self.text_lines[line_range]
            .iter()
            .filter(|line| line.bounds.contains(point.x, point.y))
            .find_map(|line| {
                self.glyphs[line.glyph_range.clone()]
                    .iter()
                    .enumerate()
                    .find_map(|(offset, glyph)| {
                        Self::quad_rect(&glyph.quad)
                            .contains(point.x, point.y)
                            .then_some(line.glyph_range.start + offset)
                    })
            })
    }

    fn nearby_glyph_at_pdf(
        &self,
        page_idx: usize,
        point: mupdf::Point,
        tolerance: f32,
    ) -> Option<usize> {
        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        self.text_lines[line_range]
            .iter()
            .flat_map(|line| {
                self.glyphs[line.glyph_range.clone()]
                    .iter()
                    .enumerate()
                    .map(move |(offset, glyph)| (line.glyph_range.start + offset, glyph))
            })
            .filter_map(|(glyph_ix, glyph)| {
                let distance = Self::distance_to_rect(point, Self::quad_rect(&glyph.quad));
                (distance <= tolerance).then_some((glyph_ix, distance))
            })
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(ix, _)| ix)
    }

    fn glyph_at_pdf(&self, page_idx: usize, point: mupdf::Point) -> Option<usize> {
        if self.pages.get(page_idx)?.glyph_range.is_empty() {
            return None;
        }

        if let Some(ix) = self.exact_glyph_at_pdf(page_idx, point) {
            return Some(ix);
        }

        let (_, start, end, line_rect) = self.nearest_line(page_idx, point)?;
        if point.x <= line_rect.x0 {
            return Some(start);
        }
        if point.x >= line_rect.x1 {
            return Some(end);
        }

        let mut nearest = start;
        let mut nearest_dist = f32::INFINITY;
        for ix in start..=end {
            let rect = Self::quad_rect(&self.glyphs[ix].quad);
            let center_x = (rect.x0 + rect.x1) / 2.0;
            let dist = (point.x - center_x).abs();
            if dist < nearest_dist {
                nearest = ix;
                nearest_dist = dist;
            }
        }

        Some(nearest)
    }

    fn nearest_line(
        &self,
        page_idx: usize,
        point: mupdf::Point,
    ) -> Option<(f32, usize, usize, Rect)> {
        let line_range = self.page_line_ranges.get(page_idx)?.clone();
        if line_range.start >= line_range.end || line_range.end > self.text_lines.len() {
            return None;
        }

        Self::nearest_line_in_slice(&self.text_lines[line_range], point)
    }

    pub(super) fn nearest_line_in_slice(
        text_lines: &[TextLine],
        point: mupdf::Point,
    ) -> Option<(f32, usize, usize, Rect)> {
        text_lines
            .iter()
            .filter_map(|line| {
                let start = line.glyph_range.start;
                let end = line.glyph_range.end.checked_sub(1)?;
                let rect = line.bounds;
                Some((
                    Self::squared_distance_to_rect(point, rect),
                    start,
                    end,
                    rect,
                ))
            })
            .min_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            })
    }

    pub(super) fn selection_bounds(&self) -> Option<(usize, usize)> {
        let selection = self.selection?;
        let start = selection.anchor.min(selection.head);
        let end = selection.anchor.max(selection.head);

        if start < self.glyphs.len() && end < self.glyphs.len() {
            Some((start, end))
        } else {
            None
        }
    }

    pub(crate) fn selected_text(&self) -> Option<String> {
        self.selected_source_text()
    }

    pub(super) fn selected_source_text(&self) -> Option<String> {
        let (start, end) = self.selection_bounds()?;
        Self::text_for_glyph_range(&self.glyphs, start..end + 1)
    }

    fn selected_text_for_copy(&self, semantic: bool) -> Option<String> {
        if self.translation.view_language == DocumentViewLanguage::Translated || !semantic {
            return self.selected_text();
        }

        let blocks = self.selected_copy_blocks(true)?;
        Self::plain_text_for_blocks(&blocks)
    }

    fn selected_markdown(&self, semantic: bool) -> Option<String> {
        if self.translation.view_language == DocumentViewLanguage::Translated {
            return self
                .selected_text()
                .map(|text| Self::markdown_for_plain_text(&text));
        }

        let blocks = self.selected_copy_blocks(semantic)?;
        Some(Self::format_semantic_markdown(&blocks))
    }

    fn selected_copy_blocks(&self, semantic: bool) -> Option<Vec<TextFormatBlock>> {
        let (start, end) = self.selection_bounds()?;
        Self::copy_blocks_for_glyph_range(&self.glyphs, start..end + 1, semantic)
    }

    pub(super) fn copy_blocks_for_glyph_range(
        glyphs: &[Glyph],
        range: Range<usize>,
        semantic: bool,
    ) -> Option<Vec<TextFormatBlock>> {
        let mut blocks = Self::text_blocks_for_glyph_range(glyphs, range)?;
        if semantic {
            for block in &mut blocks {
                block.lines = Self::semantic_logical_lines(&block.lines);
            }
            blocks = Self::merge_semantic_text_blocks(blocks);
        }
        Some(blocks)
    }

    fn merge_semantic_text_blocks(blocks: Vec<TextFormatBlock>) -> Vec<TextFormatBlock> {
        let mut merged = Vec::<TextFormatBlock>::with_capacity(blocks.len());

        for mut block in blocks {
            let can_merge = merged.last().is_some_and(|previous| {
                Self::text_blocks_are_continuous(previous, &block)
                    && block.lines.first().is_some_and(|line| {
                        let line = line.trim_start();
                        !line.is_empty()
                            && Self::strip_unordered_list_marker(line).is_none()
                            && Self::strip_ordered_list_marker(line).is_none()
                    })
            });

            if can_merge {
                let previous = merged.last_mut().unwrap();
                if let (Some(current), Some(continuation)) =
                    (previous.lines.last_mut(), block.lines.first())
                {
                    Self::append_semantic_continuation(current, continuation.trim_start());
                }
                previous.lines.extend(block.lines.drain(1..));
                previous.average_font_size =
                    (previous.average_font_size + block.average_font_size) / 2.0;
                previous.bold_ratio = (previous.bold_ratio + block.bold_ratio) / 2.0;
                if let (Some(previous_layout), Some(block_layout)) =
                    (&mut previous.layout, block.layout)
                {
                    previous_layout.last_line = block_layout.last_line;
                }
            } else {
                merged.push(block);
            }
        }

        merged
    }

    fn text_blocks_are_continuous(left: &TextFormatBlock, right: &TextFormatBlock) -> bool {
        let (Some(left_layout), Some(right_layout)) = (left.layout, right.layout) else {
            return false;
        };
        if left_layout.page_idx != right_layout.page_idx {
            return false;
        }

        let font_ratio = left.average_font_size.max(right.average_font_size)
            / left
                .average_font_size
                .min(right.average_font_size)
                .max(f32::EPSILON);
        if font_ratio > 1.15 {
            return false;
        }

        let left_rect = left_layout.last_line;
        let right_rect = right_layout.first_line;
        let line_height = (left_rect.y1 - left_rect.y0)
            .max(right_rect.y1 - right_rect.y0)
            .max(1.0);
        let vertical_gap = right_rect.y0 - left_rect.y1;
        if vertical_gap < -line_height * 0.25 || vertical_gap > line_height * 1.5 {
            return false;
        }

        let horizontal_overlap = left_rect.x1.min(right_rect.x1) - left_rect.x0.max(right_rect.x0);
        horizontal_overlap > 0.0
    }

    pub(super) fn plain_text_for_blocks(blocks: &[TextFormatBlock]) -> Option<String> {
        let mut text = String::new();
        for (block_index, block) in blocks.iter().enumerate() {
            if block_index > 0 {
                text.push_str("\n\n");
            }
            for (line_index, line) in block.lines.iter().enumerate() {
                if line_index > 0 {
                    text.push('\n');
                }
                text.push_str(line);
            }
        }
        (!text.is_empty()).then_some(text)
    }

    pub(super) fn semantic_logical_lines(lines: &[String]) -> Vec<String> {
        let mut logical_lines = Vec::<String>::new();

        for line in lines {
            let line = line.trim_end_matches(['\r', '\n']).trim_end();
            if line.is_empty() {
                if logical_lines.last().is_some_and(|line| !line.is_empty()) {
                    logical_lines.push(String::new());
                }
                continue;
            }

            let continuation = line.trim_start();
            let starts_list_item = Self::strip_unordered_list_marker(continuation).is_some()
                || Self::strip_ordered_list_marker(continuation).is_some();
            let should_join = logical_lines
                .last()
                .is_some_and(|line| !line.is_empty() && !starts_list_item);

            if should_join {
                let current = logical_lines.last_mut().unwrap();
                Self::append_semantic_continuation(current, continuation);
            } else {
                logical_lines.push(line.to_string());
            }
        }

        while logical_lines.last().is_some_and(String::is_empty) {
            logical_lines.pop();
        }
        logical_lines
    }

    fn append_semantic_continuation(current: &mut String, continuation: &str) {
        let dehyphenate = current.ends_with('-')
            && current.chars().rev().nth(1).is_some_and(Self::is_word_char)
            && continuation.chars().next().is_some_and(Self::is_word_char);
        if dehyphenate {
            current.pop();
        } else if !current.chars().last().is_some_and(char::is_whitespace) {
            current.push(' ');
        }
        current.push_str(continuation);
    }

    pub(super) fn markdown_for_plain_text(text: &str) -> String {
        let blocks = text
            .split("\n\n")
            .map(|block| TextFormatBlock {
                lines: block.lines().map(str::to_string).collect(),
                average_font_size: 0.0,
                bold_ratio: 0.0,
                layout: None,
            })
            .filter(|block| !block.lines.is_empty())
            .collect::<Vec<_>>();
        Self::format_block_markdown(&blocks)
    }

    pub(super) fn text_blocks_for_glyph_range(
        glyphs: &[Glyph],
        range: Range<usize>,
    ) -> Option<Vec<TextFormatBlock>> {
        if range.start >= range.end || range.end > glyphs.len() {
            return None;
        }

        let mut blocks = Vec::new();
        let mut current_block_idx = None;
        let mut current_line_idx = None;
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut font_size_total = 0.0;
        let mut character_count = 0_usize;
        let mut bold_count = 0_usize;
        let mut previous_ix = None;
        let mut page_idx = None;
        let mut first_line_rect = None;
        let mut last_line_rect = None;

        let flush_line = |lines: &mut Vec<String>, line: &mut String| {
            lines.push(std::mem::take(line));
        };
        let flush_block = |blocks: &mut Vec<TextFormatBlock>,
                           lines: &mut Vec<String>,
                           font_size_total: &mut f32,
                           character_count: &mut usize,
                           bold_count: &mut usize,
                           page_idx: &mut Option<usize>,
                           first_line_rect: &mut Option<Rect>,
                           last_line_rect: &mut Option<Rect>| {
            if lines.iter().all(|line| line.trim().is_empty()) {
                lines.clear();
                *font_size_total = 0.0;
                *character_count = 0;
                *bold_count = 0;
                *page_idx = None;
                *first_line_rect = None;
                *last_line_rect = None;
                return;
            }
            blocks.push(TextFormatBlock {
                lines: std::mem::take(lines),
                average_font_size: if *character_count == 0 {
                    0.0
                } else {
                    *font_size_total / *character_count as f32
                },
                bold_ratio: if *character_count == 0 {
                    0.0
                } else {
                    *bold_count as f32 / *character_count as f32
                },
                layout: match (*page_idx, *first_line_rect, *last_line_rect) {
                    (Some(page_idx), Some(first_line), Some(last_line)) => Some(TextFormatLayout {
                        page_idx,
                        first_line,
                        last_line,
                    }),
                    _ => None,
                },
            });
            *font_size_total = 0.0;
            *character_count = 0;
            *bold_count = 0;
            *page_idx = None;
            *first_line_rect = None;
            *last_line_rect = None;
        };

        for glyph_ix in range {
            let glyph = &glyphs[glyph_ix];
            if current_block_idx != Some(glyph.block_idx) {
                if current_line_idx.is_some() {
                    flush_line(&mut lines, &mut line);
                }
                if current_block_idx.is_some() {
                    flush_block(
                        &mut blocks,
                        &mut lines,
                        &mut font_size_total,
                        &mut character_count,
                        &mut bold_count,
                        &mut page_idx,
                        &mut first_line_rect,
                        &mut last_line_rect,
                    );
                }
                current_block_idx = Some(glyph.block_idx);
                current_line_idx = None;
            } else if current_line_idx != Some(glyph.line_idx) && current_line_idx.is_some() {
                flush_line(&mut lines, &mut line);
            }

            page_idx.get_or_insert(glyph.page_idx);
            first_line_rect.get_or_insert(glyph.line_rect);
            last_line_rect = Some(glyph.line_rect);

            if current_line_idx == Some(glyph.line_idx)
                && let Some(previous_ix) = previous_ix
                && Self::has_inferred_space_between(glyphs, previous_ix, glyph_ix)
            {
                line.push(' ');
            }

            line.push(glyph.c);
            if !glyph.c.is_whitespace() {
                font_size_total += glyph.font_size.max(0.0);
                character_count += 1;
                bold_count += usize::from(glyph.is_bold);
            }
            current_line_idx = Some(glyph.line_idx);
            previous_ix = Some(glyph_ix);
        }

        if current_line_idx.is_some() {
            flush_line(&mut lines, &mut line);
        }
        if current_block_idx.is_some() {
            flush_block(
                &mut blocks,
                &mut lines,
                &mut font_size_total,
                &mut character_count,
                &mut bold_count,
                &mut page_idx,
                &mut first_line_rect,
                &mut last_line_rect,
            );
        }

        (!blocks.is_empty()).then_some(blocks)
    }

    pub(crate) fn document_text(&self) -> Option<String> {
        Self::text_for_glyph_range(&self.glyphs, 0..self.glyphs.len())
    }

    pub(super) fn text_for_glyph_range(glyphs: &[Glyph], range: Range<usize>) -> Option<String> {
        if range.start >= range.end || range.end > glyphs.len() {
            return None;
        }

        let mut text = String::new();
        let mut previous_ix: Option<usize> = None;

        for glyph_ix in range {
            let glyph = &glyphs[glyph_ix];
            if let Some(previous_ix) = previous_ix {
                let previous = &glyphs[previous_ix];

                if glyph.block_idx != previous.block_idx {
                    text.push_str("\n\n");
                } else if glyph.line_idx != previous.line_idx {
                    text.push('\n');
                } else if Self::has_inferred_space_between(glyphs, previous_ix, glyph_ix) {
                    text.push(' ');
                }
            }

            text.push(glyph.c);
            previous_ix = Some(glyph_ix);
        }

        (!text.is_empty()).then_some(text)
    }

    pub(super) fn copy_selection_as(&self, format: CopyFormat, cx: &mut Context<Self>) {
        let semantic = cx.global::<SettingsStore>().settings.behavior.semantic_copy;
        let text = match format {
            CopyFormat::PlainText => self.selected_text_for_copy(semantic),
            CopyFormat::Markdown => self.selected_markdown(semantic),
        };
        if let Some(text) = text {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub(super) fn copy_selection(&self, cx: &mut Context<Self>) {
        let format = cx.global::<SettingsStore>().settings.behavior.copy_format;
        self.copy_selection_as(format, cx);
    }
}
