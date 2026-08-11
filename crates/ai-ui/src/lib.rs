use std::sync::Arc;

use gpui::{App, Task};

pub mod ai_panel;

use anyhow::Result;
use glyphst_ai::agent::AgentToolExecutor;
use glyphst_ai::ai_chat::{ContextBlock, DocumentContextScope, ImageContext, MentionReference};
use glyphst_core::{RegionImageReference, SelectedPagesReference};

pub use ai_panel::{AI_PANEL_WIDTH, AiPanel, AiPanelEvent};

#[derive(Clone, Debug, Default)]
pub struct AiContext {
    pub document_file_name: Option<String>,
    pub document_format: Option<String>,
    pub location_label: Option<String>,
    pub page_count: usize,
    pub current_page: usize,
    pub selected_text: Option<String>,
    pub selected_region: Option<RegionImageReference>,
    pub selected_pages: Option<SelectedPagesReference>,
    pub document_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AiPageState {
    pub page_count: usize,
    pub current_page: usize,
}

#[derive(Clone, Debug)]
pub enum ResolvedPageContext {
    Text(ContextBlock),
    Image(ImageContext),
}

#[derive(Clone, Debug)]
pub struct ResolvedRegionContext {
    pub reference: RegionImageReference,
    pub image: ImageContext,
}

pub trait AiContextProvider: 'static {
    fn context(&self, include_document_text: bool, cx: &App) -> Option<AiContext>;

    fn page_state(&self, cx: &App) -> Option<AiPageState>;

    fn page_state_for_scope(
        &self,
        _scope: Option<&DocumentContextScope>,
        cx: &App,
    ) -> Option<AiPageState> {
        self.page_state(cx)
    }

    fn document_scope(&self, _cx: &App) -> Option<DocumentContextScope> {
        None
    }

    fn context_for_scope(
        &self,
        _scope: Option<&DocumentContextScope>,
        include_document_text: bool,
        cx: &App,
    ) -> Option<AiContext> {
        self.context(include_document_text, cx)
    }

    fn resolve_page_references(
        &self,
        references: Vec<MentionReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>>;

    fn resolve_region_references(
        &self,
        references: Vec<RegionImageReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>>;

    fn resolve_page_references_for_scope(
        &self,
        _scope: Option<&DocumentContextScope>,
        references: Vec<MentionReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedPageContext>>> {
        self.resolve_page_references(references, cx)
    }

    fn resolve_region_references_for_scope(
        &self,
        _scope: Option<&DocumentContextScope>,
        references: Vec<RegionImageReference>,
        cx: &App,
    ) -> Task<Result<Vec<ResolvedRegionContext>>> {
        self.resolve_region_references(references, cx)
    }

    /// Returns a read-only executor backed by a stable document snapshot.
    /// Providers without a document can leave agent tools unavailable.
    fn agent_tools(&self, _cx: &App) -> Option<Arc<dyn AgentToolExecutor>> {
        None
    }
}
