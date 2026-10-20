//! `[[wikilinks]]` as Obsidian reads them: `[[note]]`, `[[note|shown]]`,
//! `[[note#heading]]`, `![[note]]` (an embed) and `[[folder/note.md]]`.
//! Links inside code (fenced or inline) are not links.

/// One link in a note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WikiLink {
    /// What it points at, as written (`folder/note`, without `.md`).
    pub target: String,
    pub heading: Option<String>,
    /// The text shown instead of the target.
    pub alias: Option<String>,
    /// `![[…]]`: shown in place.
    pub embed: bool,
}

impl WikiLink {
    /// The note it points at, as Obsidian finds it (`link_key`).
    pub fn key(&self) -> String {
        link_key(&self.target)
    }
}

/// The links in `markdown`, in order.
pub fn wikilinks(markdown: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();
    let mut fence: Option<&str> = None;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker));
        match (fence, marker) {
            (None, Some(marker)) => {
                fence = Some(marker);
                continue;
            }
            (Some(open), Some(marker)) if open == marker => {
                fence = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }
        links.extend(line_links(line));
    }
    links
}

/// The links in one line outside a fence, skipping inline code.
fn line_links(line: &str) -> Vec<WikiLink> {
    let mut links = Vec::new();
    let mut rest = line;
    let mut in_code = false;
    let mut embed_before = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`') {
            in_code = !in_code;
            rest = after;
            embed_before = false;
            continue;
        }
        if !in_code
            && let Some(after) = rest.strip_prefix("[[")
            && let Some(close) = after.find("]]")
        {
            let inner = &after[..close];
            if !inner.is_empty() && !inner.contains('\n') && !inner.contains("[[") {
                links.push(parse(inner, embed_before));
                rest = &after[close + 2..];
                embed_before = false;
                continue;
            }
        }
        let mut chars = rest.chars();
        let c = chars.next().unwrap_or_default();
        embed_before = c == '!';
        rest = chars.as_str();
    }
    links
}

fn parse(inner: &str, embed: bool) -> WikiLink {
    let (target, alias) = match inner.split_once('|') {
        Some((target, alias)) => (target, Some(alias.trim().to_string())),
        None => (inner, None),
    };
    let (target, heading) = match target.split_once('#') {
        Some((target, heading)) => (target, Some(heading.trim().to_string())),
        None => (target, None),
    };
    let target = target.trim();
    WikiLink {
        target: target.strip_suffix(".md").unwrap_or(target).to_string(),
        heading: heading.filter(|heading| !heading.is_empty()),
        alias: alias.filter(|alias| !alias.is_empty()),
        embed,
    }
}

/// The name a link finds a note by: its file name without `.md`, folded
/// to lower case (`Items/Vaswani2017Attention.md` → `vaswani2017attention`).
pub fn link_key(target: &str) -> String {
    let name = target.rsplit(['/', '\\']).next().unwrap_or(target).trim();
    name.strip_suffix(".md").unwrap_or(name).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_parse_with_aliases_headings_and_embeds_but_not_in_code() {
        let note = "See [[vaswani2017attention|the Transformer]] and [[Mamba#Selective scan]].\n\
                    ![[figure.png]] and [[items/devlin2018bert.md]].\n\
                    `[[not a link]]` but [[after code]].\n\
                    ```\n[[in a fence]]\n```\n\
                    [[]] and [[broken";
        let links = wikilinks(note);
        let targets = links
            .iter()
            .map(|link| link.target.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            [
                "vaswani2017attention",
                "Mamba",
                "figure.png",
                "items/devlin2018bert",
                "after code"
            ]
        );
        assert_eq!(links[0].alias.as_deref(), Some("the Transformer"));
        assert_eq!(links[1].heading.as_deref(), Some("Selective scan"));
        assert!(links[2].embed && !links[0].embed);
        assert_eq!(links[3].key(), "devlin2018bert");
        assert_eq!(
            link_key("Items/Vaswani2017Attention.md"),
            "vaswani2017attention"
        );
    }
}
