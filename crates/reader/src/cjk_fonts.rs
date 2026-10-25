//! The CJK fonts, a pack a language (Settings · Resources, frame 07c): one
//! download a pack, shared by the Resources page and the progress card at
//! the bottom right (frame 03h), and which packs finished this session, so
//! the pages translated into their language are set again in them.

use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

use glyphst_models::{
    catalog::{self, ModelEntry},
    setup::{Phase, Setup, SetupState},
};
use glyphst_ui::overlay::{ProgressAnswer, ProgressCard, Tone};

/// The packs, with the language code each is for and its name.
const PACKS: [(&str, &str, &str); 4] = [
    ("cjk-fonts-sc", "zh-hans", "Chinese (Simplified)"),
    ("cjk-fonts-tc", "zh-hant", "Chinese (Traditional)"),
    ("cjk-fonts-jp", "ja", "Japanese"),
    ("cjk-fonts-kr", "ko", "Korean"),
];

/// How long the card says the fonts are ready.
const DONE_SECONDS: f32 = 6.0;

/// The pack a translation into `language` (a code) is set in.
pub fn pack_for(language: &str) -> &'static ModelEntry {
    let pack = glyphst_pdf_translation_service::cjk_faces(language).pack;
    catalog::entry(pack).expect("every CJK pack is in the catalog")
}

/// The language code and name `pack` is for.
pub fn language_of(pack: &ModelEntry) -> (&'static str, &'static str) {
    PACKS
        .iter()
        .find(|(id, _, _)| *id == pack.id)
        .map_or(("zh-hans", "Chinese"), |(_, code, name)| (*code, *name))
}

/// Whether `pack`'s faces are here: downloaded, or in a glyphst-fonts
/// checkout (or `GLYPHST_FONTS_DIR`) laid out the same way.
pub fn is_here(pack: &ModelEntry) -> bool {
    catalog::is_installed(pack)
        || glyphst_core::paths::fonts_dir().is_some_and(|dir| {
            pack.files
                .iter()
                .filter(|file| file.name.ends_with(".otf"))
                .all(|file| dir.join(file.name).is_file())
        })
}

/// Whether this computer has `pack`'s families of its own (a distribution's
/// Noto CJK, say): it then needs no download.
pub fn on_system(pack: &ModelEntry) -> bool {
    glyphst_pdf_translation_service::system_has_cjk_families(language_of(pack).0)
}

/// `33.1 / 79.6 MB`, as the Resources page sizes downloads.
fn sizes(done: u64, total: u64) -> String {
    let total = crate::sidebar::format_size(total);
    let done = crate::sidebar::format_size(done);
    match total.split_once(' ') {
        Some((_, unit)) if done.ends_with(unit) => {
            format!("{} / {total}", done.trim_end_matches(unit).trim_end())
        }
        _ => format!("{done} / {total}"),
    }
}

/// What the progress card asked for.
pub enum CardAction {
    OpenResources,
}

#[derive(Default)]
pub struct CjkFonts {
    setups: HashMap<&'static str, Setup>,
    /// Packs started this session; those that finish are noted once.
    started: HashSet<&'static str>,
    finished: HashSet<&'static str>,
    /// The pack the card follows, and when it finished.
    card: Option<(&'static str, Option<Instant>)>,
}

impl CjkFonts {
    /// `pack`'s download, read from the disk the first time.
    pub fn setup(&mut self, pack: &'static ModelEntry) -> &mut Setup {
        self.setups
            .entry(pack.id)
            .or_insert_with(|| Setup::files(pack.task.files_dir(), pack.files.to_vec()))
    }

    /// Starts (or resumes) downloading `pack`, followed by the card.
    pub fn start(&mut self, pack: &'static ModelEntry, ctx: &egui::Context) {
        let ctx = ctx.clone();
        self.setup(pack).start(move || ctx.request_repaint());
        self.started.insert(pack.id);
        self.finished.remove(pack.id);
        self.card = Some((pack.id, None));
    }

    /// Forgets `pack`'s download, after its files went.
    pub fn forget(&mut self, pack: &ModelEntry) {
        self.setups.remove(pack.id);
        self.started.remove(pack.id);
    }

