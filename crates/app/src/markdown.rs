use gpui::{
    AbsoluteLength, Div, FontStyle, FontWeight, Rems, SharedString, StrikethroughStyle,
    StyledText, TextRun, TextStyle, TextStyleRefinement, UnderlineStyle, div, prelude::*, px, rems,
    rgb, rgba,
};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme::Theme;

const MONO_FONT_FAMILY: &str = ".ZedMono";

/// Renders markdown content as a gpui [Div] element tree.
///
/// Follows zed editor's approach: parse with `pulldown-cmark`, then build a tree
/// of block-level `div` containers with inline text rendered via [StyledText]
/// using a stack of [TextStyleRefinement]s for bold, italic, code, links, etc.
pub(crate) fn render_markdown(source: &str, theme: Theme, base_text_style: TextStyle) -> Div {
    let options = Options::ENABLE_TABLES
        .union(Options::ENABLE_STRIKETHROUGH)
        .union(Options::ENABLE_TASKLISTS);

    let mut builder = MarkdownBuilder::new(theme, base_text_style);
    for event in Parser::new_ext(source, options) {
        builder.process_event(event);
    }
    builder.build()
}

struct MarkdownBuilder {
    theme: Theme,
    base_text_style: TextStyle,
    div_stack: Vec<Div>,
    text_style_stack: Vec<TextStyleRefinement>,
    pending_text: String,
    pending_runs: Vec<TextRun>,
    list_stack: Vec<Option<u64>>,
    table_alignments: Vec<Alignment>,
    in_table_head: bool,
    table_col_count: usize,
    table_col_index: usize,
}

impl MarkdownBuilder {
    fn new(theme: Theme, base_text_style: TextStyle) -> Self {
        Self {
            theme,
            base_text_style,
            div_stack: vec![div().flex().flex_col().w_full()],
            text_style_stack: Vec::new(),
            pending_text: String::new(),
            pending_runs: Vec::new(),
            list_stack: Vec::new(),
            table_alignments: Vec::new(),
            in_table_head: false,
            table_col_count: 0,
            table_col_index: 0,
        }
    }

    fn text_style(&self) -> TextStyle {
        let mut style = self.base_text_style.clone();
        for refinement in &self.text_style_stack {
            style.refine(refinement);
        }
        style
    }

    fn push_text_style(&mut self, refinement: TextStyleRefinement) {
        self.text_style_stack.push(refinement);
    }

    fn pop_text_style(&mut self) {
        self.text_style_stack.pop();
    }

    fn push_div(&mut self, div: Div) {
        self.flush_text();
        self.div_stack.push(div);
    }

    fn pop_div(&mut self) {
        self.flush_text();
        let div = self.div_stack.pop().unwrap();
        self.append_child(div.into_any_element());
    }

    fn append_child(&mut self, child: gpui::AnyElement) {
        self.div_stack.last_mut().unwrap().extend([child]);
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let style = self.text_style();
        self.pending_text.push_str(text);
        self.pending_runs.push(style.to_run(text.len()));
    }

    fn flush_text(&mut self) {
        let text = std::mem::take(&mut self.pending_text);
        let runs = std::mem::take(&mut self.pending_runs);
        if text.is_empty() {
            return;
        }
        let styled = StyledText::new(SharedString::from(text)).with_runs(runs);
        self.append_child(styled.into_any_element());
    }

    fn trim_trailing_newline(&mut self) {
        if self.pending_text.ends_with('\n') {
            self.pending_text.truncate(self.pending_text.len() - 1);
            if let Some(last) = self.pending_runs.last_mut() {
                last.len -= 1;
            }
        }
    }

