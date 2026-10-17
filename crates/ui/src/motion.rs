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
/// Approach rate of chrome fading once it rests: 95% in 0.4 s.
pub const FADE: f32 = 8.0;
/// Approach rate of hover and selection fills: 95% in 0.1 s.
pub const HOVER: f32 = 30.0;
/// Approach rate of the document's scroll and zoom: 95% in 0.17 s.
pub const SCROLL: f64 = 18.0;

/// The longest frame a value advances by. A slow frame covers more of the
/// way, as the clock says, so the motion keeps its pace; only a stall this
/// long is cut short. (egui's `stable_dt`, which values here advance by, is
/// a predicted frame after an idle one, never the idle gap.)
const MAX_STEP: f64 = 0.1;
/// How close a value snaps to its target: a thousandth of a point, or of
/// a fraction's 0..1 (under a pixel on a long meter).
const SNAP: f32 = 0.001;

/// `value` moved toward `target` for `seconds` at `rate` per second.
pub fn approach(value: f64, target: f64, rate: f64, seconds: f64) -> f64 {
    value + (target - value) * (1.0 - (-rate * seconds.clamp(0.0, MAX_STEP)).exp())
}

fn reduced_id() -> egui::Id {
    egui::Id::new("glyphst-reduce-motion")
}

/// Sets whether motion is reduced ("replace all motion with short
/// fades"): smoothed values then jump to their targets and egui's scroll
/// animations are off; its fades stay. Call it once a frame, before the
/// UI.
pub fn set_reduced(ctx: &egui::Context, reduced: bool) {
    ctx.data_mut(|data| data.insert_temp(reduced_id(), reduced));
    let scroll = if reduced {
        egui::style::ScrollAnimation::none()
    } else {
        egui::Style::default().scroll_animation
    };
    if ctx.global_style().scroll_animation != scroll {
        ctx.all_styles_mut(|style| style.scroll_animation = scroll);
    }
}

/// Whether motion is reduced.
pub fn is_reduced(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp(reduced_id()))
        .unwrap_or(false)
}

fn moving_id() -> egui::Id {
    egui::Id::new("glyphst-motion-moving")
}

/// Asks for the next frame, and notes that something moved in this one.
fn moved(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(moving_id(), true));
    ctx.request_repaint();
}

/// Whether a value moved since this was last asked: the app counts such
/// frames as animation, not idle ones.
pub fn take_moving(ctx: &egui::Context) -> bool {
    ctx.data_mut(|data| data.remove_temp::<bool>(moving_id()))
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
        moved(ctx);
    }
    value
}

/// 0 when off and 1 when on, smoothed under `id`.
pub fn smooth_bool(ctx: &egui::Context, id: egui::Id, on: bool, rate: f32) -> f32 {
    smooth(ctx, id, if on { 1.0 } else { 0.0 }, rate)
}

/// The colour kept under `id`, moved toward `target` at [`HOVER`]: a
/// control's fill fades as it is hovered, pressed or selected.
pub fn color(ctx: &egui::Context, id: egui::Id, target: egui::Color32) -> egui::Color32 {
    let id = id.with("smoothed-color");
    let goal = target.to_array().map(f32::from);
    let reduced = is_reduced(ctx);
    let seconds = f64::from(ctx.input(|input| input.stable_dt));
    let (value, moving) = ctx.data_mut(|data| {
        let value = data.get_temp_mut_or(id, goal);
        if reduced {
            *value = goal;
        }
        for (channel, goal) in value.iter_mut().zip(goal) {
            let next = approach(
                f64::from(*channel),
                f64::from(goal),
                f64::from(HOVER),
                seconds,
            );
            // Within half a step of the 0..255 channel it has arrived.
            *channel = if (next as f32 - goal).abs() < 0.5 {
                goal
            } else {
                next as f32
            };
        }
        (*value, *value != goal)
    });
    if moving {
        moved(ctx);
    }
    let [r, g, b, a] = value.map(|channel| channel.round() as u8);
    egui::Color32::from_rgba_premultiplied(r, g, b, a)
}

/// A glide of a scroll area's offset: where it goes, and where it was set
/// last frame.
#[derive(Clone, Copy, Default)]
struct ScrollGlide {
    target: f32,
    set: f32,
}

/// Glides the scroll area `id` toward `target` once one is given, from
/// `offset`, where it is now; returns the offset to give the area this
/// frame (`ScrollArea::vertical_scroll_offset`), or `None` when it isn't
/// gliding. Scrolling it by hand ends the glide.
pub fn scroll_glide(
    ctx: &egui::Context,
    id: egui::Id,
    offset: f32,
    target: Option<f32>,
) -> Option<f32> {
    let id = id.with("scroll-glide");
    let glide = match target {
        Some(target) => ScrollGlide {
            target,
            set: offset,
        },
        None => {
            let glide = ctx.data(|data| data.get_temp::<ScrollGlide>(id))?;
            // Moved since: by hand.
            if (offset - glide.set).abs() > 0.5 {
                ctx.data_mut(|data| data.remove_temp::<ScrollGlide>(id));
                return None;
            }
            glide
        }
    };
    let seconds = ctx.input(|input| input.stable_dt);
    let next = approach(
        f64::from(offset),
        f64::from(glide.target),
        SCROLL,
        f64::from(seconds),
    ) as f32;
    if is_reduced(ctx) || (next - glide.target).abs() < 0.5 {
        ctx.data_mut(|data| data.remove_temp::<ScrollGlide>(id));
        return Some(glide.target);
    }
    ctx.data_mut(|data| {
        data.insert_temp(
            id,
            ScrollGlide {
                target: glide.target,
                set: next,
            },
        );
    });
    moved(ctx);
    Some(next)
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
    fn a_scroll_glide_eases_to_its_target_and_ends_when_scrolled_by_hand() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("list");
        let frame = |offset: f32, target| {
            let mut next = None;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                next = scroll_glide(ui.ctx(), id, offset, target);
            });
            output.textures_delta.clear();
            next
        };
        let mut offset = frame(0.0, Some(300.0)).unwrap();
        assert!(offset > 0.0 && offset < 300.0);
        for _ in 0..60 {
            match frame(offset, None) {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(offset, 300.0);
        assert_eq!(frame(offset, None), None, "done");
        let offset = frame(0.0, Some(300.0)).unwrap();
        assert_eq!(frame(offset + 40.0, None), None, "scrolled by hand");
    }

    #[test]
    fn a_color_fades_to_its_target() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("fill");
        let frame = |target| {
            let mut shown = egui::Color32::TRANSPARENT;
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                shown = color(ui.ctx(), id, target);
            });
            output.textures_delta.clear();
            shown
        };
        let hover = egui::Color32::from_rgba_premultiplied(40, 40, 40, 80);
        assert_eq!(
            frame(egui::Color32::TRANSPARENT),
            egui::Color32::TRANSPARENT
        );
        let first = frame(hover);
        assert!(first.a() > 0 && first.a() < 80, "{first:?}");
        let mut last = first;
        for _ in 0..30 {
            last = frame(hover);
        }
        assert_eq!(last, hover);
    }

    #[test]
    fn reduced_motion_jumps_to_the_target() {
        let ctx = egui::Context::default();
        set_reduced(&ctx, true);
        assert_eq!(frames(&ctx, &[false, true]), [0.0, 1.0]);
        assert_eq!(
            ctx.global_style().scroll_animation,
            egui::style::ScrollAnimation::none()
        );
        set_reduced(&ctx, false);
        assert_ne!(
            ctx.global_style().scroll_animation,
            egui::style::ScrollAnimation::none()
        );
    }
}
