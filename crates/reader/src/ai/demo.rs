//! Canned states of the panel for unattended screenshots
//! (`--assistant-demo STATE`): a chat about the open document with chips,
//! a folded and a running answer, the menu, a chip's preview, an edit, an
//! error and the history. Nothing reaches a model.

use std::{sync::mpsc, time::Instant};

use glyphst_ai::{agent::AgentCancellationToken, ai_chat::ChatMessage};
use glyphst_library::{AiSession, AiSessionAgent, current_timestamp};

use super::{Assistant, Editing, MessageView, Run, Segment, ToolRow, ToolState};
use crate::{
    ai_editor::{ChipKind, Draft, MentionEditor},
    ai_mentions::{Mention, Sources},
};

/// The states `--assistant-demo` takes.
pub const DEMO_STATES: [&str; 8] = [
    "empty", "menu", "hover", "running", "done", "edit", "error", "history",
];

const ANSWER: &str = "The paper argues that **attention alone** is enough for sequence transduction: the Transformer drops recurrence and convolutions entirely.\n\n- **Encoder and decoder** are stacks of six identical layers, each with multi-head self-attention and a feed-forward network.\n- **Scaled dot-product attention** divides the scores by √dₖ so the softmax keeps useful gradients.\n- **Positional encodings** use sines and cosines, so the model can attend by relative position.\n\nOn WMT 2014 English–German it reaches `28.4` BLEU, more than 2 points above the best ensembles, at a fraction of the training cost.";

const FOLLOW_UP: &str = "Self-attention connects every pair of positions in a constant number of operations, while a recurrent layer needs `O(n)` sequential steps. That makes long-range dependencies easier to learn and the layer far easier to parallelize.";

fn tool(name: &str, arguments: &str, output: &str, state: ToolState) -> Segment {
    Segment::Tool(ToolRow {
        id: format!("demo-{name}-{}", arguments.len()),
        name: name.into(),
        arguments: arguments.into(),
        state,
        output: Some(output.into()),
    })
}

impl Assistant {
    /// Whether the panel shows a canned state.
    pub fn is_demo(&self) -> bool {
        self.demo.is_some()
    }

