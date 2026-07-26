use super::*;

impl LibraryStore {
    pub fn ai_sessions_for(&self, path: &Path) -> Vec<AiSession> {
        let path = normalize_path(path);
        self.library
            .ai_sessions
            .iter()
            .filter(|session| session.path == path)
            .cloned()
            .collect()
    }

    pub fn next_ai_session_id(&mut self) -> u64 {
        let id = self.library.next_ai_session_id.max(1);
        self.library.next_ai_session_id = id + 1;
        id
    }
    pub fn save_ai_session(&mut self, mut session: AiSession) -> Result<()> {
        session.path = normalize_path(&session.path);
        session.title = session.title.trim().to_string();
        if session.title.is_empty() {
            session.title = "New Chat".to_string();
        }
        let session_id = session.id;

        let mut library = self.library.clone();
        if let Some(session_ix) = library
            .ai_sessions
            .iter()
            .position(|existing| existing.id == session_id)
        {
            library.ai_sessions[session_ix] = session;
        } else {
            library.ai_sessions.push(session);
        }
        library.sanitize();

        let session = library
            .ai_sessions
            .iter()
            .find(|existing| existing.id == session_id)
            .ok_or_else(|| anyhow!("saved AI session is invalid"))?;
        let retained_ids = library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .collect::<HashSet<_>>();
        let removed_ids = self
            .library
            .ai_sessions
            .iter()
            .map(|existing| existing.id)
            .filter(|id| !retained_ids.contains(id))
            .collect::<Vec<_>>();

        self.write_ai_session_changes(session, &removed_ids)?;
        self.library = library;
        Ok(())
    }
}
