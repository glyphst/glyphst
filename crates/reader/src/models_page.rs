//! Settings · Models (frame 07c): every model that runs on this computer,
//! by task. A model is a row of two lines: whether it's in use, its name,
//! size and whether it suits this machine, then its download's progress and
//! what fits its state (download, pause, resume, remove); under them what
//! it gives, at full width, with its device. Nothing downloads unasked.

use std::{collections::HashMap, time::Instant};

use egui::{Align2, Rect, Sense, Ui, vec2};
use glyphst_models::{
    catalog::{self, DiskUsage, ModelEntry, Task},
    machine::Machine,
    setup::{Phase, Setup, SetupState},
};
use glyphst_settings::{OcrDevice, Settings};
use glyphst_ui::{
    button::Button,
    meter::Meter,
    settings as ui_settings,
    theme::{self, Tokens},
    toggle::Switch,
};

use crate::{
    meaning::{MeaningAction, MeaningCard},
    settings_window::{LibraryAction, LibraryFacts},
    user_settings::UserSettings,
};

/// The width of a row's action slot, its progress slot and the gap
/// between a row's parts (frame 07c).
const ACTION_WIDTH: f32 = 104.0;
const PROGRESS_WIDTH: f32 = 210.0;
const GAP: f32 = 10.0;
/// The description's indent, past the in-use dot.
const INDENT: f32 = 22.0;
/// How often the disk usage is read again while the page shows.
const USAGE_SECONDS: f32 = 2.0;

