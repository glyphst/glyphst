//! Setting meaning search up: the engine (unless one is given), then the
//! model, under one progress bar. Network failures are retried after 5 s,
//! 30 s and 2 min before the setup stops and says so; what came is kept,
//! so a later run (or the next launch) goes on from there. Nothing starts
//! until asked.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use crate::{
    download::{self, DownloadError, Progress},
    install,
    manifest::{self, Engine, MODEL},
};

const RETRIES: [u64; 3] = [5, 30, 120];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Engine,
    Unpack,
    Model,
}

/// Why setting up stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupError {
    Network(String),
    Http(u16),
    DiskFull,
    Disk(String),
    /// The download was not the file pinned; it was deleted.
    Checksum,
    Unpack(String),
    /// llama.cpp builds no engine for this computer; one can be named in
    /// Settings.
    NoEngine,
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(error) => write!(formatter, "network lost · {error}"),
            Self::Http(status) => write!(formatter, "the server answered {status}"),
            Self::DiskFull => formatter.write_str("the disk is full"),
            Self::Disk(error) => write!(formatter, "could not write: {error}"),
            Self::Checksum => formatter.write_str("the download did not match its checksum"),
            Self::Unpack(error) => write!(formatter, "could not unpack the engine: {error}"),
            Self::NoEngine => formatter.write_str("no engine is built for this computer"),
        }
    }
}

/// Where setting up is.
#[derive(Clone, Debug, PartialEq)]
pub enum SetupState {
    /// Not asked yet; `kept` bytes from an earlier run wait to be resumed.
    Offer {
        kept: u64,
        total: u64,
    },
    Running {
        phase: Phase,
        done: u64,
        total: u64,
        rate: f64,
        /// Waiting this many seconds before trying again.
        retry_in: Option<u64>,
    },
    /// Paused by the user; resumes from `done`.
    Paused {
        done: u64,
        total: u64,
    },
    Failed {
        error: SetupError,
        done: u64,
        total: u64,
    },
    Ready,
}

impl SetupState {
    pub fn seconds_left(&self) -> Option<u64> {
        match self {
            Self::Running {
                done, total, rate, ..
            } if *rate > 1.0 => Some((total.saturating_sub(*done) as f64 / rate) as u64),
            _ => None,
        }
    }
}

/// What has to be fetched, and where.
#[derive(Clone, Debug)]
struct Plan {
    root: PathBuf,
    /// None when the user names their own server, or none is built here.
    engine: Option<Engine>,
}

impl Plan {
    fn engine_needed(&self) -> bool {
        self.engine
            .as_ref()
            .is_some_and(|engine| install::engine_exe(&self.root, engine).is_none())
    }

    fn total(&self) -> u64 {
        let engine = self
            .engine
            .as_ref()
            .filter(|_| self.engine_needed())
            .map_or(0, |engine| engine.archive.size);
        engine + MODEL.size
    }

    /// Bytes on disk already: installed parts whole, partial ones as far as
    /// they came.
    fn kept(&self) -> u64 {
        let engine = match &self.engine {
            Some(engine) if self.engine_needed() => download::resumable(
                &install::engine_archive(&self.root, engine),
                &engine.archive,
            )
            .unwrap_or(0),
            _ => 0,
        };
        let model = if install::model_ready(&self.root) {
            MODEL.size
        } else {
            download::resumable(&install::model_path(&self.root), &MODEL).unwrap_or(0)
        };
        engine + model
    }
}