    /// Packs whose download finished since the last call.
    pub fn newly_ready(&mut self) -> Vec<&'static str> {
        let ready: Vec<&'static str> = self
            .started
            .iter()
            .filter(|id| !self.finished.contains(*id))
            .filter(|id| {
                self.setups
                    .get(*id)
                    .is_some_and(|setup| setup.state() == SetupState::Ready)
            })
            .copied()
            .collect();
        self.finished.extend(ready.iter().copied());
        ready
    }

    /// A download coming in, for screenshots: the card follows `pack`.
    pub fn show_download(&mut self, pack: &'static ModelEntry) {
        let total = pack.size();
        self.setups.insert(
            pack.id,
            Setup::showing(SetupState::Running {
                phase: Phase::Model,
                done: total * 42 / 100,
                total,
                rate: 2_100_000.0,
                retry_in: None,
            }),
        );
        self.card = Some((pack.id, None));
    }

    /// Draws the card (frame 03h) `lift` points above the bottom right.
    pub fn show_card(&mut self, ctx: &egui::Context, lift: f32) -> Option<CardAction> {
        let (id, finished_at) = self.card?;
        let pack = catalog::entry(id)?;
        let state = self.setups.get(id)?.state();
        let (_, language) = language_of(pack);
        let percent = |done: u64, total: u64| done as f32 / total.max(1) as f32;
        let (icon, tone, title, fraction, line, actions): (_, _, String, _, String, &[&str]) =
            match &state {
                SetupState::Running {
                    phase,
                    done,
                    total,
                    rate,
                    retry_in,
                } => {
                    let line = match (retry_in, phase) {
                        (Some(seconds), _) => format!("network lost · again in {seconds} s"),
                        (None, Phase::Checking) => "connecting…".to_owned(),
                        _ => format!(
                            "{:.0}% · {} · {}",
                            percent(*done, *total) * 100.0,
                            sizes(*done, *total),
                            crate::models_page::rate_line(*rate, state.seconds_left())
                        ),
                    };
                    (
                        "download",
                        Tone::Info,
                        format!("Downloading CJK fonts · {language}"),
                        Some(percent(*done, *total)),
                        line,
                        &["pause", "resources"],
                    )
                }
                SetupState::Paused { done, total } => (
                    "pause",
                    Tone::Info,
                    format!("CJK fonts · {language} · paused"),
                    Some(percent(*done, *total)),
                    format!(
                        "{:.0}% · {} · paused",
                        percent(*done, *total) * 100.0,
                        sizes(*done, *total)
                    ),
                    &["resume", "resources"],
                ),
                SetupState::Failed { error, done, total } => (
                    "circle-alert",
                    Tone::Err,
                    format!("CJK fonts · {language} · stopped"),
                    Some(percent(*done, *total)),
                    format!("stopped: {error}"),
                    &["retry", "resources"],
                ),
                SetupState::Ready => {
                    let finished_at = finished_at.unwrap_or_else(Instant::now);
                    self.card = Some((id, Some(finished_at)));
                    if finished_at.elapsed().as_secs_f32() > DONE_SECONDS {
                        self.card = None;
                        return None;
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(250));
                    (
                        "check",
                        Tone::Ok,
                        format!("CJK fonts ready · {language}"),
                        Some(1.0),
                        "translated pages now use them".to_owned(),
                        &[],
                    )
                }
                // Discarded: nothing comes.
                SetupState::Offer { .. } => {
                    self.card = None;
                    return None;
                }
            };
        let answer = ProgressCard {
            id,
            icon,
            tone,
            title: &title,
            fraction,
            line: &line,
            actions,
        }
        .show(ctx, lift)?;
        match (answer, &state) {
            (ProgressAnswer::Dismiss, _) => self.card = None,
            (ProgressAnswer::Action(0), SetupState::Running { .. }) => {
                if let Some(setup) = self.setups.get(id) {
                    setup.pause();
                }
            }
            (ProgressAnswer::Action(0), _) => self.start(pack, ctx),
            (ProgressAnswer::Action(_), _) => return Some(CardAction::OpenResources),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pack_has_its_language() {
        for (id, code, _) in PACKS {
            let pack = catalog::entry(id).unwrap();
            assert_eq!(pack_for(code).id, id);
            assert_eq!(language_of(pack).0, code);
        }
        assert_eq!(pack_for("fr").id, "cjk-fonts-sc");
    }
}
