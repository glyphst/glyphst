//! Reading a JSON object out of a model's answer, which may come fenced in
//! Markdown or with words around it.

use anyhow::{Context as _, Result, anyhow};
use serde_json::Value;

/// The JSON object in `content`; `what` names the answer in errors.
pub(crate) fn parse_json_object(content: &str, what: &str) -> Result<Value> {
    let content = content.trim();
    if let Ok(value) = serde_json::from_str::<Value>(content) {
        return Ok(value);
    }
    let start = content
        .find('{')
        .ok_or_else(|| anyhow!("{what} was not JSON"))?;
    let end = content
        .rfind('}')
        .ok_or_else(|| anyhow!("{what} was not JSON"))?;
    serde_json::from_str::<Value>(&content[start..=end])
        .with_context(|| format!("{what} was not JSON"))
}
