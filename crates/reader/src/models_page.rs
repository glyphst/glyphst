//! Settings · Models (frame 07c): every model that runs on this computer,
//! by task. Each row says what the model gives and costs, whether it suits
//! this machine, and offers what fits its state: download (accepting a
//! license first when it has terms), pause, resume, use, or remove. Local
//! OCR's models choose their device here too. Nothing downloads unasked.

use std::collections::HashMap;

use egui::RichText;
use glyphst_models::{
    catalog::{self, ModelEntry, Task},
    machine::Machine,
    setup::{Setup, SetupState},
};
use glyphst_settings::{OcrDevice, Settings};
use glyphst_ui::{button::Button, settings as ui_settings, theme, toggle::Switch};

use crate::{
    meaning::{MeaningAction, MeaningCard},
    settings_window::{LibraryAction, LibraryFacts},
    user_settings::UserSettings,
};

/// The page's downloads between frames: one setup a model (and device).
#[derive(Default)]
pub struct ModelsPage {
    ocr: HashMap<(&'static str, OcrDevice), Setup>,
    /// Search models other than the ones in use (those are the app's), by
    /// id and the `llama-server` they were made with.
    search: HashMap<(&'static str, String), Setup>,
    /// What went wrong removing a model.
    notice: Option<String>,
}

/// What the page needs from the app.
pub struct ModelsContext<'a> {
    pub user: &'a mut UserSettings,
    pub library: &'a mut crate::prefs::LibraryPrefs,
    pub facts: &'a LibraryFacts,
    pub machine: &'a Machine,
    pub actions: &'a mut Vec<LibraryAction>,
}

impl ModelsPage {
    /// Starts downloading the local OCR model `settings` use, as the
    /// translation panel's install button asks.
    pub fn start_local_ocr(&mut self, settings: &Settings, ctx: &egui::Context) {
        let entry = catalog::entry(&settings.ocr.local.model)
            .unwrap_or_else(|| catalog::default_for(Task::TextRecognition));
        let ctx = ctx.clone();
        self.ocr_setup(entry, settings.ocr.local.device)
            .start(move || ctx.request_repaint());
    }

    fn ocr_setup(&mut self, entry: &'static ModelEntry, device: OcrDevice) -> &mut Setup {
        self.ocr
            .entry((entry.id, device))
            .or_insert_with(|| glyphst_ocr::setup(entry, ocr_device(device)))
    }

    pub fn show(&mut self, ui: &mut egui::Ui, app: ModelsContext<'_>) {
        let ModelsContext {
            user,
            library,
            facts,
            machine,
            actions,
        } = app;
        ui_settings::group(ui, "", |group| {
            group.note(&format!(
                "this computer ── {}. ★ marks what suits it; nothing downloads until you ask.",
                machine.summary()
            ));
        });
        if let Some(notice) = &self.notice {
            ui_settings::group(ui, "", |group| group.note(notice));
        }
        self.text_recognition(ui, user, machine);
        self.search(ui, library, facts, machine, actions, Task::Embedding);
        self.search(ui, library, facts, machine, actions, Task::Rerank);
        let usage = catalog::disk_usage();
        ui_settings::group(ui, "on disk", |group| {
            group.row(
                "models and engines",
                Some(&format!(
                    "models {} · engines {} · partial downloads {}",
                    size(usage.models),
                    size(usage.engines),
                    size(usage.partial)
                )),
                |ui| {
                    if ui.add(Button::bracket_muted("open folder")).clicked() {
                        actions.push(LibraryAction::OpenFile(glyphst_core::paths::data_dir()));
                    }
                },
            );
        });
    }

