//! Canned states of the panel for unattended screenshots
//! (`--assistant-demo STATE`): a chat about the open document with chips,
//! a folded and a running answer, the menu, a chip's preview, an edit, an
//! error, the history and one answer using every kind of Markdown block.
//! `STATE:SCRIPT` picks the chat's script (`transformer` by default, or
//! `zero2prod` and `mamba` for the website's shots). Nothing reaches a model.

use std::{sync::mpsc, time::Instant};

use glyphst_ai::{agent::AgentCancellationToken, ai_chat::ChatMessage};
use glyphst_library::{AiSession, AiSessionAgent, current_timestamp};
use glyphst_settings::{AiProvider, ModelRef};

use super::{Assistant, Editing, MessageView, Run, Segment, ToolRow, ToolState};
use crate::{
    ai_editor::{ChipKind, Draft, MentionEditor},
    ai_mentions::{Mention, Sources},
};

/// The states `--assistant-demo` takes.
pub const DEMO_STATES: [&str; 10] = [
    "empty", "menu", "hover", "running", "done", "edit", "error", "history", "markdown", "library",
];

/// The scripts `--assistant-demo STATE:SCRIPT` takes.
pub const DEMO_SCRIPTS: [&str; 3] = ["transformer", "zero2prod", "mamba"];

