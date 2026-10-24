//! Streamed translation replies: the server-sent events a provider sends a
//! reply in, and the units of a `{"translations": [{"id": …, "text": …}]}`
//! reply as each is written, so a document shows them one by one.

use std::{collections::HashSet, io::Read};

use anyhow::{Context as _, Result};

use crate::translation::SemanticTranslationOutput;

/// Finds each whole unit of a streamed `{"translations": [...]}` reply as
/// its text arrives.
pub(crate) struct UnitScanner {
    text: String,
    /// Where scanning resumes.
    at: usize,
    /// Inside the array: the open objects' depth, whether in a string (and
    /// after a backslash) and where the open unit starts.
    array: Option<ArrayScan>,
    done: bool,
    expected: HashSet<String>,
    seen: HashSet<String>,
}

#[derive(Default)]
struct ArrayScan {
    depth: usize,
    in_string: bool,
    escaped: bool,
    start: usize,
}

impl UnitScanner {
    /// A scanner that keeps only units with `expected` ids, each once.
    pub(crate) fn new(expected: impl IntoIterator<Item = String>) -> Self {
        Self {
            text: String::new(),
            at: 0,
            array: None,
            done: false,
            expected: expected.into_iter().collect(),
            seen: HashSet::new(),
        }
    }

    /// Adds `delta` to the reply; returns the units it completed.
    pub(crate) fn push(&mut self, delta: &str) -> Vec<SemanticTranslationOutput> {
        self.text.push_str(delta);
        let mut units = Vec::new();
        if self.done {
            return units;
        }
        if self.array.is_none() {
            let Some(key) = self.text.find("\"translations\"") else {
                return units;
            };
            let after = key + "\"translations\"".len();
            let Some(open) = self.text[after..].find('[') else {
                return units;
            };
            // Only a colon and spaces may sit between the key and its array.
            if !self.text[after..after + open]
                .chars()
                .all(|character| character == ':' || character.is_whitespace())
            {
                self.done = true;
                return units;
            }
            self.at = after + open + 1;
            self.array = Some(ArrayScan::default());
        }
        let scan = self.array.as_mut().expect("the array was found");
        for (offset, character) in self.text[self.at..].char_indices() {
            let index = self.at + offset;
            if scan.in_string {
                if scan.escaped {
                    scan.escaped = false;
                } else if character == '\\' {
                    scan.escaped = true;
                } else if character == '"' {
                    scan.in_string = false;
                }
                continue;
            }
            match character {
                '"' => scan.in_string = true,
                '{' => {
                    if scan.depth == 0 {
                        scan.start = index;
                    }
                    scan.depth += 1;
                }
                '}' if scan.depth > 0 => {
                    scan.depth -= 1;
                    if scan.depth == 0
                        && let Ok(unit) = serde_json::from_str::<SemanticTranslationOutput>(
                            &self.text[scan.start..=index],
                        )
                        && self.expected.contains(&unit.id)
                        && self.seen.insert(unit.id.clone())
                    {
                        units.push(unit);
                    }
                }
                ']' if scan.depth == 0 => {
                    self.done = true;
                    break;
                }
                _ => {}
            }
        }
        self.at = self.text.len();
        units
    }
}

/// Reads server-sent events from `response`, handing each event's data to
/// `on_data` until it returns false or the stream ends.
pub(crate) fn read_events(
    mut response: impl Read,
    mut on_data: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    let mut pending = Vec::new();
    let mut data = Vec::<String>::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = response
            .read(&mut buffer)
            .context("failed to read the AI stream")?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&buffer[..read]);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .context("the AI stream emitted invalid UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() && !on_data(&data.join("\n"))? {
                    return Ok(());
                }
                data.clear();
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim_start().to_string());
            }
        }
    }
    if let Ok(line) = std::str::from_utf8(&pending)
        && let Some(value) = line.trim().strip_prefix("data:")
    {
        data.push(value.trim_start().to_string());
    }
    if !data.is_empty() {
        on_data(&data.join("\n"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLY: &str = r#"```json
{"translations": [
  {"id": "a", "text": "Bonjour {le} \"monde\""},
  {"id": "skip", "text": "not asked"},
  {"id": "b", "text": "Au revoir ]"},
  {"id": "a", "text": "again"}
]}
```"#;

    #[test]
    fn each_unit_comes_out_once_whole_however_the_reply_is_split() {
        for split in 0..=REPLY.len() {
            if !REPLY.is_char_boundary(split) {
                continue;
            }
            let mut scanner = UnitScanner::new(["a".to_string(), "b".to_string()]);
            let mut units = scanner.push(&REPLY[..split]);
            units.extend(scanner.push(&REPLY[split..]));
            assert_eq!(
                units,
                vec![
                    SemanticTranslationOutput {
                        id: "a".into(),
                        text: "Bonjour {le} \"monde\"".into(),
                    },
                    SemanticTranslationOutput {
                        id: "b".into(),
                        text: "Au revoir ]".into(),
                    },
                ],
                "split at {split}"
            );
            assert_eq!(scanner.text, REPLY);
        }
    }

    #[test]
    fn a_unit_comes_out_as_soon_as_it_closes() {
        let mut scanner = UnitScanner::new(["a".to_string(), "b".to_string()]);
        assert!(
            scanner
                .push(r#"{"translations":[{"id":"a","text":"un"#)
                .is_empty()
        );
        assert_eq!(scanner.push(r#""},{"id":"b""#).len(), 1);
        assert_eq!(scanner.push(r#","text":"deux"}]}"#).len(), 1);
    }

    #[test]
    fn events_are_read_across_reads_and_crlf() {
        let stream = "data: one\r\n\r\n: comment\ndata: two\ndata: lines\n\ndata: [DONE]\n\n";
        let mut seen = Vec::new();
        read_events(stream.as_bytes(), |data| {
            seen.push(data.to_string());
            Ok(data != "[DONE]")
        })
        .unwrap();
        assert_eq!(seen, ["one", "two\nlines", "[DONE]"]);
    }
}