    fn process_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.process_start(tag),
            Event::End(tag) => self.process_end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => self.process_code_span(&code),
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.push_text("\n"),
            Event::Rule => self.process_rule(),
            Event::TaskListMarker(checked) => self.push_text(if checked { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }

    fn process_start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.push_div(div().mb_2().line_height(rems(1.3)));
            }
            Tag::Heading { level, .. } => {
                let factor = heading_scale(level);
                let heading_div = div()
                    .mt_4()
                    .mb_2()
                    .line_height(rems(1.2))
                    .text_color(rgb(self.theme.text));
                self.push_div(heading_div);
                self.push_text_style(TextStyleRefinement {
                    font_size: Some(scaled_font_size(&self.base_text_style, factor)),
                    font_weight: Some(FontWeight::BOLD),
                    color: Some(rgb(self.theme.text).into()),
                    ..Default::default()
                });
            }
            Tag::BlockQuote(_) => {
                self.push_div(
                    div()
                        .border_l_4()
                        .border_color(rgb(self.theme.border_variant))
                        .pl_4()
                        .mb_2(),
                );
                self.push_text_style(TextStyleRefinement {
                    color: Some(rgb(self.theme.text_muted).into()),
                    ..Default::default()
                });
            }
            Tag::CodeBlock(kind) => {
                let language = match &kind {
                    CodeBlockKind::Fenced(lang) if !lang.trim().is_empty() => {
                        Some(lang.trim().to_string())
                    }
                    _ => None,
                };
                self.push_div(
                    div()
                        .bg(rgb(self.theme.panel_background))
                        .border_1()
                        .border_color(rgb(self.theme.border_variant))
                        .rounded(px(4.0))
                        .p_3()
                        .mb_2(),
                );
                if let Some(ref lang) = language {
                    self.flush_text();
                    self.append_child(
                        div()
                            .text_color(rgb(self.theme.text_muted))
                            .text_size(rems(0.75))
                            .mb_1()
                            .child(lang.clone())
                            .into_any_element(),
                    );
                }
                self.push_text_style(TextStyleRefinement {
                    font_family: Some(MONO_FONT_FAMILY.into()),
                    color: Some(rgb(self.theme.text).into()),
                    ..Default::default()
                });
            }
            Tag::List(start) => {
                self.list_stack.push(start);
                self.push_div(div().flex().flex_col().gap_0p5().mb_2().pl_4());
            }
            Tag::Item => {
                let bullet = match self.list_stack.last_mut() {
                    Some(Some(n)) => {
                        let b = format!("{n}. ");
                        *n += 1;
                        b
                    }
                    _ => "• ".to_string(),
                };
                self.push_div(div().flex().flex_row().items_start().gap_1());
                self.append_child(div().child(bullet).into_any_element());
                self.push_div(div().flex_1().w_0());
            }
            Tag::Emphasis => {
                self.push_text_style(TextStyleRefinement {
                    font_style: Some(FontStyle::Italic),
                    ..Default::default()
                });
            }
            Tag::Strong => {
                self.push_text_style(TextStyleRefinement {
                    font_weight: Some(FontWeight::BOLD),
                    color: Some(rgb(self.theme.text).into()),
                    ..Default::default()
                });
            }
            Tag::Strikethrough => {
                self.push_text_style(TextStyleRefinement {
                    strikethrough: Some(StrikethroughStyle {
                        thickness: px(1.0),
                        color: None,
                    }),
                    ..Default::default()
                });
            }
            Tag::Link { .. } => {
                self.push_text_style(TextStyleRefinement {
                    color: Some(rgb(self.theme.accent).into()),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(rgb(self.theme.accent).into()),
                        wavy: false,
                    }),
                    ..Default::default()
                });
            }
            Tag::Image { .. } => {}
            Tag::Table(alignments) => {
                self.table_col_count = alignments.len();
                self.table_alignments = alignments;
                self.table_col_index = 0;
                self.push_div(
                    div()
                        .grid()
                        .grid_cols(self.table_col_count as u16)
                        .w_full()
                        .mb_2()
                        .border_1()
                        .border_color(rgb(self.theme.border))
                        .rounded(px(4.0))
                        .overflow_hidden(),
                );
            }
            Tag::TableHead => {
                self.in_table_head = true;
                self.push_text_style(TextStyleRefinement {
                    font_weight: Some(FontWeight::BOLD),
                    ..Default::default()
                });
            }
            Tag::TableRow => {
                self.table_col_index = 0;
            }
            Tag::TableCell => {
                let alignment = self
                    .table_alignments
                    .get(self.table_col_index)
                    .copied()
                    .unwrap_or(Alignment::None);
                let mut cell = div()
                    .flex()
                    .flex_col()
                    .h_full()
                    .px_1()
                    .py_0p5()
                    .when(self.table_col_index > 0, |d| d.border_l_1())
                    .border_color(rgb(self.theme.border));
                if self.in_table_head {
                    cell = cell.bg(rgb(self.theme.panel_background));
                }
                cell = match alignment {
                    Alignment::Center => cell.items_center().text_center(),
                    Alignment::Right => cell.items_end().text_right(),
                    _ => cell.items_start().text_left(),
                };
                self.push_div(cell);
            }
            Tag::HtmlBlock => {
                self.push_div(div().mb_2());
            }
            _ => {}
        }
    }

    fn process_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.pop_div();
            }
            TagEnd::Heading(_) => {
                self.pop_text_style();
                self.pop_div();
            }
            TagEnd::BlockQuote(_) => {
                self.pop_text_style();
                self.pop_div();
            }
            TagEnd::CodeBlock => {
                self.trim_trailing_newline();
                self.pop_text_style();
                self.pop_div();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.pop_div();
            }
            TagEnd::Item => {
                self.pop_div();
                self.pop_div();
            }
            TagEnd::Emphasis => {
                self.pop_text_style();
            }
            TagEnd::Strong => {
                self.pop_text_style();
            }
            TagEnd::Strikethrough => {
                self.pop_text_style();
            }
            TagEnd::Link => {
                self.pop_text_style();
            }
            TagEnd::Image => {}
            TagEnd::Table => {
                self.pop_div();
            }
            TagEnd::TableHead => {
                self.pop_text_style();
                self.in_table_head = false;
            }
            TagEnd::TableRow => {}
            TagEnd::TableCell => {
                self.pop_div();
                self.table_col_index += 1;
            }
            TagEnd::HtmlBlock => {
                self.pop_div();
            }
            _ => {}
        }
    }

    fn process_code_span(&mut self, code: &str) {
        let bg = rgba((self.theme.control_background << 8) | 0x88).into();
        self.push_text_style(TextStyleRefinement {
            font_family: Some(MONO_FONT_FAMILY.into()),
            background_color: Some(bg),
            color: Some(rgb(self.theme.text).into()),
            ..Default::default()
        });
        self.push_text(code);
        self.pop_text_style();
    }

    fn process_rule(&mut self) {
        self.push_div(
            div()
                .border_b_1()
                .my_2()
                .border_color(rgb(self.theme.border)),
        );
        self.pop_div();
    }

    fn build(mut self) -> Div {
        self.flush_text();
        self.div_stack.pop().unwrap()
    }
}

fn heading_scale(level: HeadingLevel) -> f32 {
    match level {
        HeadingLevel::H1 => 1.6,
        HeadingLevel::H2 => 1.4,
        HeadingLevel::H3 => 1.2,
        HeadingLevel::H4 => 1.1,
        HeadingLevel::H5 => 1.0,
        HeadingLevel::H6 => 0.9,
    }
}

fn scaled_font_size(base: &TextStyle, factor: f32) -> AbsoluteLength {
    match base.font_size {
        AbsoluteLength::Pixels(px) => AbsoluteLength::Pixels(px * factor),
        AbsoluteLength::Rems(r) => AbsoluteLength::Rems(Rems(r.0 * factor)),
    }
}
