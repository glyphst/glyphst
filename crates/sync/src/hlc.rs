//! Hybrid logical clocks: wall time in milliseconds, a counter for changes
//! made in the same millisecond (or while the wall clock is behind one
//! already seen), and the device, so any two changes are ordered and the
//! same on every machine.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// How far ahead of this machine's clock a remote time may pull it. A
/// change from further ahead still merges by its time, but the clock does
/// not follow it, so one machine with a wrong clock cannot drag every other
/// one into the future.
pub const MAX_DRIFT_MS: u64 = 24 * 60 * 60 * 1000;

/// A machine taking part in a library's sync: one per library database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub Uuid);

impl DeviceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const NIL: Self = Self(Uuid::nil());
}

impl Default for DeviceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

impl FromStr for DeviceId {
    type Err = uuid::Error;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

/// One moment: ordered by time, then counter, then device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    pub millis: u64,
    pub counter: u32,
    pub device: DeviceId,
}

impl Hlc {
    pub const ZERO: Self = Self {
        millis: 0,
        counter: 0,
        device: DeviceId::NIL,
    };
}

/// `<millis>.<counter>.<device>` in fixed-width hex, so the text sorts as
/// the clock does.
impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:016x}.{:08x}.{}",
            self.millis,
            self.counter,
            self.device.0.simple()
        )
    }
}

impl FromStr for Hlc {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> anyhow::Result<Self> {
        let mut parts = text.split('.');
        let (Some(millis), Some(counter), Some(device), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            anyhow::bail!("not a clock: {text:?}");
        };
        Ok(Self {
            millis: u64::from_str_radix(millis, 16)?,
            counter: u32::from_str_radix(counter, 16)?,
            device: DeviceId(Uuid::parse_str(device)?),
        })
    }
}

impl Serialize for Hlc {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hlc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// This machine's clock.
#[derive(Clone, Debug)]
pub struct Clock {
    last: Hlc,
}

/// What [`Clock::observe`] made of a remote time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    Followed,
    /// Further ahead than [`MAX_DRIFT_MS`], by this much: not followed.
    Ahead(u64),
}

impl Clock {
    /// The clock of `device`, never behind `persisted` (the last time it
    /// gave out, kept with the library).
    pub fn new(device: DeviceId, persisted: Option<Hlc>) -> Self {
        let last = persisted.map_or(
            Hlc {
                device,
                ..Hlc::ZERO
            },
            |hlc| Hlc { device, ..hlc },
        );
        Self { last }
    }

    pub fn device(&self) -> DeviceId {
        self.last.device
    }

    /// The last time given out or followed.
    pub fn last(&self) -> Hlc {
        self.last
    }

    /// A new time, after every one given out or followed.
    pub fn now(&mut self, wall_ms: u64) -> Hlc {
        if wall_ms > self.last.millis {
            self.last.millis = wall_ms;
            self.last.counter = 0;
        } else {
            self.last.counter += 1;
        }
        self.last
    }

    /// Follows a time seen in another machine's changes, so changes made
    /// here after it come after it.
    pub fn observe(&mut self, remote: Hlc, wall_ms: u64) -> Observed {
        if remote.millis > wall_ms.saturating_add(MAX_DRIFT_MS) {
            return Observed::Ahead(remote.millis - wall_ms);
        }
        if (remote.millis, remote.counter) > (self.last.millis, self.last.counter) {
            self.last.millis = remote.millis;
            self.last.counter = remote.counter;
        }
        Observed::Followed
    }
}

/// Milliseconds since the Unix epoch.
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_sort_as_text_and_round_trip() {
        let device = DeviceId::new();
        let early = Hlc {
            millis: 9,
            counter: 300,
            device,
        };
        let late = Hlc {
            millis: 10,
            counter: 0,
            device,
        };
        assert!(early < late);
        assert!(early.to_string() < late.to_string());
        assert_eq!(late.to_string().parse::<Hlc>().unwrap(), late);
        let json = serde_json::to_string(&early).unwrap();
        assert_eq!(serde_json::from_str::<Hlc>(&json).unwrap(), early);
    }

    #[test]
    fn the_clock_never_runs_backwards() {
        let mut clock = Clock::new(DeviceId::new(), None);
        let first = clock.now(1_000);
        let same_ms = clock.now(1_000);
        let wall_behind = clock.now(500);
        assert!(first < same_ms && same_ms < wall_behind);
        assert_eq!(wall_behind.millis, 1_000);

        let persisted = clock.last();
        let mut reopened = Clock::new(persisted.device, Some(persisted));
        assert!(reopened.now(10) > persisted);
    }

    #[test]
    fn a_clock_far_ahead_is_not_followed() {
        let mut clock = Clock::new(DeviceId::new(), None);
        let wall = 1_000_000;
        let other = DeviceId::new();
        let near = Hlc {
            millis: wall + 60_000,
            counter: 4,
            device: other,
        };
        assert_eq!(clock.observe(near, wall), Observed::Followed);
        assert!(clock.now(wall) > near);

        let far = Hlc {
            millis: wall + MAX_DRIFT_MS + 5,
            counter: 0,
            device: other,
        };
        assert_eq!(clock.observe(far, wall), Observed::Ahead(MAX_DRIFT_MS + 5));
        assert!(clock.now(wall) < far);
    }
}
