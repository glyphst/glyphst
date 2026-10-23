//! The thread that owns gilrs. It blocks on pad events, so an idle app
//! wakes on a button press instead of noticing it on its next frame, and
//! keeps the sticks, triggers and connected pads where frames can read them.

use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use gilrs::{Axis, Button, EventType, Gamepad, Gilrs, MappingSource, PowerInfo};

use super::{Axes, Family, PadButton, PadEvent, PadInfo, Power};

/// How often batteries are read again while no events come.
const POWER_EVERY: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Shared {
    axes: Axes,
    pads: Vec<PadInfo>,
}

pub struct PadThread {
    events: Receiver<PadEvent>,
    shared: Arc<Mutex<Shared>>,
}

impl PadThread {
    pub fn drain(&mut self) -> Vec<PadEvent> {
        self.events.try_iter().collect()
    }

    pub fn axes(&self) -> Axes {
        self.shared
            .lock()
            .map(|shared| shared.axes)
            .unwrap_or_default()
    }

    pub fn connected(&self) -> Vec<PadInfo> {
        self.shared
            .lock()
            .map(|shared| shared.pads.clone())
            .unwrap_or_default()
    }
}

/// Starts the thread; an error when gilrs can't read pads (for example
/// without access to input devices).
pub fn spawn(ctx: egui::Context) -> Result<PadThread, String> {
    let (ready, started) = mpsc::channel();
    let (sender, events) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shared::default()));
    let thread_shared = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("gamepads".into())
        .spawn(move || {
            // Gilrs stays on the thread that made it.
            let mut gilrs = match Gilrs::new() {
                Ok(gilrs) => gilrs,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            run(&mut gilrs, &sender, &thread_shared, &ctx);
        })
        .map_err(|error| error.to_string())?;
    started.recv().map_err(|error| error.to_string())??;
    Ok(PadThread { events, shared })
}

fn run(gilrs: &mut Gilrs, sender: &Sender<PadEvent>, shared: &Mutex<Shared>, ctx: &egui::Context) {
    let mut roster = Roster::default();
    let mut connected = Vec::new();
    for (id, pad) in gilrs.gamepads() {
        if roster.connected(id.into(), known(&pad)) {
            connected.push(PadEvent::Connected {
                pad: info(&pad),
                initial: true,
            });
        }
    }
    publish(gilrs, shared, &roster);
    if connected
        .into_iter()
        .any(|event| sender.send(event).is_err())
    {
        return;
    }
    let mut published = Instant::now();
    loop {
        let first = gilrs.next_event_blocking(Some(POWER_EVERY));
        let mut woke = false;
        let mut next = first;
        while let Some(event) = next {
            woke = true;
            let id = usize::from(event.id);
            let mut sent = Vec::new();
            match event.event {
                EventType::ButtonPressed(button, _) => {
                    let button = PadButton::from_gilrs(button);
                    if roster.pressed(id, button.is_some()) {
                        sent.push(PadEvent::Connected {
                            pad: info(&gilrs.gamepad(event.id)),
                            initial: false,
                        });
                    }
                    if roster.counts(id) {
                        sent.extend(button.map(PadEvent::Pressed));
                    }
                }
                EventType::ButtonReleased(button, _) if roster.counts(id) => {
                    sent.extend(PadButton::from_gilrs(button).map(PadEvent::Released));
                }
                EventType::Connected => {
                    let pad = gilrs.gamepad(event.id);
                    if roster.connected(id, known(&pad)) {
                        sent.push(PadEvent::Connected {
                            pad: info(&pad),
                            initial: false,
                        });
                    }
                }
                EventType::Disconnected if roster.disconnected(id) => {
                    sent.push(PadEvent::Disconnected(id));
                }
                _ => {}
            }
            // The pads are published before their events, so a frame that
            // reads an event finds the pads as they are after it.
            if sent.iter().any(|event| {
                matches!(
                    event,
                    PadEvent::Connected { .. } | PadEvent::Disconnected(_)
                )
            }) {
                publish(gilrs, shared, &roster);
            }
            if sent.into_iter().any(|event| sender.send(event).is_err()) {
                return;
            }
            next = gilrs.next_event();
        }
        if woke || published.elapsed() >= POWER_EVERY {
            publish(gilrs, shared, &roster);
            published = Instant::now();
        }
        if woke {
            ctx.request_repaint();
        }
    }
}

