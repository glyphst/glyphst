//! Writing concept pages: the model researches a topic with the library's
//! search tools on a thread of its own, and its page is checked before it
//! is written: a citation stays only when its passage exists and belongs
//! to the item it names, a `[[link]]` only when the note exists. Then the
//! page goes into the vault's `concepts/` (merged, so the user's notes and
//! edits stay) and waits for the user to keep it or undo it.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
};

use anyhow::{Context as _, Result, anyhow, bail};
use glyphst_ai::{
    agent::{AgentCancellationToken, AgentLimits, AgentMessage, AgentRole, AgentRun, run_agent},
    pages::{PAGE_TOOL_ROUNDS, PageInput, page_request},
    tools::{ToolContext, ToolRegistry},
};
use glyphst_library::{DocumentId, LibraryStore, PageRecord, PageTopic, SyncKind};
use glyphst_settings::ResolvedAiConfig;
use glyphst_vault::{
    Written, block_hash, block_text,
    index::VaultIndex,
    pages::{PAGE_BLOCK, PAGES_FOLDER, PageSnapshot, merge_page, render_page},
    replace_block,
};

use crate::search_tools::IndexHandle;

/// A page to write.
#[derive(Clone, Debug)]
pub struct PageJob {
    pub page_id: u64,
    pub topic: PageTopic,
    pub input: PageInput,
    /// Notes the page may link to besides its sources (the vault's).
    pub known_notes: HashSet<String>,
}

/// A page the model wrote, checked.
#[derive(Clone, Debug)]
pub struct WrittenPage {
    pub job: PageJob,
    pub body: String,
    /// Citations kept, and those dropped (made up, or not the item's).
    pub cited: usize,
    pub dropped: usize,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
}

pub enum PageResult {
    Written(Box<WrittenPage>),
    Failed {
        page_id: u64,
        title: String,
        error: String,
    },
}

/// Pages being written, one thread each, and what came back.
pub struct PageWriter {
    results: Receiver<PageResult>,
    sender: Sender<PageResult>,
    running: Vec<(u64, AgentCancellationToken)>,
}

impl Default for PageWriter {
    fn default() -> Self {
        let (sender, results) = mpsc::channel();
        Self {
            results,
            sender,
            running: Vec::new(),
        }
    }
}