pub struct Setup {
    plan: Plan,
    /// The user's own server, when set.
    custom_engine: Option<PathBuf>,
    state: Arc<Mutex<SetupState>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Setup {
    /// What `root` holds now; `custom_engine` replaces the download.
    pub fn new(root: PathBuf, custom_engine: Option<PathBuf>) -> Self {
        let engine = if custom_engine.is_some() {
            None
        } else {
            manifest::engine()
        };
        let plan = Plan { root, engine };
        let state = Self::state_on_disk(&plan, custom_engine.as_deref());
        Self {
            plan,
            custom_engine,
            state: Arc::new(Mutex::new(state)),
            cancel: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    /// A setup that only shows `state`, for screenshots; it never
    /// downloads.
    pub fn showing(state: SetupState) -> Self {
        Self {
            plan: Plan {
                root: std::env::temp_dir().join("glyphst-meaning-demo"),
                engine: None,
            },
            custom_engine: None,
            state: Arc::new(Mutex::new(state)),
            cancel: Arc::new(AtomicBool::new(true)),
            worker: None,
        }
    }

    /// Whether it downloads now.
    pub fn is_running(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
    }

    fn state_on_disk(plan: &Plan, custom_engine: Option<&Path>) -> SetupState {
        let engine_ok = custom_engine.is_some_and(Path::is_file)
            || plan
                .engine
                .as_ref()
                .is_some_and(|engine| install::engine_exe(&plan.root, engine).is_some());
        if engine_ok && install::model_ready(&plan.root) {
            SetupState::Ready
        } else {
            SetupState::Offer {
                kept: plan.kept(),
                total: plan.total(),
            }
        }
    }

    pub fn state(&self) -> SetupState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or(SetupState::Offer { kept: 0, total: 0 })
    }

    pub fn is_ready(&self) -> bool {
        self.state() == SetupState::Ready
    }

    /// The server to run and the model, once ready.
    pub fn paths(&self) -> Option<(PathBuf, PathBuf)> {
        if !self.is_ready() {
            return None;
        }
        let exe = match &self.custom_engine {
            Some(exe) => exe.clone(),
            None => install::engine_exe(&self.plan.root, self.plan.engine.as_ref()?)?,
        };
        Some((exe, install::model_path(&self.plan.root)))
    }

    /// Downloads (or resumes) on a thread; `notify` is called as it goes.
    pub fn start(&mut self, notify: impl Fn() + Send + 'static) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return;
        }
        if self.plan.engine.is_none() && self.custom_engine.is_none() {
            self.set(SetupState::Failed {
                error: SetupError::NoEngine,
                done: 0,
                total: self.plan.total(),
            });
            notify();
            return;
        }
        self.cancel.store(false, Ordering::Release);
        let (plan, state, cancel) = (self.plan.clone(), self.state.clone(), self.cancel.clone());
        self.worker = std::thread::Builder::new()
            .name("meaning-setup".to_string())
            .spawn(move || run(&plan, &state, &cancel, &notify))
            .ok();
    }

    /// Stops the download; what came is kept.
    pub fn pause(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    /// Stops and forgets what came.
    pub fn discard(&mut self) {
        self.pause();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(engine) = &self.plan.engine {
            download::discard(&install::engine_archive(&self.plan.root, engine));
        }
        download::discard(&install::model_path(&self.plan.root));
        let state = Self::state_on_disk(&self.plan, self.custom_engine.as_deref());
        self.set(state);
    }

    fn set(&self, state: SetupState) {
        if let Ok(mut shared) = self.state.lock() {
            *shared = state;
        }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

fn run(plan: &Plan, state: &Mutex<SetupState>, cancel: &AtomicBool, notify: &dyn Fn()) {
    let set = |next: SetupState| {
        if let Ok(mut shared) = state.lock() {
            *shared = next;
        }
        notify();
    };
    let total = plan.total();
    let mut base = 0;
    // The engine: download, then unpack.
    if let Some(engine) = plan.engine.as_ref().filter(|_| plan.engine_needed()) {
        let archive = install::engine_archive(&plan.root, engine);
        if let Err(stop) = with_retries(
            &engine.archive,
            &archive,
            Phase::Engine,
            base,
            total,
            cancel,
            &set,
        ) {
            set(stop);
            return;
        }
        set(SetupState::Running {
            phase: Phase::Unpack,
            done: engine.archive.size,
            total,
            rate: 0.0,
            retry_in: None,
        });
        if let Err(error) = install::unpack(&plan.root, engine, &archive) {
            set(SetupState::Failed {
                error: SetupError::Unpack(format!("{error:#}")),
                done: engine.archive.size,
                total,
            });
            return;
        }
        install::remove_old_engines(&plan.root, engine);
        base = engine.archive.size;
    }
    // The model.
    if !install::model_ready(&plan.root) {
        let target = install::model_path(&plan.root);
        if let Err(stop) = with_retries(&MODEL, &target, Phase::Model, base, total, cancel, &set) {
            set(stop);
            return;
        }
    }
    set(SetupState::Ready);
}

/// Fetches `asset`, trying again after network failures; returns the state
/// to stop in when it gives up or is paused.
#[allow(clippy::too_many_arguments)]
fn with_retries(
    asset: &manifest::Asset,
    target: &Path,
    phase: Phase,
    base: u64,
    total: u64,
    cancel: &AtomicBool,
    set: &dyn Fn(SetupState),
) -> Result<(), SetupState> {
    let mut attempt = 0;
    loop {
        let mut last = Progress::default();
        let result = download::fetch(asset, target, cancel, &mut |progress| {
            last = progress;
            set(SetupState::Running {
                phase,
                done: base + progress.done,
                total,
                rate: progress.rate,
                retry_in: None,
            });
        });
        let done = base + download::resumable(target, asset).unwrap_or(last.done);
        let error = match result {
            Ok(()) => return Ok(()),
            Err(DownloadError::Cancelled) => return Err(SetupState::Paused { done, total }),
            Err(DownloadError::Network(error)) => SetupError::Network(error),
            Err(DownloadError::Http(status)) => SetupError::Http(status),
            Err(DownloadError::DiskFull) => SetupError::DiskFull,
            Err(DownloadError::Disk(error)) => SetupError::Disk(error),
            Err(DownloadError::Checksum) => SetupError::Checksum,
        };
        let retryable = matches!(error, SetupError::Network(_))
            || matches!(error, SetupError::Http(status) if status >= 500 || status == 429);
        let Some(wait) = RETRIES.get(attempt).filter(|_| retryable) else {
            return Err(SetupState::Failed { error, done, total });
        };
        attempt += 1;
        // Counting down, and stopping early when paused.
        for left in (1..=*wait).rev() {
            if cancel.load(Ordering::Acquire) {
                return Err(SetupState::Paused { done, total });
            }
            set(SetupState::Running {
                phase,
                done,
                total,
                rate: 0.0,
                retry_in: Some(left),
            });
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setup_offers_what_it_kept_and_is_ready_with_its_files() {
        let root = std::env::temp_dir().join(format!("glyphst-embed-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The user's own server, and no model yet.
        let exe = root.join("my-llama-server");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&exe, b"").unwrap();
        let setup = Setup::new(root.clone(), Some(exe.clone()));
        assert_eq!(
            setup.state(),
            SetupState::Offer {
                kept: 0,
                total: MODEL.size
            },
            "only the model to fetch"
        );
        assert_eq!(setup.paths(), None);

        // A model on disk: ready.
        let model = install::model_path(&root);
        std::fs::create_dir_all(model.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&model).unwrap();
        file.set_len(MODEL.size).unwrap();
        let setup = Setup::new(root.clone(), Some(exe.clone()));
        assert!(setup.is_ready());
        assert_eq!(setup.paths(), Some((exe, model)));
        let _ = std::fs::remove_dir_all(root);
    }
}
