//! The Markdown vault: one note per library item, Obsidian-compatible,
//! that Glyphst writes and the user (or an agent) edits, and concept pages
//! (`concepts/`) written on request from several items (`pages`).
//!
//! Glyphst owns some frontmatter keys and the blocks between
//! `<!-- glyphst:NAME begin -->` and `<!-- glyphst:NAME end -->`; the rest
//! of the note is the user's. A block the user edited, or deleted, is left
//! alone from then on; tags the user changes in the frontmatter go back to
//! the catalog.

pub mod frontmatter;
pub mod index;
pub mod pages;
pub mod wikilinks;

use std::collections::HashSet;

use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization as _;

use frontmatter::{Frontmatter, Value, split};

/// The frontmatter keys Glyphst writes. `glyphst_id` (the item's local
/// number) is only read now, from notes written before `glyphst_uid`.
pub const OWNED_KEYS: [&str; 18] = [
    "glyphst_id",
    "glyphst_uid",
    "glyphst_rev",
    "title",
    "authors",
    "year",
    "kind",
    "doi",
    "arxiv",
    "isbn",
    "publication",
    "tags",
    "collections",
    "status",
    "aliases",
    "ai_tags",
    "ai_model",
    "ai_updated",
];
/// The folder item notes go in.
pub const ITEMS_FOLDER: &str = "items";

/// What a note says about its item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NoteSnapshot {
    pub id: u64,
    /// The item's uid, the same on every computer it syncs to; empty for
    /// none (the note then names `id`).
    pub uid: String,
    /// When the item last changed, as the sync's clock says: a note
    /// written from a newer state than this computer has is left alone
    /// until that state arrives. Empty while the library does not sync.
    pub rev: String,
    pub title: String,
    pub authors: Vec<String>,
    pub year: Option<i32>,
    /// `paper`, `book` or `other`.
    pub kind: String,
    pub doi: Option<String>,
    pub arxiv: Option<String>,
    pub isbn: Option<String>,
    pub publication: String,
    pub tags: Vec<String>,
    /// As paths, `a/b`.
    pub collections: Vec<String>,
    /// `to-read`, `reading` or `read`.
    pub status: String,
    pub ai_tags: Vec<String>,
    pub summary: Option<Summary>,
    /// The document's file name, shown in the meta block.
    pub file: Option<String>,
    /// The note text to start the user's section with (only for a new note).
    pub seed_notes: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Summary {
    pub tldr: String,
    pub key_points: Vec<String>,
    pub model: String,
    /// `YYYY-MM-DD`.
    pub date: String,
}

/// What Glyphst last wrote into a note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// The tags as written in the frontmatter.
    pub tags: Vec<String>,
    pub frontmatter_sha256: [u8; 32],
    /// Each owned block and the hash of its contents; a block missing here
    /// was never written, one with no block in the note was deleted.
    pub blocks: Vec<(String, [u8; 32])>,
}

/// The outcome of writing a snapshot over a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Merged {
    pub text: String,
    pub written: Written,
    /// The tags the user set in the note's frontmatter (catalog names),
    /// when they differ from what Glyphst wrote.
    pub user_tags: Option<Vec<String>>,
    /// Owned blocks the user edited (left as they are).
    pub edited_blocks: Vec<String>,
}

/// A tag as an Obsidian tag: no spaces.
pub fn vault_tag(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("-")
}

/// A note's file name: the first author's surname, the year and the
/// title's first word that says something, like a citation key
/// (`vaswani2017attention`).
pub fn slug(authors: &[String], year: Option<i32>, title: &str) -> String {
    let fold = |text: &str| {
        text.nfkd()
            .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .to_lowercase()
    };
    let surname = authors
        .first()
        .map(|name| match name.split_once(',') {
            Some((last, _)) => last.to_string(),
            None => name
                .split_whitespace()
                .last()
                .unwrap_or_default()
                .to_string(),
        })
        .map(|surname| fold(&surname).replace(' ', ""))
        .unwrap_or_default();
    let word = fold(title)
        .split_whitespace()
        .find(|word| {
            word.len() > 2
                && !matches!(
                    *word,
                    "the"
                        | "and"
                        | "for"
                        | "with"
                        | "from"
                        | "into"
                        | "are"
                        | "how"
                        | "why"
                        | "what"
                        | "towards"
                        | "toward"
                        | "on"
                        | "of"
                )
        })
        .unwrap_or_default()
        .to_string();
    let mut slug = format!(
        "{surname}{}{word}",
        year.map(|year| year.to_string()).unwrap_or_default()
    );
    if slug.is_empty() {
        slug = "untitled".to_string();
    }
    slug.chars().take(60).collect()
}

