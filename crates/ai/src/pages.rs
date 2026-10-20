//! Concept pages: the request that has the model research a topic across
//! several library items (with the library's search tools) and write a
//! page that cites the passages it read. The run and the checks on its
//! citations are the app's; this is what the model is told.

/// Bumped when the prompt changes.
pub const PAGE_PROMPT_VERSION: u32 = 1;
/// Tool rounds a page may take (searches and reads).
pub const PAGE_TOOL_ROUNDS: usize = 10;
/// Tokens a page takes to write, for estimates.
pub const PAGE_ANSWER_TOKENS: u64 = 1_500;
/// Tokens the search results take, for estimates: about six rounds.
const TOOL_RESULT_TOKENS: u64 = 6 * 1_800;

/// A library item a page draws on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageSource {
    pub id: u64,
    /// Its note's name in the vault: the page links it as `[[slug]]`.
    pub slug: String,
    pub title: String,
    pub year: Option<i32>,
    /// Its summary, when it has one.
    pub tldr: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageInput {
    pub title: String,
    /// What the page is about, in a sentence: "the papers tagged nlp",
    /// or what the user asked.
    pub about: String,
    pub sources: Vec<PageSource>,
}

/// The request. The page's text is the model's final answer.
pub fn page_request(input: &PageInput) -> String {
    let sources = input
        .sources
        .iter()
        .map(|source| {
            let year = source
                .year
                .map(|year| format!(", {year}"))
                .unwrap_or_default();
            let tldr = if source.tldr.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", source.tldr.trim())
            };
            format!(
                "- [[{}]] (lib_{}) {}{year}{tldr}",
                source.slug, source.id, source.title
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "<concept_page>
Write a concept page about \"{title}\" for a personal research library, in Obsidian Markdown.
{about}

<sources>
{sources}
</sources>

First research it: use search_library (search for the ideas, not only the name) and read_chunk to read passages in full, mostly in the sources above. Then answer with the page only:
- Begin with an overview of two or three sentences. No title heading and no frontmatter.
- Then `##` sections on the main ideas, and on how the sources relate, agree or differ.
- Cite every claim with a passage you read, as a Markdown link exactly as the tools gave it, such as [p 5](glyphst://open?id=12&page=5&chunk=381). Never make up a link.
- Refer to a source by its link, as written above: [[slug]].
- 300 to 700 words.
</concept_page>",
        title = input.title,
        about = input.about,
    )
}

/// Roughly the tokens a page takes in and out, for the cost shown before
/// writing it: the request and the reader's prompt, the search results it
/// reads (sent again with each round, so counted twice), and the page.
pub fn estimate_tokens(input: &PageInput) -> (u64, u64) {
    let request = page_request(input).chars().count() as u64 / 4;
    (
        request + 1_500 + 2 * TOOL_RESULT_TOKENS,
        PAGE_ANSWER_TOKENS + 400,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_lists_sources_as_links_and_asks_for_real_citations() {
        let input = PageInput {
            title: "Attention mechanisms".to_string(),
            about: "The papers tagged nlp.".to_string(),
            sources: vec![PageSource {
                id: 12,
                slug: "vaswani2017attention".to_string(),
                title: "Attention Is All You Need".to_string(),
                year: Some(2017),
                tldr: "The Transformer.".to_string(),
            }],
        };
        let request = page_request(&input);
        assert!(request.starts_with("<concept_page>"));
        assert!(request.contains(
            "- [[vaswani2017attention]] (lib_12) Attention Is All You Need, 2017 — The Transformer."
        ));
        assert!(request.contains("Never make up a link."));
        let (input_tokens, output_tokens) = estimate_tokens(&input);
        assert!(input_tokens > 20_000 && output_tokens > 1_000);
    }
}
