mod ai;
mod appearance;
mod general;
mod ocr;
mod translation;

use super::SettingsWindow;
use gpui::Context;
use gpui_component::setting::SettingPage;

impl SettingsWindow {
    pub(super) fn setting_pages(&self, cx: &mut Context<Self>) -> Vec<SettingPage> {
        vec![
            self.general_settings_page(cx),
            self.keymap_settings_page(cx),
            self.appearance_settings_page(cx),
            self.ai_settings_page(cx),
            self.provider_settings_page(cx),
            self.translation_settings_page(cx),
            self.ocr_settings_page(cx),
        ]
    }
}
