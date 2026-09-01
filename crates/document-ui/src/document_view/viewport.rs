use super::*;

const ZOOM_RESPONSE_PER_SECOND: f32 = 24.0;
const ZOOM_SNAP: f32 = 0.0005;
const MAX_ZOOM_FRAME_ELAPSED: Duration = Duration::from_micros(16_667);
const GPU_SCROLL_MAX_CAMERA_STEP_PX: f32 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MotionRefresh {
    Zoom,
    Pan,
}

fn motion_refresh(
    changed_zoom: bool,
    changed_x: bool,
    changed_y: bool,
    hold_camera_for_gpu: bool,
    zoom_pending: bool,
) -> Option<MotionRefresh> {
    if changed_zoom || (hold_camera_for_gpu && zoom_pending) {
        Some(MotionRefresh::Zoom)
    } else if changed_x || changed_y {
        Some(MotionRefresh::Pan)
    } else {
        None
    }
}

fn animated_zoom_step(current: f32, target: f32, elapsed: Duration) -> f32 {
    let delta = target - current;
    if delta.abs() <= ZOOM_SNAP {
        return target;
    }
    let elapsed_seconds = elapsed.min(MAX_ZOOM_FRAME_ELAPSED).as_secs_f32();
    let factor = 1.0 - (-ZOOM_RESPONSE_PER_SECOND * elapsed_seconds).exp();
    current + delta * factor
}

