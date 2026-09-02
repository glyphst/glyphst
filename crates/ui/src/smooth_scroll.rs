use std::time::Duration;

const SMOOTH_SCROLL_RESPONSE_PER_SECOND: f64 = 24.0;
const SMOOTH_SCROLL_MAX_ELAPSED: Duration = Duration::from_millis(64);
const SMOOTH_SCROLL_SNAP: f64 = 0.5;

pub fn advance_toward(current: &mut f64, target: f64, smooth: bool, elapsed: Duration) -> bool {
    let previous = *current;
    if !smooth {
        *current = target;
    } else {
        let delta = target - *current;
        if delta.abs() <= SMOOTH_SCROLL_SNAP {
            *current = target;
        } else {
            let elapsed = elapsed.min(SMOOTH_SCROLL_MAX_ELAPSED).as_secs_f64();
            let factor = 1.0 - (-SMOOTH_SCROLL_RESPONSE_PER_SECOND * elapsed).exp();
            *current += delta * factor;
        }
    }

    (*current - previous).abs() > f64::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damping_is_stable_across_refresh_rates() {
        let mut sixty_hz = 0.0;
        for _ in 0..60 {
            advance_toward(
                &mut sixty_hz,
                100.0,
                true,
                Duration::from_secs_f32(1.0 / 60.0),
            );
        }
        let mut one_twenty_hz = 0.0;
        for _ in 0..120 {
            advance_toward(
                &mut one_twenty_hz,
                100.0,
                true,
                Duration::from_secs_f32(1.0 / 120.0),
            );
        }
        assert!((sixty_hz - one_twenty_hz).abs() <= SMOOTH_SCROLL_SNAP);
        assert_eq!(sixty_hz, 100.0);
        assert_eq!(one_twenty_hz, 100.0);
    }

    #[test]
    fn long_frames_are_capped_and_immediate_motion_is_exact() {
        let mut smooth = 0.0;
        advance_toward(&mut smooth, 100.0, true, Duration::from_secs(1));
        assert!(smooth > 0.0 && smooth < 100.0);

        let mut immediate = 0.0;
        advance_toward(&mut immediate, 100.0, false, Duration::ZERO);
        assert_eq!(immediate, 100.0);
    }
}
