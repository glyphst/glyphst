//! Motion. Values that follow a target move toward it by exponential
//! smoothing (lisyarus, "Exponential smoothing"): each frame closes
//! `1 − e^(−rate·dt)` of the gap, so the motion doesn't depend on the frame
//! rate and stays smooth when the target moves mid-way, where egui's
//! `animate_value_with_time` restarts. Also the durations of the
//! assistant's small motions, after Zeron's `motion.rs`.

/// A disclosure opening or closing.
pub const COLLAPSE: f32 = 0.18;
/// A chevron turning.
pub const CHEVRON: f32 = 0.2;
/// Controls fading in on hover.
pub const HOVER_FADE: f32 = 0.15;
/// A menu appearing.
pub const MENU_IN: f32 = 0.14;
/// One sweep of a shimmer over a running label.
pub const SHIMMER_PERIOD: f64 = 2.2;

/// Approach rate of panels, bars and overlays opening or closing, per
/// second: 95% of the way in 0.15 s.
pub const PANEL: f32 = 20.0;
/// Approach rate of hover and selection fills: 95% in 0.1 s.
pub const HOVER: f32 = 30.0;
/// Approach rate of the document's scroll and zoom: 95% in 0.17 s.
pub const SCROLL: f64 = 18.0;

/// The longest frame a value advances by, so a stalled frame doesn't make
/// it jump.
const MAX_STEP: f64 = 1.0 / 30.0;
/// How close a value snaps to its target: under a hundredth of a point,
/// or of a fraction's 0..1.
const SNAP: f32 = 0.01;

/// `value` moved toward `target` for `seconds` at `rate` per second.
pub fn approach(value: f64, target: f64, rate: f64, seconds: f64) -> f64 {
    value + (target - value) * (1.0 - (-rate * seconds.clamp(0.0, MAX_STEP)).exp())
}

fn reduced_id() -> egui::Id {
    egui::Id::new("glyphst-reduce-motion")
}

/// Sets whether motion is reduced: smoothed values then jump to their
/// targets, and egui's own fades and scroll animations are off. Call it
/// once a frame, before the UI.
pub fn set_reduced(ctx: &egui::Context, reduced: bool) {
    ctx.data_mut(|data| data.insert_temp(reduced_id(), reduced));
    let default = egui::Style::default();
    let time = if reduced { 0.0 } else { default.animation_time };
    if ctx.global_style().animation_time != time {
        ctx.all_styles_mut(|style| {
            style.animation_time = time;
            style.scroll_animation = if reduced {
                egui::style::ScrollAnimation::none()
            } else {
                default.scroll_animation
            };
        });
    }
}

/// Whether motion is reduced.
pub fn is_reduced(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(reduced_id()))
        .unwrap_or(false)
}

/// The value kept under `id`, moved toward `target` at `rate` for this
/// frame; asks for a repaint until it arrives. The first call returns
/// `target`, so call it every frame, also while what it moves is hidden.
pub fn smooth(ctx: &egui::Context, id: egui::Id, target: f32, rate: f32) -> f32 {
    let reduced = is_reduced(ctx);
    let seconds = ctx.input(|input| input.stable_dt);
    let value = ctx.data_mut(|data| {
        let value = data.get_temp_mut_or(id, target);
        *value = if reduced {
            target
        } else {
            approach(
                f64::from(*value),
                f64::from(target),
                f64::from(rate),
                f64::from(seconds),
            ) as f32
        };
        if (*value - target).abs() < SNAP {
            *value = target;
        }
        *value
    });
    if value != target {
        ctx.request_repaint();
    }
    value
}

/// 0 when off and 1 when on, smoothed under `id`.
pub fn smooth_bool(ctx: &egui::Context, id: egui::Id, on: bool, rate: f32) -> f32 {
    smooth(ctx, id, if on { 1.0 } else { 0.0 }, rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(value: f64, frames: usize, seconds: f64) -> f64 {
        (0..frames).fold(value, |value, _| approach(value, 1.0, 20.0, seconds))
    }

    #[test]
    fn approach_does_not_depend_on_the_frame_rate() {
        let at_60 = run(0.0, 6, 1.0 / 60.0);
        let at_120 = run(0.0, 12, 1.0 / 120.0);
        assert!((at_60 - at_120).abs() < 1e-9, "{at_60} vs {at_120}");
        assert!((at_60 - (1.0 - (-2.0f64).exp())).abs() < 1e-9);
    }

    #[test]
    fn a_stalled_frame_advances_by_one_long_frame_only() {
        assert_eq!(run(0.0, 1, 2.0), run(0.0, 1, MAX_STEP));
        assert_eq!(approach(0.3, 1.0, 20.0, -1.0), 0.3);
    }

    /// Frames of `ctx` calling `smooth_bool(on)`; returns the values.
    fn frames(ctx: &egui::Context, on: &[bool]) -> Vec<f32> {
        on.iter()
            .map(|&on| {
                let mut value = 0.0;
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                    value = smooth_bool(ui.ctx(), egui::Id::new("t"), on, PANEL);
                });
                output.textures_delta.clear();
                value
            })
            .collect()
    }

    #[test]
    fn a_smoothed_value_starts_at_its_target_and_eases_to_the_next() {
        let ctx = egui::Context::default();
        let mut on = vec![false];
        on.extend([true; 30]);
        let values = frames(&ctx, &on);
        assert_eq!(values[0], 0.0);
        assert!(values[1] > 0.0 && values[1] < 0.5, "{}", values[1]);
        assert!(values.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(*values.last().unwrap(), 1.0, "snapped");
    }

    #[test]
    fn reduced_motion_jumps_to_the_target() {
        let ctx = egui::Context::default();
        set_reduced(&ctx, true);
        assert_eq!(frames(&ctx, &[false, true]), [0.0, 1.0]);
        assert_eq!(ctx.global_style().animation_time, 0.0);
        set_reduced(&ctx, false);
        assert!(ctx.global_style().animation_time > 0.0);
    }
}
