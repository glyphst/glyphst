//! Keeping long conversations within the model's context, and naming them.
//!
//! Compaction replaces the whole conversation so far with a summary written
//! by the model (and replays no earlier reasoning, which providers bind to
//! the exact conversation that produced it).

use anyhow::{Result, anyhow};

use crate::{
    agent::{AgentCancellationToken, AgentLimits, AgentMessage, AgentRun, AgentUsage, run_agent},
    ai_chat::{TITLE_GENERATION_PROMPT, title_from_response},
    settings::{ModelLimits, ResolvedAiConfig},
    tools::ToolContext,
};

/// Compact once a turn used this share of the context window.
const COMPACT_AT: f64 = 0.8;
/// Characters of each message a title is written from.
const TITLE_SOURCE_CHARS: usize = 2_000;

const SUMMARY_PROMPT: &str = "Summarize this conversation so far so that you can continue it from the summary alone. Keep: every question and request of mine (quoted), the documents and page numbers involved, what you found and concluded, what you changed in the reader (annotations, bookmarks, the library), and anything left to do. Be complete but concise; write the summary only.";

/// Whether a turn that read `input_tokens` should be compacted before the
/// next one.
pub fn needs_compaction(model: &str, input_tokens: u64) -> bool {
    let window = ModelLimits::for_model(model).context_window;
    input_tokens as f64 >= window as f64 * COMPACT_AT
}

/// The model's summary of `history`.
pub fn summarize(
    settings: ResolvedAiConfig,
    mut history: Vec<AgentMessage>,
    cancel: AgentCancellationToken,
) -> Result<String> {
    history.push(AgentMessage::user_text(SUMMARY_PROMPT));
    let summary = answer(settings, history, cancel)?;
    if summary.trim().is_empty() {
        return Err(anyhow!("the model wrote an empty summary"));
    }
    Ok(summary)
}

/// The message that stands for the summarized conversation.
pub fn summary_message(summary: &str) -> AgentMessage {
    AgentMessage::user_text(format!(
        "<conversation_summary>\nThe earlier part of this conversation, summarized:\n{summary}\n</conversation_summary>"
    ))
}

/// A short title for a conversation that began with `question` and `answer`.
pub fn generate_title(
    settings: ResolvedAiConfig,
    question: &str,
    answer_text: &str,
    cancel: AgentCancellationToken,
) -> Result<String> {
    let clip = |text: &str| text.chars().take(TITLE_SOURCE_CHARS).collect::<String>();
    let request = format!(
        "{TITLE_GENERATION_PROMPT}\n\n<question>\n{}\n</question>\n<answer>\n{}\n</answer>",
        clip(question),
        clip(answer_text)
    );
    let response = answer(settings, vec![AgentMessage::user_text(request)], cancel)?;
    title_from_response(&response).ok_or_else(|| anyhow!("the model wrote no title"))
}

fn answer(
    settings: ResolvedAiConfig,
    messages: Vec<AgentMessage>,
    cancel: AgentCancellationToken,
) -> Result<String> {
    answer_with_usage(settings, messages, cancel).map(|(text, _)| text)
}

/// One answer without tools, and the tokens it took.
pub(crate) fn answer_with_usage(
    settings: ResolvedAiConfig,
    messages: Vec<AgentMessage>,
    cancel: AgentCancellationToken,
) -> Result<(String, AgentUsage)> {
    let result = run_agent(
        AgentRun {
            settings,
            messages,
            tools: None,
            context: ToolContext {
                cancel,
                ..ToolContext::default()
            },
            limits: AgentLimits {
                max_tool_rounds: 1,
                ..AgentLimits::default()
            },
            cache_key: None,
        },
        &mut |_| Ok(()),
    )?;
    let text = result
        .appended_messages
        .iter()
        .map(AgentMessage::text)
        .collect::<Vec<_>>()
        .join("");
    Ok((text, result.usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compacts_near_the_context_window() {
        assert!(!needs_compaction("claude-haiku-4-5", 100_000));
        assert!(needs_compaction("claude-haiku-4-5", 170_000));
        assert!(!needs_compaction("claude-opus-5-5", 170_000));
        assert!(
            summary_message("We read page 3.")
                .text()
                .contains("We read page 3.")
        );
    }
}