    fn text_recognition(&mut self, ui: &mut egui::Ui, user: &mut UserSettings, machine: &Machine) {
        let Some(current) = user.draft().cloned() else {
            return;
        };
        let mut draft = Settings::clone(&current);
        let suggested = machine.suggestion(Task::TextRecognition).id;
        let gpu = gpu_possible(machine);
        ui_settings::group(ui, "text recognition (ocr)", |group| {
            for entry in catalog::for_task(Task::TextRecognition) {
                let in_use = draft.ocr.local.model == entry.id;
                let device = if in_use {
                    draft.ocr.local.device
                } else {
                    OcrDevice::Cpu
                };
                let setup = self
                    .ocr
                    .entry((entry.id, device))
                    .or_insert_with(|| glyphst_ocr::setup(entry, ocr_device(device)));
                let state = setup.state();
                let mut caption = description(entry, suggested == entry.id);
                caption.push_str(&format!("\n{}", state_line(&state, entry)));
                if in_use && device == OcrDevice::Gpu {
                    caption.push_str(&format!("\n{}", gpu_line(machine)));
                }
                let title = row_title(entry, in_use);
                let mut removed = None;
                group.row(&title, Some(&caption), |ui| {
                    match action_buttons(ui, setup, &state, entry, in_use) {
                        Some(RowAction::Use) => draft.ocr.local.model = entry.id.to_owned(),
                        Some(RowAction::Remove) => removed = Some(entry),
                        None => {}
                    }
                    if in_use && entry.device_choice {
                        let mut chosen = draft.ocr.local.device;
                        ui.add_enabled_ui(gpu || chosen == OcrDevice::Gpu, |ui| {
                            glyphst_ui::segmented::labels(
                                ui,
                                &mut chosen,
                                &[(OcrDevice::Cpu, "cpu"), (OcrDevice::Gpu, "gpu")],
                            );
                        });
                        draft.ocr.local.device = chosen;
                    }
                });
                if let Some(entry) = removed {
                    self.remove(entry);
                }
            }
        });
        if draft != *current {
            user.edit(move |settings| *settings = draft);
        }
    }

    fn search(
        &mut self,
        ui: &mut egui::Ui,
        library: &mut crate::prefs::LibraryPrefs,
        facts: &LibraryFacts,
        machine: &Machine,
        actions: &mut Vec<LibraryAction>,
        task: Task,
    ) {
        let (title, chosen, card) = match task {
            Task::Embedding => (
                "search by meaning",
                library.meaning_model.clone(),
                facts.meaning.as_ref(),
            ),
            _ => (
                "rerank",
                library.rerank_model.clone(),
                facts.rerank.as_ref(),
            ),
        };
        let suggested = machine.suggestion(task).id;
        let engine = library.llama_server.clone();
        ui_settings::group(ui, title, |group| {
            for entry in catalog::for_task(task) {
                let in_use = chosen == entry.id;
                let mut caption = description(entry, suggested == entry.id);
                let row = row_title(entry, in_use);
                if in_use && let Some(card) = card {
                    // The one in use is the app's: its state and engine.
                    caption.push_str(&format!("\n{}", card_line(card)));
                    group.row(&row, Some(&caption), |ui| {
                        card_buttons(ui, card, |action| {
                            actions.push(match task {
                                Task::Embedding => LibraryAction::Meaning(action),
                                _ => LibraryAction::Rerank(action),
                            });
                        });
                        if task == Task::Rerank && card_ready(card) {
                            ui.add(Switch(&mut library.rerank));
                        }
                    });
                    continue;
                }
                let setup = self
                    .search
                    .entry((entry.id, engine.clone()))
                    .or_insert_with(|| match task {
                        Task::Embedding => crate::meaning::setup(Some(&engine), entry.id),
                        _ => crate::meaning::rerank_setup(Some(&engine), entry.id),
                    });
                let state = setup.state();
                caption.push_str(&format!("\n{}", state_line(&state, entry)));
                if task == Task::Embedding && state == SetupState::Ready {
                    caption.push_str("\nusing it reads the library's passages again");
                }
                let mut removed = None;
                group.row(&row, Some(&caption), |ui| {
                    match action_buttons(ui, setup, &state, entry, false) {
                        Some(RowAction::Use) => match task {
                            Task::Embedding => library.meaning_model = entry.id.to_owned(),
                            _ => library.rerank_model = entry.id.to_owned(),
                        },
                        Some(RowAction::Remove) => removed = Some(entry),
                        None => {}
                    }
                });
                if let Some(entry) = removed {
                    self.remove(entry);
                }
            }
        });
    }

