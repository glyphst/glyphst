//! Zotero's HTML notes as Markdown, for the vault. The notes are simple
//! HTML (paragraphs, headings, lists, emphasis, links, quotes, code), not
//! always well-formed, so this reads tags as they come.

/// `html` as Markdown.
pub fn html_to_markdown(html: &str) -> String {
    let mut out = String::new();
    let mut lists: Vec<(bool, usize)> = Vec::new();
    let mut link: Option<String> = None;
    let mut quote = false;
    let mut pre = false;
    let mut rest = html;
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_text(&mut out, rest, pre, quote);
            break;
        };
        push_text(&mut out, &rest[..open], pre, quote);
        rest = &rest[open..];
        let Some(close) = rest.find('>') else {
            push_text(&mut out, rest, pre, quote);
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default()
            .to_lowercase();
        match (name.as_str(), closing) {
            // Paragraphs inside a quote stay in it.
            ("p" | "div", false) if quote => {
                if !out.ends_with("> ") {
                    out.push_str("\n>\n> ");
                }
            }
            ("p" | "div", true) if quote => {}
            ("p" | "div", _) => block(&mut out),
            ("br", _) => out.push_str(if quote { "\n> " } else { "\n" }),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                block(&mut out);
                let level = name[1..].parse::<usize>().unwrap_or(1);
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => block(&mut out),
            ("strong" | "b", _) => out.push_str("**"),
            ("em" | "i", _) => out.push('*'),
            ("code", _) if !pre => out.push('`'),
            ("pre", false) => {
                block(&mut out);
                out.push_str("```\n");
                pre = true;
            }
            ("pre", true) => {
                out.push_str("\n```");
                pre = false;
                block(&mut out);
            }
            ("blockquote", false) => {
                block(&mut out);
                out.push_str("> ");
                quote = true;
            }
            ("blockquote", true) => {
                quote = false;
                block(&mut out);
            }
            ("ul", false) => lists.push((false, 0)),
            ("ol", false) => lists.push((true, 0)),
            ("ul" | "ol", true) => {
                lists.pop();
                if lists.is_empty() {
                    block(&mut out);
                }
            }
            ("li", false) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                let depth = lists.len().saturating_sub(1);
                out.push_str(&"  ".repeat(depth));
                match lists.last_mut() {
                    Some((true, number)) => {
                        *number += 1;
                        out.push_str(&format!("{number}. "));
                    }
                    _ => out.push_str("- "),
                }
            }
            ("a", false) => {
                link = attribute(tag, "href");
                out.push('[');
            }
            ("a", true) => match link.take() {
                Some(href) => out.push_str(&format!("]({href})")),
                None => out.push(']'),
            },
            _ => {}
        }
    }
    let mut markdown = out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    while markdown.contains("\n\n\n") {
        markdown = markdown.replace("\n\n\n", "\n\n");
    }
    markdown.trim().to_string()
}

fn block(out: &mut String) {
    if out.is_empty() {
        return;
    }
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.ends_with("\n\n") {
        out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
    }
}

fn push_text(out: &mut String, text: &str, pre: bool, quote: bool) {
    let decoded = decode(text);
    if pre {
        out.push_str(&decoded);
        return;
    }
    let collapsed = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        if !decoded.is_empty() && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
        return;
    }
    if decoded.starts_with(char::is_whitespace) && !out.ends_with([' ', '\n']) && !out.is_empty() {
        out.push(' ');
    }
    if quote {
        out.push_str(&collapsed.replace('\n', "\n> "));
    } else {
        out.push_str(&collapsed);
    }
    if decoded.ends_with(char::is_whitespace) {
        out.push(' ');
    }
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let at = tag.find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next()?;
    let value = if quote == '"' || quote == '\'' {
        rest[1..].split(quote).next()?
    } else {
        rest.split_whitespace().next()?
    };
    Some(decode(value))
}

fn decode(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_become_markdown() {
        let html = r#"<div data-schema-version="9"><h1>Reading notes</h1><p>The <strong>key</strong> idea is <em>attention</em> &amp; <a href="https://arxiv.org/abs/1706.03762">the paper</a>.</p><ul><li>one</li><li>two<ol><li>first</li></ol></li></ul><blockquote><p>a quote</p></blockquote><pre>let x = 1;</pre></div>"#;
        assert_eq!(
            html_to_markdown(html),
            "# Reading notes\n\nThe **key** idea is *attention* & [the paper](https://arxiv.org/abs/1706.03762).\n\n- one\n- two\n  1. first\n\n> a quote\n\n```\nlet x = 1;\n```"
        );
        assert_eq!(html_to_markdown("plain"), "plain");
    }
}