    /// Puts the panel in `state`, with chips on the active document of
    /// `sources`.
    pub fn demo(&mut self, state: &str, ctx: &egui::Context, sources: Sources) {
        self.new_session();
        let (sender, events) = mpsc::channel();
        self.demo = Some(sender);
        let Some(document) = sources.documents.first().cloned() else {
            return;
        };
        let page = |first: usize, last: usize| {
            ChipKind::Mention(Mention::Pages {
                document: document.clone(),
                first,
                last,
            })
        };
        let now = current_timestamp();
        if state == "history" {
            self.sessions = [
                ("Summary of the Transformer paper", 600, 6, None),
                ("Why scaled dot-product attention?", 3_000, 4, None),
                (
                    "Notes on positional encodings",
                    90_000,
                    8,
                    Some("claude-code"),
                ),
                ("Compare with the ByteNet results", 300_000, 2, None),
                ("Highlight the key claims", 900_000, 10, Some("codex")),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (title, ago, messages, agent))| AiSession {
                id: index as u64 + 1,
                title: title.into(),
                messages: vec![super::session_message_stub(); messages],
                queued_messages: Vec::new(),
                created: now - ago,
                updated: now - ago,
                summary: None,
                agent: agent.map(|agent: &str| AiSessionAgent {
                    agent: agent.into(),
                    session: None,
                }),
            })
            .collect();
            self.session_id = 1;
            self.show_sessions = true;
            return;
        }
        if state == "empty" {
            return;
        }
        self.title = Some("Summary of the Transformer paper".into());
        self.answerer = "claude-opus-5-5".into();
        self.context_tokens = (68_000, "claude-opus-5-5".into());
        let push_user = |assistant: &mut Self, draft: Draft, ago: u64| {
            let (text, _) = draft.prompt();
            assistant.messages.push(ChatMessage::user_text(text));
            assistant.views.push(MessageView {
                draft: Some(draft),
                sent_at: Some(now - ago),
                ..MessageView::default()
            });
        };
        let mut first = Draft::plain("Summarize ");
        first.push_chip(page(0, 0));
        first
            .text
            .push_str(" and explain the main idea in simple terms.");
        push_user(self, first, 420);
        self.messages.push(ChatMessage::assistant(ANSWER.into()));
        self.views.push(MessageView {
            segments: vec![
                Segment::Thought("The user wants a summary of the first page. I should search for the model's name, then read the abstract and the introduction before answering.".into()),
                tool("search_document", r#"{"query":"Transformer"}"#, "Page 1: … the Transformer, a model architecture eschewing recurrence …", ToolState::Done),
                tool("read_document", r#"{"start_page":1,"end_page":3}"#, "## Page 1\nAttention Is All You Need …", ToolState::Done),
                tool("render_document_pages", r#"{"pages":[3]}"#, "[image]", ToolState::Done),
                Segment::Text(ANSWER.into()),
            ],
            duration_ms: Some(12_400),
            ..MessageView::default()
        });
        let mut second = Draft::plain("Why is self-attention faster than recurrence? See ");
        second.push_chip(page(5, 5));
        push_user(self, second, 120);
        let answer = self.messages.len();
        let running = state == "running";
        self.messages.push(ChatMessage::assistant(if running {
            String::new()
        } else {
            FOLLOW_UP.into()
        }));
        let mut segments = vec![
            Segment::Thought("Section 4 compares self-attention with recurrent and convolutional layers; table 1 lists the complexity per layer.".into()),
            tool("search_document", r#"{"query":"complexity per layer"}"#, "Page 6: Table 1 …", ToolState::Done),
        ];
        if running {
            segments.push(tool(
                "read_document",
                r#"{"start_page":6,"end_page":7}"#,
                "",
                ToolState::Running,
            ));
            self.run = Some(Run {
                cancel: AgentCancellationToken::default(),
                events,
                answer,
                started: Instant::now() - std::time::Duration::from_secs(7),
            });
        } else {
            segments.push(tool(
                "read_document",
                r#"{"start_page":6,"end_page":7}"#,
                "## Page 6 …",
                ToolState::Done,
            ));
            segments.push(tool(
                "add_highlight",
                r#"{"page":6,"quote":"constant number of sequentially executed operations"}"#,
                "Highlighted.",
                ToolState::Done,
            ));
            segments.push(Segment::Text(FOLLOW_UP.into()));
        }
        self.views.push(MessageView {
            segments,
            duration_ms: (!running).then_some(8_200),
            ..MessageView::default()
        });
        match state {
            "menu" => {
                let mut draft = Draft::plain("Compare it with ");
                draft.text.push('@');
                self.editor.set(ctx, draft);
                ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
            }
            "hover" => {
                let mut draft = Draft::plain("Now explain ");
                draft.push_chip(page(0, 0));
                self.editor.set(ctx, draft);
                self.demo_hover = true;
            }
            "running" => {
                let mut draft = Draft::plain("And the decoder? ");
                draft.push_chip(page(3, 3));
                self.queue.push(draft);
            }
            "edit" => {
                let index = answer - 1;
                let mut editor = MentionEditor::new(egui::Id::unique(("assistant-edit", index)));
                editor.set(ctx, self.views[index].draft.clone().unwrap_or_default());
                ctx.memory_mut(|memory| memory.request_focus(editor.id));
                self.editing = Some(Editing { index, editor });
            }
            "error" => {
                push_user(
                    self,
                    Draft::plain("Now list every equation in section 3."),
                    30,
                );
                self.error = Some(
                    "Anthropic: 529 Overloaded — the API is temporarily overloaded. Your message was kept; try again in a moment."
                        .into(),
                );
            }
            _ => {}
        }
    }
}
