use super::*;

fn initially_collapsed_toc_entries(toc: &[TocEntry]) -> HashSet<usize> {
    toc.iter()
        .enumerate()
        .filter_map(|(entry_ix, entry)| {
            (entry.has_children && !entry.initially_open).then_some(entry_ix)
        })
        .collect()
}

impl DocumentView {
    pub(super) fn flatten_toc(outlines: Vec<mupdf::Outline>, page_count: usize) -> Vec<TocEntry> {
        let mut entries = Vec::new();
        for outline in outlines {
            Self::push_toc_outline(outline, 0, None, page_count, &mut entries);
        }
        entries
    }

    fn push_toc_outline(
        outline: mupdf::Outline,
        depth: usize,
        parent_ix: Option<usize>,
        page_count: usize,
        entries: &mut Vec<TocEntry>,
    ) {
        let mupdf::Outline {
            title, dest, down, ..
        } = outline;
        let has_children = !down.is_empty();
        let (page_idx, target_top, target_left, navigable) = match dest {
            Some(dest) => {
                let page_idx = dest.loc.page_number as usize;
                let navigable = page_idx < page_count;
                let page_idx = page_idx.min(page_count.saturating_sub(1));
                let (target_top, target_left) = Self::destination_coordinates(dest.kind);
                (page_idx, target_top, target_left, navigable)
            }
            None => (0, None, None, false),
        };

        let entry_ix = entries.len();
        entries.push(TocEntry {
            title: title.into(),
            depth,
            parent_ix,
            has_children,
            page_idx: navigable.then_some(page_idx),
            target_top,
            target_left,
            navigable,
            native_action: None,
            initially_open: true,
            color: None,
            italic: false,
            bold: false,
        });

        for child in down {
            Self::push_toc_outline(child, depth + 1, Some(entry_ix), page_count, entries);
        }
    }

    pub(super) fn flatten_gpu_outline(
        outline: &[GpuOutlineItem],
        page_count: usize,
    ) -> Vec<TocEntry> {
        let mut entries = Vec::new();
        for item in outline {
            Self::push_gpu_outline(item, 0, None, page_count, &mut entries);
        }
        entries
    }

    fn push_gpu_outline(
        item: &GpuOutlineItem,
        depth: usize,
        parent_ix: Option<usize>,
        page_count: usize,
        entries: &mut Vec<TocEntry>,
    ) {
        let has_children = !item.children.is_empty();
        let destination = item
            .action
            .as_ref()
            .and_then(|action| Self::gpu_outline_destination(action, page_count));
        let (page_idx, target_top, target_left) = destination
            .map(|(page_idx, top, left)| (Some(page_idx), top, left))
            .unwrap_or((None, None, None));
        let entry_ix = entries.len();
        entries.push(TocEntry {
            title: item.title.to_string().into(),
            depth,
            parent_ix,
            has_children,
            page_idx,
            target_top,
            target_left,
            navigable: item.action.is_some(),
            native_action: item.action.clone(),
            initially_open: item.initially_open,
            color: item.color,
            italic: item.italic,
            bold: item.bold,
        });

        for child in item.children.iter() {
            Self::push_gpu_outline(child, depth + 1, Some(entry_ix), page_count, entries);
        }
    }

    fn gpu_outline_destination(
        action: &GpuLinkAction,
        page_count: usize,
    ) -> Option<(usize, Option<f32>, Option<f32>)> {
        let GpuLinkAction::GoTo { page, view } = action else {
            return None;
        };
        let page_idx = usize::try_from(*page).ok()?;
        if page_idx >= page_count {
            return None;
        }
        let (target_top, target_left) = match view {
            GpuDestinationView::Xyz { x, y, .. } => {
                (y.map(|value| value as f32), x.map(|value| value as f32))
            }
            GpuDestinationView::FitWidth { top } => (top.map(|value| value as f32), None),
            GpuDestinationView::FitHeight { left } => (None, left.map(|value| value as f32)),
            GpuDestinationView::FitPage => (None, None),
            GpuDestinationView::FitBoundingBox { .. } => (None, None),
            GpuDestinationView::FitBoundingBoxWidth { top, .. } => {
                (top.map(|value| value as f32), None)
            }
            GpuDestinationView::FitBoundingBoxHeight { left, .. } => {
                (None, left.map(|value| value as f32))
            }
            GpuDestinationView::FitRectangle(rect) => (Some(rect[1] as f32), Some(rect[0] as f32)),
        };
        Some((page_idx, target_top, target_left))
    }

