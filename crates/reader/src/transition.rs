//! View transitions: tab switches, opening a document and presentation
//! page turns. This module picks the effect for each (a random one, or
//! reduced motion's short fade), times it, and keeps what its outgoing
//! image is; the pixels come from a transition effect.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use glyphst_effects::{CROSSFADE, EffectDef, EffectKind, MAX_PARAMS, ParamKind, RANDOM};

use crate::gpu::EffectUse;

/// How long a transition takes with reduce motion on.
const REDUCED_MS: u32 = 150;

/// A small seedable generator (SplitMix64): random transitions, their
/// varied parameters and seeds. Seeded the same, it repeats, for tests and
/// screenshots.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Seeded from the clock.
    pub fn from_time() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos() as u64);
        Self(nanos)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// An index below `count` (which must be above 0).
    fn below(&mut self, count: usize) -> usize {
        (self.next_u64() % count as u64) as usize
    }
}

/// The transition chosen for one switch or turn.
#[derive(Clone, Debug, PartialEq)]
pub struct Planned {
    pub effect: EffectUse,
    pub duration_ms: u32,
    pub seed: f32,
}

/// The transition for `choice`: the effect it names, or with [`RANDOM`]
/// one of `defs`' transitions other than `last`. Parameters a manifest
/// marks `random` are varied unless the user saved values for that
/// effect. Reduce motion makes every transition a short crossfade. `None`
/// when the effect is not there.
pub fn plan<'a>(
    choice: &EffectUse,
    defs: impl Iterator<Item = &'a EffectDef>,
    saved: &BTreeMap<String, [f32; MAX_PARAMS]>,
    reduce_motion: bool,
    rng: &mut Rng,
    last: Option<&str>,
) -> Option<Planned> {
    let transitions: Vec<&EffectDef> = defs
        .filter(|def| def.kind == EffectKind::Transition && !def.hidden)
        .collect();
    let seed = rng.unit() * 1000.0;
    if reduce_motion {
        let fade = transitions.iter().find(|def| def.id == CROSSFADE)?;
        return Some(Planned {
            effect: EffectUse {
                id: fade.id.clone(),
                params: fade.default_params(),
            },
            duration_ms: REDUCED_MS,
            seed,
        });
    }
    let (def, mut params) = if choice.id == RANDOM {
        let pool: Vec<&EffectDef> = transitions
            .iter()
            .copied()
            .filter(|def| Some(def.id.as_str()) != last)
            .collect();
        let pool = if pool.is_empty() { transitions } else { pool };
        let def = *pool.get(rng.below(pool.len().max(1)))?;
        let params = saved.get(&def.id).map_or_else(
            || def.default_params(),
            |values| def.sanitize_params(*values),
        );
        (def, params)
    } else {
        let def = *transitions.iter().find(|def| def.id == choice.id)?;
        (def, choice.params)
    };
    if !saved.contains_key(&def.id) {
        for (value, param) in params.iter_mut().zip(&def.params) {
            if !param.random {
                continue;
            }
            *value = match param.kind {
                ParamKind::Float { min, max } => min + (max - min) * rng.unit(),
                ParamKind::Toggle => f32::from(u8::from(rng.unit() >= 0.5)),
                ParamKind::Colour => *value,
            };
        }
    }
    Some(Planned {
        effect: EffectUse {
            id: def.id.clone(),
            params,
        },
        duration_ms: def.duration_ms,
        seed,
    })
}

/// What a transition's outgoing image is, made on its first frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Capture {
    /// The last image of a tab's view (a closed tab's while it lasts).
    View(u64),
    /// What a running transition shows, drawn once more: a switch while
    /// another runs starts from what is on screen.
    Composite {
        tab: u64,
        effect: EffectUse,
        progress: f32,
        direction: f32,
        origin: Option<egui::Pos2>,
        seed: f32,
        reversed: bool,
    },
    /// Nothing: the view comes in over the background (from Home).
    Clear,
}

#[derive(Clone, Debug)]
pub struct ViewTransition {
    pub effect: EffectUse,
    /// The tab whose view (pane) runs the transition.
    pub tab: u64,
    /// +1 forward (next page, a tab to the right), -1 backward.
    pub direction: f32,
    /// Where the switch came from, in points (the tab or card picked);
    /// `None` is the pane's centre.
    pub origin: Option<egui::Pos2>,
    pub seed: f32,
    /// The tab whose image is the outgoing view, while it is one: going
    /// back to it plays the transition in reverse.
    pub from_tab: Option<u64>,
    /// The other side's background when it differs from the shown book's:
    /// the backdrop transitions with the pages (the outgoing side, or the
    /// incoming one when played back).
    pub from_background: Option<EffectUse>,
    started: Instant,
    /// The whole transition's length, 0 to 1.
    duration: Duration,
    /// Progress when this run started, and where it goes: 1, or 0 when
    /// played back.
    start: f32,
    end: f32,
    /// Shown at this progress and never finished (screenshots).
    frozen: Option<f32>,
    capture: Option<Capture>,
}

