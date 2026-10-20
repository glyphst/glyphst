//! Concept pages: one note per topic in `concepts/`, written by the AI on
//! request from several items and citing their passages. Glyphst owns a
//! few frontmatter keys and the `page` block; the rest, and the block once
//! the user edits it, is the user's. A page written again over an edited
//! `page` block goes into `page-next` beside it, for the user to compare.

use crate::{
    Merged, Written, block, frontmatter::Frontmatter, frontmatter::Value, frontmatter::split,
    merge_blocks, sha,
};

/// The folder concept pages go in.
pub const PAGES_FOLDER: &str = "concepts";

/// The frontmatter keys Glyphst writes on a page; `aliases` keeps the
/// user's and adds the title.
pub const PAGE_OWNED_KEYS: [&str; 5] =
    ["glyphst_page", "title", "sources", "ai_model", "ai_updated"];

/// The block a page's text is in, and the one a newer text waits in when
/// the user edited it.
pub const PAGE_BLOCK: &str = "page";
pub const NEXT_BLOCK: &str = "page-next";

/// What a page says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageSnapshot {
    pub id: u64,
    pub title: String,
    /// The items it was written from, as their notes' names (slugs).
    pub sources: Vec<String>,
    /// Markdown with `[[slug]]` links and `glyphst://` citations.
    pub body: String,
    pub model: String,
    /// `YYYY-MM-DD`.
    pub date: String,
}

fn apply_keys(frontmatter: &mut Frontmatter, snapshot: &PageSnapshot) {
    let scalar = |value: &str| Value::Scalar(value.to_string());
    frontmatter.set("glyphst_page", &scalar(&snapshot.id.to_string()));
    frontmatter.set("title", &scalar(&snapshot.title));
    let mut aliases = frontmatter
        .get("aliases")
        .map(|value| value.as_list())
        .unwrap_or_default();
    if !aliases.contains(&snapshot.title) {
        aliases.push(snapshot.title.clone());
    }
    frontmatter.set("aliases", &Value::List(aliases));
    frontmatter.set(
        "sources",
        &Value::List(
            snapshot
                .sources
                .iter()
                .map(|slug| format!("[[{slug}]]"))
                .collect(),
        ),
    );
    frontmatter.set("ai_model", &scalar(&snapshot.model));
    frontmatter.set("ai_updated", &scalar(&snapshot.date));
}

/// A new page.
pub fn render_page(snapshot: &PageSnapshot) -> Merged {
    let mut frontmatter = Frontmatter::default();
    apply_keys(&mut frontmatter, snapshot);
    let header = frontmatter.render();
    let content = snapshot.body.trim_matches('\n');
    let body = format!(
        "\n# {}\n\n{}\n\n## Notes\n\n",
        snapshot.title,
        block(PAGE_BLOCK, content)
    );
    Merged {
        written: Written {
            tags: Vec::new(),
            frontmatter_sha256: sha(&header),
            blocks: vec![(PAGE_BLOCK.to_string(), sha(content))],
        },
        text: format!("{header}{body}"),
        user_tags: None,
        edited_blocks: Vec::new(),
    }
}

/// Writes `snapshot` over the page `existing`, as last written `previous`.
/// An edited `page` block stays, and the new text goes into `page-next`.
pub fn merge_page(existing: &str, snapshot: &PageSnapshot, previous: &Written) -> Merged {
    let (mut frontmatter, body) = split(existing);
    apply_keys(&mut frontmatter, snapshot);
    let header = frontmatter.render();
    let mut body = body.to_string();
    let content = snapshot.body.trim_matches('\n').to_string();
    let (mut blocks, edited_blocks) =
        merge_blocks(&mut body, vec![(PAGE_BLOCK, content.clone())], previous);
    blocks.retain(|(name, _)| name != NEXT_BLOCK);
    let next = if edited_blocks.iter().any(|name| name == PAGE_BLOCK) {
        Some(content.as_str())
    } else {
        None
    };
    // The waiting text, or none once the page block takes it.
    body = crate::replace_block_in_body(&body, NEXT_BLOCK, next);
    if let Some(next) = next {
        blocks.push((NEXT_BLOCK.to_string(), sha(next)));
    }
    Merged {
        text: format!("{header}{body}"),
        written: Written {
            tags: Vec::new(),
            frontmatter_sha256: sha(&header),
            blocks,
        },
        user_tags: None,
        edited_blocks,
    }
}

/// The page's id (`glyphst_page`), to find a page the user renamed.
pub fn page_id(note: &str) -> Option<u64> {
    let (frontmatter, _) = split(note);
    match frontmatter.get("glyphst_page")? {
        Value::Scalar(id) => id.parse().ok(),
        Value::List(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_text;

    fn snapshot(body: &str) -> PageSnapshot {
        PageSnapshot {
            id: 7,
            title: "Attention mechanisms".to_string(),
            sources: vec![
                "vaswani2017attention".to_string(),
                "devlin2018bert".to_string(),
            ],
            body: body.to_string(),
            model: "haiku-5-5".to_string(),
            date: "2026-10-20".to_string(),
        }
    }

    #[test]
    fn a_page_renders_with_its_sources_and_keeps_the_users_edits() {
        let first = render_page(&snapshot(
            "Attention weighs tokens [[vaswani2017attention]].",
        ));
        assert_eq!(page_id(&first.text), Some(7));
        assert!(
            first.text.contains("  - \"[[vaswani2017attention]]\""),
            "{}",
            first.text
        );
        assert!(first.text.contains("# Attention mechanisms"));

        // The user adds notes and an alias: regenerating keeps both.
        let edited = first
            .text
            .replace("## Notes\n\n", "## Notes\n\nCompare with RoPE.\n")
            .replace(
                "  - Attention mechanisms\n",
                "  - Attention mechanisms\n  - attention\n",
            );
        let second = merge_page(&edited, &snapshot("A newer text."), &first.written);
        assert_eq!(
            block_text(&second.text, PAGE_BLOCK).as_deref(),
            Some("A newer text.")
        );
        assert!(second.text.contains("Compare with RoPE."));
        assert!(second.text.contains("  - attention\n"), "{}", second.text);
        assert!(block_text(&second.text, NEXT_BLOCK).is_none());

        // The user edits the page's text: it stays, the next waits beside it.
        let mine = second.text.replace("A newer text.", "My own words.");
        let third = merge_page(&mine, &snapshot("The newest text."), &second.written);
        assert_eq!(
            block_text(&third.text, PAGE_BLOCK).as_deref(),
            Some("My own words.")
        );
        assert_eq!(
            block_text(&third.text, NEXT_BLOCK).as_deref(),
            Some("The newest text.")
        );
        assert_eq!(third.edited_blocks, [PAGE_BLOCK]);

        // A deleted page block is not written again.
        let gone = crate::replace_block(&second.text, PAGE_BLOCK, None);
        let fourth = merge_page(&gone, &snapshot("Again."), &second.written);
        assert!(block_text(&fourth.text, PAGE_BLOCK).is_none());
    }
}