    pub(super) fn install_native_gpu_outline(
        &mut self,
        outline: &[GpuOutlineItem],
        cx: &mut Context<Self>,
    ) {
        self.toc = Self::flatten_gpu_outline(outline, self.page_count);
        self.toc_collapsed = initially_collapsed_toc_entries(&self.toc);
        self.toc_sidebar_selected = None;
        self.toc_sidebar_follow_current = true;
        self.toc_sidebar_manual_scroll.set(false);
        self.toc_sidebar_visible_entry_ixs.clear();
        self.toc_sidebar_entries_dirty = true;
        if let Some(palette) = self.toc_palette.clone() {
            let entries = self.toc_palette_entries();
            palette.update(cx, |picker, cx| {
                picker.update_delegate(move |delegate| delegate.replace_entries(entries), cx);
            });
        }
        cx.notify();
    }

    fn destination_coordinates(kind: DestinationKind) -> (Option<f32>, Option<f32>) {
        match kind {
            DestinationKind::XYZ { left, top, .. } => (top, left),
            DestinationKind::FitH { top } | DestinationKind::FitBH { top } => (top, None),
            DestinationKind::FitV { left } | DestinationKind::FitBV { left } => (None, left),
            DestinationKind::FitR { left, top, .. } => (Some(top), Some(left)),
            DestinationKind::Fit | DestinationKind::FitB => (None, None),
        }
    }

    pub(super) fn extract_explicit_links(
        page: &mupdf::Page,
        page_idx: usize,
        page_count: usize,
    ) -> Result<Vec<DocumentLink>> {
        let mut links = Vec::new();

        for link in page.links()? {
            let Some(bounds) = Self::normalized_rect(link.bounds) else {
                continue;
            };

            let target = match link.dest {
                Some(dest) => {
                    let target_page = dest.loc.page_number as usize;
                    if target_page >= page_count {
                        continue;
                    }

                    let (target_top, target_left) = Self::destination_coordinates(dest.kind);
                    LinkTarget::Internal {
                        page_idx: target_page,
                        target_top,
                        target_left,
                    }
                }
                None => {
                    let Some(uri) = Self::normalize_external_uri(&link.uri) else {
                        continue;
                    };
                    LinkTarget::External(uri)
                }
            };

            links.push(DocumentLink {
                page_idx,
                rects: vec![bounds],
                target,
                pdf_annotation: true,
            });
        }

        Ok(links)
    }

    pub(super) fn detect_text_url_links(
        page_idx: usize,
        glyphs: &[Glyph],
        glyph_range: Range<usize>,
        explicit_links: &[DocumentLink],
    ) -> Vec<DocumentLink> {
        let mut chars = Vec::new();
        let mut glyph_indices = Vec::new();

        for glyph_ix in glyph_range {
            let c = glyphs[glyph_ix].c;
            if c.is_control() {
                continue;
            }
            chars.push(c);
            glyph_indices.push(glyph_ix);
        }

        let mut links = Vec::new();
        let mut ix = 0;
        while ix < chars.len() {
            if !Self::url_start_boundary(&chars, ix) {
                ix += 1;
                continue;
            }

            let Some(prefix_len) = Self::detected_url_prefix_len(&chars, ix) else {
                ix += 1;
                continue;
            };

            let mut end = ix + prefix_len;
            while end < chars.len() && Self::is_url_body_char(chars[end]) {
                end += 1;
            }

            let trimmed_end = Self::trim_url_end(&chars, ix, end);
            if trimmed_end <= ix + prefix_len {
                ix += 1;
                continue;
            }

            let raw_url = chars[ix..trimmed_end].iter().collect::<String>();
            let Some(url) = Self::normalize_detected_url(&raw_url) else {
                ix = end.max(ix + 1);
                continue;
            };

            let rects = Self::link_rects_for_glyph_indices(glyphs, &glyph_indices[ix..trimmed_end]);
            if !rects.is_empty()
                && !Self::link_rects_overlap_existing(page_idx, &rects, explicit_links)
            {
                links.push(DocumentLink {
                    page_idx,
                    rects,
                    target: LinkTarget::External(url),
                    pdf_annotation: false,
                });
            }

            ix = end.max(ix + 1);
        }

        links
    }

