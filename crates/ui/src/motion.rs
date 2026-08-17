//! Shared motion primitives for Glyphst.
//!
//! The timings intentionally stay short for interactive UI. Layout motion uses
//! an explicit tween because GPUI element animations restart when a keyed
//! element is remounted; the explicit state also lets a drawer reverse without
//! jumping.

use std::time::{Duration, Instant};

use gpui::{Animation, AnimationElement, AnimationExt as _, ElementId, IntoElement, Styled, px};

/// A CSS-compatible `cubic-bezier(x1, y1, x2, y2)` timing curve.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(first: f32, second: f32) -> (f32, f32, f32) {
        let c = 3.0 * first;
        let b = 3.0 * (second - first) - c;
        let a = 1.0 - c - b;
        (a, b, c)
    }

    fn sample_x(self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        ((a * t + b) * t + c) * t
    }

    fn sample_y(self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.y1, self.y2);
        ((a * t + b) * t + c) * t
    }

    fn sample_x_derivative(self, t: f32) -> f32 {
        let (a, b, c) = Self::coefficients(self.x1, self.x2);
        (3.0 * a * t + 2.0 * b) * t + c
    }

    fn solve_t_for_x(self, x: f32) -> f32 {
        let mut t = x;
        for _ in 0..8 {
            let error = self.sample_x(t) - x;
            if error.abs() < 1e-6 {
                return t;
            }
            let derivative = self.sample_x_derivative(t);
            if derivative.abs() < 1e-6 {
                break;
            }
            t = (t - error / derivative).clamp(0.0, 1.0);
        }

        let (mut lower, mut upper) = (0.0, 1.0);
        for _ in 0..24 {
            let middle = (lower + upper) * 0.5;
            if self.sample_x(middle) < x {
                lower = middle;
            } else {
                upper = middle;
            }
        }
        (lower + upper) * 0.5
    }

    pub fn eval(self, progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        if progress == 0.0 || progress == 1.0 {
            return progress;
        }
        self.sample_y(self.solve_t_for_x(progress)).clamp(0.0, 1.0)
    }

    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |progress| self.eval(progress)
    }
}

pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
pub const EASE_OUT: CubicBezier = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
pub const EASE_OUT_EXPO: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
pub const EASE_STANDARD: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSpec {
    pub duration: Duration,
    pub curve: CubicBezier,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: CubicBezier) -> Self {
        Self {
            duration: Duration::from_millis(duration_ms),
            curve,
        }
    }

    pub fn animation(self) -> Animation {
        Animation::new(self.duration).with_easing(self.curve.easing())
    }
}

pub const HOVER: MotionSpec = MotionSpec::new(150, EASE_STANDARD);
pub const MENU: MotionSpec = MotionSpec::new(140, EASE);
pub const MODAL: MotionSpec = MotionSpec::new(180, EASE);
pub const LAYOUT: MotionSpec = MotionSpec::new(200, EASE_OUT);
pub const CONTENT: MotionSpec = MotionSpec::new(240, EASE_OUT_EXPO);

/// Interruptible animation state for layout values such as drawer widths.
#[derive(Clone, Copy, Debug)]
pub struct ScalarTween {
    from: f32,
    target: f32,
    started_at: Instant,
    spec: MotionSpec,
}

impl ScalarTween {
    pub fn new(value: f32, spec: MotionSpec) -> Self {
        Self {
            from: value,
            target: value,
            started_at: Instant::now(),
            spec,
        }
    }

    pub fn target(self) -> f32 {
        self.target
    }

    pub fn is_animating(self, now: Instant) -> bool {
        self.from != self.target
            && now.saturating_duration_since(self.started_at) < self.spec.duration
    }

    pub fn value(self, now: Instant) -> f32 {
        if self.from == self.target || self.spec.duration.is_zero() {
            return self.target;
        }
        let elapsed = now.saturating_duration_since(self.started_at);
        let raw = elapsed.as_secs_f32() / self.spec.duration.as_secs_f32();
        let progress = self.spec.curve.eval(raw);
        self.from + (self.target - self.from) * progress
    }

    pub fn retarget(&mut self, target: f32, now: Instant, reduce_motion: bool) {
        if self.target == target {
            if reduce_motion && self.from != self.target {
                self.from = target;
                self.started_at = now;
            }
            return;
        }
        let current = self.value(now);
        self.from = if reduce_motion { target } else { current };
        self.target = target;
        self.started_at = now;
    }

    pub fn snap(&mut self, value: f32) {
        self.from = value;
        self.target = value;
        self.started_at = Instant::now();
    }
}

pub fn menu_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU.animation(), |element, progress| {
        element
            .relative()
            .opacity(progress)
            .top(px(-2.0 * (1.0 - progress)))
    })
}

pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU.animation(), |element, progress| {
        element.opacity(progress)
    })
}

pub fn modal_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MODAL.animation(), |element, progress| {
        element
            .relative()
            .opacity(progress)
            .top(px(4.0 * (1.0 - progress)))
    })
}

pub fn content_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, CONTENT.animation(), |element, progress| {
        element
            .relative()
            .opacity(progress)
            .top(px(4.0 * (1.0 - progress)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_uses_the_x_axis() {
        let curve = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
        assert!((curve.eval(0.5) - 0.8024).abs() < 0.002);
        assert_eq!(curve.eval(-1.0), 0.0);
        assert_eq!(curve.eval(2.0), 1.0);
    }

    #[test]
    fn tween_retargets_from_its_sampled_value() {
        let started = Instant::now();
        let mut tween = ScalarTween::new(0.0, LAYOUT);
        tween.started_at = started;
        tween.retarget(100.0, started, false);
        let halfway = started + Duration::from_millis(100);
        let sampled = tween.value(halfway);
        tween.retarget(0.0, halfway, false);
        assert!((tween.from - sampled).abs() < 0.001);
        assert_eq!(tween.target(), 0.0);
    }

    #[test]
    fn reduced_motion_retarget_snaps() {
        let now = Instant::now();
        let mut tween = ScalarTween::new(10.0, LAYOUT);
        tween.retarget(100.0, now, true);
        assert_eq!(tween.value(now), 100.0);
        assert!(!tween.is_animating(now));
    }

    #[test]
    fn enabling_reduced_motion_finishes_an_existing_tween() {
        let started = Instant::now();
        let mut tween = ScalarTween::new(0.0, LAYOUT);
        tween.retarget(100.0, started, false);
        let halfway = started + Duration::from_millis(100);
        assert!(tween.is_animating(halfway));
        tween.retarget(100.0, halfway, true);
        assert_eq!(tween.value(halfway), 100.0);
        assert!(!tween.is_animating(halfway));
    }
}
