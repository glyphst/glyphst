use super::*;

impl LibraryStore {
    pub fn reader_session(&self) -> Option<&ReaderSessionSnapshot> {
        self.library.reader_session.as_ref()
    }

    pub fn set_reader_session(&mut self, session: ReaderSessionSnapshot) -> Result<()> {
        self.write_reader_session(&session)?;
        self.library.reader_session = Some(session);
        Ok(())
    }
}