/// Whether gilrs knows `pad` as a game controller from SDL's list (Xbox,
/// PlayStation, Switch, 8BitDo…). Linux also offers devices that only
/// look like one to gilrs (a KVM's absolute mouse, a keyboard's extra
/// keys); those count once they press a pad button.
fn known(pad: &Gamepad<'_>) -> bool {
    pad.mapping_source() == MappingSource::SdlMappings
}

/// The devices that count as pads.
#[derive(Debug, Default)]
struct Roster {
    pads: HashSet<usize>,
}

impl Roster {
    fn counts(&self, id: usize) -> bool {
        self.pads.contains(&id)
    }

    /// A device appeared; whether it counts as a pad now.
    fn connected(&mut self, id: usize, known: bool) -> bool {
        known && self.pads.insert(id)
    }

    /// A device pressed a button (`mapped`: one a pad has); whether that
    /// made it count.
    fn pressed(&mut self, id: usize, mapped: bool) -> bool {
        mapped && self.pads.insert(id)
    }

    /// A device went; whether it was a pad.
    fn disconnected(&mut self, id: usize) -> bool {
        self.pads.remove(&id)
    }
}

fn publish(gilrs: &Gilrs, shared: &Mutex<Shared>, roster: &Roster) {
    let mut axes = Axes::default();
    let mut pads = Vec::new();
    for (id, pad) in gilrs.gamepads() {
        if !roster.counts(id.into()) {
            continue;
        }
        let trigger = |button| pad.button_data(button).map_or(0.0, |data| data.value());
        axes.add(Axes {
            // gilrs reports up as positive.
            left: egui::vec2(pad.value(Axis::LeftStickX), -pad.value(Axis::LeftStickY)),
            right: egui::vec2(pad.value(Axis::RightStickX), -pad.value(Axis::RightStickY)),
            left_trigger: trigger(Button::LeftTrigger2),
            right_trigger: trigger(Button::RightTrigger2),
        });
        pads.push(info(&pad));
    }
    if let Ok(mut shared) = shared.lock() {
        *shared = Shared { axes, pads };
    }
}

fn info(pad: &Gamepad<'_>) -> PadInfo {
    PadInfo {
        id: pad.id().into(),
        name: pad.name().to_string(),
        family: Family::from_vendor(pad.vendor_id()),
        power: match pad.power_info() {
            PowerInfo::Unknown => Power::Unknown,
            PowerInfo::Wired => Power::Wired,
            PowerInfo::Discharging(percent) => Power::Battery {
                percent,
                charging: false,
            },
            PowerInfo::Charging(percent) => Power::Battery {
                percent,
                charging: true,
            },
            PowerInfo::Charged => Power::Battery {
                percent: 100,
                charging: false,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug where "Controller connected" came up with no pad: devices
    /// gilrs doesn't know as pads count only once they press a pad button.
    #[test]
    fn unknown_devices_count_once_they_press_a_pad_button() {
        let mut roster = Roster::default();
        assert!(roster.connected(1, true), "a known pad counts at once");
        assert!(!roster.connected(1, true), "and is announced once");
        assert!(!roster.connected(2, false), "a KVM's mouse doesn't");
        assert!(!roster.pressed(2, false), "nor do its own buttons");
        assert!(!roster.counts(2));
        assert!(roster.pressed(2, true), "a pad button makes it count");
        assert!(!roster.pressed(2, true));
        assert!(roster.disconnected(2));
        assert!(!roster.disconnected(3), "a device that never counted");
    }
}
