use super::*;

fn should_hit_test_document_content(dragging: bool, panning: bool) -> bool {
    !dragging && !panning
}

fn native_link_contains_point(interaction: &GpuLinkInteraction, x: f64, y: f64) -> bool {
    !interaction.action.is_inert() && interaction.contains_point(x, y)
}

impl DocumentView {
    pub(super) fn collect_images(display_list: &DisplayList) -> Result<Vec<CollectedImage>> {
        let collector = Rc::new(RefCell::new(ImageCollector::default()));
        let device = Device::from_native(collector.clone())?;
        display_list
            .run(&device, &Matrix::IDENTITY, Rect::INF)
            .context("failed to collect PDF images")?;
        drop(device);

        Ok(std::mem::take(&mut collector.borrow_mut().images))
    }

    pub(super) fn image_at_screen(
        &mut self,
        point: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) -> Option<(usize, usize)> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;
        let display_list = match self.get_or_create_display_list(page_idx, cx) {
            Ok(display_list) => display_list,
            Err(error) => {
                eprintln!("failed to load PDF page for image hit-test: {error:#}");
                return None;
            }
        };
        let images = match Self::collect_images(&display_list) {
            Ok(images) => images,
            Err(error) => {
                eprintln!("failed to hit-test PDF images: {error:#}");
                return None;
            }
        };

        images
            .iter()
            .enumerate()
            .rev()
            .find_map(|(image_idx, image)| {
                Self::image_contains_pdf_point(image, pdf_point).then_some((page_idx, image_idx))
            })
    }

    pub(super) fn link_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<usize> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;
        let native_interactions = self.native_gpu_link_interactions(page_idx);
        let native_mapping = native_interactions.and_then(|interactions| {
            Self::native_link_mapping(page_idx, &self.links, interactions)
        });

        self.links.iter().enumerate().find_map(|(link_ix, link)| {
            if link.page_idx != page_idx {
                return None;
            }
            let native_hit = native_mapping.as_ref().and_then(|mapping| {
                let interaction_ix =
                    mapping
                        .iter()
                        .find_map(|(candidate_link_ix, interaction_ix)| {
                            (*candidate_link_ix == link_ix).then_some(*interaction_ix)
                        })?;
                native_interactions
                    .and_then(|interactions| interactions.get(interaction_ix))
                    .map(|interaction| {
                        native_link_contains_point(
                            interaction,
                            f64::from(pdf_point.x),
                            f64::from(pdf_point.y),
                        )
                    })
            });
            native_hit
                .unwrap_or_else(|| {
                    link.rects
                        .iter()
                        .any(|rect| rect.contains(pdf_point.x, pdf_point.y))
                })
                .then_some(link_ix)
        })
    }

    pub(super) fn highlight_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<u64> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;

        self.highlights.iter().rev().find_map(|highlight| {
            highlight
                .rects
                .iter()
                .any(|rect| rect.contains(page_idx, pdf_point.x, pdf_point.y))
                .then_some(highlight.id)
        })
    }

    pub(super) fn highlight_color_for_id(&self, id: u64) -> Option<HighlightColor> {
        self.highlights
            .iter()
            .find(|highlight| highlight.id == id)
            .map(|highlight| highlight.color)
    }

    pub(super) fn note_at_screen(&self, point: GpuiPoint<Pixels>) -> Option<u64> {
        let page_idx = self.page_at_screen(point)?;
        let pdf_point = self.screen_to_pdf(page_idx, point)?;

        self.notes.iter().rev().find_map(|note| {
            note.rects
                .iter()
                .any(|rect| rect.contains(page_idx, pdf_point.x, pdf_point.y))
                .then_some(note.id)
        })
    }

    pub(super) fn note_body_for_id(&self, id: u64) -> Option<SharedString> {
        self.notes
            .iter()
            .find(|note| note.id == id)
            .map(|note| note.body.clone().into())
    }

    pub(super) fn set_hovered_link(&mut self, link_ix: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered_link == link_ix {
            return;
        }

        self.hovered_link = link_ix;
        cx.notify();
    }

    pub(super) fn set_hovered_note(
        &mut self,
        note_id: Option<u64>,
        position: Option<GpuiPoint<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if self.hovered_note == note_id && self.hovered_note_position == position {
            return;
        }

        self.hovered_note = note_id;
        self.hovered_note_position = position;
        cx.notify();
    }

    pub(super) fn update_hovered_link(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let link_ix = if should_hit_test_document_content(self.dragging, self.panning.is_some()) {
            self.link_at_screen(position)
        } else {
            None
        };
        self.set_hovered_link(link_ix, cx);
    }

    pub(super) fn update_hovered_note(
        &mut self,
        position: GpuiPoint<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let note_id = if should_hit_test_document_content(self.dragging, self.panning.is_some()) {
            self.note_at_screen(position)
        } else {
            None
        };
        let position = note_id.map(|_| position);
        self.set_hovered_note(note_id, position, cx);
    }

    fn image_contains_pdf_point(image: &CollectedImage, point: mupdf::Point) -> bool {
        if !image.bounds.contains(point.x, point.y) {
            return false;
        }

        let Some(inverse) = image.matrix.invert() else {
            return true;
        };

        let (x, y) = inverse.transform_xy(point.x, point.y);
        (-0.001..=1.001).contains(&x) && (-0.001..=1.001).contains(&y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_content_hover_hit_testing_pauses_during_drag_and_pan() {
        assert!(should_hit_test_document_content(false, false));
        assert!(!should_hit_test_document_content(true, false));
        assert!(!should_hit_test_document_content(false, true));
    }

    #[test]
    fn inert_native_links_keep_geometry_but_are_not_hittable() {
        let interaction = GpuLinkInteraction {
            annotation_bounds: [10.0, 20.0, 30.0, 40.0],
            regions: Arc::from([[10.0, 20.0, 30.0, 20.0, 30.0, 40.0, 10.0, 40.0]]),
            highlight_mode: GpuLinkHighlightMode::None,
            action: GpuLinkAction::NoOp {
                code: Arc::from("invalid-link-destination-noop"),
                message: Arc::from("no destination"),
            },
        };

        assert!(interaction.contains_point(20.0, 30.0));
        assert!(!native_link_contains_point(&interaction, 20.0, 30.0));
    }
}
