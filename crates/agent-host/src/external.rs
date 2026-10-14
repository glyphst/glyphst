//! Running a turn of an external coding agent, as one process per turn
//! that resumes the agent's own session.

use std::path::PathBuf;

use anyhow::Result;
use glyphst_ai::agent::{AgentCancellationToken, AgentImage};

use crate::{claude, codex, scratch::Scratch};

/// The external agents the Reader can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    pub const ALL: [Self; 2] = [Self::ClaudeCode, Self::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// The program it runs.
    pub fn program(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
        }
    }

    /// The stable id saved with conversations.
    pub fn id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.id() == id)
    }
}

/// How an agent reaches the Reader's MCP server.
#[derive(Clone, Debug)]
pub struct McpAccess {
    pub url: String,
    pub token: String,
}

/// One user turn for an external agent.
#[derive(Clone, Debug)]
pub struct ExternalTurn {
    pub program: PathBuf,
    pub scratch: Scratch,
    pub mcp: McpAccess,
    /// The agent's session to continue.
    pub resume: Option<String>,
    pub model: Option<String>,
    pub prompt: String,
    pub images: Vec<AgentImage>,
}

/// What a turn reports.
#[derive(Clone, Debug, PartialEq)]
pub enum ExternalEvent {
    /// The agent's session, to resume the next turn with.
    Session(String),
    Text(String),
    Reasoning(String),
    ToolStarted {
        id: String,
        title: String,
    },
    ToolFinished {
        id: String,
        failed: bool,
    },
    /// A tool call the Reader refused (a write outside the working folder).
    Refused(String),
}

/// Runs a turn to its end; `Err` when the agent failed.
pub fn run_turn(
    kind: AgentKind,
    turn: &ExternalTurn,
    cancel: &AgentCancellationToken,
    events: &mut dyn FnMut(ExternalEvent),
) -> Result<()> {
    match kind {
        AgentKind::ClaudeCode => claude::run_turn(turn, cancel, events),
        AgentKind::Codex => codex::run_turn(turn, cancel, events),
    }
}

/// What the agent is told about where it runs.
pub fn instructions(scratch: &Scratch) -> String {
    format!(
        "You are working for the user inside Glyphst Reader, a PDF and EPUB reader. The `glyphst` MCP tools give you the documents the user has open and the app: start with get_app_state or list_documents, then read, search and render pages, and use the annotation and library tools when the user asks for changes there (they apply at once; the user can undo them). Each user message ends with a <reader_state> block describing the reader at that moment. Your working folder {} belongs to this document: keep files you make there (notes, code, data). The user reviews and can undo your changes to it.",
        scratch.dir.display()
    )
}

/// Whether `path` (absolute or relative to the working folder) lies in it.
pub fn inside(scratch: &Scratch, path: &str) -> bool {
    let path = PathBuf::from(path);
    let path = if path.is_absolute() {
        path
    } else {
        scratch.dir.join(path)
    };
    // The file may not exist yet: resolve the nearest existing folder.
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return false;
        };
        rest.push(name.to_os_string());
        existing = parent;
    }
    let Ok(mut resolved) = existing.canonicalize() else {
        return false;
    };
    for name in rest.into_iter().rev() {
        if name == ".." {
            return false;
        }
        resolved.push(name);
    }
    let Ok(root) = scratch.dir.canonicalize() else {
        return false;
    };
    resolved.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_inside_the_working_folder_are_told_apart() {
        let root = std::env::temp_dir().join(format!(
            "glyphst-inside-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let scratch = Scratch::open(&root, "doc", "Doc").unwrap();
        let dir = scratch.dir.display().to_string();
        assert!(inside(&scratch, "notes.md"));
        assert!(inside(&scratch, &format!("{dir}/new/folder/file.py")));
        assert!(!inside(&scratch, "/etc/passwd"));
        assert!(!inside(&scratch, "../escape.txt"));
        assert!(!inside(&scratch, &format!("{dir}/../escape.txt")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
