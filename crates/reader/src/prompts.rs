//! The questions the Reader asks in a prompt (frames 09a, 09j, 03f, 03g):
//! whether a pad that connects should switch to gamepad mode, whether to
//! leave gamepad mode when the pad goes away, and whether to download the
//! CJK fonts for a translation into Chinese, Japanese or Korean. One shows
//! at a time.

use std::collections::VecDeque;

use glyphst_ui::{
    pad_glyph::{Face, Scale},
    prompt::{Choice, Prompt},
};

use crate::pad::{PadEvent, PadInfo};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    /// A pad connected: switch to gamepad mode?
    Connected { name: String },
    /// The last pad went away in gamepad mode: leave it?
    Disconnected,
    /// B on gamepad mode's home: leave it?
    Leave,
    /// A translation into `language` starts, and this computer has CJK
    /// faces of its own: download the CJK fonts? Asked once ever.
    CjkFontsOffer,
    /// A translation into `language` can't start: this computer has no CJK
    /// faces. Asked each time.
    CjkFontsNeeded { language: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    GamepadMode,
    NotNow,
    NeverAsk,
    LeaveGamepadMode,
    Stay,
    DownloadCjkFonts,
    UseSystemFonts,
    DownloadAndTranslate,
    CancelTranslation,
}

#[derive(Debug, Default)]
pub struct Prompts {
    queue: VecDeque<Question>,
    /// "Not now" holds for the rest of the session.
    declined: bool,
}

impl Prompts {
    pub fn is_open(&self) -> bool {
        !self.queue.is_empty()
    }

    /// Asks `question`, unless it is already waiting.
    pub fn ask(&mut self, question: Question) {
        if !self.queue.contains(&question) {
            self.queue.push_back(question);
        }
    }

    /// Asks what the pad's events call for. `connected` is the pads still
    /// there after them.
    pub fn note(
        &mut self,
        events: &[PadEvent],
        connected: &[PadInfo],
        gamepad_mode: bool,
        ask: bool,
    ) {
        for event in events {
            match event {
                PadEvent::Connected { pad, .. } => {
                    self.queue.retain(|q| *q != Question::Disconnected);
                    let asking = self
                        .queue
                        .iter()
                        .any(|q| matches!(q, Question::Connected { .. }));
                    if ask && !gamepad_mode && !self.declined && !asking {
                        self.queue.push_back(Question::Connected {
                            name: pad.name.clone(),
                        });
                    }
                }
                PadEvent::Disconnected(_) if connected.is_empty() => {
                    self.queue
                        .retain(|q| !matches!(q, Question::Connected { .. }));
                    if gamepad_mode && !self.queue.contains(&Question::Disconnected) {
                        self.queue.push_back(Question::Disconnected);
                    }
                }
                _ => {}
            }
        }
        // A question about a pad that has gone since is not asked.
        if connected.is_empty() {
            self.queue
                .retain(|q| !matches!(q, Question::Connected { .. }));
        }
    }

