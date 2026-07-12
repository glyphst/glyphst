use gpui::App;

pub mod ai_panel;

pub use ai_panel::{AI_PANEL_WIDTH, AiPanel, AiPanelEvent};

#[derive(Clone, Debug, Default)]
pub struct AiContext {
    pub document_file_name: Option<String>,
    pub document_format: Option<String>,
    pub location_label: Option<String>,
    pub page_count: usize,
    pub current_page: usize,
    pub current_page_text: Option<String>,
    pub selected_text: Option<String>,
    pub document_text: Option<String>,
}

pub trait AiContextProvider: 'static {
    fn context(&self, cx: &mut App) -> Option<AiContext>;
}
