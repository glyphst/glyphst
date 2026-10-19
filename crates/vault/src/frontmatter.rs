//! The YAML frontmatter of a note, as much of YAML as notes use: scalars
//! and lists (flow `[a, b]` or one `- item` a line). Glyphst rewrites the
//! keys it owns and keeps every other line as it was.

/// A frontmatter value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(String),
    List(Vec<String>),
}

impl Value {
    pub fn as_list(&self) -> Vec<String> {
        match self {
            Self::Scalar(value) if value.is_empty() => Vec::new(),
            Self::Scalar(value) => vec![value.clone()],
            Self::List(values) => values.clone(),
        }
    }
}

/// One key and the lines it was written as.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    key: Option<String>,
    lines: Vec<String>,
}

/// A note's frontmatter: its entries in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frontmatter {
    entries: Vec<Entry>,
}

/// Splits a note into its frontmatter (without the `---` fences) and its
/// body. A note without frontmatter has an empty one.
pub fn split(note: &str) -> (Frontmatter, &str) {
    let Some(rest) = note
        .strip_prefix("---\n")
        .or_else(|| note.strip_prefix("---\r\n"))
    else {
        return (Frontmatter::default(), note);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return (Frontmatter::parse(yaml), body);
        }
        offset += line.len();
    }
    (Frontmatter::default(), note)
}

impl Frontmatter {
    pub fn parse(yaml: &str) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        for line in yaml.lines() {
            let continues =
                line.starts_with(' ') || line.starts_with('\t') || line.starts_with('-');
            let key = (!continues)
                .then(|| line.split_once(':'))
                .flatten()
                .map(|(key, _)| key.trim())
                .filter(|key| {
                    !key.is_empty()
                        && key
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                });
            match (key, entries.last_mut()) {
                (Some(key), _) => entries.push(Entry {
                    key: Some(key.to_string()),
                    lines: vec![line.to_string()],
                }),
                (None, Some(last)) if continues => last.lines.push(line.to_string()),
                (None, _) => entries.push(Entry {
                    key: None,
                    lines: vec![line.to_string()],
                }),
            }
        }
        Self { entries }
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.key.as_deref() == Some(key))?;
        let first = entry.lines.first()?;
        let inline = first
            .split_once(':')
            .map(|(_, value)| value.trim())
            .unwrap_or_default();
        if inline.starts_with('[') && inline.ends_with(']') {
            return Some(Value::List(
                inline[1..inline.len() - 1]
                    .split(',')
                    .map(unquote)
                    .filter(|item| !item.is_empty())
                    .collect(),
            ));
        }
        let items = entry.lines[1..]
            .iter()
            .filter_map(|line| line.trim_start().strip_prefix('-'))
            .map(unquote)
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>();
        if inline.is_empty() && !items.is_empty() {
            return Some(Value::List(items));
        }
        Some(Value::Scalar(unquote(inline)))
    }

    /// Sets `key`, in place when it is there, else at the end.
    pub fn set(&mut self, key: &str, value: &Value) {
        let lines = render_entry(key, value);
        match self
            .entries
            .iter_mut()
            .find(|entry| entry.key.as_deref() == Some(key))
        {
            Some(entry) => entry.lines = lines,
            None => self.entries.push(Entry {
                key: Some(key.to_string()),
                lines,
            }),
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.entries
            .retain(|entry| entry.key.as_deref() != Some(key));
    }

    /// The frontmatter with its fences, ready to start a note.
    pub fn render(&self) -> String {
        let mut text = String::from("---\n");
        for entry in &self.entries {
            for line in &entry.lines {
                text.push_str(line);
                text.push('\n');
            }
        }
        text.push_str("---\n");
        text
    }
}

fn render_entry(key: &str, value: &Value) -> Vec<String> {
    match value {
        Value::Scalar(value) => vec![format!("{key}: {}", quote(value))],
        Value::List(values) if values.is_empty() => vec![format!("{key}: []")],
        Value::List(values) => std::iter::once(format!("{key}:"))
            .chain(values.iter().map(|value| format!("  - {}", quote(value))))
            .collect(),
    }
}

/// A scalar as YAML: plain when that reads back the same, else quoted.
pub fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value.trim() == value
        && !value.starts_with([
            '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '\'', '"', '%',
            '@', '`',
        ])
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(':')
        && !matches!(
            value.to_lowercase().as_str(),
            "true" | "false" | "yes" | "no" | "null" | "~" | "on" | "off"
        );
    if plain {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_and_comments_survive_a_rewrite() {
        let note = "---\ncssclasses: [wide]\n# my comment\ntitle: Old\ntags:\n  - a\n  - \"b c\"\naliases: [x, 'y']\n---\n# Body\n";
        let (mut frontmatter, body) = split(note);
        assert_eq!(body, "# Body\n");
        assert_eq!(
            frontmatter.get("tags"),
            Some(Value::List(vec!["a".to_string(), "b c".to_string()]))
        );
        assert_eq!(
            frontmatter.get("aliases"),
            Some(Value::List(vec!["x".to_string(), "y".to_string()]))
        );
        frontmatter.set("title", &Value::Scalar("Attention: Is All".to_string()));
        frontmatter.set("year", &Value::Scalar("2017".to_string()));
        assert_eq!(
            frontmatter.render(),
            "---\ncssclasses: [wide]\n# my comment\ntitle: \"Attention: Is All\"\ntags:\n  - a\n  - \"b c\"\naliases: [x, 'y']\nyear: 2017\n---\n"
        );
        let (again, _) = split(&frontmatter.render());
        assert_eq!(
            again.get("title"),
            Some(Value::Scalar("Attention: Is All".to_string()))
        );
    }

    #[test]
    fn a_note_without_frontmatter_has_an_empty_one() {
        let (frontmatter, body) = split("# Just text\n---\n");
        assert_eq!(frontmatter, Frontmatter::default());
        assert_eq!(body, "# Just text\n---\n");
        assert_eq!(quote("true"), "\"true\"");
        assert_eq!(quote("machine learning/llms"), "machine learning/llms");
        assert_eq!(quote(""), "\"\"");
    }
}
