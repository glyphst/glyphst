const SMOOTH_SCROLL_FACTOR: f32 = 0.25;
const SMOOTH_SCROLL_SNAP: f32 = 0.5;

#[derive(Clone, Copy, Debug, Default)]
pub struct SmoothScroll {
    current: f32,
    target: f32,
}

impl SmoothScroll {
    pub fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
        }
    }

    pub fn current(self) -> f32 {
        self.current
    }

    pub fn target(self) -> f32 {
        self.target
    }

    pub fn jump_to(&mut self, value: f32, min: f32, max: f32) -> bool {
        let value = value.clamp(min, max);
        let changed = (self.current - value).abs() > f32::EPSILON
            || (self.target - value).abs() > f32::EPSILON;
        self.current = value;
        self.target = value;
        changed
    }

    pub fn set_target(&mut self, value: f32, min: f32, max: f32) -> bool {
        let value = value.clamp(min, max);
        let changed = (self.target - value).abs() > f32::EPSILON;
        self.target = value;
        changed
    }

    pub fn clamp(&mut self, min: f32, max: f32) -> bool {
        let current = self.current.clamp(min, max);
        let target = self.target.clamp(min, max);
        let changed = (self.current - current).abs() > f32::EPSILON
            || (self.target - target).abs() > f32::EPSILON;
        self.current = current;
        self.target = target;
        changed
    }

    pub fn advance(&mut self, smooth: bool) -> bool {
        let previous = self.current;
        if !smooth {
            self.current = self.target;
        } else {
            let delta = self.target - self.current;
            if delta.abs() <= SMOOTH_SCROLL_SNAP {
                self.current = self.target;
            } else {
                self.current += delta * SMOOTH_SCROLL_FACTOR;
            }
        }

        (self.current - previous).abs() > f32::EPSILON
    }

    pub fn is_animating(self) -> bool {
        (self.current - self.target).abs() > f32::EPSILON
    }
}

pub fn advance_toward(current: &mut f32, target: f32, smooth: bool) -> bool {
    let previous = *current;
    if !smooth {
        *current = target;
    } else {
        let delta = target - *current;
        if delta.abs() <= SMOOTH_SCROLL_SNAP {
            *current = target;
        } else {
            *current += delta * SMOOTH_SCROLL_FACTOR;
        }
    }

    (*current - previous).abs() > f32::EPSILON
}
