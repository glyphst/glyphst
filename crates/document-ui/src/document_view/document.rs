use super::*;

impl DocumentView {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_document_translation_jobs(
        &mut self,
        jobs: Entity<DocumentTranslationJobManager>,
        cx: &mut Context<Self>,
    ) {
        self.document_translation_jobs = jobs;
        cx.notify();
    }

    pub fn format(&self) -> DocumentFormat {
        self.format
    }

    pub fn is_password_protected(&self) -> bool {
        self.password_protected
    }

    pub fn document_snapshot(&self) -> Arc<DocumentSnapshot> {
        self.document_snapshot.clone()
    }

    pub fn capabilities(&self) -> DocumentCapabilities {
        self.capabilities
    }

    pub fn file_name(&self) -> SharedString {
        self.file_name.clone()
    }
}
