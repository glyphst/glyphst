//! Markdown parsed into the blocks the view lays out. The dollars that are
//! not formulas are escaped first (see `dollars`), so pulldown-cmark's math
//! extension only sees real delimiters.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use crate::dollars;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Quote(Vec<Block>),
    List {
        start: Option<u64>,
        items: Vec<Item>,
    },
    Code {
        lang: String,
        text: String,
        /// Still streaming: its fence hasn't closed.
        open: bool,
    },
    /// Display math, `$$…$$`.
    Math(String),
    Table {
        align: Vec<Alignment>,
        head: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Rule,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Item {
    /// `Some(done)` for a task list item.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Inline {
    Text {
        text: String,
        strong: bool,
        emphasis: bool,
        strike: bool,
        link: Option<String>,
    },
    Code(String),
    Math(String),
    /// A hard line break.
    Break,
}

fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

/// A container being filled while the events stream past.
enum Frame {
    Blocks(Vec<Block>),
    Quote(Vec<Block>),
    List(Option<u64>, Vec<Item>),
    Item(Item),
    /// A footnote definition, drawn as its blocks after a `[label]` marker.
    Footnote(String, Vec<Block>),
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Code(String, String),
    Table(Table),
    Html(String),
}

#[derive(Default)]
struct Table {
    align: Vec<Alignment>,
    head: Vec<Vec<Inline>>,
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
    cell: Vec<Inline>,
}

#[derive(Default)]
struct Spans {
    strong: u32,
    emphasis: u32,
    strike: u32,
    links: Vec<String>,
}

struct Builder {
    stack: Vec<Frame>,
    spans: Spans,
    /// The fenced code block being built has no closing fence yet.
    fence_open: bool,
}

/// The blocks of `source`; its plain text if pulldown-cmark panics, which
/// a chat message must not turn into a crash.
pub(crate) fn parse(source: &str) -> Vec<Block> {
    std::panic::catch_unwind(|| parse_blocks(source)).unwrap_or_else(|_| {
        vec![Block::Paragraph(vec![Inline::Text {
            text: source.to_owned(),
            strong: false,
            emphasis: false,
            strike: false,
            link: None,
        }])]
    })
}

fn parse_blocks(source: &str) -> Vec<Block> {
    let options = options();
    let source = dollars::prepare(source, options);
    let mut builder = Builder {
        stack: vec![Frame::Blocks(Vec::new())],
        spans: Spans::default(),
        fence_open: false,
    };
    for (event, range) in Parser::new_ext(&source, options).into_offset_iter() {
        // pulldown-cmark closes a fence at the end of the text itself, so
        // a streaming block is told apart by its source.
        if let Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) = &event {
            builder.fence_open = !closes_fence(&source[range]);
        }
        builder.event(event);
    }
    builder.finish()
}