    /// Shows the first question; the answer given this frame.
    pub fn show(&mut self, ctx: &egui::Context, gamepad_mode: bool) -> Option<Answer> {
        let question = self.queue.front()?.clone();
        let answer = match &question {
            Question::Connected { name } => {
                let body = format!(
                    "{name} is ready. Switch to gamepad mode, a fullscreen reader made for the couch?"
                );
                let choices = [
                    Choice::new("don't ask again", Face::West),
                    Choice::new("not now", Face::East),
                    Choice::new("gamepad mode", Face::South),
                ];
                Prompt {
                    id: egui::Id::unique("prompt-pad-connected"),
                    icon: Some("gamepad-2"),
                    title: "Controller connected",
                    body: &body,
                    choices: &choices,
                    apart: 1,
                    scale: Scale::Chrome,
                }
                .show(ctx)
                .map(|index| [Answer::NeverAsk, Answer::NotNow, Answer::GamepadMode][index])
            }
            Question::Disconnected => {
                let choices = [
                    Choice::new("stay", Face::East),
                    Choice::new("leave gamepad mode", Face::South),
                ];
                Prompt {
                    id: egui::Id::unique("prompt-pad-disconnected"),
                    icon: Some("unplug"),
                    title: "Controller disconnected",
                    body: "Gamepad mode needs a controller. Reconnect it to keep reading here, \
                           or go back to the desktop Reader.",
                    choices: &choices,
                    apart: 0,
                    scale: if gamepad_mode {
                        Scale::Couch
                    } else {
                        Scale::Chrome
                    },
                }
                .show(ctx)
                .map(|index| [Answer::Stay, Answer::LeaveGamepadMode][index])
            }
            Question::Leave => {
                let choices = [
                    Choice::new("stay", Face::East),
                    Choice::new("leave", Face::South),
                ];
                Prompt {
                    id: egui::Id::unique("prompt-leave-gamepad-mode"),
                    icon: Some("log-out"),
                    title: "Leave gamepad mode?",
                    body: "Back to the desktop Reader, in a window again. Select + start \
                           comes back here.",
                    choices: &choices,
                    apart: 0,
                    scale: Scale::Couch,
                }
                .show(ctx)
                .map(|index| [Answer::Stay, Answer::LeaveGamepadMode][index])
            }
            Question::CjkFontsOffer => {
                let body = format!(
                    "Chinese, Japanese and Korean translations use this computer's fonts for \
                     now. The CJK fonts (Noto Sans CJK and Source Han Serif, {}) set them as \
                     the Reader's pages are designed, and may go into exported PDFs. Asked \
                     once; Settings · Resources has them.",
                    cjk_fonts_size()
                );
                let choices = [
                    Choice::new("use this computer's", Face::East),
                    Choice::new("download", Face::South),
                ];
                Prompt {
                    id: egui::Id::unique("prompt-cjk-fonts-offer"),
                    icon: Some("languages"),
                    title: "Download the CJK fonts?",
                    body: &body,
                    choices: &choices,
                    apart: 0,
                    scale: Scale::Chrome,
                }
                .show(ctx)
                .map(|index| [Answer::UseSystemFonts, Answer::DownloadCjkFonts][index])
            }
            Question::CjkFontsNeeded { language } => {
                let body = format!(
                    "This computer has no Chinese, Japanese or Korean fonts, so a translation \
                     into {language} can't be set. Download the CJK fonts ({})? The \
                     translation starts as they arrive.",
                    cjk_fonts_size()
                );
                let choices = [
                    Choice::new("cancel", Face::East),
                    Choice::new("download and translate", Face::South),
                ];
                Prompt {
                    id: egui::Id::unique("prompt-cjk-fonts-needed"),
                    icon: Some("languages"),
                    title: "CJK fonts needed",
                    body: &body,
                    choices: &choices,
                    apart: 0,
                    scale: Scale::Chrome,
                }
                .show(ctx)
                .map(|index| [Answer::CancelTranslation, Answer::DownloadAndTranslate][index])
            }
        };
        if let Some(answer) = answer {
            self.queue.pop_front();
            if answer == Answer::NotNow {
                self.declined = true;
            }
        }
        answer
    }
}

/// The CJK fonts' download, as the Resources page sizes it.
fn cjk_fonts_size() -> String {
    let fonts = glyphst_models::catalog::default_for(glyphst_models::catalog::Task::Fonts);
    crate::sidebar::format_size(fonts.size())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pad::{Family, Power};

    fn pad() -> PadInfo {
        PadInfo {
            id: 0,
            name: "8BitDo Pro 2".into(),
            family: Family::Generic,
            power: Power::Unknown,
        }
    }

    fn connected() -> PadEvent {
        PadEvent::Connected {
            pad: pad(),
            initial: false,
        }
    }

    #[test]
    fn a_pad_connecting_asks_once_and_not_after_not_now() {
        let mut prompts = Prompts::default();
        prompts.note(&[connected(), connected()], &[pad()], false, true);
        assert_eq!(prompts.queue.len(), 1, "one question for two pads");
        prompts.queue.clear();
        prompts.declined = true;
        prompts.note(&[connected()], &[pad()], false, true);
        assert!(!prompts.is_open(), "not now holds for the session");
    }

    /// The bug where "Controller connected" stayed up after its pad went:
    /// a frame with no pads left drops the question, even when the
    /// disconnect itself was seen while the pad was still listed.
    #[test]
    fn a_question_about_a_pad_that_went_is_dropped() {
        let mut prompts = Prompts::default();
        prompts.note(&[connected()], &[pad()], false, true);
        prompts.note(&[PadEvent::Disconnected(0)], &[pad()], false, true);
        assert!(prompts.is_open(), "the pad was still listed");
        prompts.note(&[], &[], false, true);
        assert!(!prompts.is_open());
    }

    #[test]
    fn nothing_is_asked_in_gamepad_mode_or_when_told_not_to() {
        let mut prompts = Prompts::default();
        prompts.note(&[connected()], &[pad()], true, true);
        prompts.note(&[connected()], &[pad()], false, false);
        assert!(!prompts.is_open());
    }

    #[test]
    fn leaving_is_asked_once() {
        let mut prompts = Prompts::default();
        prompts.ask(Question::Leave);
        prompts.ask(Question::Leave);
        assert_eq!(prompts.queue.len(), 1);
    }

    #[test]
    fn losing_the_last_pad_in_gamepad_mode_asks_to_leave() {
        let mut prompts = Prompts::default();
        prompts.note(&[PadEvent::Disconnected(0)], &[pad()], true, true);
        assert!(!prompts.is_open(), "another pad is still there");
        prompts.note(&[PadEvent::Disconnected(0)], &[], true, true);
        assert_eq!(prompts.queue.front(), Some(&Question::Disconnected));
        prompts.note(&[connected()], &[pad()], true, true);
        assert!(!prompts.is_open(), "reconnecting answers it");
    }
}
