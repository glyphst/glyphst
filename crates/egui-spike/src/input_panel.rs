//! Text fields for trying input methods (fcitx5) and CJK display.

use crate::fonts::FontStatus;

const SAMPLE: &str =
    "中文：平滑缩放测试\n日本語：かなカナ漢字\n한국어: 확대 테스트\nEmoji fallback: ✓ → ★";

#[derive(Default)]
pub struct InputPanel {
    line: String,
    notes: String,
}

impl InputPanel {
    pub fn ui(&mut self, ui: &mut egui::Ui, fonts: &FontStatus) {
        ui.heading("Text input");
        match (&fonts.cjk, &fonts.error) {
            (Some(face), _) => ui.label(format!("CJK font: {face}")),
            (None, Some(error)) => {
                ui.colored_label(ui.visuals().warn_fg_color, format!("No CJK font: {error}"))
            }
            (None, None) => ui.label("No CJK font"),
        };
        ui.label(SAMPLE);
        ui.add_space(6.0);
        ui.label("Single line (try fcitx5 Pinyin):");
        ui.add(egui::TextEdit::singleline(&mut self.line).hint_text("输入中文…"));
        ui.label("Multi-line:");
        ui.add(
            egui::TextEdit::multiline(&mut self.notes)
                .desired_rows(4)
                .hint_text("日本語も…"),
        );
        ui.label(format!(
            "{} characters typed",
            self.line.chars().count() + self.notes.chars().count()
        ));
    }

    pub fn prefill(&mut self, line: &str, notes: &str) {
        self.line = line.into();
        self.notes = notes.into();
    }

    /// Whether a text field has keyboard focus, so page keys go to it.
    pub fn has_focus(ctx: &egui::Context) -> bool {
        ctx.memory(|memory| memory.focused().is_some())
    }
}