/// A tool call shown done: name, arguments, output.
type Call = (&'static str, &'static str, &'static str);

/// One canned chat about a document: two questions with page chips (pages
/// count from 0), their answers and tool calls, and the drafts the other
/// states leave in the composer.
struct Script {
    title: &'static str,
    /// Who answers, and the composer's model; `None` keeps the settings'.
    model: Option<(AiProvider, &'static str)>,
    first: (&'static str, usize, &'static str),
    first_thought: &'static str,
    first_calls: &'static [Call],
    answer: &'static str,
    first_ms: u64,
    second: (&'static str, usize),
    second_thought: &'static str,
    second_search: Call,
    /// The read that runs in the `running` state.
    second_read: (&'static str, &'static str),
    highlight: &'static str,
    follow_up: &'static str,
    second_ms: u64,
    /// The `menu`, `hover` and queued (`running`) drafts.
    menu: &'static str,
    hover: (&'static str, usize),
    queued: (&'static str, usize),
    error_question: &'static str,
}

const TRANSFORMER: Script = Script {
    title: "Summary of the Transformer paper",
    model: None,
    first: (
        "Summarize ",
        0,
        " and explain the main idea in simple terms.",
    ),
    first_thought: "The user wants a summary of the first page. I should search for the model's name, then read the abstract and the introduction before answering.",
    first_calls: &[
        (
            "search_document",
            r#"{"query":"Transformer"}"#,
            "Page 1: … the Transformer, a model architecture eschewing recurrence …",
        ),
        (
            "read_document",
            r#"{"start_page":1,"end_page":3}"#,
            "## Page 1\nAttention Is All You Need …",
        ),
        ("render_document_pages", r#"{"pages":[3]}"#, "[image]"),
    ],
    answer: ANSWER,
    first_ms: 12_400,
    second: ("Why is self-attention faster than recurrence? See ", 5),
    second_thought: "Section 4 compares self-attention with recurrent and convolutional layers; table 1 lists the complexity per layer.",
    second_search: (
        "search_document",
        r#"{"query":"complexity per layer"}"#,
        "Page 6: Table 1 …",
    ),
    second_read: (r#"{"start_page":6,"end_page":7}"#, "## Page 6 …"),
    highlight: r#"{"page":6,"quote":"constant number of sequentially executed operations"}"#,
    follow_up: FOLLOW_UP,
    second_ms: 8_200,
    menu: "Compare it with ",
    hover: ("Now explain ", 0),
    queued: ("And the decoder? ", 3),
    error_question: "Now list every equation in section 3.",
};

const ZERO2PROD: Script = Script {
    title: "Sharing startup logic with the tests",
    model: Some((AiProvider::DeepSeek, "DeepSeek-V4.1-Flash-1017")),
    first: (
        "Summarize ",
        289,
        " and explain why the book moves startup into `build`.",
    ),
    first_thought: "The page starts 7.3.8.1 Extracting Our Startup Code. I should compare spawn_app with main on the pages before it, then read the extraction itself.",
    first_calls: &[
        (
            "search_document",
            r#"{"query":"spawn_app"}"#,
            "Page 288: 7.3.8 Sharing Startup Logic … pub async fn spawn_app() -> TestApp …",
        ),
        (
            "read_document",
            r#"{"start_page":288,"end_page":291}"#,
            "## Page 288\n7.3.8 Sharing Startup Logic …",
        ),
    ],
    answer: ZERO2PROD_ANSWER,
    first_ms: 9_600,
    second: (
        "How do the tests learn the random port once `build` binds it? See ",
        294,
    ),
    second_thought: "Section 7.3.8.2 sets the port to 0 and then needs the port back; the book wraps the server in an Application type.",
    second_search: (
        "search_document",
        r#"{"query":"random port"}"#,
        "Page 292: let listener = TcpListener::bind(\"127.0.0.1:0\") …",
    ),
    second_read: (r#"{"start_page":294,"end_page":296}"#, "## Page 295 …"),
    highlight: r#"{"page":295,"quote":"does not allow us to retrieve the application port"}"#,
    follow_up: ZERO2PROD_FOLLOW_UP,
    second_ms: 7_300,
    menu: "Compare it with ",
    hover: ("Now explain ", 289),
    queued: ("And the API client? ", 296),
    error_question: "Now list every test helper in section 7.3.",
};

const MAMBA: Script = Script {
    title: "What makes Mamba selective",
    model: Some((AiProvider::DeepSeek, "DeepSeek-V4.1-Flash-1017")),
    first: (
        "Summarize ",
        0,
        " and explain the main idea in simple terms.",
    ),
    first_thought: "The user wants a summary of the first page. I should read the abstract and the introduction, then look at the overview figure.",
    first_calls: &[
        (
            "search_document",
            r#"{"query":"selective state space"}"#,
            "Page 1: Mamba: Linear-Time Sequence Modeling with Selective State Spaces …",
        ),
        (
            "read_document",
            r#"{"start_page":1,"end_page":2}"#,
            "## Page 1\nAbstract: Foundation models, now powering …",
        ),
        ("render_document_pages", r#"{"pages":[3]}"#, "[image]"),
    ],
    answer: MAMBA_ANSWER,
    first_ms: 11_200,
    second: ("What changes between S4 and S6? See ", 5),
    second_thought: "Algorithms 1 and 2 on page 6 put S4 and S6 side by side; section 3.2 on page 5 names the projections.",
    second_search: (
        "search_document",
        r#"{"query":"Algorithm 2"}"#,
        "Page 6: Algorithm 2 SSM + Selection (S6) …",
    ),
    second_read: (r#"{"start_page":5,"end_page":6}"#, "## Page 5 …"),
    highlight: r#"{"page":5,"quote":"making several parameters functions of the input"}"#,
    follow_up: MAMBA_FOLLOW_UP,
    second_ms: 6_900,
    menu: "Compare it with ",
    hover: ("Now explain ", 0),
    queued: ("And the hardware-aware scan? ", 6),
    error_question: "Now list every equation in section 2.",
};

/// The `library` state's answer: passages from two papers, cited.
const LIBRARY_ANSWER: &str = "Two of them take it on directly. Mamba replaces attention with a selective state space, so its cost grows linearly with the length of the sequence rather than quadratically. [Mamba · p.1](glyphst://open?id=9&page=1&chunk=412)\n\nRetrieval-Augmented Generation sidesteps it: the generator reads a few retrieved passages instead of the whole corpus. [RAG · p.4](glyphst://open?id=8&page=4&chunk=88)";

const ANSWER: &str = "The paper argues that **attention alone** is enough for sequence transduction: the Transformer drops recurrence and convolutions entirely.\n\n- **Encoder and decoder** are stacks of six identical layers, each with multi-head self-attention and a feed-forward network.\n- **Scaled dot-product attention** divides the scores by √dₖ so the softmax keeps useful gradients.\n- **Positional encodings** use sines and cosines, so the model can attend by relative position.\n\nOn WMT 2014 English–German it reaches `28.4` BLEU, more than 2 points above the best ensembles, at a fraction of the training cost.";

/// A table that has to wrap in the 380pt panel, inline and display math,
/// prices that must stay text, and bold CJK.
const FOLLOW_UP: &str = r"Self-attention connects every pair of positions in a constant number of operations, while a recurrent layer needs $O(n)$ sequential steps:

| Layer | Complexity per layer | Sequential operations | Maximum path length |
|:--|:--|--:|--:|
| Self-attention | $O(n^2 \cdot d)$ | $O(1)$ | $O(1)$ |
| Recurrent | $O(n \cdot d^2)$ | $O(n)$ | $O(n)$ |
| Convolutional | $O(k \cdot n \cdot d^2)$ | $O(1)$ | $O(\log_k n)$ |

Each head computes

$$\mathrm{Attention}(Q, K, V) = \mathrm{softmax}\left(\frac{QK^\top}{\sqrt{d_k}}\right)V$$

**简而言之**：自注意力让任意两个位置直接相连，长距离依赖更容易学习，训练也更容易并行。The base model trained in 12 hours on 8 P100s, about $300 of cloud time at $3 an hour.";

/// Headings, lists, a quote, code, a diagram and a table, for the
/// `markdown` state.
const MARKDOWN: &str = r#"### Encoder layer

1. **Multi-head attention** over the previous layer, then a residual add.
2. A position-wise **feed-forward** network:
   - two linear maps with a ReLU between them,
   - inner size $d_{ff} = 2048$.

> Dropout $P_{drop} = 0.1$ applies to every sub-layer's output.

```python
def ffn(x):
    return relu(x @ w1 + b1) @ w2 + b2
```

```mermaid
flowchart LR
  x[Input] --> a[Multi-head attention] --> n1[Add & norm]
  n1 --> f[Feed-forward] --> n2[Add & norm]
```

| Model | BLEU | Cost (FLOPs) |
|:--|--:|--:|
| Transformer (base) | 27.3 | $3.3 \cdot 10^{18}$ |
| Transformer (big) | **28.4** | $2.3 \cdot 10^{19}$ |"#;

const ZERO2PROD_ANSWER: &str = "Until this section `main` and the tests' `spawn_app` each wire the application together by hand: the configuration, the connection pool, the `EmailClient` and the `TcpListener`.\n\n- **Two places to change:** every new dependency or change to the server's constructor has to be made in both, as it just was for `EmailClient`.\n- **Untested startup:** the code in `main` never runs under `cargo test`, so the two paths can drift apart and the tests stop describing production.\n- **The fix:** `build(configuration: Settings)` takes the settings and returns the `Server`, and `main` shrinks to telemetry, `get_configuration()` and `build(configuration).await?`.\n\nThe tests can then start the application through the same code path as production.";

const ZERO2PROD_FOLLOW_UP: &str = r#"Setting `c.application.port = 0` lets the OS pick a free port, but `build` keeps the listener to itself and `actix_web::dev::Server` cannot tell you which port it got. The book wraps both in a new type:

```rust
pub struct Application {
    port: u16,
    server: Server,
}

impl Application {
    pub fn port(&self) -> u16 {
        self.port
    }
}
```

`Application::build` reads `listener.local_addr()` before handing the listener to `run`, so `spawn_app` can format the address first and then spawn `run_until_stopped`:

| Before | After |
|:--|:--|
| `build() -> Server` | `Application::build()` |
| port unknown | `app.port()` |
| `server.await` | `run_until_stopped()` |"#;

const MAMBA_ANSWER: &str = "Mamba is a sequence model without attention. It builds on **structured state space models** (SSMs), which run in linear time but fell short on language because they cannot reason about content.\n\n- **Selection:** the SSM's parameters Δ, B and C become functions of the input, so each token decides what the state keeps and what it forgets.\n- **A hardware-aware scan:** that rules out the fast convolution, so the recurrence runs as a parallel scan kept in fast GPU memory.\n- **One simple block:** selective SSMs replace both attention and the MLP.\n\nThe result is `5×` the inference throughput of a Transformer, linear scaling up to million-length sequences, and a 3B model that matches Transformers twice its size.";

const MAMBA_FOLLOW_UP: &str = r"Algorithm 2 keeps the S4 recipe but makes three parameters depend on the input $x$, which gives them a length dimension $L$:

| | S4 | S6 · selective |
|:--|:--|:--|
| $B$ | $(D, N)$ | $(B, L, N)$ from $s_B(x)$ |
| $C$ | $(D, N)$ | $(B, L, N)$ from $s_C(x)$ |
| $\Delta$ | $\,(D)$ | $(B, L, D)$ from $s_\Delta(x)$ |
| runs as | convolution | scan |

with $\tau_\Delta = \mathrm{softplus}$, $s_B(x) = \mathrm{Linear}_N(x)$ and

$$s_\Delta(x) = \mathrm{Broadcast}_D(\mathrm{Linear}_1(x))$$

**简而言之**：参数随输入变化，模型才能按内容决定记住或忽略哪些 token；代价是不能再用卷积，只能用扫描来计算。";

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
        let (state, script) = state.split_once(':').unwrap_or((state, "transformer"));
        let script = match script {
            "zero2prod" => &ZERO2PROD,
            "mamba" => &MAMBA,
            _ => &TRANSFORMER,
        };
        self.new_session();
        let (sender, events) = mpsc::channel();
        self.demo = Some(sender);
        // Asking the library, with no document (frame 05 · ask the library).
        if state == "library" {
            let now = current_timestamp();
            self.title = Some("Ask the library".into());
            self.answerer = "claude-opus-5-5".into();
            let question = Draft::plain("Which of my papers deal with long context, and how?");
            self.messages
                .push(ChatMessage::user_text(question.text.clone()));
            self.views.push(MessageView {
                draft: Some(question),
                sent_at: Some(now - 40),
                ..MessageView::default()
            });
            self.messages
                .push(ChatMessage::assistant(LIBRARY_ANSWER.into()));
            self.views.push(MessageView {
                segments: vec![
                    tool(
                        "search_library",
                        r#"{"query":"long context"}"#,
                        "Items for \"long context\": …",
                        ToolState::Done,
                    ),
                    tool(
                        "read_chunk",
                        r#"{"chunk_id":"c_412"}"#,
                        "From lib_9 Mamba …",
                        ToolState::Done,
                    ),
                    tool(
                        "read_chunk",
                        r#"{"chunk_id":"c_88"}"#,
                        "From lib_8 Retrieval-Augmented Generation …",
                        ToolState::Done,
                    ),
                    Segment::Text(LIBRARY_ANSWER.into()),
                ],
                duration_ms: Some(3_100),
                ..MessageView::default()
            });
            self.compose(ctx, Draft::plain("and which of them is newest?"));
            return;
        }
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
        if state == "markdown" {
            let mut question = Draft::plain("Walk me through one encoder layer of ");
            question.push_chip(page(2, 2));
            self.messages
                .push(ChatMessage::user_text(question.text.clone()));
            self.views.push(MessageView {
                draft: Some(question),
                sent_at: Some(now - 60),
                ..MessageView::default()
            });
            self.messages.push(ChatMessage::assistant(MARKDOWN.into()));
            self.views.push(MessageView {
                segments: vec![Segment::Text(MARKDOWN.into())],
                duration_ms: Some(5_100),
                ..MessageView::default()
            });
            return;
        }
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
                item: None,
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
        self.title = Some(script.title.into());
        match script.model {
            Some((provider, model)) => {
                self.answerer = model.to_lowercase();
                self.context_tokens = (68_000, model.to_lowercase());
                self.demo_model = Some((
                    ModelRef::new(provider, model),
                    format!("{} · {model}", provider.label()),
                ));
            }
            None => {
                self.answerer = "claude-opus-5-5".into();
                self.context_tokens = (68_000, "claude-opus-5-5".into());
            }
        }
        let push_user = |assistant: &mut Self, draft: Draft, ago: u64| {
            let (text, _) = draft.prompt();
            assistant.messages.push(ChatMessage::user_text(text));
            assistant.views.push(MessageView {
                draft: Some(draft),
                sent_at: Some(now - ago),
                ..MessageView::default()
            });
        };
        let (before, first_page, after) = script.first;
        let mut first = Draft::plain(before);
        first.push_chip(page(first_page, first_page));
        first.text.push_str(after);
        push_user(self, first, 420);
        self.messages
            .push(ChatMessage::assistant(script.answer.into()));
        let mut segments = vec![Segment::Thought(script.first_thought.into())];
        segments.extend(
            script
                .first_calls
                .iter()
                .map(|&(name, arguments, output)| tool(name, arguments, output, ToolState::Done)),
        );
        segments.push(Segment::Text(script.answer.into()));
        self.views.push(MessageView {
            segments,
            duration_ms: Some(script.first_ms),
            ..MessageView::default()
        });
        let (before, second_page) = script.second;
        let mut second = Draft::plain(before);
        second.push_chip(page(second_page, second_page));
        push_user(self, second, 120);
        let answer = self.messages.len();
        let running = state == "running";
        self.messages.push(ChatMessage::assistant(if running {
            String::new()
        } else {
            script.follow_up.into()
        }));
        let (name, arguments, output) = script.second_search;
        let mut segments = vec![
            Segment::Thought(script.second_thought.into()),
            tool(name, arguments, output, ToolState::Done),
        ];
        let (read_arguments, read_output) = script.second_read;
        if running {
            segments.push(tool(
                "read_document",
                read_arguments,
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
                read_arguments,
                read_output,
                ToolState::Done,
            ));
            segments.push(tool(
                "add_highlight",
                script.highlight,
                "Highlighted.",
                ToolState::Done,
            ));
            segments.push(Segment::Text(script.follow_up.into()));
        }
        self.views.push(MessageView {
            segments,
            duration_ms: (!running).then_some(script.second_ms),
            ..MessageView::default()
        });
        match state {
            "menu" => {
                let mut draft = Draft::plain(script.menu);
                draft.text.push('@');
                self.editor.set(ctx, draft);
                ctx.memory_mut(|memory| memory.request_focus(self.editor.id));
            }
            "hover" => {
                let (before, at) = script.hover;
                let mut draft = Draft::plain(before);
                draft.push_chip(page(at, at));
                self.editor.set(ctx, draft);
                self.demo_hover = true;
            }
            "running" => {
                let (before, at) = script.queued;
                let mut draft = Draft::plain(before);
                draft.push_chip(page(at, at));
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
                push_user(self, Draft::plain(script.error_question), 30);
                self.error = Some(
                    "Anthropic: 529 Overloaded — the API is temporarily overloaded. Your message was kept; try again in a moment."
                        .into(),
                );
            }
            _ => {}
        }
    }
}
