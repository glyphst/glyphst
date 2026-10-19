//! Search by meaning, as the user sees it (frame 04o): offered once with
//! its size, downloaded with a progress bar that survives network failures
//! and quitting, then running. The card in the search results and the row
//! in Settings › Library show the same state.

use std::sync::{Arc, mpsc::Receiver};

use glyphst_embed::{
    llama::{LlamaEmbedder, LlamaReranker},
    server::{ServerConfig, ServerError},
    setup::{Phase, Setup, SetupState},
};

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
    /// Downloaded; the engine starts.
    Starting,
    /// The engine did not start, or stopped: the line that says why.
    EngineFailed {
        log: String,
    },
    /// On, on the CPU: no GPU it could use.
    OnCpu,
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

/// The card once the engine's state is known: a ready setup is only
/// ready once its engine runs.
pub fn with_engine(card: MeaningCard, engine: &EngineStatus) -> MeaningCard {
    match (card, engine) {
        (MeaningCard::Ready, EngineStatus::Off | EngineStatus::Starting) => MeaningCard::Starting,
        (MeaningCard::Ready, EngineStatus::Failed(log)) => {
            MeaningCard::EngineFailed { log: log.clone() }
        }
        (MeaningCard::Ready, EngineStatus::Running { on_gpu: false }) => MeaningCard::OnCpu,
        (card, _) => card,
    }
}

/// Where the engine is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineStatus {
    Off,
    Starting,
    Running { on_gpu: bool },
    Failed(String),
}

/// A model served by its own `llama-server`: the embedder or the reranker.
pub trait Served: Send + Sync + Sized + 'static {
    const NAME: &'static str;
    fn start(config: &ServerConfig) -> Result<Self, ServerError>;
    fn is_alive(&self) -> bool;
    fn log_tail(&self) -> String;
    /// Whether it found a GPU (else it runs on the CPU, more slowly).
    fn on_gpu(&self) -> bool {
        true
    }
}

impl Served for LlamaEmbedder {
    const NAME: &'static str = "meaning";
    fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        LlamaEmbedder::start(config)
    }
    fn is_alive(&self) -> bool {
        LlamaEmbedder::is_alive(self)
    }
    fn log_tail(&self) -> String {
        LlamaEmbedder::log_tail(self)
    }
    fn on_gpu(&self) -> bool {
        LlamaEmbedder::on_gpu(self)
    }
}

impl Served for LlamaReranker {
    const NAME: &'static str = "rerank";
    fn start(config: &ServerConfig) -> Result<Self, ServerError> {
        LlamaReranker::start(config)
    }
    fn is_alive(&self) -> bool {
        LlamaReranker::is_alive(self)
    }
    fn log_tail(&self) -> String {
        LlamaReranker::log_tail(self)
    }
}

enum Engine<T> {
    Off,
    Starting(Receiver<Result<Arc<T>, String>>),
    Running(Arc<T>),
    Failed(String),
}

/// A model's engine once its setup is ready: started on a thread (loading
/// the model takes a moment), watched, and stopped when dropped or no
/// longer wanted.
pub struct Runtime<T> {
    engine: Engine<T>,
}

pub type MeaningRuntime = Runtime<LlamaEmbedder>;
pub type RerankRuntime = Runtime<LlamaReranker>;

impl<T> Default for Runtime<T> {
    fn default() -> Self {
        Self {
            engine: Engine::Off,
        }
    }
}

impl<T: Served> Runtime<T> {
    pub fn status(&self) -> EngineStatus {
        match &self.engine {
            Engine::Off => EngineStatus::Off,
            Engine::Starting(_) => EngineStatus::Starting,
            Engine::Running(served) => EngineStatus::Running {
                on_gpu: served.on_gpu(),
            },
            Engine::Failed(log) => EngineStatus::Failed(log.clone()),
        }
    }

    /// Tries starting again after a failure.
    pub fn retry(&mut self) {
        if matches!(self.engine, Engine::Failed(_)) {
            self.engine = Engine::Off;
        }
    }

    /// Starts the engine for `paths` (the server and the model, once
    /// ready; None stops it). Returns the model when it starts, or `None`
    /// inside when it stops, for the search to take.
    #[must_use]
    pub fn poll(
        &mut self,
        paths: Option<(std::path::PathBuf, std::path::PathBuf)>,
        ctx: &egui::Context,
    ) -> Option<Option<Arc<T>>> {
        let Some((exe, model)) = paths else {
            let was_running = matches!(self.engine, Engine::Running(_));
            self.engine = Engine::Off;
            return was_running.then_some(None);
        };
        match &self.engine {
            Engine::Off => {
                let config = ServerConfig {
                    exe,
                    model,
                    library_dirs: glyphst_embed::server::vulkan_dirs(),
                    pid_file: Some(
                        glyphst_embed::install::root().join(format!("{}-server.pid", T::NAME)),
                    ),
                };
                let (sender, receiver) = std::sync::mpsc::channel();
                let ctx = ctx.clone();
                let started = std::thread::Builder::new()
                    .name(format!("{}-engine", T::NAME))
                    .spawn(move || {
                        let result = T::start(&config)
                            .map(Arc::new)
                            .map_err(|error| error.to_string());
                        let _ = sender.send(result);
                        ctx.request_repaint();
                    });
                self.engine = match started {
                    Ok(_) => Engine::Starting(receiver),
                    Err(error) => Engine::Failed(error.to_string()),
                };
                None
            }
            Engine::Starting(receiver) => match receiver.try_recv() {
                Ok(Ok(served)) => {
                    log::info!("{} engine runs (on a GPU: {})", T::NAME, served.on_gpu());
                    self.engine = Engine::Running(served.clone());
                    Some(Some(served))
                }
                Ok(Err(error)) => {
                    self.engine = Engine::Failed(error);
                    None
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.engine = Engine::Failed("the engine's starter stopped".to_string());
                    None
                }
            },
            Engine::Running(served) => {
                if served.is_alive() {
                    return None;
                }
                let log = served.log_tail();
                self.engine = Engine::Failed(if log.is_empty() {
                    "the engine stopped".to_string()
                } else {
                    log
                });
                Some(None)
            }
            Engine::Failed(_) => None,
        }
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

/// The reranker's setup: its model on the same engine.
pub fn rerank_setup(custom_engine: Option<&str>) -> Setup {
    let custom = custom_engine
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    Setup::for_model(
        glyphst_embed::install::root(),
        custom,
        glyphst_embed::manifest::RERANKER,
    )
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
