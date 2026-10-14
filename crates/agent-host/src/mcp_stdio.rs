//! The MCP server over stdio, for agents that only start MCP servers as
//! commands (ACP agents without HTTP support): `glyphst-reader --mcp-stdio`
//! relays each JSON-RPC line to the running Reader's server, whose URL and
//! token come in `GLYPHST_MCP_URL` and `GLYPHST_MCP_TOKEN`.

use anyhow::{Context as _, Result};

pub const URL_VARIABLE: &str = "GLYPHST_MCP_URL";
pub const TOKEN_VARIABLE: &str = "GLYPHST_MCP_TOKEN";

/// Relays stdin's lines to the server until stdin closes.
pub fn run() -> Result<()> {
    let url = std::env::var(URL_VARIABLE).with_context(|| format!("{URL_VARIABLE} is not set"))?;
    let token =
        std::env::var(TOKEN_VARIABLE).with_context(|| format!("{TOKEN_VARIABLE} is not set"))?;
    relay(
        &url,
        &token,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}

fn relay(
    url: &str,
    token: &str,
    input: impl std::io::BufRead,
    mut output: impl std::io::Write,
) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(None)
        .build()
        .context("failed to create HTTP client")?;
    for line in input.lines() {
        let line = line.context("failed to read stdin")?;
        if line.trim().is_empty() {
            continue;
        }
        let response = client
            .post(url)
            .bearer_auth(token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .body(line)
            .send()
            .context("the Reader's MCP server did not answer")?;
        let body = response.text().context("failed to read the answer")?;
        if !body.trim().is_empty() {
            writeln!(output, "{}", body.trim()).context("failed to write stdout")?;
            output.flush().context("failed to write stdout")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use glyphst_ai::tools::{ToolContext, ToolRegistry};
    use serde_json::Value;

    use super::*;
    use crate::mcp::{McpServer, McpSession, McpSessions};

    #[test]
    fn lines_go_to_the_server_and_answers_come_back() {
        let sessions = McpSessions::default();
        let server = McpServer::start(sessions.clone()).unwrap();
        let token = sessions.open(McpSession {
            tools: Arc::new(ToolRegistry::new()),
            context: ToolContext::default(),
        });
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n"
        );
        let mut output = Vec::new();
        relay(&server.url, &token, input.as_bytes(), &mut output).unwrap();
        let lines = String::from_utf8(output).unwrap();
        let answers = lines
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(answers.len(), 2, "the notification has no answer");
        assert_eq!(answers[0]["id"], 1);
        assert_eq!(answers[1]["result"]["tools"], Value::Array(Vec::new()));
    }
}
