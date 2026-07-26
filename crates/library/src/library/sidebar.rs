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
}