/// `slug`, or with a letter after it when `taken` has it already.
pub fn unique_slug(slug: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(slug) {
        return slug.to_string();
    }
    for suffix in 'a'..='z' {
        let candidate = format!("{slug}{suffix}");
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    let mut index = 2;
    loop {
        let candidate = format!("{slug}-{index}");
        if !taken.contains(&candidate) {
            return candidate;
        }
        index += 1;
    }
}

fn sha(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

fn begin(name: &str) -> String {
    format!("<!-- glyphst:{name} begin -->")
}

fn end(name: &str) -> String {
    format!("<!-- glyphst:{name} end -->")
}

/// The contents of block `name` in `body` and where the whole block is.
fn find_block(body: &str, name: &str) -> Option<(std::ops::Range<usize>, String)> {
    let start = body.find(&begin(name))?;
    let content_start = start + begin(name).len();
    let end_at = content_start + body[content_start..].find(&end(name))?;
    let whole = start..end_at + end(name).len();
    Some((
        whole,
        body[content_start..end_at].trim_matches('\n').to_string(),
    ))
}

fn block(name: &str, content: &str) -> String {
    format!(
        "{}\n{}\n{}",
        begin(name),
        content.trim_matches('\n'),
        end(name)
    )
}

fn meta_block(snapshot: &NoteSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(file) = &snapshot.file {
        let link = if snapshot.uid.is_empty() {
            format!("glyphst://open?id={}", snapshot.id)
        } else {
            format!("glyphst://open?uid={}", snapshot.uid)
        };
        parts.push(format!("[open in Glyphst]({link}) · `{file}`"));
    }
    let mut line = Vec::new();
    match snapshot.authors.as_slice() {
        [] => {}
        [one] => line.push(one.clone()),
        [first, ..] => line.push(format!("{first} et al.")),
    }
    if let Some(year) = snapshot.year {
        line.push(year.to_string());
    }
    if !snapshot.publication.is_empty() {
        line.push(snapshot.publication.clone());
    }
    if let Some(doi) = &snapshot.doi {
        line.push(format!("[doi:{doi}](https://doi.org/{doi})"));
    }
    if let Some(arxiv) = &snapshot.arxiv {
        line.push(format!("[arXiv:{arxiv}](https://arxiv.org/abs/{arxiv})"));
    }
    if let Some(isbn) = &snapshot.isbn {
        line.push(format!("ISBN {isbn}"));
    }
    if !line.is_empty() {
        parts.push(line.join(" · "));
    }
    parts.join("\n")
}

fn summary_block(summary: &Summary) -> String {
    let mut text = format!(
        "> [!abstract] Summary · {} · {}\n> {}",
        summary.model, summary.date, summary.tldr
    );
    if !summary.key_points.is_empty() {
        text.push_str("\n\n");
        text.push_str(
            &summary
                .key_points
                .iter()
                .map(|point| format!("- {point}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    text
}

fn owned_blocks(snapshot: &NoteSnapshot) -> Vec<(&'static str, String)> {
    let mut blocks = vec![("meta", meta_block(snapshot))];
    if let Some(summary) = &snapshot.summary {
        blocks.push(("summary", summary_block(summary)));
    }
    blocks
}

fn apply_owned_keys(frontmatter: &mut Frontmatter, snapshot: &NoteSnapshot, tags: &[String]) {
    let scalar = |value: &str| Value::Scalar(value.to_string());
    let optional = |frontmatter: &mut Frontmatter, key: &str, value: &Option<String>| match value {
        Some(value) => frontmatter.set(key, &scalar(value)),
        None => frontmatter.remove(key),
    };
    if snapshot.uid.is_empty() {
        frontmatter.set("glyphst_id", &scalar(&snapshot.id.to_string()));
    } else {
        // The local number differs between computers; the uid does not.
        frontmatter.remove("glyphst_id");
        frontmatter.set("glyphst_uid", &scalar(&snapshot.uid));
    }
    if snapshot.rev.is_empty() {
        frontmatter.remove("glyphst_rev");
    } else {
        frontmatter.set("glyphst_rev", &scalar(&snapshot.rev));
    }
    frontmatter.set("title", &scalar(&snapshot.title));
    frontmatter.set("authors", &Value::List(snapshot.authors.clone()));
    optional(
        frontmatter,
        "year",
        &snapshot.year.map(|year| year.to_string()),
    );
    frontmatter.set("kind", &scalar(&snapshot.kind));
    optional(frontmatter, "doi", &snapshot.doi);
    optional(frontmatter, "arxiv", &snapshot.arxiv);
    optional(frontmatter, "isbn", &snapshot.isbn);
    optional(
        frontmatter,
        "publication",
        &Some(snapshot.publication.clone()).filter(|publication| !publication.is_empty()),
    );
    frontmatter.set("tags", &Value::List(tags.to_vec()));
    frontmatter.set("collections", &Value::List(snapshot.collections.clone()));
    frontmatter.set("status", &scalar(&snapshot.status));
    frontmatter.set("aliases", &Value::List(vec![snapshot.title.clone()]));
    if snapshot.ai_tags.is_empty() {
        frontmatter.remove("ai_tags");
    } else {
        frontmatter.set(
            "ai_tags",
            &Value::List(snapshot.ai_tags.iter().map(|tag| vault_tag(tag)).collect()),
        );
    }
    match &snapshot.summary {
        Some(summary) => {
            frontmatter.set("ai_model", &scalar(&summary.model));
            frontmatter.set("ai_updated", &scalar(&summary.date));
        }
        None => {
            frontmatter.remove("ai_model");
            frontmatter.remove("ai_updated");
        }
    }
}

/// A new note for `snapshot`.
pub fn render(snapshot: &NoteSnapshot) -> Merged {
    let tags = snapshot
        .tags
        .iter()
        .map(|tag| vault_tag(tag))
        .collect::<Vec<_>>();
    let mut frontmatter = Frontmatter::default();
    apply_owned_keys(&mut frontmatter, snapshot, &tags);
    let header = frontmatter.render();
    let mut body = format!("\n# {}\n", snapshot.title);
    let mut blocks = Vec::new();
    for (name, content) in owned_blocks(snapshot) {
        body.push('\n');
        body.push_str(&block(name, &content));
        body.push('\n');
        blocks.push((name.to_string(), sha(content.trim_matches('\n'))));
    }
    body.push_str("\n## Notes\n\n");
    if !snapshot.seed_notes.trim().is_empty() {
        body.push_str(snapshot.seed_notes.trim());
        body.push('\n');
    }
    Merged {
        written: Written {
            tags,
            frontmatter_sha256: sha(&header),
            blocks,
        },
        text: format!("{header}{body}"),
        user_tags: None,
        edited_blocks: Vec::new(),
    }
}

/// Writes `snapshot` over the note `existing`, as last written `previous`:
/// owned keys and untouched owned blocks are replaced; everything else,
/// and blocks the user edited or deleted, stay.
pub fn merge(existing: &str, snapshot: &NoteSnapshot, previous: &Written) -> Merged {
    let (mut frontmatter, body) = split(existing);
    // Tags changed in the note win; the caller gives them to the catalog.
    let note_tags = frontmatter
        .get("tags")
        .map(|value| value.as_list())
        .unwrap_or_default();
    let snapshot_tags = snapshot
        .tags
        .iter()
        .map(|tag| vault_tag(tag))
        .collect::<Vec<_>>();
    let same = |left: &[String], right: &[String]| {
        let fold = |tags: &[String]| {
            let mut tags = tags
                .iter()
                .map(|tag| tag.trim_start_matches('#').to_lowercase())
                .collect::<Vec<_>>();
            tags.sort();
            tags
        };
        fold(left) == fold(right)
    };
    // Tags that are what Glyphst would write are not the user's (another
    // computer wrote them, from the same catalog).
    let user_tags =
        (!same(&note_tags, &previous.tags) && !same(&note_tags, &snapshot_tags)).then(|| {
            note_tags
                .iter()
                .map(|tag| tag.trim_start_matches('#').to_string())
                .collect::<Vec<_>>()
        });
    let tags = match &user_tags {
        Some(tags) => tags.clone(),
        None => snapshot_tags,
    };
    apply_owned_keys(&mut frontmatter, snapshot, &tags);
    let header = frontmatter.render();

    let mut body = body.to_string();
    let (blocks, edited_blocks) = merge_blocks(&mut body, owned_blocks(snapshot), previous);
    Merged {
        text: format!("{header}{body}"),
        written: Written {
            tags,
            frontmatter_sha256: sha(&header),
            blocks,
        },
        user_tags,
        edited_blocks,
    }
}

/// Writes the `owned` blocks into `body` as last written `previous`: an
/// untouched block is replaced, one the user edited stays (and is listed),
/// one the user deleted is not written again, and a new one goes before
/// `## Notes`. Blocks it does not own (an agent's) keep their hashes.
fn merge_blocks(
    body: &mut String,
    owned: Vec<(&str, String)>,
    previous: &Written,
) -> (Vec<(String, [u8; 32])>, Vec<String>) {
    let mut blocks = Vec::new();
    let mut edited_blocks = Vec::new();
    for (name, content) in owned {
        let written = previous
            .blocks
            .iter()
            .find(|(written, _)| written == name)
            .map(|(_, hash)| *hash);
        match find_block(body, name) {
            Some((range, current)) => {
                // Untouched since written, or already what would be written
                // (another computer wrote the same): replaced.
                if written.is_none_or(|hash| hash == sha(&current))
                    || current.trim_matches('\n') == content.trim_matches('\n')
                {
                    body.replace_range(range, &block(name, &content));
                    blocks.push((name.to_string(), sha(content.trim_matches('\n'))));
                } else {
                    edited_blocks.push(name.to_string());
                    if let Some(hash) = written {
                        blocks.push((name.to_string(), hash));
                    }
                }
            }
            // Deleted by the user: not written again.
            None if written.is_some() => {}
            None => {
                let at = body.find("\n## Notes").unwrap_or(body.len());
                body.insert_str(at, &format!("\n{}\n", block(name, &content)));
                blocks.push((name.to_string(), sha(content.trim_matches('\n'))));
            }
        }
    }
    for (name, hash) in &previous.blocks {
        if !blocks.iter().any(|(written, _)| written == name) {
            blocks.push((name.clone(), *hash));
        }
    }
    (blocks, edited_blocks)
}

/// The hash Glyphst keeps of a block's contents, to tell later edits.
pub fn block_hash(content: &str) -> [u8; 32] {
    sha(content.trim_matches('\n'))
}

/// The contents of block `name` in `note`.
pub fn block_text(note: &str, name: &str) -> Option<String> {
    let (_, body) = split(note);
    find_block(body, name).map(|(_, content)| content)
}

/// `note` with block `name` holding `content`, put before `## Notes` when
/// it is new; `None` takes the block out.
pub fn replace_block(note: &str, name: &str, content: Option<&str>) -> String {
    let (_, body) = split(note);
    let header = &note[..note.len() - body.len()];
    format!("{header}{}", replace_block_in_body(body, name, content))
}

/// `replace_block` on a note's body (no frontmatter).
fn replace_block_in_body(body: &str, name: &str, content: Option<&str>) -> String {
    let mut body = body.to_string();
    match (find_block(&body, name), content) {
        (Some((range, _)), Some(content)) => body.replace_range(range, &block(name, content)),
        (Some((range, _)), None) => {
            // With its line and the blank line before it.
            let end = if body[range.end..].starts_with('\n') {
                range.end + 1
            } else {
                range.end
            };
            let start = if body[..range.start].ends_with("\n\n") {
                range.start - 1
            } else {
                range.start
            };
            body.replace_range(start..end, "");
        }
        (None, Some(content)) => {
            let at = body.find("\n## Notes").unwrap_or(body.len());
            body.insert_str(at, &format!("\n{}\n", block(name, content)));
        }
        (None, None) => {}
    }
    body
}

/// The note's id (`glyphst_id`), to find a note the user renamed.
pub fn note_id(note: &str) -> Option<u64> {
    scalar_key(note, "glyphst_id")?.parse().ok()
}

/// The item uid a note names (`glyphst_uid`).
pub fn note_uid(note: &str) -> Option<String> {
    scalar_key(note, "glyphst_uid")
}

/// The state of the catalog a note was written from (`glyphst_rev`).
pub fn note_rev(note: &str) -> Option<String> {
    scalar_key(note, "glyphst_rev")
}

pub(crate) fn scalar_key(note: &str, key: &str) -> Option<String> {
    let (frontmatter, _) = split(note);
    match frontmatter.get(key)? {
        Value::Scalar(value) => Some(value.clone()),
        Value::List(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> NoteSnapshot {
        NoteSnapshot {
            id: 12,
            uid: String::new(),
            rev: String::new(),
            title: "Attention Is All You Need".to_string(),
            authors: vec!["Ashish Vaswani".to_string(), "Noam Shazeer".to_string()],
            year: Some(2017),
            kind: "paper".to_string(),
            arxiv: Some("1706.03762".to_string()),
            publication: "NeurIPS".to_string(),
            tags: vec!["transformers".to_string(), "to read".to_string()],
            collections: vec!["machine learning/llms".to_string()],
            status: "reading".to_string(),
            ai_tags: vec!["transformers".to_string()],
            summary: Some(Summary {
                tldr: "Introduces the Transformer.".to_string(),
                key_points: vec!["multi-head attention".to_string()],
                model: "haiku".to_string(),
                date: "2026-10-19".to_string(),
            }),
            file: Some("/papers/attention.pdf".to_string()),
            seed_notes: "Compare with RoPE.".to_string(),
            ..NoteSnapshot::default()
        }
    }

    #[test]
    fn a_note_another_computer_wrote_from_the_same_catalog_is_taken_as_is() {
        let mut ours = snapshot();
        ours.uid = "6f1c".to_string();
        ours.rev = "0001".to_string();
        let theirs = render(&ours);
        // Nothing recorded here: this computer never wrote the note.
        let merged = merge(&theirs.text, &ours, &Written::default());
        assert_eq!(merged.text, theirs.text);
        assert!(merged.edited_blocks.is_empty());
        assert_eq!(merged.user_tags, None);
        assert!(merged.text.contains("glyphst_uid: 6f1c"));
        assert!(merged.text.contains("glyphst://open?uid=6f1c"));
        assert_eq!(note_uid(&merged.text).as_deref(), Some("6f1c"));
        assert_eq!(note_rev(&merged.text).as_deref(), Some("0001"));
        assert_eq!(note_id(&merged.text), None);
    }

    #[test]
    fn slugs_read_like_citation_keys_and_never_collide() {
        assert_eq!(
            slug(
                &["Ashish Vaswani".to_string()],
                Some(2017),
                "Attention Is All You Need"
            ),
            "vaswani2017attention"
        );
        assert_eq!(
            slug(
                &["Gödel, Kurt".to_string()],
                None,
                "On Formally Undecidable Propositions"
            ),
            "godelformally"
        );
        assert_eq!(slug(&[], None, ""), "untitled");
        let taken = HashSet::from(["vaswani2017attention".to_string()]);
        assert_eq!(
            unique_slug("vaswani2017attention", &taken),
            "vaswani2017attentiona"
        );
    }

    #[test]
    fn a_new_note_has_frontmatter_blocks_and_the_users_section() {
        let note = render(&snapshot());
        assert!(
            note.text
                .starts_with("---\nglyphst_id: 12\ntitle: Attention Is All You Need\n")
        );
        assert!(note.text.contains("tags:\n  - transformers\n  - to-read\n"));
        assert!(note.text.contains("<!-- glyphst:summary begin -->\n> [!abstract] Summary · haiku · 2026-10-19\n> Introduces the Transformer.\n\n- multi-head attention\n<!-- glyphst:summary end -->"));
        assert!(note.text.ends_with("## Notes\n\nCompare with RoPE.\n"));
        assert_eq!(note_id(&note.text), Some(12));
        assert_eq!(note.written.tags, vec!["transformers", "to-read"]);
    }

    #[test]
    fn a_rewrite_keeps_the_users_text_keys_and_edited_blocks() {
        let first = render(&snapshot());
        let mut edited = first
            .text
            .replace("aliases:", "cssclasses: [wide]\naliases:")
            .replace(
                "Compare with RoPE.",
                "Compare with RoPE.\n\nMy own thoughts.",
            )
            .replace(
                "- multi-head attention",
                "- multi-head attention (my words)",
            );
        edited.push_str("\n[[vaswani2017attention]] is cited by [[devlin2018bert]].\n");
        let mut changed = snapshot();
        changed.title = "Attention Is All You Need (v7)".to_string();
        changed.summary.as_mut().unwrap().tldr = "A newer summary.".to_string();
        let merged = merge(&edited, &changed, &first.written);
        assert!(
            merged
                .text
                .contains("title: Attention Is All You Need (v7)")
        );
        assert!(merged.text.contains("cssclasses: [wide]"));
        assert!(merged.text.contains("My own thoughts."));
        assert!(merged.text.contains("[[devlin2018bert]]"));
        assert!(merged.text.contains("(my words)"), "an edited block stays");
        assert!(!merged.text.contains("A newer summary."));
        assert_eq!(merged.edited_blocks, vec!["summary"]);
        assert_eq!(merged.user_tags, None);

        // A deleted block is not written again.
        let without_meta = {
            let (range, _) = find_block(&merged.text, "meta").unwrap();
            let mut text = merged.text.clone();
            text.replace_range(range, "");
            text
        };
        let again = merge(&without_meta, &changed, &merged.written);
        assert!(!again.text.contains("glyphst:meta"));
    }

    #[test]
    fn tags_the_user_changes_in_the_note_come_back() {
        let first = render(&snapshot());
        let edited = first.text.replace("  - to-read\n", "  - \"#reviewed\"\n");
        let merged = merge(&edited, &snapshot(), &first.written);
        assert_eq!(
            merged.user_tags,
            Some(vec!["transformers".to_string(), "reviewed".to_string()])
        );
        assert!(merged.text.contains("  - reviewed\n"));
        assert_eq!(merged.written.tags, vec!["transformers", "reviewed"]);
        // Once the catalog has them, nothing comes back.
        let mut caught_up = snapshot();
        caught_up.tags = vec!["transformers".to_string(), "reviewed".to_string()];
        let next = merge(&merged.text, &caught_up, &merged.written);
        assert_eq!(next.user_tags, None);
    }

    #[test]
    fn an_agent_block_goes_before_the_notes_and_survives_a_rewrite() {
        let first = render(&snapshot());
        let with_block = replace_block(
            &first.text,
            "agent-claude",
            Some("Cites [[devlin2018bert]]."),
        );
        let notes = with_block.find("## Notes").unwrap();
        let block_at = with_block
            .find("<!-- glyphst:agent-claude begin -->")
            .unwrap();
        assert!(block_at < notes);
        assert_eq!(
            block_text(&with_block, "agent-claude").as_deref(),
            Some("Cites [[devlin2018bert]].")
        );
        let replaced = replace_block(&with_block, "agent-claude", Some("Updated."));
        assert_eq!(
            block_text(&replaced, "agent-claude").as_deref(),
            Some("Updated.")
        );
        assert_eq!(replaced.matches("glyphst:agent-claude begin").count(), 1);

        // The sync keeps the block and the hash an agent recorded.
        let mut written = first.written.clone();
        written
            .blocks
            .push(("agent-claude".to_string(), block_hash("Updated.")));
        let merged = merge(&replaced, &snapshot(), &written);
        assert!(merged.text.contains("Updated."));
        assert!(
            merged
                .written
                .blocks
                .contains(&("agent-claude".to_string(), block_hash("Updated.")))
        );

        let removed = replace_block(&replaced, "agent-claude", None);
        assert_eq!(block_text(&removed, "agent-claude"), None);
        assert_eq!(removed, first.text, "taken out, the note is as before");
    }
}
