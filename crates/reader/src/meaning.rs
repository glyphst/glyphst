//! Search by meaning, as the user sees it (frame 04o): offered once with
//! its size, downloaded with a progress bar that survives network failures
//! and quitting, then running. The card in the search results and the row
//! in Settings › Library show the same state.

use glyphst_embed::setup::{Phase, Setup, SetupState};

/// What the card says.
#[derive(Clone, Debug, PartialEq)]
pub enum MeaningCard {
    /// `[ download ]` (or `[ resume ]` when `kept` > 0).
    Offer {
        kept: u64,
        total: u64,
    },
    Downloading {
        what: &'static str,
        done: u64,
        total: u64,
        rate: f64,
        seconds_left: Option<u64>,
    },
    Retrying {
        done: u64,
        total: u64,
        seconds: u64,
    },
    Paused {
        done: u64,
        total: u64,
    },
    Failed {
        title: String,
        detail: String,
        done: u64,
        total: u64,
        /// The download was deleted (a checksum): start over.
        again: bool,
    },
    Ready,
}

/// What the card asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeaningAction {
    /// Download, resume or retry.
    Download,
    Pause,
    Discard,
    /// Hide the offer (Settings still has it).
    NotNow,
    Settings,
}

pub fn card(state: &SetupState) -> MeaningCard {
    match state {
        SetupState::Offer { kept, total } => MeaningCard::Offer {
            kept: *kept,
            total: *total,
        },
        SetupState::Running {
            done,
            total,
            retry_in: Some(seconds),
            ..
        } => MeaningCard::Retrying {
            done: *done,
            total: *total,
            seconds: *seconds,
        },
        SetupState::Running {
            phase,
            done,
            total,
            rate,
            ..
        } => MeaningCard::Downloading {
            what: match phase {
                Phase::Engine => "engine",
                Phase::Unpack => "unpacking the engine",
                Phase::Model => "model",
            },
            done: *done,
            total: *total,
            rate: *rate,
            seconds_left: state.seconds_left(),
        },
        SetupState::Paused { done, total } => MeaningCard::Paused {
            done: *done,
            total: *total,
        },
        SetupState::Failed { error, done, total } => {
            use glyphst_embed::setup::SetupError;
            let (title, detail, again) = match error {
                SetupError::Network(_) => (
                    "network lost".to_string(),
                    "Tried three times. What came is kept; it goes on from there.".to_string(),
                    false,
                ),
                SetupError::Checksum => (
                    "checksum failed".to_string(),
                    "It was deleted, in case it was damaged on the way.".to_string(),
                    true,
                ),
                SetupError::DiskFull => (
                    "the disk is full".to_string(),
                    "Free some space; what came is kept.".to_string(),
                    false,
                ),
                SetupError::NoEngine => (
                    "no engine for this computer".to_string(),
                    "llama.cpp builds none here; name your own llama-server in Settings › Library."
                        .to_string(),
                    false,
                ),
                other => (other.to_string(), String::new(), false),
            };
            MeaningCard::Failed {
                title,
                detail,
                done: *done,
                total: *total,
                again,
            }
        }
        SetupState::Ready => MeaningCard::Ready,
    }
}

/// `142 / 341 MB`.
pub fn megabytes(done: u64, total: u64) -> String {
    let mb = |bytes: u64| (bytes as f64 / 1_000_000.0).round() as u64;
    format!("{} / {} MB", mb(done), mb(total))
}

/// `4.1 MB/s · about 1 min left`.
pub fn speed(rate: f64, seconds_left: Option<u64>) -> String {
    let mut line = if rate > 0.0 {
        format!("{:.1} MB/s", rate / 1_000_000.0)
    } else {
        "starting".to_string()
    };
    if let Some(seconds) = seconds_left {
        line.push_str(&match seconds {
            0..60 => " · under a minute left".to_string(),
            60..3_600 => format!(" · about {} min left", seconds.div_ceil(60)),
            _ => format!(" · about {:.1} h left", seconds as f64 / 3_600.0),
        });
    }
    line
}

/// The setup for this app: the engine and model in the data folder, or the
/// user's own server.
pub fn setup(custom_engine: Option<&str>) -> Setup {
    let custom = custom_engine
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    Setup::new(glyphst_embed::install::root(), custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_follows_the_setup() {
        assert_eq!(
            card(&SetupState::Running {
                phase: Phase::Model,
                done: 142_000_000,
                total: 341_000_000,
                rate: 4_100_000.0,
                retry_in: None,
            }),
            MeaningCard::Downloading {
                what: "model",
                done: 142_000_000,
                total: 341_000_000,
                rate: 4_100_000.0,
                seconds_left: Some(48),
            }
        );
        assert!(matches!(
            card(&SetupState::Running {
                phase: Phase::Model,
                done: 1,
                total: 2,
                rate: 0.0,
                retry_in: Some(25),
            }),
            MeaningCard::Retrying { seconds: 25, .. }
        ));
        assert_eq!(megabytes(142_000_000, 341_000_000), "142 / 341 MB");
        assert_eq!(
            speed(4_100_000.0, Some(48)),
            "4.1 MB/s · under a minute left"
        );
        assert_eq!(speed(4_100_000.0, Some(130)), "4.1 MB/s · about 3 min left");
    }
}