impl ViewTransition {
    pub fn new(
        planned: Planned,
        capture: Capture,
        tab: u64,
        direction: f32,
        origin: Option<egui::Pos2>,
        now: Instant,
    ) -> Self {
        let from_tab = match capture {
            Capture::View(from) => Some(from),
            _ => None,
        };
        Self {
            effect: planned.effect,
            tab,
            direction,
            origin,
            seed: planned.seed,
            from_tab,
            from_background: None,
            started: now,
            duration: Duration::from_millis(u64::from(planned.duration_ms.max(1))),
            start: 0.0,
            end: 1.0,
            frozen: None,
            capture: Some(capture),
        }
    }

    /// Runs `factor` times as fast (the switcher's quick peeks).
    pub fn faster(mut self, factor: f32) -> Self {
        self.duration = self.duration.div_f32(factor.max(0.1));
        self
    }

    /// How far this run has gone, 0 to 1.
    fn run(&self, now: Instant) -> f32 {
        let span = self.duration.as_secs_f32() * (self.end - self.start).abs();
        if span <= 0.0 {
            return 1.0;
        }
        (now.saturating_duration_since(self.started).as_secs_f32() / span).clamp(0.0, 1.0)
    }

    /// Progress from 0 (all outgoing) to 1 (all incoming).
    pub fn progress(&self, now: Instant) -> f32 {
        self.frozen
            .unwrap_or_else(|| self.start + (self.end - self.start) * self.run(now))
    }

    pub fn finished(&self, now: Instant) -> bool {
        self.frozen.is_none() && self.run(now) >= 1.0
    }

    /// Whether it plays back: the view drawn is the outgoing one and the
    /// snapshot the incoming one.
    pub fn reversed(&self) -> bool {
        self.end < self.start
    }

    /// Plays back from where it is to the tab it came from: that tab's
    /// view runs it, with this one's last image.
    pub fn reverse(&mut self, now: Instant) {
        let Some(from) = self.from_tab else {
            return;
        };
        let progress = self.progress(now);
        let was_reversed = self.reversed();
        self.capture = Some(Capture::View(self.tab));
        self.from_tab = Some(self.tab);
        self.tab = from;
        self.start = progress;
        self.end = if was_reversed { 1.0 } else { 0.0 };
        self.started = now;
    }

    /// What it shows now, as a later transition's outgoing image.
    pub fn composite(&self, now: Instant) -> Capture {
        Capture::Composite {
            tab: self.tab,
            effect: self.effect.clone(),
            progress: self.progress(now),
            direction: self.direction,
            origin: self.origin,
            seed: self.seed,
            reversed: self.reversed(),
        }
    }

    /// This transition again for another switch while it runs: the same
    /// effect, parameters and length.
    pub fn planned(&self) -> Planned {
        Planned {
            effect: self.effect.clone(),
            duration_ms: self.duration.as_millis() as u32,
            seed: self.seed,
        }
    }

    /// Holds it at `progress` (screenshots).
    pub fn freeze(&mut self, progress: f32) {
        self.frozen = Some(progress.clamp(0.0, 1.0));
    }

    /// The outgoing image to make, on the first call only.
    pub fn take_capture(&mut self) -> Option<Capture> {
        self.capture.take()
    }
}

#[cfg(test)]
mod tests {
    use glyphst_effects::{ParamDef, SLIDE_OVER};

    use super::*;

    fn transition(id: &str, params: Vec<ParamDef>) -> EffectDef {
        EffectDef {
            id: id.into(),
            name: id.into(),
            kind: EffectKind::Transition,
            animated: false,
            duration_ms: 400,
            image: false,
            hidden: false,
            themed: false,
            params,
            source: String::new(),
            user: false,
        }
    }

    fn angle(random: bool) -> ParamDef {
        ParamDef {
            id: "angle".into(),
            label: "Angle".into(),
            kind: ParamKind::Float {
                min: 10.0,
                max: 20.0,
            },
            default: 10.0,
            random,
        }
    }

    fn planned(id: &str) -> Planned {
        Planned {
            effect: EffectUse {
                id: id.into(),
                params: [0.0; 4],
            },
            duration_ms: 300,
            seed: 0.0,
        }
    }

    #[test]
    fn progress_runs_over_the_duration_and_the_capture_happens_once() {
        let start = Instant::now();
        let mut transition =
            ViewTransition::new(planned(SLIDE_OVER), Capture::View(7), 8, 1.0, None, start);
        assert_eq!(transition.progress(start), 0.0);
        assert!((transition.progress(start + Duration::from_millis(150)) - 0.5).abs() < 1e-3);
        assert!(transition.finished(start + Duration::from_millis(300)));
        assert_eq!(transition.take_capture(), Some(Capture::View(7)));
        assert_eq!(transition.take_capture(), None);
        assert_eq!(transition.from_tab, Some(7));
    }