    /// Deletes `entry`'s files and forgets its setups, so its rows read
    /// the disk again.
    fn remove(&mut self, entry: &'static ModelEntry) {
        self.notice = catalog::remove(entry)
            .err()
            .map(|error| format!("could not remove {}: {error}", entry.name));
        self.ocr.retain(|(id, _), _| *id != entry.id);
        self.search.retain(|(id, _), _| *id != entry.id);
    }
}

enum RowAction {
    Use,
    Remove,
}

/// A row's buttons for its download `state` (laid out right to left).
fn action_buttons(
    ui: &mut egui::Ui,
    setup: &mut Setup,
    state: &SetupState,
    entry: &ModelEntry,
    in_use: bool,
) -> Option<RowAction> {
    let start = |setup: &mut Setup, ui: &egui::Ui| {
        let ctx = ui.ctx().clone();
        setup.start(move || ctx.request_repaint());
    };
    match state {
        SetupState::Offer { kept, .. } => {
            let label = match (*kept > 0, entry.license.must_accept) {
                (true, _) => "resume",
                (false, true) => "accept & download",
                (false, false) => "download",
            };
            if ui.add(Button::bracket(label)).clicked() {
                start(setup, ui);
            }
            None
        }
        SetupState::Running { .. } => {
            if ui.add(Button::bracket_muted("pause")).clicked() {
                setup.pause();
            }
            None
        }
        SetupState::Paused { .. } => {
            if ui.add(Button::bracket("resume")).clicked() {
                start(setup, ui);
            }
            if ui.add(Button::bracket_muted("discard")).clicked() {
                setup.discard();
            }
            None
        }
        SetupState::Failed { .. } => {
            if ui.add(Button::bracket("retry")).clicked() {
                start(setup, ui);
            }
            None
        }
        SetupState::Ready if in_use => {
            ui.label(
                RichText::new("● in use")
                    .font(theme::mono(11.0))
                    .color(theme::Tokens::get(ui.ctx()).accent_text),
            );
            None
        }
        SetupState::Ready => {
            let mut action = None;
            if ui.add(Button::bracket_muted("remove")).clicked() {
                action = Some(RowAction::Remove);
            }
            if ui.add(Button::bracket("use")).clicked() {
                action = Some(RowAction::Use);
            }
            action
        }
    }
}

/// `pp-ocrv6 small ★` (lower-cased by the row).
fn row_title(entry: &ModelEntry, in_use: bool) -> String {
    if in_use {
        format!("● {}", entry.name)
    } else {
        entry.name.to_owned()
    }
}

/// What a model gives, its size and license, and ★ when it suits.
fn description(entry: &ModelEntry, suggested: bool) -> String {
    let mut line = String::new();
    if suggested {
        line.push_str("★ suits this computer · ");
    }
    line.push_str(entry.note);
    line.push_str(&format!(
        "\n{} · {}",
        size(entry.size()),
        entry.license.name
    ));
    if entry.license.must_accept {
        line.push_str(&format!(
            "\n{} Terms: {}",
            entry.license.summary, entry.license.url
        ));
    }
    line
}

/// How a model's download stands.
fn state_line(state: &SetupState, entry: &ModelEntry) -> String {
    match state {
        SetupState::Offer { kept, total } if *kept > 0 => {
            format!("paused · {} of {} kept", size(*kept), size(*total))
        }
        SetupState::Offer { total, .. } if *total > entry.size() => {
            format!("not downloaded · {} with its engine", size(*total))
        }
        SetupState::Offer { .. } => "not downloaded".to_owned(),
        SetupState::Running {
            done,
            total,
            retry_in: Some(seconds),
            ..
        } => format!(
            "network lost · trying again in {seconds} s · {} of {}",
            size(*done),
            size(*total)
        ),
        SetupState::Running {
            done, total, rate, ..
        } => format!(
            "↓ {} of {} · {:.1} MB/s",
            size(*done),
            size(*total),
            rate / 1_000_000.0
        ),
        SetupState::Paused { done, total } => {
            format!("paused · {} of {} kept", size(*done), size(*total))
        }
        SetupState::Failed { error, .. } => format!("stopped: {error}"),
        SetupState::Ready => "✓ installed".to_owned(),
    }
}

/// Whether local OCR can use a GPU here: an NVIDIA card (with a CUDA build
/// of the runtime for this system), or a Mac.
fn gpu_possible(machine: &Machine) -> bool {
    glyphst_ocr::manifest::runtime(glyphst_ocr::Device::Gpu).is_some()
        && (cfg!(target_os = "macos") || machine.gpu.as_ref().is_some_and(|gpu| gpu.nvidia))
}

fn gpu_line(machine: &Machine) -> String {
    if cfg!(target_os = "macos") {
        "on the GPU through CoreML".to_owned()
    } else if gpu_possible(machine) {
        "on the GPU through CUDA: needs CUDA 12 and cuDNN 9; switching takes effect after a restart"
            .to_owned()
    } else {
        "no NVIDIA GPU found: the GPU build cannot run here".to_owned()
    }
}

fn ocr_device(device: OcrDevice) -> glyphst_ocr::Device {
    match device {
        OcrDevice::Cpu => glyphst_ocr::Device::Cpu,
        OcrDevice::Gpu => glyphst_ocr::Device::Gpu,
    }
}

/// The search model in use's state, as Settings · Library says it.
fn card_line(card: &MeaningCard) -> String {
    use crate::meaning::{megabytes, speed};
    match card {
        MeaningCard::Offer { kept, total } if *kept > 0 => {
            format!("paused · {} kept", megabytes(*kept, *total))
        }
        MeaningCard::Offer { total, .. } => format!("not downloaded · {}", size(*total)),
        MeaningCard::Downloading {
            done,
            total,
            rate,
            seconds_left,
            ..
        } => format!(
            "↓ {} · {}",
            megabytes(*done, *total),
            speed(*rate, *seconds_left)
        ),
        MeaningCard::Retrying { seconds, .. } => {
            format!("network lost · trying again in {seconds} s")
        }
        MeaningCard::Paused { done, total } => {
            format!("paused · {} kept", megabytes(*done, *total))
        }
        MeaningCard::Failed { title, .. } => title.clone(),
        MeaningCard::Starting => "starting the engine".to_owned(),
        MeaningCard::EngineFailed { log } => format!("the engine did not start · {log}"),
        MeaningCard::OnCpu => "✓ running on the CPU (no GPU it can use)".to_owned(),
        MeaningCard::Ready => "✓ running".to_owned(),
    }
}

fn card_ready(card: &MeaningCard) -> bool {
    matches!(
        card,
        MeaningCard::Starting
            | MeaningCard::EngineFailed { .. }
            | MeaningCard::OnCpu
            | MeaningCard::Ready
    )
}

/// The in-use search model's buttons.
fn card_buttons(ui: &mut egui::Ui, card: &MeaningCard, mut act: impl FnMut(MeaningAction)) {
    let mut button = |label: &str, accent: bool, action: MeaningAction| {
        let button = if accent {
            Button::bracket(label)
        } else {
            Button::bracket_muted(label)
        };
        if ui.add(button).clicked() {
            act(action);
        }
    };
    match card {
        MeaningCard::Offer { kept, .. } => button(
            if *kept > 0 { "resume" } else { "download" },
            true,
            MeaningAction::Download,
        ),
        MeaningCard::Downloading { .. } | MeaningCard::Retrying { .. } => {
            button("pause", false, MeaningAction::Pause);
        }
        MeaningCard::Paused { .. } => {
            button("discard", false, MeaningAction::Discard);
            button("resume", true, MeaningAction::Download);
        }
        MeaningCard::Failed { .. } | MeaningCard::EngineFailed { .. } => {
            button("retry", true, MeaningAction::Download);
        }
        MeaningCard::Starting | MeaningCard::OnCpu | MeaningCard::Ready => {}
    }
}

fn size(bytes: u64) -> String {
    crate::sidebar::format_size(bytes)
}