impl PageWriter {
    pub fn start(
        &mut self,
        job: PageJob,
        settings: ResolvedAiConfig,
        index: IndexHandle,
        ctx: &egui::Context,
    ) {
        let cancel = AgentCancellationToken::default();
        self.running.push((job.page_id, cancel.clone()));
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("library-pages".to_string())
            .spawn(move || {
                let (page_id, title) = (job.page_id, job.input.title.clone());
                let result = match write_page(job, settings, &index, cancel) {
                    Ok(page) => PageResult::Written(Box::new(page)),
                    Err(error) => PageResult::Failed {
                        page_id,
                        title,
                        error: format!("{error:#}"),
                    },
                };
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        if let Err(error) = spawned {
            log::warn!("no thread for a page: {error}");
        }
    }

    /// The pages that finished since the last call.
    pub fn take(&mut self) -> Vec<PageResult> {
        let results = self.results.try_iter().collect::<Vec<_>>();
        for result in &results {
            let id = match result {
                PageResult::Written(page) => page.job.page_id,
                PageResult::Failed { page_id, .. } => *page_id,
            };
            self.running.retain(|(running, _)| *running != id);
        }
        results
    }
}

impl Drop for PageWriter {
    /// Pages still being written stop with the app.
    fn drop(&mut self) {
        for (_, cancel) in &self.running {
            cancel.cancel();
        }
    }
}

/// The model's run and the checks, on the page's thread.
pub fn write_page(
    job: PageJob,
    settings: ResolvedAiConfig,
    index: &IndexHandle,
    cancel: AgentCancellationToken,
) -> Result<WrittenPage> {
    let mut registry = ToolRegistry::new();
    index.register(&mut registry);
    let model = settings.model.clone();
    let result = run_agent(
        AgentRun {
            settings,
            messages: vec![AgentMessage::user_text(page_request(&job.input))],
            tools: Some(Arc::new(registry)),
            context: ToolContext {
                cancel,
                ..ToolContext::default()
            },
            limits: AgentLimits {
                max_tool_rounds: PAGE_TOOL_ROUNDS,
                ..AgentLimits::default()
            },
            cache_key: None,
        },
        &mut |_| Ok(()),
    )?;
    // The page is the last thing it said.
    let text = result
        .appended_messages
        .iter()
        .rev()
        .filter(|message| message.role == AgentRole::Assistant)
        .map(AgentMessage::text)
        .find(|text| !text.trim().is_empty())
        .ok_or_else(|| anyhow!("the model wrote no page"))?;
    let store = index.open()?;
    let mut known = job.known_notes.clone();
    known.extend(
        job.input
            .sources
            .iter()
            .map(|source| source.slug.to_lowercase()),
    );
    let checked = check(&text, &known, |item, chunk| {
        store
            .chunk(chunk)
            .ok()
            .flatten()
            .is_some_and(|location| location.items.contains(&item))
    });
    if checked.cited == 0 {
        bail!("the page cited no passage it read, so it was not written");
    }
    Ok(WrittenPage {
        job,
        body: checked.text,
        cited: checked.cited,
        dropped: checked.dropped,
        model,
        tokens_in: result.usage.input_tokens,
        tokens_out: result.usage.output_tokens,
    })
}

/// A page's text with its citations and links checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub text: String,
    pub cited: usize,
    pub dropped: usize,
}

/// Keeps `[text](glyphst://open?…)` citations whose passage `exists` for
/// their item (others become their text), and `[[links]]` to notes in
/// `known` (others become plain words).
pub fn check(text: &str, known: &HashSet<String>, exists: impl Fn(u64, i64) -> bool) -> Checked {
    let (mut cited, mut dropped) = (0, 0);
    // Citations.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("](glyphst://") {
        let open = rest[..at].rfind('[');
        let close = rest[at + 2..].find(')').map(|end| at + 2 + end);
        let (Some(open), Some(close)) = (open, close) else {
            out.push_str(&rest[..at + 2]);
            rest = &rest[at + 2..];
            continue;
        };
        let label = &rest[open + 1..at];
        let url = &rest[at + 2..close];
        let real = crate::links::parse(url).is_some_and(|link| match (link.item, link.chunk) {
            (Some(item), Some(chunk)) => exists(item, chunk),
            _ => false,
        });
        out.push_str(&rest[..open]);
        if real {
            cited += 1;
            out.push_str(&rest[open..=close]);
        } else {
            dropped += 1;
            out.push_str(label);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    // Links to notes.
    let mut text = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some(open) = rest.find("[[") {
        let Some(close) = rest[open + 2..].find("]]").map(|end| open + 2 + end) else {
            break;
        };
        let inner = &rest[open + 2..close];
        let (target, shown) = inner.split_once('|').unwrap_or((inner, inner));
        text.push_str(&rest[..open]);
        if known.contains(&glyphst_vault::wikilinks::link_key(target)) {
            text.push_str(&rest[open..close + 2]);
        } else {
            text.push_str(shown.split('#').next().unwrap_or(shown).trim());
        }
        rest = &rest[close + 2..];
    }
    text.push_str(rest);
    Checked {
        text,
        cited,
        dropped,
    }
}

/// The page's citations naming their items by uid, as the vault keeps
/// them (the local numbers differ between computers).
fn portable_citations(library: &mut LibraryStore, body: &str) -> Result<String> {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("](glyphst://") {
        let start = at + 2;
        let end = rest[start..]
            .find(')')
            .map_or(rest.len(), |end| start + end);
        let url = &rest[start..end];
        out.push_str(&rest[..start]);
        match crate::links::parse(url).and_then(|link| link.item) {
            Some(item) => {
                let item = library.resolve_item_id(DocumentId(item));
                let uid = library.sync_uid_for(SyncKind::Item, item.0)?;
                out.push_str(&crate::links::portable(url, uid).unwrap_or_else(|| url.to_string()));
            }
            None => out.push_str(url),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A page's file name from its title: lowercase words joined by `-`.
pub fn page_slug(title: &str) -> String {
    let slug = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug.chars().take(60).collect::<String>();
    if slug.is_empty() {
        "page".to_string()
    } else {
        slug
    }
}

/// What writing a page did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub path: PathBuf,
    /// The user had edited the page's text: the new one waits beside it.
    pub beside_edit: bool,
}

/// Writes a checked page into the vault under `root`, merging with what is
/// there, and records it as waiting for the user.
pub fn apply(
    library: &mut LibraryStore,
    root: &Path,
    index: &VaultIndex,
    page: &WrittenPage,
) -> Result<Applied> {
    let id = page.job.page_id;
    let record = library.page(id).cloned();
    let relative = match &record {
        Some(record) => record.relative_path.clone(),
        None => {
            let taken = index
                .notes
                .iter()
                .map(|note| note.key.clone())
                .collect::<HashSet<_>>();
            let slug = glyphst_vault::unique_slug(&page_slug(&page.job.input.title), &taken);
            format!("{PAGES_FOLDER}/{slug}.md")
        }
    };
    let path = root.join(&relative);
    let uid = library.sync_uid_for(SyncKind::Page, id)?;
    let snapshot = PageSnapshot {
        id,
        uid: uid.to_string(),
        title: page.job.input.title.clone(),
        sources: page
            .job
            .input
            .sources
            .iter()
            .map(|source| source.slug.clone())
            .collect(),
        body: portable_citations(library, &page.body)?,
        model: page.model.clone(),
        date: today(),
    };
    let existing = std::fs::read_to_string(&path).ok();
    let (merged, previous) = match (&existing, &record) {
        (Some(existing), Some(record)) => (
            merge_page(
                existing,
                &snapshot,
                &Written {
                    tags: Vec::new(),
                    frontmatter_sha256: record.frontmatter_sha256,
                    blocks: record.blocks.clone(),
                },
            ),
            block_text(existing, PAGE_BLOCK),
        ),
        _ => (render_page(&snapshot), None),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomically(&path, &merged.text)?;
    let beside_edit = !merged.edited_blocks.is_empty();
    library.set_page(
        id,
        Some(PageRecord {
            id,
            title: page.job.input.title.clone(),
            relative_path: relative,
            topic: page.job.topic.clone(),
            sources: page
                .job
                .input
                .sources
                .iter()
                .map(|source| DocumentId(source.id))
                .collect(),
            frontmatter_sha256: merged.written.frontmatter_sha256,
            blocks: merged.written.blocks,
            modified: modified(&path),
            written_at: glyphst_library::current_timestamp(),
            model: page.model.clone(),
            tokens_in: page.tokens_in,
            tokens_out: page.tokens_out,
            // Undo puts the old text back; with the user's edit kept, the
            // new text only waits beside it, so there is nothing to undo.
            previous: if beside_edit { None } else { previous },
            unreviewed: true,
        }),
    )?;
    Ok(Applied { path, beside_edit })
}

/// The user keeps a page as written.
pub fn keep(library: &mut LibraryStore, id: u64) -> Result<()> {
    let mut record = library.page(id).cloned().context("no such page")?;
    record.unreviewed = false;
    record.previous = None;
    library.set_page(id, Some(record))
}

/// Undoes the last write of page `id`: its text before comes back, or a
/// new page is deleted. Refused when the page changed since, or a new page
/// has the user's notes.
pub fn undo(library: &mut LibraryStore, root: &Path, id: u64) -> Result<()> {
    let mut record = library.page(id).cloned().context("no such page")?;
    let path = root.join(&record.relative_path);
    let text = std::fs::read_to_string(&path).context("could not read the page")?;
    let written = record
        .blocks
        .iter()
        .find(|(name, _)| name == PAGE_BLOCK)
        .map(|(_, hash)| *hash);
    let current = block_text(&text, PAGE_BLOCK);
    if current.as_deref().map(block_hash) != written {
        bail!("the page changed since it was written; it was left as it is");
    }
    match record.previous.take() {
        Some(previous) => {
            write_atomically(&path, &replace_block(&text, PAGE_BLOCK, Some(&previous)))?;
            record.blocks.retain(|(name, _)| name != PAGE_BLOCK);
            record
                .blocks
                .push((PAGE_BLOCK.to_string(), block_hash(&previous)));
            record.modified = modified(&path);
            record.unreviewed = false;
            library.set_page(id, Some(record))
        }
        None => {
            let notes = text
                .split_once("\n## Notes")
                .map_or("", |(_, notes)| notes.trim());
            if !notes.is_empty() {
                bail!("the page has your notes; delete it in your editor");
            }
            std::fs::remove_file(&path)?;
            library.set_page(id, None)
        }
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    let temporary = path.with_extension("md.glyphst-writing");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

fn modified(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

/// `YYYY-MM-DD`, as the vault's summaries date themselves.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use glyphst_ai::pages::PageSource;
    use glyphst_search::{ItemText, LibraryFile, Snapshot, Store};

    use super::*;

    #[test]
    fn citations_must_be_real_and_links_must_name_notes() {
        let known = HashSet::from(["vaswani2017attention".to_string()]);
        let text = "One [p 2](glyphst://open?id=3&page=2&chunk=7), two [p 9](glyphst://open?id=3&page=9&chunk=999), \
                    [[Vaswani2017Attention|the paper]] and [[ghost#intro|a ghost]] and [a site](https://example.org).";
        let checked = check(text, &known, |item, chunk| item == 3 && chunk == 7);
        assert_eq!((checked.cited, checked.dropped), (1, 1));
        assert_eq!(
            checked.text,
            "One [p 2](glyphst://open?id=3&page=2&chunk=7), two p 9, \
             [[Vaswani2017Attention|the paper]] and a ghost and [a site](https://example.org)."
        );
        assert_eq!(
            page_slug("Attention: mechanisms & more!"),
            "attention-mechanisms-more"
        );
    }

    #[test]
    fn a_page_is_researched_checked_written_rewritten_and_undone() {
        let dir = std::env::temp_dir().join(format!("glyphst-page-writer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The library's index: one paper about attention.
        let pdf = dir.join("paper.pdf");
        std::fs::write(
            &pdf,
            glyphst_pdf::test_pdf::text_pages_pdf(&[&[
                "Scaled dot product attention weighs tokens",
            ]]),
        )
        .unwrap();
        let index_path = dir.join("search.sqlite");
        {
            let mut store = Store::open(&index_path).unwrap();
            store
                .reconcile(
                    &Snapshot {
                        files: vec![LibraryFile {
                            item_id: 3,
                            attachment_id: 30,
                            path: pdf,
                            sha256: [3; 32],
                            size: 10,
                            priority: 0,
                        }],
                        items: vec![ItemText {
                            item_id: 3,
                            title: "Attention Is All You Need".to_string(),
                            ..ItemText::default()
                        }],
                    },
                    0,
                )
                .unwrap();
            let indexer = glyphst_search::Indexer::start(index_path.clone(), || {}).unwrap();
            let started = std::time::Instant::now();
            while indexer.status().done < 1 {
                assert!(started.elapsed().as_secs() < 30);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let index = IndexHandle::default();
        index.set(Some(index_path));
        let port = crate::fake_providers::start_ai_server().unwrap();
        let mut settings = glyphst_settings::Settings::default();
        crate::fake_providers::use_fake_ai(&mut settings, port);
        let model = settings.ai.resolved_main();

        let job = PageJob {
            page_id: 1,
            topic: PageTopic::Asked("attention".to_string()),
            input: PageInput {
                title: "Attention".to_string(),
                about: "How the papers use attention.".to_string(),
                sources: vec![PageSource {
                    id: 3,
                    slug: "vaswani2017attention".to_string(),
                    title: "Attention Is All You Need".to_string(),
                    year: Some(2017),
                    tldr: String::new(),
                }],
            },
            known_notes: HashSet::new(),
        };
        let page = write_page(job, model, &index, AgentCancellationToken::default()).unwrap();
        assert_eq!((page.cited, page.dropped), (2, 1), "{}", page.body);
        assert!(
            page.body.contains("[[vaswani2017attention]]"),
            "{}",
            page.body
        );
        assert!(page.body.contains("See no-such-note and"), "{}", page.body);
        assert!(!page.body.contains("999999"));

        // Written into concepts/, waiting for the user.
        let vault = dir.join("vault");
        let mut library = LibraryStore::memory();
        let applied = apply(&mut library, &vault, &VaultIndex::default(), &page).unwrap();
        assert_eq!(applied.path, vault.join("concepts/attention.md"));
        assert!(!applied.beside_edit);
        let record = library.page(1).unwrap().clone();
        assert!(record.unreviewed && record.previous.is_none());
        let first = std::fs::read_to_string(&applied.path).unwrap();
        // Named by uids, the same on every computer it syncs to.
        let uid = library
            .sync_uid(glyphst_library::SyncKind::Page, 1)
            .unwrap();
        assert!(
            first.contains(&format!("glyphst_page_uid: {uid}")),
            "{first}"
        );
        let item = library
            .sync_uid(glyphst_library::SyncKind::Item, 3)
            .unwrap();
        assert!(
            first.contains(&format!("glyphst://open?uid={item}&page=")),
            "{first}"
        );
        assert!(!first.contains("glyphst://open?id="), "{first}");

        // The user keeps it and adds notes; a rewrite keeps the notes, and
        // undo brings the first text back.
        keep(&mut library, 1).unwrap();
        std::fs::write(
            &applied.path,
            first.replace("## Notes\n\n", "## Notes\n\nCompare with RoPE.\n"),
        )
        .unwrap();
        let mut again = page.clone();
        again.body = "A second take [p 1](glyphst://open?id=3&page=1&chunk=1).".to_string();
        apply(&mut library, &vault, &VaultIndex::default(), &again).unwrap();
        let second = std::fs::read_to_string(&applied.path).unwrap();
        assert!(second.contains("A second take") && second.contains("Compare with RoPE."));
        undo(&mut library, &vault, 1).unwrap();
        let undone = std::fs::read_to_string(&applied.path).unwrap();
        assert!(
            undone.contains("Attention lets a model weigh")
                && undone.contains("Compare with RoPE.")
        );
        assert!(!library.page(1).unwrap().unreviewed);

        // A new page undone is deleted, unless it has the user's notes.
        let mut other = page.clone();
        other.job.page_id = 2;
        other.job.input.title = "Tokens".to_string();
        let applied = apply(&mut library, &vault, &VaultIndex::default(), &other).unwrap();
        undo(&mut library, &vault, 2).unwrap();
        assert!(!applied.path.exists() && library.page(2).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