/// The page's downloads between frames: one setup a model (and device).
#[derive(Default)]
pub struct ModelsPage {
    ocr: HashMap<(&'static str, OcrDevice), Setup>,
    /// Search models other than the ones in use (those are the app's), by
    /// id and the `llama-server` they were made with.
    search: HashMap<(&'static str, String), Setup>,
    /// What went wrong removing a model.
    notice: Option<String>,
    /// The folders' sizes, and when they were read.
    usage: Option<(DiskUsage, Instant)>,
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

    /// Downloads in each state, for screenshots: medium coming in, Indra
    /// checking what it kept and PaddleOCR-VL paused.
    pub fn show_downloads(&mut self) {
        let model = |id| catalog::entry(id).expect("a catalog model");
        let medium = model("pp-ocrv6-medium");
        let indra = model("indra-v1.1.0");
        let vl = model("paddleocr-vl-1.5");
        let states = [
            (
                medium,
                SetupState::Running {
                    phase: Phase::Model,
                    done: medium.size() * 42 / 100,
                    total: medium.size(),
                    rate: 2_100_000.0,
                    retry_in: None,
                },
            ),
            (
                indra,
                SetupState::Running {
                    phase: Phase::Checking,
                    done: 40_000_000,
                    total: indra.size(),
                    rate: 0.0,
                    retry_in: None,
                },
            ),
            (
                vl,
                SetupState::Paused {
                    done: vl.size() * 31 / 100,
                    total: vl.size(),
                },
            ),
        ];
        for (entry, state) in states {
            self.ocr
                .insert((entry.id, OcrDevice::Cpu), Setup::showing(state));
        }
    }

    fn ocr_setup(&mut self, entry: &'static ModelEntry, device: OcrDevice) -> &mut Setup {
        self.ocr
            .entry((entry.id, device))
            .or_insert_with(|| glyphst_ocr::setup(entry, ocr_device(device)))
    }

    pub fn show(&mut self, ui: &mut Ui, app: ModelsContext<'_>) {
        let ModelsContext {
            user,
            library,
            facts,
            machine,
            actions,
        } = app;
        ui_settings::group(ui, "", |group| {
            group.note(&format!(
                "this computer ── {}. ★ suggested marks what suits it; nothing downloads until you ask.",
                machine.summary()
            ));
        });
        if let Some(notice) = &self.notice {
            ui_settings::group(ui, "", |group| group.note(notice));
        }
        self.text_recognition(ui, user, machine);
        self.search(ui, library, facts, machine, actions, Task::Embedding);
        self.search(ui, library, facts, machine, actions, Task::Rerank);
        let fresh = self
            .usage
            .as_ref()
            .is_some_and(|(_, read)| read.elapsed().as_secs_f32() < USAGE_SECONDS);
        if !fresh {
            self.usage = Some((catalog::disk_usage(), Instant::now()));
        }
        let usage = self
            .usage
            .as_ref()
            .map(|(usage, _)| *usage)
            .unwrap_or_default();
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

    fn text_recognition(&mut self, ui: &mut Ui, user: &mut UserSettings, machine: &Machine) {
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
                let mut description = entry.note.to_owned();
                if in_use && device == OcrDevice::Gpu {
                    description.push_str(&format!(" {}", gpu_line(machine)));
                }
                let row = Row {
                    in_use,
                    ready: state == SetupState::Ready,
                    entry,
                    suggested: suggested == entry.id,
                    description,
                    progress: progress(&state),
                };
                let mut action = None;
                group.full(|ui| {
                    action = model_row(
                        ui,
                        &row,
                        |ui| action_buttons(ui, setup, &state),
                        |ui| {
                            if !entry.device_choice {
                                muted(ui, "cpu only");
                            } else if in_use {
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
                        },
                    );
                });
                match action {
                    Some(RowAction::Use) => draft.ocr.local.model = entry.id.to_owned(),
                    Some(RowAction::Remove) => self.remove(entry),
                    None => {}
                }
            }
        });
        if draft != *current {
            user.edit(move |settings| *settings = draft);
        }
    }

    fn search(
        &mut self,
        ui: &mut Ui,
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
                if in_use && let Some(card) = card {
                    // The one in use is the app's: its state and engine.
                    let row = Row {
                        in_use: true,
                        ready: card_ready(card),
                        entry,
                        suggested: suggested == entry.id,
                        description: entry.note.to_owned(),
                        progress: card_progress(card),
                    };
                    group.full(|ui| {
                        model_row(
                            ui,
                            &row,
                            |ui| {
                                card_buttons(ui, card, |action| {
                                    actions.push(match task {
                                        Task::Embedding => LibraryAction::Meaning(action),
                                        _ => LibraryAction::Rerank(action),
                                    });
                                });
                                None
                            },
                            |ui| {
                                if task == Task::Rerank && card_ready(card) {
                                    ui.add(Switch(&mut library.rerank));
                                }
                            },
                        );
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
                let mut description = entry.note.to_owned();
                if task == Task::Embedding && state == SetupState::Ready {
                    description.push_str(" Using it reads the library's passages again.");
                }
                let row = Row {
                    in_use: false,
                    ready: state == SetupState::Ready,
                    entry,
                    suggested: suggested == entry.id,
                    description,
                    progress: progress(&state),
                };
                let mut action = None;
                group.full(|ui| {
                    action = model_row(ui, &row, |ui| action_buttons(ui, setup, &state), |_| {});
                });
                match action {
                    Some(RowAction::Use) => match task {
                        Task::Embedding => library.meaning_model = entry.id.to_owned(),
                        _ => library.rerank_model = entry.id.to_owned(),
                    },
                    Some(RowAction::Remove) => self.remove(entry),
                    None => {}
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
        self.usage = None;
    }
}

enum RowAction {
    Use,
    Remove,
}

/// What a row shows.
struct Row {
    in_use: bool,
    /// Downloaded, so the dot picks it.
    ready: bool,
    entry: &'static ModelEntry,
    suggested: bool,
    description: String,
    progress: Option<Progress>,
}

/// A download's progress, in the row's progress slot.
enum Progress {
    /// `━━━━──── 42%  2.1 MB/s · 40 s`; muted while paused.
    Bar {
        fraction: f32,
        text: String,
        paused: bool,
    },
    /// `checking kept 40 MB…`, or why it stopped.
    Note { text: String, error: bool },
}

/// Draws a model's row (frame 07c) and returns what its dot or `actions`
/// asked for; `device` fills the slot beside the description.
fn model_row(
    ui: &mut Ui,
    row: &Row,
    actions: impl FnOnce(&mut Ui) -> Option<RowAction>,
    device: impl FnOnce(&mut Ui),
) -> Option<RowAction> {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width();
    let (line, _) = ui.allocate_exact_size(vec2(width, t.row(28.0)), Sense::hover());
    let y = line.center().y;
    let painter = ui.painter().clone();
    // The dot: in use, or (once downloaded) a choice.
    let dot = Rect::from_center_size(egui::pos2(line.left() + 6.0, y), vec2(18.0, 18.0));
    let mut chosen = None;
    let dot_color = if row.in_use {
        t.accent
    } else {
        let response = ui
            .interact(
                dot,
                ui.scope_id().with(("use", row.entry.id)),
                Sense::click(),
            )
            .on_hover_text(if row.ready {
                "use this model"
            } else {
                "download it to use it"
            });
        if row.ready && response.clicked() {
            chosen = Some(RowAction::Use);
        }
        if row.ready && response.hovered() {
            t.text
        } else {
            t.muted
        }
    };
    painter.text(
        dot.center(),
        Align2::CENTER_CENTER,
        if row.in_use { "●" } else { "○" },
        theme::mono(12.0),
        dot_color,
    );
    let mut x = line.left() + INDENT;
    let mut put = |text: &str, size: f32, color| {
        let rect = painter.text(
            egui::pos2(x, y),
            Align2::LEFT_CENTER,
            text,
            theme::mono(size),
            color,
        );
        x = rect.right() + GAP;
    };
    put(row.entry.name, 12.0, t.text);
    put(&size(row.entry.size()), 11.0, t.muted);
    if row.suggested {
        put("★ suggested", 10.0, t.accent_text);
    }
    let action = Rect::from_min_max(
        egui::pos2(line.right() - ACTION_WIDTH, line.top()),
        line.max,
    );
    let progress_rect = Rect::from_min_max(
        egui::pos2(action.left() - GAP - PROGRESS_WIDTH, line.top()),
        egui::pos2(action.left() - GAP, line.bottom()),
    );
    if let Some(progress) = &row.progress {
        let mut slot = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(progress_rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        slot.spacing_mut().item_spacing.x = 8.0;
        match progress {
            Progress::Bar {
                fraction,
                text,
                paused,
            } => {
                let color = if *paused { t.muted } else { t.accent_text };
                slot.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(theme::mono(10.0))
                            .color(color),
                    )
                    .truncate(),
                );
                let percent = format!("{:.0}%", fraction * 100.0);
                let (bar, _) = slot.allocate_exact_size(vec2(96.0, 14.0), Sense::hover());
                Meter::new(*fraction)
                    .width(bar.width())
                    .label(&percent)
                    .color(if *paused { t.muted } else { t.accent })
                    .paint(&slot, bar);
            }
            Progress::Note { text, error } => {
                slot.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .font(theme::mono(10.0))
                            .color(if *error { t.err } else { t.muted }),
                    )
                    .truncate(),
                );
            }
        }
    }
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(action)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    slot.spacing_mut().item_spacing.x = 6.0;
    let asked = actions(&mut slot);
    // Under them: what it gives, at full width, and its device.
    ui.add_space(4.0);
    let top = ui.cursor().min.y;
    let text_width = (width - INDENT - GAP - ACTION_WIDTH).max(80.0);
    let galley = egui::WidgetText::from(
        egui::RichText::new(&row.description)
            .font(theme::regular(12.0))
            .color(t.muted),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        text_width,
        egui::FontSelection::Default,
    );
    let text_height = galley.size().y;
    painter.galley(egui::pos2(line.left() + INDENT, top), galley, t.muted);
    let device_rect = Rect::from_min_size(
        egui::pos2(line.right() - ACTION_WIDTH, top - 3.0),
        vec2(ACTION_WIDTH, t.row(25.0)),
    );
    let mut slot = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(device_rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    device(&mut slot);
    let bottom = (top + text_height).max(slot.min_rect().bottom());
    ui.allocate_rect(
        Rect::from_min_max(
            egui::pos2(line.left(), top),
            egui::pos2(line.right(), bottom),
        ),
        Sense::hover(),
    );
    chosen.or(asked)
}

/// A row's action for its download `state` (laid out right to left): one
/// button, or resume and discard while paused.
fn action_buttons(ui: &mut Ui, setup: &mut Setup, state: &SetupState) -> Option<RowAction> {
    let start = |setup: &mut Setup, ui: &Ui| {
        let ctx = ui.ctx().clone();
        setup.start(move || ctx.request_repaint());
    };
    let wide = |label| Button::ghost(label).min_width(ACTION_WIDTH);
    match state {
        SetupState::Offer { kept, .. } => {
            if ui
                .add(wide(if *kept > 0 { "resume" } else { "download" }))
                .clicked()
            {
                start(setup, ui);
            }
            None
        }
        SetupState::Running { .. } => {
            if ui.add(wide("pause")).clicked() {
                setup.pause();
            }
            None
        }
        SetupState::Paused { .. } => {
            if glyphst_ui::button::icon(ui, "x", 14.0, false, "discard what came").clicked() {
                setup.discard();
            }
            if ui
                .add(Button::ghost("resume").min_width(ACTION_WIDTH - 34.0))
                .clicked()
            {
                start(setup, ui);
            }
            None
        }
        SetupState::Failed { .. } => {
            if ui.add(wide("retry")).clicked() {
                start(setup, ui);
            }
            None
        }
        SetupState::Ready => ui
            .add(wide("remove"))
            .clicked()
            .then_some(RowAction::Remove),
    }
}

/// How a model's download stands, for its progress slot.
fn progress(state: &SetupState) -> Option<Progress> {
    let fraction = |done: u64, total: u64| done as f32 / total.max(1) as f32;
    let note = |text: String| Progress::Note { text, error: false };
    match state {
        SetupState::Offer { kept, total } if *kept > 0 => Some(Progress::Bar {
            fraction: fraction(*kept, *total),
            text: "paused".to_owned(),
            paused: true,
        }),
        SetupState::Offer { .. } | SetupState::Ready => None,
        SetupState::Running {
            retry_in: Some(seconds),
            ..
        } => Some(note(format!("network lost · again in {seconds} s"))),
        SetupState::Running {
            phase: Phase::Checking,
            done,
            ..
        } => Some(note(if *done > 0 {
            format!("checking kept {}…", size(*done))
        } else {
            "connecting…".to_owned()
        })),
        SetupState::Running {
            phase: Phase::Unpack,
            ..
        } => Some(note("unpacking the engine…".to_owned())),
        SetupState::Running {
            done, total, rate, ..
        } => Some(Progress::Bar {
            fraction: fraction(*done, *total),
            text: rate_line(*rate, state.seconds_left()),
            paused: false,
        }),
        SetupState::Paused { done, total } => Some(Progress::Bar {
            fraction: fraction(*done, *total),
            text: "paused".to_owned(),
            paused: true,
        }),
        SetupState::Failed { error, .. } => Some(Progress::Note {
            text: format!("stopped: {error}"),
            error: true,
        }),
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
        "On the GPU through CoreML.".to_owned()
    } else if gpu_possible(machine) {
        "On the GPU through CUDA: needs CUDA 12 and cuDNN 9; switching takes effect after a restart."
            .to_owned()
    } else {
        "No NVIDIA GPU found: the GPU build cannot run here.".to_owned()
    }
}

fn ocr_device(device: OcrDevice) -> glyphst_ocr::Device {
    match device {
        OcrDevice::Cpu => glyphst_ocr::Device::Cpu,
        OcrDevice::Gpu => glyphst_ocr::Device::Gpu,
    }
}

/// The search model in use's state, for its progress slot.
fn card_progress(card: &MeaningCard) -> Option<Progress> {
    let fraction = |done: u64, total: u64| done as f32 / total.max(1) as f32;
    let note = |text: &str| {
        Some(Progress::Note {
            text: text.to_owned(),
            error: false,
        })
    };
    match card {
        MeaningCard::Offer { kept, total } if *kept > 0 => Some(Progress::Bar {
            fraction: fraction(*kept, *total),
            text: "paused".to_owned(),
            paused: true,
        }),
        MeaningCard::Offer { .. } => None,
        MeaningCard::Downloading {
            done,
            total,
            rate,
            seconds_left,
            ..
        } => Some(Progress::Bar {
            fraction: fraction(*done, *total),
            text: rate_line(*rate, *seconds_left),
            paused: false,
        }),
        MeaningCard::Retrying { seconds, .. } => {
            note(&format!("network lost · again in {seconds} s"))
        }
        MeaningCard::Paused { done, total } => Some(Progress::Bar {
            fraction: fraction(*done, *total),
            text: "paused".to_owned(),
            paused: true,
        }),
        MeaningCard::Failed { title, .. } => Some(Progress::Note {
            text: title.clone(),
            error: true,
        }),
        MeaningCard::Starting => note("starting the engine…"),
        MeaningCard::EngineFailed { log } => Some(Progress::Note {
            text: format!("the engine did not start · {log}"),
            error: true,
        }),
        MeaningCard::OnCpu => note("✓ running on the CPU"),
        MeaningCard::Ready => note("✓ running"),
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

/// The in-use search model's buttons, as [`action_buttons`] lays them.
fn card_buttons(ui: &mut Ui, card: &MeaningCard, mut act: impl FnMut(MeaningAction)) {
    let button = |ui: &mut Ui, label: &str, width: f32, action: MeaningAction| {
        ui.add(Button::ghost(label).min_width(width))
            .clicked()
            .then_some(action)
    };
    let asked = match card {
        MeaningCard::Offer { kept, .. } => button(
            ui,
            if *kept > 0 { "resume" } else { "download" },
            ACTION_WIDTH,
            MeaningAction::Download,
        ),
        MeaningCard::Downloading { .. } | MeaningCard::Retrying { .. } => {
            button(ui, "pause", ACTION_WIDTH, MeaningAction::Pause)
        }
        MeaningCard::Paused { .. } => {
            let discard = glyphst_ui::button::icon(ui, "x", 14.0, false, "discard what came")
                .clicked()
                .then_some(MeaningAction::Discard);
            button(ui, "resume", ACTION_WIDTH - 34.0, MeaningAction::Download).or(discard)
        }
        MeaningCard::Failed { .. } | MeaningCard::EngineFailed { .. } => {
            button(ui, "retry", ACTION_WIDTH, MeaningAction::Download)
        }
        MeaningCard::Starting | MeaningCard::OnCpu | MeaningCard::Ready => None,
    };
    if let Some(action) = asked {
        act(action);
    }
}

/// `2.1 MB/s · 40 s`, short enough for the progress slot.
fn rate_line(rate: f64, seconds_left: Option<u64>) -> String {
    if rate <= 0.0 {
        return "starting".to_owned();
    }
    let mut line = format!("{:.1} MB/s", rate / 1_000_000.0);
    if let Some(seconds) = seconds_left {
        line.push_str(&match seconds {
            0..60 => format!(" · {seconds} s"),
            60..3_600 => format!(" · {} min", seconds.div_ceil(60)),
            _ => format!(" · {:.1} h", seconds as f64 / 3_600.0),
        });
    }
    line
}

fn muted(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(
        egui::RichText::new(text)
            .font(theme::mono(11.0))
            .color(t.muted),
    );
}

fn size(bytes: u64) -> String {
    crate::sidebar::format_size(bytes)
}
