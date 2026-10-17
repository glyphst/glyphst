//! A scripted pad for screens and tests (`--pad-demo`): steps separated by
//! `;`, one per frame once it plays.
//!
//! - `connect` or `connect:NAME`, `disconnect`
//! - `press:BUTTON`, `release:BUTTON`, `tap:BUTTON` (both in one frame)
//! - `stick:L:X,Y` or `stick:R:X,Y` (y down; stays until moved again)
//! - `trigger:L:V` or `trigger:R:V`
//! - `wait`
//!
//! Any step takes `*N` to repeat it, as `tap:down*3`. Buttons are named as
//! [`PadButton::name`] gives them.

use std::collections::VecDeque;

use super::{Axes, Family, PadButton, PadEvent, PadFrame, PadInfo, Power};

const NAME: &str = "Scripted pad";

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Connect(String),
    Disconnect,
    Press(PadButton),
    Release(PadButton),
    Tap(PadButton),
    Stick { right: bool, at: egui::Vec2 },
    Trigger { right: bool, value: f32 },
    Wait,
}

#[derive(Clone, Debug, Default)]
pub struct Script {
    steps: VecDeque<Step>,
    family: Family,
    playing: bool,
    axes: Axes,
    pad: Option<PadInfo>,
}

impl Script {
    pub fn parse(text: &str, family: Family) -> Result<Self, String> {
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
            steps.extend(std::iter::repeat_n(parsed, times));
        }
        Ok(Self {
            steps,
            family,
            ..Self::default()
        })
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn frame(&mut self) -> PadFrame {
        let mut events = Vec::new();
        if self.playing
            && let Some(step) = self.steps.pop_front()
        {
            match step {
                Step::Connect(name) => {
                    let pad = PadInfo {
                        id: 0,
                        name,
                        family: self.family,
                        power: Power::Battery {
                            percent: 70,
                            charging: false,
                        },
                    };
                    self.pad = Some(pad.clone());
                    events.push(PadEvent::Connected {
                        pad,
                        initial: false,
                    });
                }
                Step::Disconnect => {
                    self.pad = None;
                    self.axes = Axes::default();
                    events.push(PadEvent::Disconnected(0));
                }
                Step::Press(button) => events.push(PadEvent::Pressed(button)),
                Step::Release(button) => events.push(PadEvent::Released(button)),
                Step::Tap(button) => {
                    events.push(PadEvent::Pressed(button));
                    events.push(PadEvent::Released(button));
                }
                Step::Stick { right: false, at } => self.axes.left = at,
                Step::Stick { right: true, at } => self.axes.right = at,
                Step::Trigger {
                    right: false,
                    value,
                } => self.axes.left_trigger = value,
                Step::Trigger { right: true, value } => self.axes.right_trigger = value,
                Step::Wait => {}
            }
        }
        PadFrame {
            events,
            axes: self.axes,
        }
    }

    pub fn connected(&self) -> Vec<PadInfo> {
        self.pad.iter().cloned().collect()
    }
}

fn parse_step(step: &str) -> Result<Step, String> {
    let (verb, rest) = step.split_once(':').unwrap_or((step, ""));
    let button = || PadButton::parse(rest).ok_or_else(|| format!("{step}: unknown button {rest}"));
    let side = |name: &str| match name {
        "L" | "l" => Ok(false),
        "R" | "r" => Ok(true),
        _ => Err(format!("{step}: L or R, not {name}")),
    };
    let number = |text: &str| {
        text.parse::<f32>()
            .map(|v| v.clamp(-1.0, 1.0))
            .map_err(|_| format!("{step}: {text} is not a number"))
    };
    Ok(match verb {
        "connect" => Step::Connect(if rest.is_empty() { NAME } else { rest }.to_string()),
        "disconnect" => Step::Disconnect,
        "press" => Step::Press(button()?),
        "release" => Step::Release(button()?),
        "tap" => Step::Tap(button()?),
        "stick" => {
            let (which, at) = rest
                .split_once(':')
                .ok_or_else(|| format!("{step}: stick:L:X,Y"))?;
            let (x, y) = at
                .split_once(',')
                .ok_or_else(|| format!("{step}: stick:L:X,Y"))?;
            Step::Stick {
                right: side(which)?,
                at: egui::vec2(number(x)?, number(y)?),
            }
        }
        "trigger" => {
            let (which, value) = rest
                .split_once(':')
                .ok_or_else(|| format!("{step}: trigger:L:V"))?;
            Step::Trigger {
                right: side(which)?,
                value: number(value)?.max(0.0),
            }
        }
        "wait" => Step::Wait,
        _ => return Err(format!("unknown pad step {step}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_play_one_a_frame_once_started() {
        let mut script = Script::parse(
            "connect; tap:down*2; stick:L:0.5,-1; press:north",
            Family::Xbox,
        )
        .unwrap();
        assert_eq!(script.frame().events, [], "nothing plays before play()");
        script.play();
        let connected = script.frame();
        assert!(matches!(
            &connected.events[..],
            [PadEvent::Connected { pad, initial: false }] if pad.family == Family::Xbox
        ));
        assert_eq!(script.connected().len(), 1);
        for _ in 0..2 {
            assert_eq!(
                script.frame().events,
                [
                    PadEvent::Pressed(PadButton::Down),
                    PadEvent::Released(PadButton::Down)
                ]
            );
        }
        assert_eq!(script.frame().axes.left, egui::vec2(0.5, -1.0));
        let held = script.frame();
        assert_eq!(held.events, [PadEvent::Pressed(PadButton::North)]);
        assert_eq!(held.axes.left, egui::vec2(0.5, -1.0), "the stick stays");
        assert_eq!(script.frame().events, [], "the script has ended");
    }

    #[test]
    fn bad_steps_say_what_is_wrong() {
        assert!(
            Script::parse("tap:A", Family::Generic)
                .unwrap_err()
                .contains("unknown button")
        );
        assert!(Script::parse("jump", Family::Generic).is_err());
        assert!(Script::parse("tap:down*x", Family::Generic).is_err());
        assert!(Script::parse("stick:Q:1,1", Family::Generic).is_err());
    }
}