impl Builder {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if let Some(Frame::Code(_, code) | Frame::Html(code)) = self.stack.last_mut() {
                    code.push_str(&text);
                } else {
                    self.text(&text);
                }
            }
            Event::Code(code) => self.inline(Inline::Code(code.into_string())),
            Event::InlineMath(tex) => self.inline(Inline::Math(tex.into_string())),
            Event::DisplayMath(tex) => self.display_math(tex.into_string()),
            Event::Html(html) => match self.stack.last_mut() {
                Some(Frame::Html(text)) => text.push_str(&html),
                _ => self.text(&html),
            },
            Event::InlineHtml(html) => self.text(&html),
            Event::FootnoteReference(label) => self.text(&format!("[{label}]")),
            Event::SoftBreak => self.text(" "),
            Event::HardBreak => self.inline(Inline::Break),
            Event::Rule => self.block(Block::Rule),
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item(item)) = self.stack.last_mut() {
                    item.task = Some(done);
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.close_paragraph();
                self.stack.push(Frame::Paragraph(Vec::new()));
            }
            Tag::Heading { level, .. } => {
                self.close_paragraph();
                self.stack.push(Frame::Heading(level as u8, Vec::new()));
            }
            Tag::BlockQuote(_) => {
                self.close_paragraph();
                self.stack.push(Frame::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.close_paragraph();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.stack.push(Frame::Code(lang, String::new()));
            }
            Tag::HtmlBlock => {
                self.close_paragraph();
                self.stack.push(Frame::Html(String::new()));
            }
            Tag::List(start) => {
                self.close_paragraph();
                self.stack.push(Frame::List(start, Vec::new()));
            }
            Tag::Item => self.stack.push(Frame::Item(Item::default())),
            Tag::FootnoteDefinition(label) => {
                self.close_paragraph();
                self.stack
                    .push(Frame::Footnote(label.into_string(), Vec::new()));
            }
            Tag::Table(align) => {
                self.close_paragraph();
                self.stack.push(Frame::Table(Table {
                    align,
                    ..Table::default()
                }));
            }
            Tag::Emphasis => self.spans.emphasis += 1,
            Tag::Strong => self.spans.strong += 1,
            Tag::Strikethrough => self.spans.strike += 1,
            Tag::Link { dest_url, .. } => self.spans.links.push(dest_url.into_string()),
            Tag::Image { dest_url, .. } => self.spans.links.push(dest_url.into_string()),
            Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::Superscript
            | Tag::Subscript
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::MetadataBlock(_) => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.close_paragraph(),
            TagEnd::Heading(_) => {
                if let Some(Frame::Heading(level, inlines)) = self.stack.pop() {
                    self.block(Block::Heading(level, inlines));
                }
            }
            TagEnd::BlockQuote(_) => {
                self.close_paragraph();
                if let Some(Frame::Quote(blocks)) = self.stack.pop() {
                    self.block(Block::Quote(blocks));
                }
            }
            TagEnd::CodeBlock => {
                if let Some(Frame::Code(lang, mut text)) = self.stack.pop() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    let open = std::mem::take(&mut self.fence_open);
                    self.block(Block::Code { lang, text, open });
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(Frame::Html(text)) = self.stack.pop() {
                    let text = text.trim_end().to_owned();
                    if !text.is_empty() {
                        self.block(Block::Code {
                            lang: "html".into(),
                            text,
                            open: false,
                        });
                    }
                }
            }
            TagEnd::List(_) => {
                self.close_paragraph();
                if let Some(Frame::List(start, items)) = self.stack.pop() {
                    self.block(Block::List { start, items });
                }
            }
            TagEnd::Item => {
                self.close_paragraph();
                if let Some(Frame::Item(item)) = self.stack.pop()
                    && let Some(Frame::List(_, items)) = self.stack.last_mut()
                {
                    items.push(item);
                }
            }
            TagEnd::FootnoteDefinition => {
                self.close_paragraph();
                if let Some(Frame::Footnote(label, mut blocks)) = self.stack.pop() {
                    let marker = Inline::Text {
                        text: format!("[{label}] "),
                        strong: false,
                        emphasis: false,
                        strike: false,
                        link: None,
                    };
                    match blocks.first_mut() {
                        Some(Block::Paragraph(inlines)) => inlines.insert(0, marker),
                        _ => blocks.insert(0, Block::Paragraph(vec![marker])),
                    }
                    for block in blocks {
                        self.block(block);
                    }
                }
            }
            TagEnd::Table => {
                if let Some(Frame::Table(table)) = self.stack.pop() {
                    self.block(Block::Table {
                        align: table.align,
                        head: table.head,
                        rows: table.rows,
                    });
                }
            }
            TagEnd::TableHead => {
                if let Some(Frame::Table(table)) = self.stack.last_mut() {
                    table.head = std::mem::take(&mut table.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(Frame::Table(table)) = self.stack.last_mut() {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                if let Some(Frame::Table(table)) = self.stack.last_mut() {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell);
                }
            }
            TagEnd::Emphasis => self.spans.emphasis = self.spans.emphasis.saturating_sub(1),
            TagEnd::Strong => self.spans.strong = self.spans.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.spans.strike = self.spans.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => {
                self.spans.links.pop();
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        let inline = Inline::Text {
            text: text.to_owned(),
            strong: self.spans.strong > 0,
            emphasis: self.spans.emphasis > 0,
            strike: self.spans.strike > 0,
            link: self.spans.links.last().cloned(),
        };
        self.inline(inline);
    }

    /// Adds `inline` to the paragraph, heading or table cell being built,
    /// opening a paragraph in a container (tight list items have none).
    fn inline(&mut self, inline: Inline) {
        match self.stack.last_mut() {
            Some(Frame::Paragraph(inlines) | Frame::Heading(_, inlines)) => {
                push_inline(inlines, inline);
            }
            Some(Frame::Table(table)) => push_inline(&mut table.cell, inline),
            Some(Frame::Code(_, text) | Frame::Html(text)) => {
                if let Inline::Text { text: more, .. } = inline {
                    text.push_str(&more);
                }
            }
            _ => self.stack.push(Frame::Paragraph(vec![inline])),
        }
    }

    /// Display math is a block of its own: the paragraph around it splits.
    fn display_math(&mut self, tex: String) {
        match self.stack.last() {
            Some(Frame::Heading(..) | Frame::Table(_)) => self.inline(Inline::Math(tex)),
            Some(Frame::Paragraph(_)) => {
                self.close_paragraph();
                self.block(Block::Math(tex));
                self.stack.push(Frame::Paragraph(Vec::new()));
            }
            _ => self.block(Block::Math(tex)),
        }
    }

    fn close_paragraph(&mut self) {
        if matches!(self.stack.last(), Some(Frame::Paragraph(_)))
            && let Some(Frame::Paragraph(mut inlines)) = self.stack.pop()
        {
            trim_edges(&mut inlines);
            if !inlines.is_empty() {
                self.block(Block::Paragraph(inlines));
            }
        }
    }

    fn block(&mut self, block: Block) {
        self.close_paragraph();
        match self.stack.last_mut() {
            Some(
                Frame::Blocks(blocks)
                | Frame::Quote(blocks)
                | Frame::Footnote(_, blocks)
                | Frame::Item(Item { blocks, .. }),
            ) => blocks.push(block),
            // A block where none belongs (malformed nesting): keep it at
            // the top level rather than lose it.
            _ => {
                if let Some(Frame::Blocks(blocks)) = self.stack.first_mut() {
                    blocks.push(block);
                }
            }
        }
    }

    fn finish(mut self) -> Vec<Block> {
        // Close whatever a truncated (streaming) source left open.
        while self.stack.len() > 1 {
            match self.stack.last() {
                Some(Frame::Paragraph(_)) => self.close_paragraph(),
                Some(Frame::Heading(..)) => {
                    self.end(TagEnd::Heading(pulldown_cmark::HeadingLevel::H1))
                }
                Some(Frame::Quote(_)) => self.end(TagEnd::BlockQuote(None)),
                Some(Frame::List(..)) => self.end(TagEnd::List(false)),
                Some(Frame::Item(_)) => self.end(TagEnd::Item),
                Some(Frame::Footnote(..)) => self.end(TagEnd::FootnoteDefinition),
                Some(Frame::Code(..)) => self.end(TagEnd::CodeBlock),
                Some(Frame::Table(_)) => self.end(TagEnd::Table),
                Some(Frame::Html(_)) => self.end(TagEnd::HtmlBlock),
                Some(Frame::Blocks(_)) | None => break,
            }
        }
        match self.stack.pop() {
            Some(Frame::Blocks(blocks)) => blocks,
            _ => Vec::new(),
        }
    }
}

/// Whether a fenced block's source ends with a fence that closes it: at
/// least as long as the opening one, of the same character.
fn closes_fence(block: &str) -> bool {
    let block = block.trim_end();
    let Some((first, rest)) = block.split_once('\n') else {
        return false;
    };
    let first = first.trim_start();
    let Some(fence) = first.chars().next().filter(|c| matches!(c, '`' | '~')) else {
        return false;
    };
    let length = first.chars().take_while(|c| *c == fence).count();
    let last = rest.lines().last().unwrap_or_default().trim();
    last.chars().count() >= length && last.chars().all(|c| c == fence)
}

/// Appends `inline`, merging text runs of the same style so a paragraph
/// lays out as few sections as possible.
fn push_inline(inlines: &mut Vec<Inline>, inline: Inline) {
    if let (
        Some(Inline::Text {
            text,
            strong,
            emphasis,
            strike,
            link,
        }),
        Inline::Text {
            text: more,
            strong: s,
            emphasis: e,
            strike: k,
            link: l,
        },
    ) = (inlines.last_mut(), &inline)
        && (*strong, *emphasis, *strike, &*link) == (*s, *e, *k, l)
    {
        text.push_str(more);
        return;
    }
    inlines.push(inline);
}

/// Drops the spaces a paragraph starts or ends with (soft breaks at its
/// edges after a split around display math).
fn trim_edges(inlines: &mut Vec<Inline>) {
    if let Some(Inline::Text { text, .. }) = inlines.first_mut() {
        let trimmed = text.trim_start().len();
        text.drain(..text.len() - trimmed);
    }
    if let Some(Inline::Text { text, .. }) = inlines.last_mut() {
        text.truncate(text.trim_end().len());
    }
    inlines.retain(|inline| !matches!(inline, Inline::Text { text, .. } if text.is_empty()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> Inline {
        Inline::Text {
            text: text.into(),
            strong: false,
            emphasis: false,
            strike: false,
            link: None,
        }
    }

    #[test]
    fn paragraphs_merge_soft_breaks_and_keep_styles() {
        let blocks = parse("one\ntwo **bold** `code`");
        let [Block::Paragraph(inlines)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(inlines[0], text("one two "));
        assert!(matches!(&inlines[1], Inline::Text { text, strong: true, .. } if text == "bold"));
        assert_eq!(inlines[3], Inline::Code("code".into()));
    }

    #[test]
    fn tight_list_items_get_paragraphs_and_tasks() {
        let blocks = parse("- [x] done\n- open\n  - nested");
        let [Block::List { start: None, items }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(items[0].task, Some(true));
        assert_eq!(items[0].blocks, vec![Block::Paragraph(vec![text("done")])]);
        assert!(matches!(
            items[1].blocks.as_slice(),
            [Block::Paragraph(_), Block::List { .. }]
        ));
    }

    #[test]
    fn tables_keep_head_rows_and_alignment() {
        let blocks = parse("| a | b |\n|:--|--:|\n| 1 | **2** |\n| 3 | 4 |");
        let [Block::Table { align, head, rows }] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(align, &[Alignment::Left, Alignment::Right]);
        assert_eq!(head[0], vec![text("a")]);
        assert_eq!(rows.len(), 2);
        assert!(matches!(&rows[0][1][0], Inline::Text { strong: true, .. }));
    }

    #[test]
    fn display_math_splits_its_paragraph() {
        let blocks = parse("before\n$$x^2$$\nafter");
        assert_eq!(
            blocks,
            vec![
                Block::Paragraph(vec![text("before")]),
                Block::Math("x^2".into()),
                Block::Paragraph(vec![text("after")]),
            ]
        );
    }

    #[test]
    fn inline_math_and_prices() {
        let blocks = parse("cost $5 and $10, size $n$");
        let [Block::Paragraph(inlines)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert_eq!(inlines.last(), Some(&Inline::Math("n".into())));
        assert!(
            matches!(&inlines[0], Inline::Text { text, .. } if text == "cost $5 and $10, size ")
        );
    }

    #[test]
    fn code_blocks_keep_language_and_text() {
        let blocks = parse("```mermaid\ngraph TD\n  A-->B\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                lang: "mermaid".into(),
                text: "graph TD\n  A-->B".into(),
                open: false,
            }]
        );
    }

    #[test]
    fn only_a_closed_fence_is_complete() {
        let closed = parse("```mermaid\ngraph TD\n```\n");
        let open = parse("```mermaid\ngraph TD\n  A-->");
        let short = parse("````\ncode\n```");
        assert!(matches!(
            closed.as_slice(),
            [Block::Code { open: false, .. }]
        ));
        assert!(matches!(open.as_slice(), [Block::Code { open: true, .. }]));
        assert!(matches!(short.as_slice(), [Block::Code { open: true, .. }]));
    }

    #[test]
    fn a_truncated_stream_still_parses() {
        let blocks = parse("> quote\n> - item\n```rust\nfn main");
        assert!(
            matches!(blocks.last(), Some(Block::Code { lang, open: true, .. }) if lang == "rust")
        );
    }

    #[test]
    fn links_carry_their_destination() {
        let blocks = parse("see [docs](https://example.com)");
        let [Block::Paragraph(inlines)] = blocks.as_slice() else {
            panic!("{blocks:?}");
        };
        assert!(
            matches!(&inlines[1], Inline::Text { link: Some(url), .. } if url == "https://example.com")
        );
    }
}
