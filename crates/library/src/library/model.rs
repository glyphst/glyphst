use super::edits::replace_snapshot_by_id;
use super::*;

impl Library {
    pub(super) fn record_history_at(
        &mut self,
        path: &Path,
        position: DocumentPosition,
        opened: u64,
    ) {
        let path = normalize_path(path);
        self.history.retain(|entry| entry.path != path);
        self.history.push(HistoryEntry {
            path,
            opened,
            position,
        });
        self.sanitize();
    }

    pub(super) fn apply_bookmark_snapshot(&mut self, id: u64, target: Option<Bookmark>) {
        replace_snapshot_by_id(&mut self.bookmarks, id, target, |bookmark| bookmark.id);
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_bookmark_id = self.next_bookmark_id.max(id.saturating_add(1));
    }

    pub(super) fn apply_highlight_snapshot(&mut self, id: u64, target: Option<Highlight>) {
        replace_snapshot_by_id(&mut self.highlights, id, target, |highlight| highlight.id);
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_highlight_id = self.next_highlight_id.max(id.saturating_add(1));
    }

    pub(super) fn apply_note_snapshot(&mut self, id: u64, target: Option<Note>) {
        replace_snapshot_by_id(&mut self.notes, id, target, |note| note.id);
        self.notes.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.next_note_id = self.next_note_id.max(id.saturating_add(1));
    }

    pub(super) fn sanitize(&mut self) {
        self.sanitize_catalog();
        self.history
            .retain(|entry| !entry.path.as_os_str().is_empty());
        self.history.sort_by(|left, right| {
            right
                .opened
                .cmp(&left.opened)
                .then_with(|| left.path.cmp(&right.path))
        });
        self.history.dedup_by(|left, right| left.path == right.path);
        self.history.truncate(MAX_HISTORY_ENTRIES);

        self.bookmarks.retain_mut(|bookmark| {
            bookmark.normalize();
            bookmark.is_valid()
        });
        self.bookmarks.sort_by(|left, right| {
            right
                .created
                .cmp(&left.created)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.id.cmp(&right.id))
        });

        self.highlights.retain_mut(|highlight| {
            highlight.normalize();
            highlight.is_valid()
        });
        self.highlights.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.notes.retain_mut(|note| {
            note.normalize();
            note.is_valid()
        });
        self.notes.sort_by(|left, right| {
            left.created
                .cmp(&right.created)
                .then_with(|| left.id.cmp(&right.id))
        });

        self.ai_sessions.retain_mut(|session| {
            session.title = session.title.trim().to_string();
            if session.title.is_empty() {
                session.title = "New Chat".to_string();
            }
            session.messages.retain_mut(sanitize_ai_session_message);
            session.queued_messages.retain_mut(|message| {
                message.role == AiSessionRole::User && sanitize_ai_session_message(message)
            });
            session.id > 0
                && !session.messages.is_empty()
                && session.created > 0
                && session.updated > 0
        });
        self.ai_sessions.sort_by(|left, right| {
            right
                .updated
                .cmp(&left.updated)
                .then_with(|| right.created.cmp(&left.created))
                .then_with(|| left.id.cmp(&right.id))
        });
        self.ai_sessions.dedup_by(|left, right| left.id == right.id);
        self.ai_sessions.truncate(MAX_AI_SESSIONS);

        self.document_preferences.retain_mut(|preferences| {
            preferences.path = normalize_path(&preferences.path);
            preferences.target_language = preferences.target_language.trim().to_ascii_lowercase();
            preferences.epub = preferences.epub.map(EpubReaderPreferences::sanitized);
            !preferences.path.as_os_str().is_empty()
                && (!preferences.target_language.is_empty()
                    || preferences.epub.is_some()
                    || preferences.rotation != DocumentRotation::default()
                    || preferences.page_layout != DocumentPageLayout::default())
        });
        self.document_preferences
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.document_preferences
            .dedup_by(|left, right| left.path == right.path);

        self.next_bookmark_id = self.next_bookmark_id.max(
            self.bookmarks
                .iter()
                .map(|bookmark| bookmark.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_highlight_id = self.next_highlight_id.max(
            self.highlights
                .iter()
                .map(|highlight| highlight.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
        self.next_note_id = self
            .next_note_id
            .max(self.notes.iter().map(|note| note.id).max().unwrap_or(0) + 1);
        self.next_ai_session_id = self.next_ai_session_id.max(
            self.ai_sessions
                .iter()
                .map(|session| session.id)
                .max()
                .unwrap_or(0)
                + 1,
        );
    }
}

fn sanitize_ai_session_message(message: &mut AiSessionMessage) -> bool {
    message.content = message.content.trim().to_string();
    message.request_content = message
        .request_content
        .take()
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty() && *content != message.content);
    message.images.retain(sane_ai_image);
    message.document_path = message
        .document_path
        .take()
        .map(|path| normalize_path(&path))
        .filter(|path| !path.as_os_str().is_empty());
    message.document_label = message
        .document_label
        .take()
        .map(|label| label.trim().to_string())
        .filter(|label| !label.is_empty());
    message.agent_parts.retain_mut(|part| match part {
        AiSessionAgentPart::AssistantText(text) => {
            *text = text.trim().to_string();
            !text.is_empty()
        }
        AiSessionAgentPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            *id = id.trim().to_string();
            *name = name.trim().to_string();
            *arguments = arguments.trim().to_string();
            !id.is_empty() && !name.is_empty()
        }
        AiSessionAgentPart::ToolResult {
            call_id,
            tool_name,
            content,
            ..
        } => {
            *call_id = call_id.trim().to_string();
            *tool_name = tool_name.trim().to_string();
            content.retain_mut(|part| match part {
                AiSessionToolResultPart::Text(text) => {
                    *text = text.trim().to_string();
                    !text.is_empty()
                }
                AiSessionToolResultPart::Image(image) => sane_ai_image(image),
            });
            !call_id.is_empty() && !tool_name.is_empty()
        }
        AiSessionAgentPart::OpaqueReasoning { api_style, value } => {
            *api_style = api_style.trim().to_string();
            *value = value.trim().to_string();
            !api_style.is_empty() && !value.is_empty()
        }
    });
    !message.content.is_empty() || !message.images.is_empty() || !message.agent_parts.is_empty()
}

fn sane_ai_image(image: &AiSessionImage) -> bool {
    image.asset_id.len() == 64
        && image.asset_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !image.media_type.trim().is_empty()
        && image.width > 0
        && image.height > 0
        && image.byte_len > 0
}
