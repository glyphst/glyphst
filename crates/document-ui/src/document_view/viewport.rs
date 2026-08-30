use super::*;

const ZOOM_RESPONSE_PER_SECOND: f32 = 24.0;
const ZOOM_SNAP: f32 = 0.0005;
const MAX_ZOOM_FRAME_ELAPSED: Duration = Duration::from_micros(16_667);

fn animated_zoom_step(current: f32, target: f32, elapsed: Duration) -> f32 {
    let delta = target - current;
    if delta.abs() <= ZOOM_SNAP {
        return target;
    }
    let elapsed_seconds = elapsed.min(MAX_ZOOM_FRAME_ELAPSED).as_secs_f32();
    let factor = 1.0 - (-ZOOM_RESPONSE_PER_SECOND * elapsed_seconds).exp();
    current + delta * factor
}

impl DocumentView {
    pub(super) fn synchronized_document_delta(pixel_delta: f32, scale: f32) -> f32 {
        pixel_delta / scale.max(0.001)
    }

    pub(super) fn clamp_scroll(&mut self) {
        let max_scroll_x = self.max_scroll_x_doc();
        self.scroll_x_doc = self.scroll_x_doc.clamp(-max_scroll_x, max_scroll_x);
        self.scroll_y_doc = self.scroll_y_doc.clamp(0.0, self.max_scroll_y_doc());
        self.target_scroll_x_doc = self.target_scroll_x_doc.clamp(-max_scroll_x, max_scroll_x);
        self.target_scroll_y_doc = self.target_scroll_y_doc.clamp(0.0, self.max_scroll_y_doc());
    }

    pub(super) fn set_scroll_doc(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.scroll_x_doc;
        let previous_y = self.scroll_y_doc;
        self.scroll_x_doc = scroll_x_doc;
        self.scroll_y_doc = scroll_y_doc;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let changed = (self.scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.mark_native_gpu_motion(cx);
            self.hovered_link = None;
            self.pending_link = None;
            self.schedule_position_save(cx);
            self.emit_live_translation_viewport_changed(SynchronizedScrollMode::Immediate, cx);
            cx.notify();
        }

        changed
    }

    pub(super) fn set_scroll_doc_target(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.target_scroll_x_doc;
        let previous_y = self.target_scroll_y_doc;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let smooth_scrolling = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .smooth_scrolling;
        if smooth_scrolling {
            let viewport_height_doc = self.viewport_height / self.effective_scale();
            let max_animated_distance = viewport_height_doc * SMOOTH_SCROLL_LARGE_JUMP_VIEWPORTS;
            let delta_y = self.target_scroll_y_doc - self.scroll_y_doc;
            if delta_y.abs() > max_animated_distance {
                self.scroll_y_doc =
                    self.target_scroll_y_doc - delta_y.signum() * max_animated_distance;
            }
        } else {
            self.scroll_x_doc = self.target_scroll_x_doc;
            self.scroll_y_doc = self.target_scroll_y_doc;
        }

        let changed = (self.target_scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.target_scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.mark_native_gpu_motion(cx);
            self.hovered_link = None;
            self.pending_link = None;
            self.schedule_position_save(cx);
            self.emit_live_translation_viewport_changed(SynchronizedScrollMode::Target, cx);
            cx.notify();
        }

        changed
    }

    pub(super) fn set_scroll_doc_synchronized(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.scroll_x_doc;
        let previous_y = self.scroll_y_doc;
        let changed = self.set_scroll_doc(scroll_x_doc, scroll_y_doc, cx);
        if changed {
            self.emit_synchronized_scroll(
                self.scroll_x_doc - previous_x,
                self.scroll_y_doc - previous_y,
                SynchronizedScrollMode::Immediate,
                cx,
            );
        }
        changed
    }

    pub(super) fn set_scroll_doc_target_synchronized(
        &mut self,
        scroll_x_doc: f32,
        scroll_y_doc: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let previous_x = self.target_scroll_x_doc;
        let previous_y = self.target_scroll_y_doc;
        let changed = self.set_scroll_doc_target(scroll_x_doc, scroll_y_doc, cx);
        if changed {
            self.emit_synchronized_scroll(
                self.target_scroll_x_doc - previous_x,
                self.target_scroll_y_doc - previous_y,
                SynchronizedScrollMode::Target,
                cx,
            );
        }
        changed
    }

