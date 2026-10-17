//! The thread that owns gilrs. It blocks on pad events, so an idle app
//! wakes on a button press instead of noticing it on its next frame, and
//! keeps the sticks, triggers and connected pads where frames can read them.

use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

use gilrs::{Axis, Button, EventType, Gamepad, Gilrs, PowerInfo};

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
    for (_, pad) in gilrs.gamepads() {
        let pad = info(&pad);
        if sender
            .send(PadEvent::Connected { pad, initial: true })
            .is_err()
        {
            return;
        }
    }
    publish(gilrs, shared);
    let mut published = Instant::now();
    loop {
        let first = gilrs.next_event_blocking(Some(POWER_EVERY));
        let mut woke = false;
        let mut next = first;
        while let Some(event) = next {
            woke = true;
            let sent = match event.event {
                EventType::ButtonPressed(button, _) => {
                    PadButton::from_gilrs(button).map(PadEvent::Pressed)
                }
                EventType::ButtonReleased(button, _) => {
                    PadButton::from_gilrs(button).map(PadEvent::Released)
                }
                EventType::Connected => Some(PadEvent::Connected {
                    pad: info(&gilrs.gamepad(event.id)),
                    initial: false,
                }),
                EventType::Disconnected => Some(PadEvent::Disconnected(event.id.into())),
                _ => None,
            };
            if let Some(sent) = sent
                && sender.send(sent).is_err()
            {
                return;
            }
            next = gilrs.next_event();
        }
        if woke || published.elapsed() >= POWER_EVERY {
            publish(gilrs, shared);
            published = Instant::now();
        }
        if woke {
            ctx.request_repaint();
        }
    }
}

fn publish(gilrs: &Gilrs, shared: &Mutex<Shared>) {
    let mut axes = Axes::default();
    let mut pads = Vec::new();
    for (_, pad) in gilrs.gamepads() {
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
