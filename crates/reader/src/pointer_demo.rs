//! A scripted pointer for screens (`--pointer-demo`): steps separated by
//! `;`, one a frame once the document is drawn, each at a target the app
//! finds on its last frame (`tab:N`, the Nth document tab; `page`, the
//! middle of the pages; or `x,y` in points):
//!
//! - `move:T`, `click:T`, `right:T` (a context menu), `press:T`,
//!   `release:T`
//! - `drag:A>B`: presses at A and moves to B over a few frames, still
//!   held (a drag in progress); `release:B` drops it
//! - `wait`; any step takes `*N` to repeat it

use std::collections::VecDeque;

use egui::{Event, PointerButton, Pos2};

/// Frames a drag takes to get from where it starts to where it goes.
const DRAG_FRAMES: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Tab(usize),
    Page,
    At(Pos2),
    /// `t` of the way from one target to another.
    Between(Box<Target>, Box<Target>, f32),
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Move(Target),
    Press(PointerButton, Target),
    Release(PointerButton, Target),
    Wait,
}

#[derive(Debug, Default)]
pub struct PointerScript {
    steps: VecDeque<Step>,
    playing: bool,
}

impl PointerScript {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut steps = VecDeque::new();
        for step in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (step, times) = match step.rsplit_once('*') {
                Some((step, times)) => (
                    step,
                    times
                        .parse::<usize>()
                        .map_err(|_| format!("{step}*{times}: the count is not a number"))?,
                ),
                None => (step, 1),
            };
            let parsed = parse_step(step)?;
            for _ in 0..times {
                steps.extend(parsed.iter().cloned());
            }
        }
        Ok(Self {
            steps,
            playing: false,
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    /// Whether steps are left to play.
    pub fn is_busy(&self) -> bool {
        self.playing && !self.steps.is_empty()
    }

    /// This frame's events; `find` places a target, and a step whose
    /// target is not drawn yet waits for it.
    pub fn frame(&mut self, find: impl Fn(&Target) -> Option<Pos2>) -> Vec<Event> {
        if !self.playing {
            return Vec::new();
        }
        let Some(step) = self.steps.front() else {
            return Vec::new();
        };
        let event = match step {
            Step::Wait => None,
            Step::Move(target) => match locate(target, &find) {
                Some(pos) => Some(Event::PointerMoved(pos)),
                None => return Vec::new(),
            },
            Step::Press(button, target) | Step::Release(button, target) => {
                let Some(pos) = locate(target, &find) else {
                    return Vec::new();
                };
                Some(Event::PointerButton {
                    pos,
                    button: *button,
                    pressed: matches!(step, Step::Press(..)),
                    modifiers: Default::default(),
                })
            }
        };
        self.steps.pop_front();
        event.into_iter().collect()
    }
}

fn locate(target: &Target, find: &impl Fn(&Target) -> Option<Pos2>) -> Option<Pos2> {
    match target {
        Target::Between(from, to, t) => Some(locate(from, find)?.lerp(locate(to, find)?, *t)),
        Target::At(pos) => Some(*pos),
        target => find(target),
    }
}

fn parse_step(step: &str) -> Result<Vec<Step>, String> {
    if step == "wait" {
        return Ok(vec![Step::Wait]);
    }
    let (verb, rest) = step
        .split_once(':')
        .ok_or_else(|| format!("{step}: expected verb:target"))?;
    let target = || parse_target(rest);
    Ok(match verb {
        "move" => vec![Step::Move(target()?)],
        "click" | "right" => {
            let button = if verb == "click" {
                PointerButton::Primary
            } else {
                PointerButton::Secondary
            };
            let at = target()?;
            vec![
                Step::Move(at.clone()),
                Step::Press(button, at.clone()),
                Step::Release(button, at),
            ]
        }
        "press" => vec![
            Step::Move(target()?),
            Step::Press(PointerButton::Primary, target()?),
        ],
        "release" => vec![Step::Release(PointerButton::Primary, target()?)],
        "drag" => {
            let (from, to) = rest
                .split_once('>')
                .ok_or_else(|| format!("{step}: expected drag:from>to"))?;
            let (from, to) = (parse_target(from)?, parse_target(to)?);
            let mut steps = vec![
                Step::Move(from.clone()),
                Step::Press(PointerButton::Primary, from.clone()),
            ];
            for frame in 1..=DRAG_FRAMES {
                steps.push(Step::Move(Target::Between(
                    Box::new(from.clone()),
                    Box::new(to.clone()),
                    frame as f32 / DRAG_FRAMES as f32,
                )));
            }
            steps
        }
        _ => return Err(format!("{step}: unknown step {verb}")),
    })
}

fn parse_target(text: &str) -> Result<Target, String> {
    if text == "page" {
        return Ok(Target::Page);
    }
    if let Some(index) = text.strip_prefix("tab:") {
        return index
            .parse()
            .map(Target::Tab)
            .map_err(|_| format!("{text}: the tab is not a number"));
    }
    let (x, y) = text
        .split_once(',')
        .ok_or_else(|| format!("{text}: expected tab:N, page or x,y"))?;
    match (x.parse(), y.parse()) {
        (Ok(x), Ok(y)) => Ok(Target::At(Pos2::new(x, y))),
        _ => Err(format!("{text}: expected x,y in points")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_wait_for_their_target_then_play_one_a_frame() {
        let mut script = PointerScript::parse("right:tab:1;wait*2").unwrap();
        let find = |target: &Target| (target == &Target::Tab(1)).then_some(Pos2::new(100.0, 18.0));
        assert!(script.frame(find).is_empty(), "not playing yet");
        script.play();
        assert!(script.frame(|_| None).is_empty(), "the tab isn't drawn");
        assert_eq!(
            script.frame(find),
            [Event::PointerMoved(Pos2::new(100.0, 18.0))]
        );
        assert!(matches!(
            script.frame(find)[..],
            [Event::PointerButton {
                button: PointerButton::Secondary,
                pressed: true,
                ..
            }]
        ));
        assert_eq!(script.frame(find).len(), 1);
        assert!(script.is_busy(), "two waits left");
        script.frame(find);
        script.frame(find);
        assert!(!script.is_busy());
    }

    #[test]
    fn a_drag_moves_held_down_to_its_end() {
        let mut script = PointerScript::parse("drag:0,0>60,0").unwrap();
        script.play();
        let events: Vec<Event> = (0..2 + DRAG_FRAMES)
            .flat_map(|_| script.frame(|_| None))
            .collect();
        assert_eq!(
            events.last(),
            Some(&Event::PointerMoved(Pos2::new(60.0, 0.0)))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::PointerButton { pressed: false, .. }))
        );
        assert!(PointerScript::parse("jump:1,2").is_err());
        assert!(PointerScript::parse("click:tab:x").is_err());
    }
}