    fn emit_synchronized_scroll(
        &self,
        delta_x_doc: f32,
        delta_y_doc: f32,
        mode: SynchronizedScrollMode,
        cx: &mut Context<Self>,
    ) {
        if self.translation.suppress_linked_events {
            return;
        }
        let scale = self.effective_scale();
        let delta = SynchronizedScrollDelta {
            x_pixels: delta_x_doc * scale,
            y_pixels: delta_y_doc * scale,
            mode,
        };
        if delta.x_pixels.abs() > f32::EPSILON || delta.y_pixels.abs() > f32::EPSILON {
            cx.emit(DocumentViewEvent::SynchronizedScroll(delta));
        }
    }

    pub fn apply_synchronized_scroll_delta(
        &mut self,
        delta: SynchronizedScrollDelta,
        cx: &mut Context<Self>,
    ) {
        if self.is_presentation_mode() {
            return;
        }
        let scale = self.effective_scale().max(0.001);
        self.translation.suppress_linked_events = true;
        match delta.mode {
            SynchronizedScrollMode::Immediate => {
                self.set_scroll_doc(
                    self.scroll_x_doc + Self::synchronized_document_delta(delta.x_pixels, scale),
                    self.scroll_y_doc + Self::synchronized_document_delta(delta.y_pixels, scale),
                    cx,
                );
            }
            SynchronizedScrollMode::Target => {
                self.set_scroll_doc_target(
                    self.target_scroll_x_doc
                        + Self::synchronized_document_delta(delta.x_pixels, scale),
                    self.target_scroll_y_doc
                        + Self::synchronized_document_delta(delta.y_pixels, scale),
                    cx,
                );
            }
        }
        self.translation.suppress_linked_events = false;
    }

    pub(super) fn step_scroll_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let smooth_scrolling = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .smooth_scrolling;
        let zoom_pending = (self.target_zoom - self.zoom).abs() > f32::EPSILON;
        let scroll_pending = (self.target_scroll_x_doc - self.scroll_x_doc).abs() > f32::EPSILON
            || (self.target_scroll_y_doc - self.scroll_y_doc).abs() > f32::EPSILON;
        if !zoom_pending && !scroll_pending {
            self.last_animation_frame = None;
            return;
        }
        let now = Instant::now();
        let elapsed = self
            .last_animation_frame
            .replace(now)
            .map_or(Duration::from_secs_f32(1.0 / 120.0), |previous| {
                now.saturating_duration_since(previous)
            });

        let mut changed_zoom = false;
        if zoom_pending {
            let zoom = animated_zoom_step(self.zoom, self.target_zoom, elapsed);
            let anchor = self.zoom_anchor.unwrap_or_else(|| {
                self.zoom_anchor_at_viewport([
                    self.viewport_width / 2.0,
                    self.viewport_height / 2.0,
                ])
            });
            self.apply_zoom_anchor(zoom, anchor);
            changed_zoom = true;
            if (self.target_zoom - self.zoom).abs() <= f32::EPSILON {
                self.zoom_anchor = None;
            }
        }

        let changed_x = advance_toward(
            &mut self.scroll_x_doc,
            self.target_scroll_x_doc,
            smooth_scrolling,
            elapsed,
        );
        let changed_y = advance_toward(
            &mut self.scroll_y_doc,
            self.target_scroll_y_doc,
            smooth_scrolling,
            elapsed,
        );
        if changed_zoom || changed_x || changed_y {
            self.mark_native_gpu_motion(cx);
            window.request_animation_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_zoom_frames_extend_animation_instead_of_jumping() {
        let delayed = animated_zoom_step(1.0, 2.0, Duration::from_millis(64));
        let capped = animated_zoom_step(1.0, 2.0, MAX_ZOOM_FRAME_ELAPSED);
        assert_eq!(delayed, capped);
        assert!(delayed < 1.34, "delayed step jumped to {delayed}");
    }

    #[test]
    fn capped_zoom_animation_still_converges_in_both_directions() {
        let mut zoom = 1.0;
        for _ in 0..100 {
            zoom = animated_zoom_step(zoom, 2.0, Duration::from_millis(64));
        }
        assert_eq!(zoom, 2.0);

        for _ in 0..100 {
            zoom = animated_zoom_step(zoom, 1.0, Duration::from_millis(64));
        }
        assert_eq!(zoom, 1.0);
    }
}
