const SMOOTH_SCROLL_FACTOR: f32 = 0.25;
const SMOOTH_SCROLL_SNAP: f32 = 0.5;

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
