//! `glyphst://` links, which the assistant cites passages with and the
//! vault's notes carry: `glyphst://open?id=12&page=5&chunk=381` opens
//! library item 12 at page 5 (1-based), at the passage when the index
//! still has chunk 381; `glyphst://page?id=3` shows concept page 3.
//!
//! What is written into the vault names the item by its uid instead
//! (`glyphst://open?uid=…&page=5&chunk=381`), which every computer the
//! library syncs to knows; there the chunk is only taken when this
//! computer's index has it in that item (each indexes on its own).

/// What a link opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenLink {
    pub item: Option<u64>,
    /// The item's uid, which outranks `item`.
    pub uid: Option<u128>,
    /// 1-based, as people read it.
    pub page: Option<usize>,
    pub chunk: Option<i64>,
}

pub const SCHEME: &str = "glyphst://";

/// Any `glyphst://` link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Open(OpenLink),
    /// A concept page, by id.
    Page(u64),
}

/// A link to a concept page.
pub fn page(id: u64) -> String {
    format!("{SCHEME}page?id={id}")
}

/// An item, as a page's `[[link]]` to its note shows it.
pub fn item(id: u64) -> String {
    format!("{SCHEME}open?id={id}")
}

/// A link of either kind.
pub fn parse_link(url: &str) -> Option<Link> {
    if let Some(link) = parse(url) {
        return Some(Link::Open(link));
    }
    let rest = url.strip_prefix(SCHEME)?;
    let (action, query) = rest.split_once('?')?;
    (action.trim_end_matches('/') == "page")
        .then(|| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix("id="))?
                .parse()
                .ok()
        })
        .flatten()
        .map(Link::Page)
}

/// `url` naming its item by `uid`, as the vault keeps it.
pub fn portable(url: &str, uid: uuid::Uuid) -> Option<String> {
    let link = parse(url)?;
    let mut parts = vec![format!("uid={uid}")];
    parts.extend(link.page.map(|page| format!("page={page}")));
    parts.extend(link.chunk.map(|chunk| format!("chunk={chunk}")));
    Some(format!("{SCHEME}open?{}", parts.join("&")))
}

/// A link to a passage (`page` 0-based).
pub fn passage(item: u64, page: usize, chunk: i64) -> String {
    format!("{SCHEME}open?id={item}&page={}&chunk={chunk}", page + 1)
}

pub fn parse(url: &str) -> Option<OpenLink> {
    let rest = url.strip_prefix(SCHEME)?;
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    if action.trim_end_matches('/') != "open" {
        return None;
    }
    let mut link = OpenLink::default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "id" => link.item = value.trim_start_matches("lib_").parse().ok(),
            "uid" => link.uid = uuid::Uuid::parse_str(value).ok().map(|uid| uid.as_u128()),
            "page" => link.page = value.parse().ok().filter(|page| *page > 0),
            "chunk" => link.chunk = value.trim_start_matches("c_").parse().ok(),
            _ => {}
        }
    }
    (link.item.is_some() || link.uid.is_some() || link.chunk.is_some()).then_some(link)
}

/// Takes this frame's `glyphst://` links out of egui's output, so they
/// open here rather than in the system's browser.
pub fn take(ctx: &egui::Context) -> Vec<Link> {
    ctx.output_mut(|output| {
        let mut links = Vec::new();
        output.commands.retain(|command| match command {
            egui::OutputCommand::OpenUrl(open) if open.url.starts_with(SCHEME) => {
                links.extend(parse_link(&open.url));
                false
            }
            _ => true,
        });
        links
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_links_name_a_page() {
        assert_eq!(parse_link(&page(3)), Some(Link::Page(3)));
        assert_eq!(
            parse_link(&item(7)),
            Some(Link::Open(OpenLink {
                item: Some(7),
                ..OpenLink::default()
            }))
        );
        assert_eq!(parse_link("glyphst://page?id=x"), None);
    }

    #[test]
    fn links_name_an_item_a_page_and_a_passage() {
        let url = passage(12, 4, 381);
        assert_eq!(url, "glyphst://open?id=12&page=5&chunk=381");
        assert_eq!(
            parse(&url),
            Some(OpenLink {
                item: Some(12),
                uid: None,
                page: Some(5),
                chunk: Some(381)
            })
        );
        // As the vault writes them, and as a model may.
        assert_eq!(
            parse("glyphst://open?id=7"),
            Some(OpenLink {
                item: Some(7),
                ..OpenLink::default()
            })
        );
        assert_eq!(parse("glyphst://open?id=lib_7&page=0").unwrap().page, None);
        assert_eq!(parse("glyphst://settings"), None);
        assert_eq!(parse("https://example.org"), None);
    }

    #[test]
    fn links_in_the_vault_name_items_by_uid() {
        let uid = uuid::Uuid::from_u128(0xfeed);
        let url = portable(&passage(12, 4, 381), uid).unwrap();
        assert_eq!(url, format!("glyphst://open?uid={uid}&page=5&chunk=381"));
        assert_eq!(
            parse(&url),
            Some(OpenLink {
                item: None,
                uid: Some(0xfeed),
                page: Some(5),
                chunk: Some(381)
            })
        );
    }
}
