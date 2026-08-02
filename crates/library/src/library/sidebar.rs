use super::*;

impl LibraryStore {
    pub fn sidebar_widths(&self) -> Option<SidebarWidths> {
        self.library.sidebar_widths
    }

    pub fn set_sidebar_widths(&mut self, widths: SidebarWidths) -> Result<()> {
        self.write_sidebar_widths(widths)?;
        self.library.sidebar_widths = Some(widths);
        Ok(())
    }

    pub fn document_browser_size(&self) -> Option<DocumentBrowserSize> {
        self.library.document_browser_size
    }

    pub fn set_document_browser_size(&mut self, size: DocumentBrowserSize) -> Result<()> {
        self.write_document_browser_size(size)?;
        self.library.document_browser_size = Some(size);
        Ok(())
    }
}