    #[test]
    fn going_back_plays_it_backwards_from_where_it_is() {
        let start = Instant::now();
        let mut transition =
            ViewTransition::new(planned(SLIDE_OVER), Capture::View(7), 8, 1.0, None, start);
        transition.take_capture();
        let back = start + Duration::from_millis(90);
        transition.reverse(back);
        // Tab 7's view runs it now, with tab 8's image, from 0.3 to 0.
        assert_eq!(transition.tab, 7);
        assert_eq!(transition.from_tab, Some(8));
        assert!(transition.reversed());
        assert_eq!(transition.take_capture(), Some(Capture::View(8)));
        assert!((transition.progress(back) - 0.3).abs() < 1e-3);
        // The way back takes the 0.3 of the way it had come.
        assert!((transition.progress(back + Duration::from_millis(45)) - 0.15).abs() < 1e-3);
        assert!(transition.finished(back + Duration::from_millis(90)));
        assert_eq!(transition.progress(back + Duration::from_millis(90)), 0.0);
    }

    #[test]
    fn a_frozen_transition_holds_and_never_finishes() {
        let start = Instant::now();
        let mut transition =
            ViewTransition::new(planned(SLIDE_OVER), Capture::Clear, 8, 1.0, None, start);
        transition.freeze(0.5);
        let later = start + Duration::from_secs(5);
        assert_eq!(transition.progress(later), 0.5);
        assert!(!transition.finished(later));
        assert_eq!(transition.from_tab, None);
    }

    #[test]
    fn random_picks_among_transitions_and_not_the_last_one() {
        let defs = [
            transition("a", Vec::new()),
            transition("b", Vec::new()),
            transition(CROSSFADE, Vec::new()),
        ];
        let random = EffectUse {
            id: RANDOM.into(),
            params: [0.0; 4],
        };
        let mut rng = Rng::new(3);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..60 {
            let picked = plan(
                &random,
                defs.iter(),
                &BTreeMap::new(),
                false,
                &mut rng,
                Some("a"),
            )
            .unwrap();
            assert_ne!(picked.effect.id, "a");
            seen.insert(picked.effect.id);
        }
        assert_eq!(seen.len(), 2, "{seen:?}");
    }

    #[test]
    fn random_parameters_vary_unless_the_user_set_them() {
        let defs = [transition("wipe", vec![angle(true)])];
        let wipe = EffectUse {
            id: "wipe".into(),
            params: [10.0, 0.0, 0.0, 0.0],
        };
        let mut rng = Rng::new(9);
        let angles: Vec<f32> = (0..20)
            .map(|_| {
                plan(&wipe, defs.iter(), &BTreeMap::new(), false, &mut rng, None)
                    .unwrap()
                    .effect
                    .params[0]
            })
            .collect();
        assert!(angles.iter().all(|angle| (10.0..=20.0).contains(angle)));
        assert!(angles.iter().any(|angle| (*angle - angles[0]).abs() > 0.5));

        let saved = BTreeMap::from([("wipe".to_string(), [12.0, 0.0, 0.0, 0.0])]);
        let kept = EffectUse {
            id: "wipe".into(),
            params: [12.0, 0.0, 0.0, 0.0],
        };
        let planned = plan(&kept, defs.iter(), &saved, false, &mut rng, None).unwrap();
        assert_eq!(planned.effect.params[0], 12.0);

        // Not marked random: the default stays.
        let fixed = [transition("wipe", vec![angle(false)])];
        let planned = plan(&wipe, fixed.iter(), &BTreeMap::new(), false, &mut rng, None).unwrap();
        assert_eq!(planned.effect.params[0], 10.0);
    }

    #[test]
    fn reduce_motion_makes_every_transition_a_short_fade() {
        let defs = [
            transition("wipe", vec![angle(true)]),
            transition(CROSSFADE, Vec::new()),
        ];
        let wipe = EffectUse {
            id: "wipe".into(),
            params: [0.0; 4],
        };
        let planned = plan(
            &wipe,
            defs.iter(),
            &BTreeMap::new(),
            true,
            &mut Rng::new(1),
            None,
        )
        .unwrap();
        assert_eq!(planned.effect.id, CROSSFADE);
        assert_eq!(planned.duration_ms, REDUCED_MS);
    }

    #[test]
    fn a_missing_effect_plans_nothing() {
        let defs = [transition("a", Vec::new())];
        let gone = EffectUse {
            id: "gone".into(),
            params: [0.0; 4],
        };
        assert!(
            plan(
                &gone,
                defs.iter(),
                &BTreeMap::new(),
                false,
                &mut Rng::new(1),
                None
            )
            .is_none()
        );
    }
}