    fn detected_url_prefix_len(chars: &[char], ix: usize) -> Option<usize> {
        for prefix in ["https://", "http://", "www."] {
            if Self::starts_with_ascii_case_insensitive(chars, ix, prefix) {
                return Some(prefix.len());
            }
        }

        None
    }

    fn starts_with_ascii_case_insensitive(chars: &[char], ix: usize, pattern: &str) -> bool {
        pattern.chars().enumerate().all(|(offset, expected)| {
            chars
                .get(ix + offset)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(&expected))
        })
    }

    pub(super) fn url_start_boundary(chars: &[char], ix: usize) -> bool {
        ix == 0 || Self::is_url_start_boundary_char(chars[ix - 1])
    }

    fn is_url_start_boundary_char(c: char) -> bool {
        c.is_whitespace()
            || c.is_control()
            || (!c.is_ascii_alphanumeric()
                && !matches!(
                    c,
                    '%' | '/' | '.' | '-' | '_' | '~' | '@' | '&' | '=' | '+' | '#'
                ))
    }

    fn is_url_body_char(c: char) -> bool {
        !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>' | '"' | '“' | '”' | '`')
    }

    fn trim_url_end(chars: &[char], start: usize, mut end: usize) -> usize {
        while end > start && Self::should_trim_url_trailing_char(chars, start, end) {
            end -= 1;
        }

        end
    }

    fn should_trim_url_trailing_char(chars: &[char], start: usize, end: usize) -> bool {
        match chars[end - 1] {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '’' | '”' => true,
            ')' => Self::has_unmatched_trailing_closer(chars, start, end, '(', ')'),
            ']' => Self::has_unmatched_trailing_closer(chars, start, end, '[', ']'),
            '}' => Self::has_unmatched_trailing_closer(chars, start, end, '{', '}'),
            _ => false,
        }
    }

    fn has_unmatched_trailing_closer(
        chars: &[char],
        start: usize,
        end: usize,
        open: char,
        close: char,
    ) -> bool {
        let mut opens = 0;
        let mut closes = 0;
        for c in &chars[start..end] {
            if *c == open {
                opens += 1;
            } else if *c == close {
                closes += 1;
            }
        }

        closes > opens
    }

    fn normalize_detected_url(url: &str) -> Option<String> {
        if Self::starts_with_ascii_case_insensitive(&url.chars().collect::<Vec<_>>(), 0, "www.") {
            Some(format!("https://{url}"))
        } else {
            Self::normalize_external_uri(url)
        }
    }

    fn normalize_external_uri(uri: &str) -> Option<String> {
        let uri = uri.trim();
        if uri.is_empty() {
            return None;
        }

        let lower = uri.to_ascii_lowercase();
        if lower.starts_with("www.") {
            return Some(format!("https://{uri}"));
        }

        let (scheme, _) = uri.split_once(':')?;
        matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto" | "ftp" | "ftps" | "tel"
        )
        .then(|| uri.to_string())
    }

    fn link_rects_for_glyph_indices(glyphs: &[Glyph], glyph_indices: &[usize]) -> Vec<Rect> {
        let mut rects = Vec::new();
        let mut current_line = None;
        let mut current_rect = None;

        for &glyph_ix in glyph_indices {
            let glyph = &glyphs[glyph_ix];
            let Some(rect) = Self::normalized_rect(Self::quad_rect(&glyph.quad)) else {
                continue;
            };

            match (current_line, current_rect) {
                (Some(line), Some(existing)) if line == glyph.line_idx => {
                    current_rect = Self::normalized_rect(Self::union_rect(existing, rect));
                }
                (Some(_), Some(existing)) => {
                    rects.push(existing);
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
                _ => {
                    current_line = Some(glyph.line_idx);
                    current_rect = Some(rect);
                }
            }
        }

        if let Some(rect) = current_rect {
            rects.push(rect);
        }

        rects
    }

    fn link_rects_overlap_existing(
        page_idx: usize,
        rects: &[Rect],
        existing_links: &[DocumentLink],
    ) -> bool {
        existing_links
            .iter()
            .filter(|link| link.page_idx == page_idx)
            .any(|link| {
                rects.iter().any(|rect| {
                    link.rects
                        .iter()
                        .any(|existing| Self::rects_intersect(*rect, *existing))
                })
            })
    }

    pub(super) fn native_link_mapping(
        page_idx: usize,
        links: &[DocumentLink],
        interactions: &[GpuLinkInteraction],
    ) -> Option<Vec<(usize, usize)>> {
        let explicit_links = links
            .iter()
            .enumerate()
            .filter(|(_, link)| link.page_idx == page_idx && link.pdf_annotation)
            .collect::<Vec<_>>();
        if explicit_links.len() != interactions.len() {
            return None;
        }

        let mut claimed = vec![false; explicit_links.len()];
        let mut mapping = Vec::with_capacity(interactions.len());
        for (interaction_ix, interaction) in interactions.iter().enumerate() {
            let explicit_ix =
                explicit_links
                    .iter()
                    .enumerate()
                    .find_map(|(explicit_ix, (_, link))| {
                        (!claimed[explicit_ix]
                            && Self::native_link_annotation_matches_reader(
                                interaction.annotation_bounds,
                                &link.rects,
                            ))
                        .then_some(explicit_ix)
                    })?;
            claimed[explicit_ix] = true;
            mapping.push((explicit_links[explicit_ix].0, interaction_ix));
        }
        Some(mapping)
    }

    fn native_link_annotation_matches_reader(annotation_bounds: [f64; 4], rects: &[Rect]) -> bool {
        let [rect] = rects else {
            return false;
        };
        Self::native_link_region_matches_rect(annotation_bounds, *rect)
    }

    fn native_link_region_matches_rect(region: [f64; 4], rect: Rect) -> bool {
        const EPSILON: f64 = 0.01;
        let expected = [
            f64::from(rect.x0),
            f64::from(rect.y0),
            f64::from(rect.x1),
            f64::from(rect.y1),
        ];
        region
            .into_iter()
            .zip(expected)
            .all(|(actual, expected)| actual.is_finite() && (actual - expected).abs() <= EPSILON)
    }

    pub(super) fn native_link_interaction_for_link(
        &self,
        link_ix: usize,
    ) -> Option<&GpuLinkInteraction> {
        let link = self.links.get(link_ix)?;
        if !link.pdf_annotation {
            return None;
        }
        let interactions = self.native_gpu_link_interactions(link.page_idx)?;
        let interaction_ix = Self::native_link_mapping(link.page_idx, &self.links, interactions)?
            .into_iter()
            .find_map(|(candidate_link_ix, interaction_ix)| {
                (candidate_link_ix == link_ix).then_some(interaction_ix)
            })?;
        interactions.get(interaction_ix)
    }

    #[ztracing::instrument(skip_all, fields(page_idx))]
    pub(super) fn extract_glyphs(
        display_list: &DisplayList,
        page_idx: usize,
        glyphs: &mut Vec<Glyph>,
        text_lines: &mut Vec<TextLine>,
        next_line_idx: &mut u32,
        next_block_idx: &mut u32,
    ) -> Result<Range<usize>> {
        let start = glyphs.len();
        // Annotation and selection geometry should follow the font's nominal
        // ascender/descender box, not the outline of whichever character
        // happens to be on a line. MuPDF's accurate boxes are ink bounds and
        // therefore make otherwise-identical lines containing `g`, `A`, or
        // punctuation different heights.
        let text_page = display_list.to_text_page(TextPageFlags::PRESERVE_WHITESPACE)?;

        for block in text_page.blocks() {
            if block.r#type() != TextBlockType::Text {
                continue;
            }

            let block_idx = *next_block_idx;
            *next_block_idx = next_block_idx.saturating_add(1);

            for line in block.lines() {
                let line_idx = *next_line_idx;
                let line_rect = line.bounds();
                let line_start = glyphs.len();
                for ch in line.chars() {
                    if let Some(c) = ch.char() {
                        glyphs.push(Glyph {
                            c,
                            quad: ch.quad(),
                            line_rect,
                            page_idx,
                            line_idx,
                            block_idx,
                            font_size: ch.size(),
                            is_bold: ch.flags().contains(TextCharFlags::BOLD)
                                || ch.font().is_some_and(|font| font.is_bold()),
                            inferred_space_after: false,
                        });
                    }
                }

                let line_end = glyphs.len();
                if line_start < line_end {
                    Self::mark_inferred_spaces(&mut glyphs[line_start..line_end]);
                    text_lines.push(TextLine {
                        glyph_range: line_start..line_end,
                        bounds: line_rect,
                    });
                }
                *next_line_idx = next_line_idx.saturating_add(1);
            }
        }

        Ok(start..glyphs.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_link(page_idx: usize, rects: Vec<Rect>, pdf_annotation: bool) -> DocumentLink {
        DocumentLink {
            page_idx,
            rects,
            target: LinkTarget::External("https://example.com".into()),
            pdf_annotation,
        }
    }

    fn interaction(annotation_bounds: [f64; 4]) -> GpuLinkInteraction {
        let [x0, y0, x1, y1] = annotation_bounds;
        GpuLinkInteraction {
            annotation_bounds,
            regions: Arc::from([[x0, y0, x1, y0, x1, y1, x0, y1]]),
            highlight_mode: GpuLinkHighlightMode::Invert,
            action: GpuLinkAction::Uri(Arc::from("https://example.com")),
        }
    }

    fn link_pdf() -> Vec<u8> {
        b"%PDF-1.4\n\
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj\n\
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj\n\
3 0 obj <</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Annots[5 0 R]>> endobj\n\
4 0 obj <</Length 0>> stream\nendstream endobj\n\
5 0 obj <</Type/Annot/Subtype/Link/Rect[10 10 50 30]/Border[0 0 0]/A<</S/URI/URI(https://example.com)>>>> endobj\n\
trailer <</Root 1 0 R>>\n%%EOF"
            .to_vec()
    }

    fn outline(title: &str, down: Vec<mupdf::Outline>) -> mupdf::Outline {
        mupdf::Outline {
            title: title.to_string(),
            uri: None,
            dest: None,
            down,
        }
    }

    #[test]
    fn flatten_toc_records_direct_parent_indices() {
        let toc = DocumentView::flatten_toc(
            vec![outline(
                "root",
                vec![
                    outline("first", vec![outline("grandchild", Vec::new())]),
                    outline("second", Vec::new()),
                ],
            )],
            1,
        );

        assert_eq!(toc.len(), 4);
        assert_eq!(toc[0].parent_ix, None);
        assert_eq!(toc[1].parent_ix, Some(0));
        assert_eq!(toc[2].parent_ix, Some(1));
        assert_eq!(toc[3].parent_ix, Some(0));
        assert!(toc[0].has_children);
        assert!(toc[1].has_children);
        assert!(!toc[2].has_children);
    }

    #[test]
    fn flatten_gpu_outline_retains_actions_style_and_initial_open_state() {
        let outline = [GpuOutlineItem {
            title: Arc::from("Native root"),
            action: Some(GpuLinkAction::GoTo {
                page: 1,
                view: GpuDestinationView::Xyz {
                    x: Some(12.0),
                    y: Some(34.0),
                    zoom: Some(2.0),
                },
            }),
            children: Arc::from([GpuOutlineItem {
                title: Arc::from("External child"),
                action: Some(GpuLinkAction::Uri(Arc::from("https://example.com"))),
                children: Arc::from([]),
                initially_open: false,
                color: None,
                italic: false,
                bold: false,
            }]),
            initially_open: false,
            color: Some([0.1, 0.2, 0.3]),
            italic: true,
            bold: true,
        }];

        let toc = DocumentView::flatten_gpu_outline(&outline, 2);
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].page_idx, Some(1));
        assert_eq!(toc[0].target_top, Some(34.0));
        assert_eq!(toc[0].target_left, Some(12.0));
        assert_eq!(toc[0].color, Some([0.1, 0.2, 0.3]));
        assert!(toc[0].italic);
        assert!(toc[0].bold);
        assert_eq!(toc[1].parent_ix, Some(0));
        assert!(toc[1].navigable);
        assert_eq!(toc[1].page_idx, None);
        assert_eq!(initially_collapsed_toc_entries(&toc), HashSet::from([0]));
    }

    #[test]
    fn native_link_mapping_ignores_detected_urls() {
        let links = vec![
            document_link(0, vec![Rect::new(10.0, 20.0, 30.0, 40.0)], true),
            document_link(0, vec![Rect::new(50.0, 60.0, 70.0, 80.0)], false),
        ];
        let interactions = [interaction([10.0, 20.0, 30.0, 40.0])];

        assert_eq!(
            DocumentView::native_link_mapping(0, &links, &interactions),
            Some(vec![(0, 0)])
        );
    }

    #[test]
    fn native_link_mapping_matches_mupdf_and_engine_canonical_coordinates() {
        let document = mupdf::Document::from_bytes(&link_pdf(), "application/pdf").unwrap();
        let page = document.load_page(0).unwrap();
        let links = DocumentView::extract_explicit_links(&page, 0, 1).unwrap();
        let interactions = [interaction([10.0, 70.0, 50.0, 90.0])];

        assert_eq!(links[0].rects, vec![Rect::new(10.0, 70.0, 50.0, 90.0)]);
        assert_eq!(
            DocumentView::native_link_mapping(0, &links, &interactions),
            Some(vec![(0, 0)])
        );
    }

    #[test]
    fn native_link_mapping_reconciles_annotation_rect_not_quad_count() {
        let links = [document_link(
            0,
            vec![Rect::new(10.0, 20.0, 90.0, 80.0)],
            true,
        )];
        let interactions = [GpuLinkInteraction {
            annotation_bounds: [10.0, 20.0, 90.0, 80.0],
            regions: Arc::from([
                [12.0, 22.0, 42.0, 22.0, 40.0, 35.0, 14.0, 35.0],
                [50.0, 55.0, 88.0, 55.0, 86.0, 76.0, 52.0, 76.0],
            ]),
            highlight_mode: GpuLinkHighlightMode::Invert,
            action: GpuLinkAction::Uri(Arc::from("https://example.com")),
        }];

        assert_eq!(
            DocumentView::native_link_mapping(0, &links, &interactions),
            Some(vec![(0, 0)])
        );
    }

    #[test]
    fn native_link_mapping_requires_a_geometry_bijection() {
        let links = vec![
            document_link(0, vec![Rect::new(50.0, 60.0, 70.0, 80.0)], true),
            document_link(0, vec![Rect::new(10.0, 20.0, 30.0, 40.0)], true),
        ];
        let interactions = [
            interaction([10.0, 20.0, 30.0, 40.0]),
            interaction([50.0, 60.0, 70.0, 80.0]),
        ];

        assert_eq!(
            DocumentView::native_link_mapping(0, &links, &interactions),
            Some(vec![(1, 0), (0, 1)])
        );
        assert!(
            DocumentView::native_link_mapping(0, &links, &[interaction([10.0, 20.0, 31.0, 40.0])])
                .is_none()
        );
    }

    #[test]
    fn native_link_mapping_requires_all_explicit_annotations() {
        let links = [document_link(
            0,
            vec![Rect::new(10.0, 20.0, 30.0, 40.0)],
            true,
        )];

        assert!(DocumentView::native_link_mapping(0, &links, &[]).is_none());
    }
}