fn advance_toward_capped(
    current: &mut f32,
    target: f32,
    smooth: bool,
    elapsed: Duration,
    max_step: f32,
) -> bool {
    let previous = *current;
    _ = advance_toward(current, target, smooth, elapsed);
    *current = current.clamp(previous - max_step, previous + max_step);
    (*current - previous).abs() > f32::EPSILON
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
        if (scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (scroll_y_doc - previous_y).abs() > f32::EPSILON
        {
            // Immediate scroll sources cannot be paused and replayed by the
            // animation driver. Reserve opportunistically before mutating the
            // camera; the paint path still preserves the last sharp frame if
            // the external-texture ring is temporarily busy.
            _ = self.native_gpu_camera_frame_ready();
        }
        self.scroll_x_doc = scroll_x_doc;
        self.scroll_y_doc = scroll_y_doc;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let changed = (self.scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.mark_native_gpu_motion(cx);
            self.record_native_gpu_camera_step(false);
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
        let previous_camera_x = self.scroll_x_doc;
        let previous_camera_y = self.scroll_y_doc;
        let camera_backlog = (previous_x - previous_camera_x).abs() > f32::EPSILON
            || (previous_y - previous_camera_y).abs() > f32::EPSILON;
        self.target_scroll_x_doc = scroll_x_doc;
        self.target_scroll_y_doc = scroll_y_doc;
        self.clamp_scroll();

        let smooth_scrolling = cx
            .global::<SettingsStore>()
            .settings
            .behavior
            .smooth_scrolling;
        let mut admitted_camera = [self.scroll_x_doc, self.scroll_y_doc];
        if smooth_scrolling {
            let viewport_height_doc = self.viewport_height / self.effective_scale();
            let max_animated_distance = viewport_height_doc * SMOOTH_SCROLL_LARGE_JUMP_VIEWPORTS;
            let delta_y = self.target_scroll_y_doc - self.scroll_y_doc;
            if delta_y.abs() > max_animated_distance {
                admitted_camera[1] =
                    self.target_scroll_y_doc - delta_y.signum() * max_animated_distance;
            }
        } else if !camera_backlog {
            admitted_camera = [self.target_scroll_x_doc, self.target_scroll_y_doc];
        }
        let camera_would_change = (admitted_camera[0] - self.scroll_x_doc).abs() > f32::EPSILON
            || (admitted_camera[1] - self.scroll_y_doc).abs() > f32::EPSILON;
        if !camera_would_change || self.native_gpu_camera_frame_ready() {
            self.scroll_x_doc = admitted_camera[0];
            self.scroll_y_doc = admitted_camera[1];
        }

        let changed = (self.target_scroll_x_doc - previous_x).abs() > f32::EPSILON
            || (self.target_scroll_y_doc - previous_y).abs() > f32::EPSILON;
        if changed {
            self.mark_native_gpu_motion(cx);
            if (self.scroll_x_doc - previous_camera_x).abs() > f32::EPSILON
                || (self.scroll_y_doc - previous_camera_y).abs() > f32::EPSILON
            {
                self.record_native_gpu_camera_step(false);
            }
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
            // Expensive same-frame PDF draws can occupy every executor tick.
            // Settle from the frame path as well as the timer so motion cannot
            // remain latched merely because the timer was starved.
            self.settle_native_gpu_motion_if_due(cx);
            return;
        }
        let now = Instant::now();
        let elapsed = self
            .last_animation_frame
            .replace(now)
            .map_or(Duration::from_secs_f32(1.0 / 120.0), |previous| {
                now.saturating_duration_since(previous)
            });

        let hold_camera_for_gpu =
            (zoom_pending || scroll_pending) && !self.native_gpu_camera_frame_ready();
        let mut changed_zoom = false;
        if zoom_pending && !hold_camera_for_gpu {
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

        let gpu_camera_step = self
            .gpu_pdf
            .has_renderer()
            .then(|| GPU_SCROLL_MAX_CAMERA_STEP_PX / self.effective_scale().max(0.001));
        let (changed_x, changed_y) = if hold_camera_for_gpu {
            (false, false)
        } else if let Some(max_step) = gpu_camera_step {
            (
                advance_toward_capped(
                    &mut self.scroll_x_doc,
                    self.target_scroll_x_doc,
                    smooth_scrolling,
                    elapsed,
                    max_step,
                ),
                advance_toward_capped(
                    &mut self.scroll_y_doc,
                    self.target_scroll_y_doc,
                    smooth_scrolling,
                    elapsed,
                    max_step,
                ),
            )
        } else {
            (
                advance_toward(
                    &mut self.scroll_x_doc,
                    self.target_scroll_x_doc,
                    smooth_scrolling,
                    elapsed,
                ),
                advance_toward(
                    &mut self.scroll_y_doc,
                    self.target_scroll_y_doc,
                    smooth_scrolling,
                    elapsed,
                ),
            )
        };
        match motion_refresh(
            changed_zoom,
            changed_x,
            changed_y,
            hold_camera_for_gpu,
            zoom_pending,
        ) {
            Some(MotionRefresh::Zoom) => {
                // Reassert zoom-specific motion after the settle timer races a
                // new scale step, avoiding obsolete exact-size surface work.
                self.mark_native_gpu_zoom_motion(cx);
            }
            Some(MotionRefresh::Pan) => self.mark_native_gpu_motion(cx),
            None => {}
        }
        if changed_zoom || changed_x || changed_y {
            self.record_native_gpu_camera_step(changed_zoom);
        }
        if hold_camera_for_gpu {
            // Keep this view dirty so the next display tick retries pan-cache
            // admission without relying on an unrelated UI notification.
            cx.notify();
        }
        if changed_zoom || changed_x || changed_y || hold_camera_for_gpu {
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

    #[test]
    fn admitted_scroll_backlog_never_becomes_one_large_camera_jump() {
        let mut immediate = 0.0;
        assert!(advance_toward_capped(
            &mut immediate,
            100.0,
            false,
            Duration::ZERO,
            32.0,
        ));
        assert_eq!(immediate, 32.0);

        let mut reverse = 100.0;
        assert!(advance_toward_capped(
            &mut reverse,
            0.0,
            false,
            Duration::ZERO,
            32.0,
        ));
        assert_eq!(reverse, 68.0);
    }

    #[test]
    fn held_pan_does_not_keep_zoom_motion_alive() {
        assert_eq!(motion_refresh(false, false, false, true, false), None);
        assert_eq!(
            motion_refresh(false, false, false, true, true),
            Some(MotionRefresh::Zoom)
        );
        assert_eq!(
            motion_refresh(false, false, true, false, false),
            Some(MotionRefresh::Pan)
        );
    }
}
